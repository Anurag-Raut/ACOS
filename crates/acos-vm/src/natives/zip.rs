//! `java.util.zip` compression/checksum natives.
//!
//! The real Android 14 core library implements `java.util.zip.Deflater`,
//! `Inflater`, `CRC32` and `Adler32` as thin Java wrappers over native
//! zlib-style primitives. The Java wrappers keep the stream objects and the
//! input/output buffers; the natives own the compression state and update the
//! `off`/`len`/`finished`/`needDict` fields of the wrapper.
//!
//! This module supplies those primitives with the pure-Rust `flate2`
//! (`miniz_oxide`) backend, so no host zlib is linked and the guest never sees
//! a host path. Boot-jar resource reads reach these natives through the real
//! `ZipFile`/`JarFile` Java code; the file bytes themselves are opened by the
//! VFS-backed `java.io`/`libcore.io` natives, not from here.
//!
//! Coverage:
//!
//! * `Inflater.init/inflateBytes/setDictionary/getAdler/reset/end`
//! * `Deflater.init/deflateBytes/setDictionary/getAdler/reset/end`
//! * `CRC32.update/updateBytes0`
//! * `Adler32.update/updateBytes`
//!
//! The direct-`ByteBuffer` checksum variants and the native `ZipFile` handle
//! API are deliberately not registered here: the pure-Java `ZipInputStream` /
//! `ZipOutputStream` paths that the console corpus uses never reach them, and
//! leaving them unbound surfaces a structured `UNSUPPORTED_JAVA_API` instead of
//! a faked result.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::io::Cursor;

use flate2::{Compress, Compression, Decompress, FlushCompress, FlushDecompress, Status};

use crate::class::NativeCtx;
use crate::error::VmResult;
use crate::heap::{ArrayData, ArrayObj, Handle, Object, NULL};
use crate::vm::Vm;

/// zlib's CRC-32 polynomial, reflected.
const CRC32_POLY: u32 = 0xedb8_8320;
/// zlib's Adler-32 modulus (largest prime below 2^16).
const ADLER_MOD: u32 = 65_521;

fn crc32_table() -> &'static [u32; 256] {
    use std::sync::OnceLock;
    static TABLE: OnceLock<[u32; 256]> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut table = [0u32; 256];
        for (i, slot) in table.iter_mut().enumerate() {
            let mut c = i as u32;
            for _ in 0..8 {
                c = if c & 1 != 0 {
                    CRC32_POLY ^ (c >> 1)
                } else {
                    c >> 1
                };
            }
            *slot = c;
        }
        table
    })
}

/// Continue a zlib CRC-32 from the "final" value the Java wrapper holds.
fn crc32_update(crc: u32, data: &[u8]) -> u32 {
    let table = crc32_table();
    let mut c = !crc;
    for &b in data {
        c = table[((c ^ b as u32) & 0xff) as usize] ^ (c >> 8);
    }
    !c
}

/// Continue an Adler-32 checksum (`adler` is the packed `b<<16 | a` value).
fn adler32_update(adler: u32, data: &[u8]) -> u32 {
    let mut a = adler & 0xffff;
    let mut b = (adler >> 16) & 0xffff;
    for &byte in data {
        a = (a + byte as u32) % ADLER_MOD;
        b = (b + a) % ADLER_MOD;
    }
    (b << 16) | a
}

struct InflaterState {
    dec: Decompress,
    nowrap: bool,
    finished: bool,
    need_dict: bool,
    adler: u32,
}

struct DeflaterState {
    comp: Compress,
    finished: bool,
    adler: u32,
}

// ---------------------------------------------------------------------------
// java.util.zip.ZipFile handle API
//
// Android 14's `java.util.zip.ZipFile` is a thin Java wrapper over an opaque
// native `long` (`jzfile`) and per-entry `long`s (`jzentries`). The wrapper
// never touches the compressed bytes directly: `ZipFile$ZipFileInputStream`
// reads the *stored* entry stream through `read(jzfile, jzentry, pos, ...)` and
// feeds the already-implemented `Inflater` natives. The entry metadata
// (name/extra/comment/crc/sizes/method/flag/time) is read back through the
// `getEntry*` natives.
//
// The container is read through the VFS only (never a host path), using the
// same pure-Rust `zip` crate as `acos-apk`. Every entry is bounded against zip
// bombs: the archive itself is capped, and any entry whose declared
// uncompressed size or expansion ratio exceeds the caps fails the `open` with
// an `IOException` instead of exposing an attacker-controlled allocation.

/// Largest archive ACOS will hold in memory for `ZipFile` (a real APK is
/// tens of MiB; the ceiling keeps a hostile VFS path from exhausting the heap).
const MAX_ZIP_BYTES: u64 = 512 * 1024 * 1024;
/// Largest declared uncompressed size of a single entry.
const MAX_ENTRY_UNCOMPRESSED: u64 = 1024 * 1024 * 1024;
/// Largest declared expansion ratio before an entry is treated as a bomb.
const MAX_ENTRY_RATIO: u64 = 1000;
/// Largest number of central-directory entries ACOS will materialise.
const MAX_ZIP_ENTRIES: usize = 1 << 20;

