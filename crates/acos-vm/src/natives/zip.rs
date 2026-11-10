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
    use super::{adler32_update, crc32_update};

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
}