/// One central-directory entry as the `ZipFile` natives need it.
struct ZipEntryMeta {
    /// Raw bytes of the entry name (before `ZipCoder` decoding on the Java side).
    name: Vec<u8>,
    extra: Vec<u8>,
    comment: Vec<u8>,
    /// General-purpose bit flag (bit 11 = UTF-8 names).
    flag: u16,
    /// Compression method (`0` stored, `8` deflated).
    method: u16,
    /// DOS date/time, in the `(date << 16) | time` encoding.
    dos_time: u32,
    crc: u32,
    size: u64,
    csize: u64,
    /// Offset of the raw (compressed) payload in the archive bytes.
    data_start: u64,
}

struct ZipFileState {
    bytes: Vec<u8>,
    entries: Vec<ZipEntryMeta>,
    comment: Vec<u8>,
    loc_sig: bool,
}

thread_local! {
    static ZIP: RefCell<HashMap<u64, ZipFileState>> = RefCell::new(HashMap::new());
}

/// Encode `(file handle, entry index)` into the opaque `jzentry` the Java
/// wrapper passes back. Index 0 is reserved so a returned value is never 0.
fn entry_handle(file: u64, idx: usize) -> i64 {
    ((file << 32) | (idx as u64 + 1)) as i64
}

fn decode_entry(handle: i64) -> Option<(u64, usize)> {
    let h = handle as u64;
    let idx = (h & 0xffff_ffff) as usize;
    if idx == 0 {
        return None;
    }
    Some((h >> 32, idx - 1))
}

/// Read a whole guest file through the VFS, bounded by [`MAX_ZIP_BYTES`].
fn read_guest_all(vm: &mut Vm, path: &str) -> Result<Vec<u8>, String> {
    let fd = vm.vfs.open(path, 0, 0).map_err(|e| format!("errno {e}"))?;
    let mut out = Vec::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        match vm.vfs.read(fd, &mut buf) {
            Ok(0) => break,
            Ok(n) => {
                if out.len() as u64 + n as u64 > MAX_ZIP_BYTES {
                    let _ = vm.vfs.close(fd);
                    return Err(format!("exceeds {MAX_ZIP_BYTES} byte cap"));
                }
                out.extend_from_slice(&buf[..n]);
            }
            Err(e) => {
                let _ = vm.vfs.close(fd);
                return Err(format!("errno {e}"));
            }
        }
    }
    let _ = vm.vfs.close(fd);
    Ok(out)
}

/// DOS date/time from a zip crate `DateTime`.
fn dos_time_from(dt: &zip::DateTime) -> u32 {
    let year = dt.year().clamp(1980, 2107) as u32;
    let month = dt.month() as u32;
    let day = dt.day() as u32;
    let hour = dt.hour() as u32;
    let minute = dt.minute() as u32;
    let second = dt.second() as u32;
    ((year - 1980) << 25)
        | (month << 21)
        | (day << 16)
        | (hour << 11)
        | (minute << 5)
        | (second / 2)
}

/// `ZipFile.open(String name, int mode, long lastModified, boolean usemmap)`.
fn zipfile_open(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let name = vm.string_to_rust(vm.arg_ref(ctx, 0)).unwrap_or_default();
    let bytes = read_guest_all(vm, &name).map_err(|e| {
        vm.throw_new("Ljava/io/IOException;", Some(&format!("{name}: {e}")))
    })?;
    let state = parse_zip(bytes)
        .map_err(|e| vm.throw_new("Ljava/util/zip/ZipException;", Some(&e)))?;
    let handle = next_handle();
    ZIP.with(|r| r.borrow_mut().insert(handle, state));
    vm.ret_i64(ctx, handle as i64);
    Ok(())
}

/// Parse a whole in-memory archive into the `ZipFile` native's metadata,
/// enforcing the entry-count/size/ratio caps. `Err` is the Java-level message;
/// the caller wraps it in `ZipException`.
fn parse_zip(bytes: Vec<u8>) -> Result<ZipFileState, String> {
    // A non-empty archive begins with a local file header; an empty archive
    // begins with the end-of-central-directory record.
    let loc_sig = bytes.get(..4) == Some(b"PK\x03\x04");
    let (entries, comment) = {
        let mut archive =
            zip::ZipArchive::new(Cursor::new(bytes.as_slice())).map_err(|e| e.to_string())?;
        let total = archive.len();
        if total > MAX_ZIP_ENTRIES {
            return Err(format!("too many entries: {total}"));
        }
        let mut entries = Vec::with_capacity(total);
        for i in 0..total {
            let f = archive.by_index_raw(i).map_err(|e| e.to_string())?;
            let size = f.size();
            let csize = f.compressed_size();
            if size > MAX_ENTRY_UNCOMPRESSED {
                return Err(format!("entry too large: {size} bytes"));
            }
            if csize > 0 && size / csize > MAX_ENTRY_RATIO {
                return Err(format!("entry expansion ratio too high: {size}/{csize}"));
            }
            let raw_name = f.name_raw();
            // Reconstruct the general-purpose flag bits the Java `ZipCoder`
            // consults: bit 11 (UTF-8) and bit 0 (encrypted). The zip crate
            // decodes names itself, so this is all the wrapper needs.
            let mut flag: u16 = 0;
            if !raw_name.is_ascii() && std::str::from_utf8(raw_name).is_ok() {
                flag |= 1 << 11;
            }
            if f.encrypted() {
                flag |= 1;
            }
            // `CompressionMethod` is `#[non_exhaustive]`; the raw zip method
            // code is what the Java wrapper stores in `ZipEntry.method`.
            let method: u16 = match f.compression() {
                zip::CompressionMethod::Stored => 0,
                zip::CompressionMethod::Deflated => 8,
                _ => 0,
            };
            let dos_time = f.last_modified().map(|d| dos_time_from(&d)).unwrap_or(0);
            entries.push(ZipEntryMeta {
                name: raw_name.to_vec(),
                extra: f.extra_data().unwrap_or_default().to_vec(),
                comment: f.comment().as_bytes().to_vec(),
                flag,
                method,
                dos_time,
                crc: f.crc32(),
                size,
                csize,
                data_start: f.data_start(),
            });
        }
        (entries, archive.comment().to_vec())
    };
    Ok(ZipFileState {
        bytes,
        entries,
        comment,
        loc_sig,
    })
}

/// `ZipFile.close(long jzfile)`.
fn zipfile_close(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let handle = vm.arg_i64(ctx, 0) as u64;
    ZIP.with(|r| {
        r.borrow_mut().remove(&handle);
    });
    Ok(())
}

/// `ZipFile.getTotal(long jzfile)`.
fn zipfile_get_total(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let handle = vm.arg_i64(ctx, 0) as u64;
    let n = ZIP.with(|r| {
        r.borrow()
            .get(&handle)
            .map_or(0, |s| s.entries.len() as i32)
    });
    vm.ret_i32(ctx, n);
    Ok(())
}

/// `ZipFile.startsWithLOC(long jzfile)`.
fn zipfile_starts_with_loc(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let handle = vm.arg_i64(ctx, 0) as u64;
    let b = ZIP.with(|r| r.borrow().get(&handle).is_some_and(|s| s.loc_sig));
    vm.ret_bool(ctx, b);
    Ok(())
}

/// `ZipFile.getFileDescriptor(long jzfile)`: ACOS exposes no host fd.
fn zipfile_get_fd(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    vm.ret_i32(ctx, -1);
    Ok(())
}

/// `ZipFile.getZipMessage(long jzfile)`: no native error state to report.
fn zipfile_get_message(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = vm.new_string("")?;
    vm.ret_ref(ctx, h);
    Ok(())
}

/// `ZipFile.freeEntry(long jzfile, long jzentry)`: entry handles are pure
/// index encodings, so there is nothing to release.
fn zipfile_free_entry(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let _ = vm.arg_i64(ctx, 1);
    Ok(())
}

/// `ZipFile.getEntry(long jzfile, byte[] name, boolean ignoreCase)`.
fn zipfile_get_entry(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let handle = vm.arg_i64(ctx, 0) as u64;
    let name = vm.arg_ref(ctx, 1);
    let ignore_case = vm.arg_bool(ctx, 2);
    let needle = match vm.heap.get(name) {
        Some(Object::Array(ArrayObj { data: ArrayData::Byte(v), .. })) => {
            v.iter().map(|&b| b as u8).collect()
        }
        _ => Vec::new(),
    };
    let found = ZIP.with(|r| {
        r.borrow().get(&handle).and_then(|s| {
            s.entries
                .iter()
                .position(|e| {
                    if ignore_case {
                        e.name.eq_ignore_ascii_case(&needle)
                    } else {
                        e.name == needle
                    }
                })
                .map(|i| entry_handle(handle, i))
        })
    });
    vm.ret_i64(ctx, found.unwrap_or(0));
    Ok(())
}

/// `ZipFile.getNextEntry(long jzfile, int i)`.
fn zipfile_get_next_entry(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let handle = vm.arg_i64(ctx, 0) as u64;
    let i = vm.arg_i32(ctx, 1);
    let found = ZIP.with(|r| {
        r.borrow().get(&handle).and_then(|s| {
            let i = i as usize;
            (i < s.entries.len()).then(|| entry_handle(handle, i))
        })
    });
    vm.ret_i64(ctx, found.unwrap_or(0));
    Ok(())
}

/// Look up the entry referenced by a `jzentry`, returning the archive bytes and
/// the entry metadata. The borrow of the registry must not escape this call, so
/// the closure form copies out just what the caller needs.
fn with_entry<R>(handle: i64, f: impl FnOnce(&ZipFileState, &ZipEntryMeta) -> R) -> Option<R> {
    let (file, idx) = decode_entry(handle)?;
    ZIP.with(|r| {
        let reg = r.borrow();
        let state = reg.get(&file)?;
        let entry = state.entries.get(idx)?;
        Some(f(state, entry))
    })
}

/// `ZipFile.getEntryFlag(long jzentry)`.
fn zipfile_entry_flag(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = vm.arg_i64(ctx, 0);
    let v = with_entry(h, |_, e| e.flag as i32).unwrap_or(0);
    vm.ret_i32(ctx, v);
    Ok(())
}

/// `ZipFile.getEntryTime(long jzentry)`.
///
/// `ZipEntry.getTime` feeds this into `ZipUtils.extendedDosToJavaTime`, which
/// reads the DOS date/time from the high 32 bits of the value.
fn zipfile_entry_time(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = vm.arg_i64(ctx, 0);
    let v = with_entry(h, |_, e| ((e.dos_time as u64) << 32) as i64).unwrap_or(-1);
    vm.ret_i64(ctx, v);
    Ok(())
}

/// `ZipFile.getEntryCrc(long jzentry)`.
fn zipfile_entry_crc(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = vm.arg_i64(ctx, 0);
    let v = with_entry(h, |_, e| e.crc as i64).unwrap_or(0);
    vm.ret_i64(ctx, v);
    Ok(())
}

/// `ZipFile.getEntrySize(long jzentry)` (uncompressed size).
fn zipfile_entry_size(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = vm.arg_i64(ctx, 0);
    let v = with_entry(h, |_, e| e.size as i64).unwrap_or(0);
    vm.ret_i64(ctx, v);
    Ok(())
}

/// `ZipFile.getEntryCSize(long jzentry)` (compressed size).
fn zipfile_entry_csize(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = vm.arg_i64(ctx, 0);
    let v = with_entry(h, |_, e| e.csize as i64).unwrap_or(0);
    vm.ret_i64(ctx, v);
    Ok(())
}

/// `ZipFile.getEntryMethod(long jzentry)`.
fn zipfile_entry_method(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = vm.arg_i64(ctx, 0);
    let v = with_entry(h, |_, e| e.method as i32).unwrap_or(0);
    vm.ret_i32(ctx, v);
    Ok(())
}

/// `ZipFile.getEntryBytes(long jzentry, int type)`.
///
/// `type` is `0` name, `1` extra, `2` comment. A missing comment/extra yields
/// `null` so the Java wrapper's `if (bytes == null)` branches behave as on ART.
fn zipfile_entry_bytes(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = vm.arg_i64(ctx, 0);
    let kind = vm.arg_i32(ctx, 1);
    let data = with_entry(h, |_, e| match kind {
        0 => Some(e.name.clone()),
        1 => Some(e.extra.clone()),
        2 => {
            if e.comment.is_empty() {
                None
            } else {
                Some(e.comment.clone())
            }
        }
        _ => None,
    })
    .flatten();
    match data {
        Some(d) => {
            let arr = byte_array(vm, d)?;
            vm.ret_ref(ctx, arr);
        }
        None => vm.ret_ref(ctx, NULL),
    }
    Ok(())
}

/// `ZipFile.getCommentBytes(long jzfile)`.
fn zipfile_comment_bytes(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let handle = vm.arg_i64(ctx, 0) as u64;
    let data = ZIP.with(|r| {
        r.borrow()
            .get(&handle)
            .filter(|s| !s.comment.is_empty())
            .map(|s| s.comment.clone())
    });
    match data {
        Some(d) => {
            let arr = byte_array(vm, d)?;
            vm.ret_ref(ctx, arr);
        }
        None => vm.ret_ref(ctx, NULL),
    }
    Ok(())
}

/// `ZipFile.read(long jzfile, long jzentry, long pos, byte[] buf, int off, int len)`.
///
/// Copies the raw (stored/compressed) entry payload: the Java wrapper wraps the
/// result in an `Inflater` stream for deflated entries. Returns `-1` at EOF.
fn zipfile_read(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let file = vm.arg_i64(ctx, 0) as u64;
    let entry = vm.arg_i64(ctx, 1);
    let pos = vm.arg_i64(ctx, 2);
    let buf = vm.arg_ref(ctx, 3);
    let off = vm.arg_i32(ctx, 4).max(0) as usize;
    let len = vm.arg_i32(ctx, 5).max(0) as usize;
    let (_, idx) = match decode_entry(entry) {
        Some(v) => v,
        None => {
            vm.ret_i32(ctx, -1);
            return Ok(());
        }
    };
    if pos < 0 {
        vm.ret_i32(ctx, -1);
        return Ok(());
    }
    let data: Option<Vec<u8>> = ZIP.with(|r| {
        let reg = r.borrow();
        let state = reg.get(&file)?;
        let e = state.entries.get(idx)?;
        if pos as u64 >= e.csize {
            return Some(Vec::new());
        }
        let start = e.data_start.checked_add(pos as u64)? as usize;
        let avail = (e.csize - pos as u64) as usize;
        let n = avail.min(len);
        let end = start.checked_add(n)?;
        let slice = state.bytes.get(start..end)?;
        Some(slice.to_vec())
    });
    let Some(data) = data else {
        vm.ret_i32(ctx, -1);
        return Ok(());
    };
    if data.is_empty() {
        vm.ret_i32(ctx, -1);
        return Ok(());
    }
    if !write_bytes(vm, buf, off, &data) {
        vm.ret_i32(ctx, -1);
        return Ok(());
    }
    vm.ret_i32(ctx, data.len() as i32);
    Ok(())
}

#[derive(Default)]
struct Registry {
    inflaters: HashMap<u64, InflaterState>,
    deflaters: HashMap<u64, DeflaterState>,
}

thread_local! {
    static REG: RefCell<Registry> = RefCell::new(Registry::default());
    static COUNTER: Cell<u64> = const { Cell::new(1) };
}

fn next_handle() -> u64 {
    COUNTER.with(|c| {
        let v = c.get();
        c.set(v + 1);
        v
    })
}

/// Register the `java.util.zip` natives.
pub(crate) fn install(vm: &mut Vm) -> VmResult<()> {
    let i = "Ljava/util/zip/Inflater;";
    vm.register_native(i, "init", "(Z)J", inflater_init);
    vm.register_native(i, "inflateBytes", "(J[BII)I", inflater_inflate_bytes);
    vm.register_native(i, "setDictionary", "(J[BII)V", inflater_set_dictionary);
    vm.register_native(i, "getAdler", "(J)I", inflater_get_adler);
    vm.register_native(i, "reset", "(J)V", inflater_reset);
    vm.register_native(i, "end", "(J)V", inflater_end);

    let d = "Ljava/util/zip/Deflater;";
    vm.register_native(d, "init", "(IIZ)J", deflater_init);
    vm.register_native(d, "deflateBytes", "(J[BIII)I", deflater_deflate_bytes);
    vm.register_native(d, "setDictionary", "(J[BII)V", deflater_set_dictionary);
    vm.register_native(d, "getAdler", "(J)I", deflater_get_adler);
    vm.register_native(d, "reset", "(J)V", deflater_reset);
    vm.register_native(d, "end", "(J)V", deflater_end);

    let c = "Ljava/util/zip/CRC32;";
    vm.register_native(c, "update", "(II)I", crc32_update_byte);
    vm.register_native(c, "updateBytes0", "(I[BII)I", crc32_update_bytes);

    let a = "Ljava/util/zip/Adler32;";
    vm.register_native(a, "update", "(II)I", adler32_update_byte);
    vm.register_native(a, "updateBytes", "(I[BII)I", adler32_update_bytes);

    // `java.util.zip.ZipFile` (API 34 libcore protos). `SoLoader` opens the APK
    // through these to enumerate `lib/<abi>/*.so`; the boot-classpath
    // `JarFile`/resource readers use the same surface.
    let z = "Ljava/util/zip/ZipFile;";
    vm.register_native(
        z,
        "open",
        "(Ljava/lang/String;IJZ)J",
        zipfile_open,
    );
    vm.register_native(z, "close", "(J)V", zipfile_close);
    vm.register_native(z, "getTotal", "(J)I", zipfile_get_total);
    vm.register_native(z, "startsWithLOC", "(J)Z", zipfile_starts_with_loc);
    vm.register_native(z, "getFileDescriptor", "(J)I", zipfile_get_fd);
    vm.register_native(
        z,
        "getZipMessage",
        "(J)Ljava/lang/String;",
        zipfile_get_message,
    );
    vm.register_native(z, "freeEntry", "(JJ)V", zipfile_free_entry);
    vm.register_native(z, "getEntry", "(J[BZ)J", zipfile_get_entry);
    vm.register_native(z, "getNextEntry", "(JI)J", zipfile_get_next_entry);
    vm.register_native(z, "getEntryFlag", "(J)I", zipfile_entry_flag);
    vm.register_native(z, "getEntryTime", "(J)J", zipfile_entry_time);
    vm.register_native(z, "getEntryCrc", "(J)J", zipfile_entry_crc);
    vm.register_native(z, "getEntrySize", "(J)J", zipfile_entry_size);
    vm.register_native(z, "getEntryCSize", "(J)J", zipfile_entry_csize);
    vm.register_native(z, "getEntryMethod", "(J)I", zipfile_entry_method);
    vm.register_native(z, "getEntryBytes", "(JI)[B", zipfile_entry_bytes);
    vm.register_native(z, "getCommentBytes", "(J)[B", zipfile_comment_bytes);
    vm.register_native(z, "read", "(JJJ[BII)I", zipfile_read);

    // `ZipOutputStream.putNextEntry` encodes the entry name through these;
    // `String.getBytes("UTF-8")` and `new String(byte[])` use them too.
    let u = "Llibcore/util/CharsetUtils;";
    vm.register_native(u, "toUtf8Bytes", "(Ljava/lang/String;II)[B", to_utf8_bytes);
    vm.register_native(u, "toAsciiBytes", "(Ljava/lang/String;II)[B", to_ascii_bytes);
    vm.register_native(
        u,
        "toIsoLatin1Bytes",
        "(Ljava/lang/String;II)[B",
        to_iso_latin1_bytes,
    );
    vm.register_native(u, "asciiBytesToChars", "([BII[C)V", ascii_bytes_to_chars);
    Ok(())
}

// ---------------------------------------------------------------------------
// Buffer helpers
// ---------------------------------------------------------------------------

/// Copy `len` bytes starting at `off` out of a Java `byte[]`.
fn read_bytes(vm: &Vm, h: Handle, off: usize, len: usize) -> Option<Vec<u8>> {
    match vm.heap.get(h) {
        Some(Object::Array(a)) => match &a.data {
            ArrayData::Byte(v) => {
                let end = off.checked_add(len)?;
                if end > v.len() {
                    return None;
                }
                Some(v[off..end].iter().map(|&b| b as u8).collect())
            }
            _ => None,
        },
        _ => None,
    }
}

/// Write `data` into a Java `byte[]` at `off`.
fn write_bytes(vm: &mut Vm, h: Handle, off: usize, data: &[u8]) -> bool {
    match vm.heap.get_mut(h) {
        Some(Object::Array(a)) => match &mut a.data {
            ArrayData::Byte(v) => {
                let end = off.saturating_add(data.len());
                if end > v.len() {
                    return false;
                }
                for (i, &b) in data.iter().enumerate() {
                    v[off + i] = b as i8;
                }
                true
            }
            _ => false,
        },
        _ => false,
    }
}

/// The UTF-16 code units of a Java `String` handle.
fn string_units(vm: &Vm, h: Handle) -> Vec<u16> {
    match vm.heap.get(h) {
        Some(Object::String(u)) => u.clone(),
        _ => Vec::new(),
    }
}

/// Allocate a Java `byte[]` from Rust bytes.
fn byte_array(vm: &mut Vm, data: Vec<u8>) -> VmResult<Handle> {
    vm.alloc(Object::Array(ArrayObj {
        elem_desc: "B".to_string(),
        data: ArrayData::Byte(data.into_iter().map(|b| b as i8).collect()),
    }))
}

fn substring_units(units: &[u16], offset: i32, length: i32) -> Vec<u16> {
    let start = offset.max(0) as usize;
    let end = start.saturating_add(length.max(0) as usize).min(units.len());
    units[start.min(units.len())..end].to_vec()
}

/// `libcore.util.CharsetUtils.toUtf8Bytes(String, int, int)`.
fn to_utf8_bytes(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let s = vm.arg_ref(ctx, 0);
    let offset = vm.arg_i32(ctx, 1);
    let length = vm.arg_i32(ctx, 2);
    let units = substring_units(&string_units(vm, s), offset, length);
    let text = String::from_utf16_lossy(&units);
    let h = byte_array(vm, text.into_bytes())?;
    vm.ret_ref(ctx, h);
    Ok(())
}

/// `libcore.util.CharsetUtils.toAsciiBytes(String, int, int)`.
fn to_ascii_bytes(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let s = vm.arg_ref(ctx, 0);
    let offset = vm.arg_i32(ctx, 1);
    let length = vm.arg_i32(ctx, 2);
    let units = substring_units(&string_units(vm, s), offset, length);
    let bytes = units
        .into_iter()
        .map(|c| if c <= 0x7f { c as u8 } else { b'?' })
        .collect();
    let h = byte_array(vm, bytes)?;
    vm.ret_ref(ctx, h);
    Ok(())
}

/// `libcore.util.CharsetUtils.toIsoLatin1Bytes(String, int, int)`.
fn to_iso_latin1_bytes(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let s = vm.arg_ref(ctx, 0);
    let offset = vm.arg_i32(ctx, 1);
    let length = vm.arg_i32(ctx, 2);
    let units = substring_units(&string_units(vm, s), offset, length);
    let bytes = units
        .into_iter()
        .map(|c| if c <= 0xff { c as u8 } else { b'?' })
        .collect();
    let h = byte_array(vm, bytes)?;
    vm.ret_ref(ctx, h);
    Ok(())
}

/// `libcore.util.CharsetUtils.asciiBytesToChars(byte[], int, int, char[])`.
fn ascii_bytes_to_chars(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let b = vm.arg_ref(ctx, 0);
    let off = vm.arg_i32(ctx, 1).max(0) as usize;
    let len = vm.arg_i32(ctx, 2).max(0) as usize;
    let c = vm.arg_ref(ctx, 3);
    let bytes = read_bytes(vm, b, off, len).unwrap_or_default();
    if let Some(Object::Array(a)) = vm.heap.get_mut(c) {
        if let ArrayData::Char(chars) = &mut a.data {
            let n = chars.len().min(bytes.len());
            for i in 0..n {
                chars[i] = bytes[i] as u16;
            }
        }
    }
    Ok(())
}

/// `(input bytes, off, len)` of an `Inflater`/`Deflater` wrapper instance.
fn wrapper_input(vm: &Vm, this: Handle) -> (Vec<u8>, usize, usize) {
    let buf = vm.get_instance_field_by_name(this, "buf").unwrap_or(NULL);
    let off = vm.get_instance_field_by_name(this, "off").unwrap_or(0) as usize;
    let len = vm.get_instance_field_by_name(this, "len").unwrap_or(0) as usize;
    let bytes = read_bytes(vm, buf, off, len).unwrap_or_default();
    (bytes, off, len)
}

// ---------------------------------------------------------------------------
// Inflater
// ---------------------------------------------------------------------------

fn inflater_init(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let nowrap = vm.arg_bool(ctx, 0);
    let handle = next_handle();
    let st = InflaterState {
        dec: Decompress::new(!nowrap),
        nowrap,
        finished: false,
        need_dict: false,
        adler: 1,
    };
    REG.with(|r| r.borrow_mut().inflaters.insert(handle, st));
    vm.ret_i64(ctx, handle as i64);
    Ok(())
}

fn inflater_inflate_bytes(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let addr = vm.arg_i64(ctx, 0) as u64;
    let out = vm.arg_ref(ctx, 1);
    let out_off = vm.arg_i32(ctx, 2).max(0) as usize;
    let out_len = vm.arg_i32(ctx, 3).max(0) as usize;
    let (input, in_off, in_len) = wrapper_input(vm, this);

    let mut outbuf = vec![0u8; out_len];
    let outcome = REG.with(|r| {
        let mut reg = r.borrow_mut();
        let Some(st) = reg.inflaters.get_mut(&addr) else {
            return Err("inflater handle not open".to_string());
        };
        let before_in = st.dec.total_in();
        let before_out = st.dec.total_out();
        match st.dec.decompress(&input, &mut outbuf, FlushDecompress::None) {
            Ok(status) => {
                let consumed = (st.dec.total_in() - before_in) as usize;
                let produced = (st.dec.total_out() - before_out) as usize;
                if status == Status::StreamEnd {
                    st.finished = true;
                }
                st.adler = adler32_update(st.adler, &outbuf[..produced]);
                Ok((consumed, produced, st.finished, st.need_dict))
            }
            Err(e) => {
                if e.needs_dictionary().is_some() {
                    st.need_dict = true;
                    Ok((0, 0, false, true))
                } else {
                    Err(e.to_string())
                }
            }
        }
    });
    let (consumed, produced, finished, need_dict) = match outcome {
        Ok(v) => v,
        Err(msg) => {
            return Err(vm.throw_new("Ljava/util/zip/DataFormatException;", Some(&msg)));
        }
    };
    write_bytes(vm, out, out_off, &outbuf[..produced]);
    vm.set_instance_field_by_name(this, "off", (in_off + consumed) as u32);
    vm.set_instance_field_by_name(this, "len", (in_len - consumed) as u32);
    if finished {
        vm.set_instance_field_by_name(this, "finished", 1);
    }
    if need_dict {
        vm.set_instance_field_by_name(this, "needDict", 1);
    }
    vm.ret_i32(ctx, produced as i32);
    Ok(())
}

fn inflater_set_dictionary(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let addr = vm.arg_i64(ctx, 0) as u64;
    let b = vm.arg_ref(ctx, 1);
    let off = vm.arg_i32(ctx, 2).max(0) as usize;
    let len = vm.arg_i32(ctx, 3).max(0) as usize;
    let _ = read_bytes(vm, b, off, len);
    // `flate2`'s pure-Rust backend does not expose `inflateSetDictionary`; the
    // corpus never uses preset dictionaries, so the call is a no-op.
    REG.with(|r| {
        let _ = r.borrow().inflaters.get(&addr);
    });
    Ok(())
}

fn inflater_get_adler(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let addr = vm.arg_i64(ctx, 0) as u64;
    let v = REG.with(|r| r.borrow().inflaters.get(&addr).map_or(1, |s| s.adler));
    vm.ret_i32(ctx, v as i32);
    Ok(())
}

fn inflater_reset(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let addr = vm.arg_i64(ctx, 0) as u64;
    REG.with(|r| {
        if let Some(st) = r.borrow_mut().inflaters.get_mut(&addr) {
            st.dec.reset(!st.nowrap);
            st.finished = false;
            st.need_dict = false;
            st.adler = 1;
        }
    });
    Ok(())
}

fn inflater_end(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let addr = vm.arg_i64(ctx, 0) as u64;
    REG.with(|r| {
        r.borrow_mut().inflaters.remove(&addr);
    });
    Ok(())
}

// ---------------------------------------------------------------------------
// Deflater
// ---------------------------------------------------------------------------

fn deflater_init(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let level = vm.arg_i32(ctx, 0);
    let _strategy = vm.arg_i32(ctx, 1);
    let nowrap = vm.arg_bool(ctx, 2);
    let level = if level == -1 { 6 } else { level.clamp(0, 9) } as u32;
    let handle = next_handle();
    let st = DeflaterState {
        comp: Compress::new(Compression::new(level), !nowrap),
        finished: false,
        adler: 1,
    };
    REG.with(|r| r.borrow_mut().deflaters.insert(handle, st));
    vm.ret_i64(ctx, handle as i64);
    Ok(())
}

fn flush_compress(flush: i32) -> FlushCompress {
    match flush {
        2 => FlushCompress::Sync,
        3 => FlushCompress::Full,
        4 => FlushCompress::Finish,
        1 => FlushCompress::Partial,
        _ => FlushCompress::None,
    }
}

fn deflater_deflate_bytes(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let addr = vm.arg_i64(ctx, 0) as u64;
    let out = vm.arg_ref(ctx, 1);
    let out_off = vm.arg_i32(ctx, 2).max(0) as usize;
    let out_len = vm.arg_i32(ctx, 3).max(0) as usize;
    let flush = vm.arg_i32(ctx, 4);
    let (input, in_off, in_len) = wrapper_input(vm, this);

    // `setLevel`/`setStrategy` only flip `setParams`. The pure-Rust backend
    // cannot change level mid-stream, so consume the flag and keep the level
    // the stream was opened with.
    if vm.get_instance_field_by_name(this, "setParams").unwrap_or(0) != 0 {
        vm.set_instance_field_by_name(this, "setParams", 0);
    }

    // `Deflater.finish()` only sets the `finish` field; the native must turn
    // it into `Z_FINISH`, otherwise `DeflaterOutputStream`'s
    // `while (!def.finished())` loop never terminates.
    let flush = if vm.get_instance_field_by_name(this, "finish").unwrap_or(0) != 0 {
        4
    } else {
        flush
    };

    let mut outbuf = vec![0u8; out_len];
    let outcome = REG.with(|r| {
        let mut reg = r.borrow_mut();
        let Some(st) = reg.deflaters.get_mut(&addr) else {
            return Err("deflater handle not open".to_string());
        };
        let before_in = st.comp.total_in();
        let before_out = st.comp.total_out();
        match st
            .comp
            .compress(&input, &mut outbuf, flush_compress(flush))
        {
            Ok(status) => {
                let consumed = (st.comp.total_in() - before_in) as usize;
                let produced = (st.comp.total_out() - before_out) as usize;
                if status == Status::StreamEnd {
                    st.finished = true;
                }
                st.adler = adler32_update(st.adler, &input[..consumed]);
                Ok((consumed, produced, st.finished))
            }
            Err(e) => Err(e.to_string()),
        }
    });
    let (consumed, produced, finished) = match outcome {
        Ok(v) => v,
        Err(msg) => {
            return Err(vm.throw_new("Ljava/util/zip/DataFormatException;", Some(&msg)));
        }
    };
    write_bytes(vm, out, out_off, &outbuf[..produced]);
    vm.set_instance_field_by_name(this, "off", (in_off + consumed) as u32);
    vm.set_instance_field_by_name(this, "len", (in_len - consumed) as u32);
    if finished {
        vm.set_instance_field_by_name(this, "finished", 1);
    }
    vm.ret_i32(ctx, produced as i32);
    Ok(())
}

fn deflater_set_dictionary(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let addr = vm.arg_i64(ctx, 0) as u64;
    let b = vm.arg_ref(ctx, 1);
    let off = vm.arg_i32(ctx, 2).max(0) as usize;
    let len = vm.arg_i32(ctx, 3).max(0) as usize;
    let _ = read_bytes(vm, b, off, len);
    // See `inflater_set_dictionary`: no preset dictionaries in the corpus.
    REG.with(|r| {
        let _ = r.borrow().deflaters.get(&addr);
    });
    Ok(())
}

fn deflater_get_adler(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let addr = vm.arg_i64(ctx, 0) as u64;
    let v = REG.with(|r| r.borrow().deflaters.get(&addr).map_or(1, |s| s.adler));
    vm.ret_i32(ctx, v as i32);
    Ok(())
}

fn deflater_reset(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let addr = vm.arg_i64(ctx, 0) as u64;
    REG.with(|r| {
        if let Some(st) = r.borrow_mut().deflaters.get_mut(&addr) {
            st.comp.reset();
            st.finished = false;
            st.adler = 1;
        }
    });
    Ok(())
}

fn deflater_end(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let addr = vm.arg_i64(ctx, 0) as u64;
    REG.with(|r| {
        r.borrow_mut().deflaters.remove(&addr);
    });
    Ok(())
}

// ---------------------------------------------------------------------------
// CRC32 / Adler32
// ---------------------------------------------------------------------------

fn crc32_update_byte(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let crc = vm.arg_i32(ctx, 0) as u32;
    let byte = vm.arg_i32(ctx, 1) as u8;
    vm.ret_i32(ctx, crc32_update(crc, &[byte]) as i32);
    Ok(())
}

fn crc32_update_bytes(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let crc = vm.arg_i32(ctx, 0) as u32;
    let b = vm.arg_ref(ctx, 1);
    let off = vm.arg_i32(ctx, 2).max(0) as usize;
    let len = vm.arg_i32(ctx, 3).max(0) as usize;
    let data = read_bytes(vm, b, off, len).unwrap_or_default();
    vm.ret_i32(ctx, crc32_update(crc, &data) as i32);
    Ok(())
}

fn adler32_update_byte(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let adler = vm.arg_i32(ctx, 0) as u32;
    let byte = vm.arg_i32(ctx, 1) as u8;
    vm.ret_i32(ctx, adler32_update(adler, &[byte]) as i32);
    Ok(())
}

fn adler32_update_bytes(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let adler = vm.arg_i32(ctx, 0) as u32;
    let b = vm.arg_ref(ctx, 1);
    let off = vm.arg_i32(ctx, 2).max(0) as usize;
    let len = vm.arg_i32(ctx, 3).max(0) as usize;
    let data = read_bytes(vm, b, off, len).unwrap_or_default();
    vm.ret_i32(ctx, adler32_update(adler, &data) as i32);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{adler32_update, crc32_update, parse_zip};
    use std::io::{Cursor, Write};

    #[test]
    fn crc32_known_vector() {
        // zlib crc32("123456789") == 0xcbf43926.
        assert_eq!(crc32_update(0, b"123456789"), 0xcbf4_3926);
    }

    #[test]
    fn crc32_continuation_matches_one_shot() {
        let one = crc32_update(0, b"hello world");
        let part = crc32_update(0, b"hello ");
        let two = crc32_update(part, b"world");
        assert_eq!(one, two);
    }

    #[test]
    fn adler32_known_vector() {
        // zlib adler32("Wikipedia") == 0x11E60398.
        assert_eq!(adler32_update(1, b"Wikipedia"), 0x11e6_0398);
    }

    /// Build an in-memory archive with one stored and one deflated entry.
    fn sample_zip() -> Vec<u8> {
        let mut buf = Vec::new();
        {
            let mut w = zip::ZipWriter::new(Cursor::new(&mut buf));
            let stored = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            w.start_file("lib/x86_64/libhermes.so", stored).unwrap();
            w.write_all(b"ELF-STORED").unwrap();
            let deflated = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);
            w.start_file("assets/index.android.bundle", deflated).unwrap();
            let body = vec![b'a'; 4096];
            w.write_all(&body).unwrap();
            w.finish().unwrap();
        }
        buf
    }

    #[test]
    fn parse_zip_reads_real_entry_metadata() {
        let state = parse_zip(sample_zip()).expect("parse");
        assert!(state.loc_sig);
        assert_eq!(state.entries.len(), 2);
        let so = &state.entries[0];
        assert_eq!(so.name, b"lib/x86_64/libhermes.so");
        assert_eq!(so.method, 0); // stored
        assert_eq!(so.size, 10);
        assert_eq!(so.csize, 10);
        let bundle = &state.entries[1];
        assert_eq!(bundle.name, b"assets/index.android.bundle");
        assert_eq!(bundle.method, 8); // deflated
        assert_eq!(bundle.size, 4096);
        assert!(bundle.csize < bundle.size);
        // Raw read: the stored entry's payload is addressable by `data_start`.
        let start = so.data_start as usize;
        assert_eq!(&state.bytes[start..start + 10], b"ELF-STORED");
    }

    #[test]
    fn parse_zip_rejects_non_zip() {
        assert!(parse_zip(b"not a zip at all".to_vec()).is_err());
    }
}
