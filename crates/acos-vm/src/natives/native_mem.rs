//! Rust-owned native memory substrate.
//!
//! Android's `jdk.internal.misc.Unsafe`, `libcore.io.Memory` and
//! `java.nio.DirectByteBuffer` all traffic in raw 64-bit addresses. ART maps
//! real process pages and hands the pointer back to Java. ACOS's VM is
//! `#![forbid(unsafe_code)]` and never exposes a host pointer, so instead it
//! owns a table of byte regions and hands out **synthetic** 64-bit addresses:
//!
//! ```text
//!   address = (region_id as u64) << 32 | offset_in_region
//! ```
//!
//! Region id `0` is reserved (the null address). The high 32 bits identify the
//! region, the low 32 bits are a bounds-checked offset. Every access goes
//! through this module, so an out-of-bounds or use-after-free access becomes a
//! structured Java exception instead of undefined behaviour or a panic.
//!
//! Backing store is a plain `Vec<u8>` per region, which gives *copy*-based
//! `mmap` semantics: `FileChannel.map` copies the VFS file contents into a
//! region (`MAP_PRIVATE`-like), and a read-only map rejects writes. This is a
//! declared divergence from a live `mmap` (no shared write-back), but it is
//! observationally equivalent for the read-only ICU data maps the framework
//! performs.
//!
//! A process-wide (per-host-thread) cap bounds total native bytes; it defaults
//! to 256 MiB and can be overridden with `ACOS_NATIVE_MEM_MAX`.

use std::cell::RefCell;
use std::collections::HashMap;

use crate::class::NativeCtx;
use crate::error::{VmError, VmResult};
use crate::heap::{ArrayData, ArrayObj, Handle, Object, NULL};
use crate::vm::Vm;

/// The default cap on total live native bytes (256 MiB).
pub(crate) const DEFAULT_MAX_BYTES: usize = 256 * 1024 * 1024;

/// A structured native-memory access failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MemError {
    /// Address `0` / a null native base.
    Null,
    /// The address range crosses the end of its region.
    OutOfBounds,
    /// The region id names no live region (freed or never allocated).
    NoRegion,
    /// The region is read-only.
    ReadOnly,
    /// Allocating would exceed the global native-memory cap.
    CapExceeded,
}

impl MemError {
    /// Render as the Java exception Android-adjacent code can observe.
    pub(crate) fn throw(self, vm: &mut Vm) -> VmError {
        match self {
            MemError::Null => vm.throw_new(
                "Ljava/lang/NullPointerException;",
                Some("native memory: null address"),
            ),
            MemError::OutOfBounds => vm.throw_new(
                "Ljava/lang/IndexOutOfBoundsException;",
                Some("native memory: address range out of bounds"),
            ),
            MemError::NoRegion => vm.throw_new(
                "Ljava/lang/IllegalStateException;",
                Some("native memory: address does not name a live region"),
            ),
            MemError::ReadOnly => vm.throw_new(
                "Ljava/lang/IllegalStateException;",
                Some("native memory: write to read-only mapping"),
            ),
            MemError::CapExceeded => vm.throw_new(
                "Ljava/lang/OutOfMemoryError;",
                Some("native memory: ACOS_NATIVE_MEM_MAX exceeded"),
            ),
        }
    }
}

/// A file-backed region's provenance (W160-A).
///
/// A snapshot must not embed a file mapping's bytes: the bytes already live on
/// disk under the boot root, and re-reading them keeps the on-disk boot cache
/// small. Instead the snapshot records *where* the mapping came from and
/// re-reads it on restore, validating that the file has not changed. A missing
/// or changed file fails closed (the restore errors and the boot-cache caller
/// cold-boots) rather than serving stale bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RegionSource {
    /// The guest path the descriptor was opened as.
    pub path: String,
    /// Byte offset of the mapping within the file.
    pub offset: u64,
    /// Number of bytes mapped.
    pub length: u64,
    /// FNV-1a hash of the exact mapped bytes (zero-fill past EOF included).
    pub hash: u64,
    /// File size in bytes at capture time.
    pub file_size: u64,
    /// File mtime in seconds at capture time.
    pub file_mtime: i64,
}

impl RegionSource {
    /// Capture a source for `bytes`, which were mapped from `path` at `offset`.
    pub(crate) fn capture(
        path: String,
        offset: u64,
        bytes: &[u8],
        file_size: u64,
        file_mtime: i64,
    ) -> RegionSource {
        RegionSource {
            path,
            offset,
            length: bytes.len() as u64,
            hash: content_hash(bytes),
            file_size,
            file_mtime,
        }
    }

    /// Whether freshly re-read `bytes` with `(file_size, file_mtime)` still
    /// match this source. Any difference fails validation.
    pub(crate) fn matches(&self, bytes: &[u8], file_size: u64, file_mtime: i64) -> bool {
        self.length == bytes.len() as u64
            && self.file_size == file_size
            && self.file_mtime == file_mtime
            && self.hash == content_hash(bytes)
    }
}

/// Stable FNV-1a 64-bit content hash. Stable across processes and builds (the
/// on-disk boot cache outlives a single binary), unlike `DefaultHasher`.
fn content_hash(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

struct Region {
    data: Vec<u8>,
    readonly: bool,
    /// The file mapping this region came from, when it was produced by
    /// `FileChannelImpl.map0` over a real file. `None` for anonymous regions
    /// (`Unsafe.allocateMemory`, JNI `NewDirectByteBuffer`, ...).
    source: Option<RegionSource>,
}

struct MemState {
    regions: HashMap<u32, Region>,
    next_id: u32,
    total: usize,
    cap: usize,
}

impl MemState {
    fn new() -> MemState {
        MemState {
            regions: HashMap::new(),
            next_id: 1,
            total: 0,
            cap: configured_cap(),
        }
    }
}

fn configured_cap() -> usize {
    match std::env::var("ACOS_NATIVE_MEM_MAX") {
        Ok(s) => parse_size(&s).unwrap_or(DEFAULT_MAX_BYTES),
        Err(_) => DEFAULT_MAX_BYTES,
    }
}

fn parse_size(s: &str) -> Option<usize> {
    let s = s.trim();
    let (num, mult) = match s.as_bytes().last().copied() {
        Some(b'K' | b'k') => (&s[..s.len() - 1], 1024usize),
        Some(b'M' | b'm') => (&s[..s.len() - 1], 1024 * 1024),
        Some(b'G' | b'g') => (&s[..s.len() - 1], 1024 * 1024 * 1024),
        _ => (s, 1),
    };
    num.trim().parse::<usize>().ok().map(|n| n * mult)
}

thread_local! {
    static MEM: RefCell<MemState> = RefCell::new(MemState::new());
}

/// Total bytes currently held by live regions.
#[cfg(test)]
pub(crate) fn total_bytes() -> usize {
    MEM.with(|m| m.borrow().total)
}

/// A serialisable copy of one live native region.
///
/// A snapshot can hold Java `DirectByteBuffer`s (and any other object) whose
/// synthetic address names a region here. The region table itself is
/// process-global native state, not VM heap state, so a restore in a fresh
/// process must re-establish it or the restored buffer reads whatever (or
/// nothing) happens to occupy that region id. This is the fix for W154-A:
/// `ICUBinary$DatPackageReader` holds a mapped read-only ICU `.dat` buffer
/// captured by the primed `Typeface.<clinit>` template, and a warm restore
/// read a stale/garbage region and threw `IndexOutOfBoundsException`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RegionEntry {
    /// Region id (the high 32 bits of every synthetic address into it).
    pub id: u32,
    /// Whether the region rejects writes (`MAP_RO`).
    pub readonly: bool,
    /// Region contents. Empty for a file-backed region whose `source` is set:
    /// the bytes are re-read from the file on restore (W160-A), so they are not
    /// embedded in the snapshot.
    pub data: Vec<u8>,
    /// File provenance, when the region came from `FileChannelImpl.map0` over a
    /// real file.
    pub source: Option<RegionSource>,
}

/// Snapshot every live region, sorted by id so the byte stream is
/// deterministic regardless of `HashMap` iteration order.
///
/// A file-backed region keeps its `source` and drops `data`: the snapshot
/// stores the provenance (path/offset/hash/size/mtime), not the bytes.
pub(crate) fn snapshot_regions() -> Vec<RegionEntry> {
    MEM.with(|m| {
        let m = m.borrow();
        let mut ids: Vec<u32> = m.regions.keys().copied().collect();
        ids.sort_unstable();
        ids.into_iter()
            .map(|id| {
                let r = &m.regions[&id];
                RegionEntry {
                    id,
                    readonly: r.readonly,
                    data: if r.source.is_some() {
                        Vec::new()
                    } else {
                        r.data.clone()
                    },
                    source: r.source.clone(),
                }
            })
            .collect()
    })
}

/// Re-establish regions captured by [`snapshot_regions`].
///
/// For a file-backed region the caller must have re-read and validated
/// `entry.data` against `entry.source` first (`Vm::restore` does this); the
/// region keeps its `source` so a later snapshot persists provenance again.
///
/// Idempotent: a region that already exists with **identical** contents is
/// left in place (the template boot and the first restore share one process in
/// which the live table is already correct). A conflicting id is skipped, with
/// a trace under `ACOS_BOOT_TRACE`, because overwriting it could clobber a
/// region some other restored object still references; the caller's snapshot
/// validation guarantees this does not happen for a legitimate template.
pub(crate) fn restore_regions(regions: &[RegionEntry]) {
    let trace = std::env::var_os("ACOS_BOOT_TRACE").is_some();
    MEM.with(|m| {
        let mut m = m.borrow_mut();
        for r in regions {
            if let Some(existing) = m.regions.get(&r.id) {
                if trace && (existing.data != r.data || existing.readonly != r.readonly) {
                    eprintln!("[native_mem] region {} conflict, keeping live bytes", r.id);
                }
                continue;
            }
            let len = r.data.len();
            if m.total.saturating_add(len) > m.cap {
                if trace {
                    eprintln!("[native_mem] region {} skipped: cap exceeded", r.id);
                }
                continue;
            }
            m.total += len;
            m.regions.insert(
                r.id,
                Region {
                    data: r.data.clone(),
                    readonly: r.readonly,
                    source: r.source.clone(),
                },
            );
            if r.id >= m.next_id {
                m.next_id = r.id.saturating_add(1);
            }
        }
    });
}

/// The configured cap in bytes.
pub(crate) fn cap_bytes() -> usize {
    MEM.with(|m| m.borrow().cap)
}

/// Drop every region. Used by tests, by [`Vm::new`](crate::vm::Vm::new) when a
/// fresh VM starts on this thread, and as the reclaim hook when a VM is torn
/// down.
///
/// A fresh boot is a fresh process-like state: regions captured by a *previous*
/// VM on the same host thread must not leak into the next VM's snapshot (they
/// would nearly double the payload with an unreferenced 28 MiB ICU map and push
/// it past the on-disk cache cap). The supported model is one `Vm` per host
/// thread; `Vm::restore` deliberately does **not** clear, because it
/// re-establishes the snapshot's own regions.
pub(crate) fn reclaim_all() {
    MEM.with(|m| {
        let mut m = m.borrow_mut();
        m.regions.clear();
        m.total = 0;
        m.next_id = 1;
    });
}

/// Allocate a zero-filled region and return its synthetic base address.
pub(crate) fn alloc(len: usize, readonly: bool) -> Result<i64, MemError> {
    MEM.with(|m| {
        let mut m = m.borrow_mut();
        if m.total.saturating_add(len) > m.cap {
            return Err(MemError::CapExceeded);
        }
        let id = m.next_id;
        m.next_id = m.next_id.checked_add(1).ok_or(MemError::CapExceeded)?;
        let mut data: Vec<u8> = Vec::new();
        if data.try_reserve_exact(len).is_err() {
            return Err(MemError::CapExceeded);
        }
        data.resize(len, 0);
        m.total += len;
        m.regions.insert(
            id,
            Region {
                data,
                readonly,
                source: None,
            },
        );
        Ok(((id as u64) << 32) as i64)
    })
}

/// Allocate a region pre-filled with `bytes`.
pub(crate) fn alloc_with(bytes: Vec<u8>, readonly: bool) -> Result<i64, MemError> {
    MEM.with(|m| {
        let mut m = m.borrow_mut();
        if m.total.saturating_add(bytes.len()) > m.cap {
            return Err(MemError::CapExceeded);
        }
        let id = m.next_id;
        m.next_id = m.next_id.checked_add(1).ok_or(MemError::CapExceeded)?;
        m.total += bytes.len();
        m.regions.insert(
            id,
            Region {
                data: bytes,
                readonly,
                source: None,
            },
        );
        Ok(((id as u64) << 32) as i64)
    })
}

/// Allocate a region pre-filled with `bytes` mapped from a file, recording the
/// mapping's provenance so a snapshot need not embed the bytes (W160-A).
pub(crate) fn alloc_mapped(
    bytes: Vec<u8>,
    readonly: bool,
    source: RegionSource,
) -> Result<i64, MemError> {
    MEM.with(|m| {
        let mut m = m.borrow_mut();
        if m.total.saturating_add(bytes.len()) > m.cap {
            return Err(MemError::CapExceeded);
        }
        let id = m.next_id;
        m.next_id = m.next_id.checked_add(1).ok_or(MemError::CapExceeded)?;
        m.total += bytes.len();
        m.regions.insert(
            id,
            Region {
                data: bytes,
                readonly,
                source: Some(source),
            },
        );
        Ok(((id as u64) << 32) as i64)
    })
}

/// Free a region. Returns `false` when the id names no live region.
pub(crate) fn free(addr: i64) -> bool {
    let id = ((addr as u64) >> 32) as u32;
    if id == 0 {
        return false;
    }
    MEM.with(|m| {
        let mut m = m.borrow_mut();
        match m.regions.remove(&id) {
            Some(r) => {
                m.total = m.total.saturating_sub(r.data.len());
                true
            }
            None => false,
        }
    })
}

/// Resize a region in place. Returns the (possibly new) base address.
pub(crate) fn realloc(addr: i64, new_len: usize) -> Result<i64, MemError> {
    let id = ((addr as u64) >> 32) as u32;
    if id == 0 {
        return Err(MemError::Null);
    }
    MEM.with(|m| {
        let mut m = m.borrow_mut();
        let old_len = m
            .regions
            .get(&id)
            .map(|r| r.data.len())
            .ok_or(MemError::NoRegion)?;
        let readonly = m.regions.get(&id).map(|r| r.readonly).unwrap_or(false);
        if readonly {
            return Err(MemError::ReadOnly);
        }
        let delta = new_len.saturating_sub(old_len);
        if m.total.saturating_add(delta) > m.cap {
            return Err(MemError::CapExceeded);
        }
        let region = m.regions.get_mut(&id).ok_or(MemError::NoRegion)?;
        if new_len > old_len && region.data.try_reserve_exact(delta).is_err() {
            return Err(MemError::CapExceeded);
        }
        region.data.resize(new_len, 0);
        m.total = m.total.saturating_sub(old_len).saturating_add(new_len);
        Ok(((id as u64) << 32) as i64)
    })
}

fn split(addr: i64) -> Result<(u32, usize), MemError> {
    let raw = addr as u64;
    let id = (raw >> 32) as u32;
    if id == 0 {
        return Err(MemError::Null);
    }
    Ok((id, (raw & 0xffff_ffff) as usize))
}

/// Run `f` over `len` bytes at `addr`, checking liveness and bounds.
pub(crate) fn with_bytes<T>(
    addr: i64,
    len: usize,
    f: impl FnOnce(&[u8]) -> T,
) -> Result<T, MemError> {
    let (id, off) = split(addr)?;
    MEM.with(|m| {
        let m = m.borrow();
        let region = m.regions.get(&id).ok_or(MemError::NoRegion)?;
        let end = off.checked_add(len).ok_or(MemError::OutOfBounds)?;
        if end > region.data.len() {
            return Err(MemError::OutOfBounds);
        }
        Ok(f(&region.data[off..end]))
    })
}

/// Run `f` over a mutable `len`-byte window, enforcing read-only regions.
pub(crate) fn with_bytes_mut<T>(
    addr: i64,
    len: usize,
    f: impl FnOnce(&mut [u8]) -> T,
) -> Result<T, MemError> {
    let (id, off) = split(addr)?;
    MEM.with(|m| {
        let mut m = m.borrow_mut();
        let region = m.regions.get_mut(&id).ok_or(MemError::NoRegion)?;
        if region.readonly {
            return Err(MemError::ReadOnly);
        }
        let end = off.checked_add(len).ok_or(MemError::OutOfBounds)?;
        if end > region.data.len() {
            return Err(MemError::OutOfBounds);
        }
        Ok(f(&mut region.data[off..end]))
    })
}

/// Copy `len` bytes out of native memory.
pub(crate) fn read_bytes(addr: i64, len: usize) -> Result<Vec<u8>, MemError> {
    with_bytes(addr, len, <[u8]>::to_vec)
}

/// Read a NUL-terminated byte string starting at `addr`, bounded by the end of
/// its region.
///
/// A C-string pointer is what every `sun.nio.fs.UnixNativeDispatcher` path
/// native receives (`NativeBuffer.address()`); unlike [`read_bytes`] the caller
/// does not know the length, so the scan stops at the first NUL or the region
/// end (a missing NUL is not an error, matching `strnlen`).
pub(crate) fn read_cstr(addr: i64) -> Result<Vec<u8>, MemError> {
    let (id, off) = split(addr)?;
    MEM.with(|m| {
        let m = m.borrow();
        let region = m.regions.get(&id).ok_or(MemError::NoRegion)?;
        if off > region.data.len() {
            return Err(MemError::OutOfBounds);
        }
        let rest = &region.data[off..];
        let end = rest.iter().position(|&b| b == 0).unwrap_or(rest.len());
        Ok(rest[..end].to_vec())
    })
}

/// Copy `bytes` into native memory.
pub(crate) fn write_bytes(addr: i64, bytes: &[u8]) -> Result<(), MemError> {
    with_bytes_mut(addr, bytes.len(), |dst| dst.copy_from_slice(bytes))
}

/// The real host address of a byte-contiguous window of a **synthetic**
/// address, or `None` when the address does not name one.
///
/// ACOS normally keeps every pointer synthetic (`region<<32|offset` or an
/// `addressOf` array alias) so the VM never hands a host pointer to guest code.
/// Native code that dereferences `GetDirectBufferAddress` directly — the
/// Flutter engine's `flutter::DispatchPointerDataPacket` does a `memcpy` out of
/// it — cannot use such an address. This is the one deliberate escape hatch:
/// it resolves the backing `Vec<u8>`/`byte[]` to its host buffer for immediate
/// use. The pointer is valid only until the region is resized/freed or the
/// array is mutated; callers must copy before returning to Java.
///
/// Returns `None` for a non-synthetic address, a null/already freed one, a
/// `byte[]`-longer-than-requested window, or a non-byte-contiguous array.
pub(crate) fn host_pointer(vm: &Vm, addr: i64, len: usize) -> Option<usize> {
    if let Some((handle, off)) = array_parts(addr) {
        if handle == 0 {
            return None;
        }
        let Object::Array(a) = vm.heap.get(handle)? else {
            return None;
        };
        let ArrayData::Byte(v) = &a.data else {
            return None;
        };
        let end = off.checked_add(len)?;
        return Some(v.get(off..end)?.as_ptr() as usize);
    }
    let (id, off) = split(addr).ok()?;
    MEM.with(|m| {
        let m = m.borrow();
        let region = m.regions.get(&id)?;
        let end = off.checked_add(len)?;
        if end > region.data.len() {
            return None;
        }
        Some(region.data[off..end].as_ptr() as usize)
    })
}

/// `memmove` between two (possibly overlapping) native regions.
#[allow(dead_code)]
pub(crate) fn copy_within(src: i64, dst: i64, len: usize) -> Result<(), MemError> {
    if len == 0 {
        return Ok(());
    }
    // Snapshot the source first so overlapping moves are well defined.
    let buf = read_bytes(src, len)?;
    write_bytes(dst, &buf)
}

macro_rules! native_prim {
    ($get:ident, $set:ident, $ty:ty, $width:expr) => {
        #[cfg(test)]
        #[allow(dead_code)]
        pub(crate) fn $get(addr: i64) -> Result<$ty, MemError> {
            with_bytes(addr, $width, |b| {
                let mut w = [0u8; $width];
                w.copy_from_slice(b);
                <$ty>::from_le_bytes(w)
            })
        }

        #[cfg(test)]
        #[allow(dead_code)]
        pub(crate) fn $set(addr: i64, v: $ty) -> Result<(), MemError> {
            let bytes = v.to_le_bytes();
            with_bytes_mut(addr, $width, |dst| dst.copy_from_slice(&bytes))
        }
    };
}

native_prim!(get_i8, set_i8, i8, 1);
native_prim!(get_i16, set_i16, i16, 2);
native_prim!(get_i32, set_i32, i32, 4);
native_prim!(get_i64, set_i64, i64, 8);
native_prim!(get_f32, set_f32, f32, 4);
native_prim!(get_f64, set_f64, f64, 8);

/// Tag bit marking a synthetic address that aliases a Java heap array.
const ARRAY_TAG: u32 = 0x8000_0000;

/// A synthetic address that aliases the backing store of Java array `handle`.
///
/// `VMRuntime.addressOf` returns this so `DirectByteBuffer`'s `MemoryRef`
/// (which is a `newNonMovableArray(byte.class, ...)` plus an alignment offset)
/// can be read/written through the same `libcore.io.Memory`/`Unsafe` natives
/// as native `malloc` memory. `handle` is in the low 31 bits of the region id
/// and the array byte offset is in the low 32 bits, so `address + k` still
/// resolves to `array[off + k]`.
pub(crate) fn array_address(handle: u32) -> i64 {
    (((ARRAY_TAG | (handle & 0x7fff_ffff)) as u64) << 32) as i64
}

/// The heap-array handle a synthetic `VMRuntime.addressOf` address aliases.
///
/// `nativeApplyStyle` receives `TypedArray.mDataAddress`/`mIndicesAddress` as
/// raw `J` arguments; the shim writes the output arrays through this.
pub(crate) fn array_handle_from_address(addr: i64) -> Option<u32> {
    array_parts(addr).map(|(handle, _)| handle)
}

fn array_parts(addr: i64) -> Option<(u32, usize)> {
    let raw = addr as u64;
    let id = (raw >> 32) as u32;
    if id & ARRAY_TAG == 0 {
        return None;
    }
    Some((id & 0x7fff_ffff, (raw & 0xffff_ffff) as usize))
}

/// Read bytes from either a native region or a heap-array alias.
pub(crate) fn read_bytes_at(vm: &mut Vm, addr: i64, len: usize) -> VmResult<Vec<u8>> {
    if let Some((handle, off)) = array_parts(addr) {
        if handle == 0 {
            return Err(MemError::Null.throw(vm));
        }
        return array_read_bytes(vm, handle, off, len);
    }
    read_bytes(addr, len).map_err(|e| e.throw(vm))
}

/// Write bytes to either a native region or a heap-array alias.
pub(crate) fn write_bytes_at(vm: &mut Vm, addr: i64, bytes: &[u8]) -> VmResult<()> {
    if let Some((handle, off)) = array_parts(addr) {
        if handle == 0 {
            return Err(MemError::Null.throw(vm));
        }
        return array_write_bytes(vm, handle, off, bytes);
    }
    write_bytes(addr, bytes).map_err(|e| e.throw(vm))
}

macro_rules! at_prim {
    ($read:ident, $write:ident, $ty:ty, $n:expr) => {
        pub(crate) fn $read(vm: &mut Vm, addr: i64) -> VmResult<$ty> {
            let b = read_bytes_at(vm, addr, $n)?;
            let mut w = [0u8; $n];
            w.copy_from_slice(&b);
            Ok(<$ty>::from_le_bytes(w))
        }

        pub(crate) fn $write(vm: &mut Vm, addr: i64, v: $ty) -> VmResult<()> {
            write_bytes_at(vm, addr, &v.to_le_bytes())
        }
    };
}

at_prim!(read_i8, write_i8, i8, 1);
at_prim!(read_i16, write_i16, i16, 2);
at_prim!(read_i32, write_i32, i32, 4);
at_prim!(read_i64, write_i64, i64, 8);
at_prim!(read_f32, write_f32, f32, 4);
at_prim!(read_f64, write_f64, f64, 8);

// ---------------------------------------------------------------------------
// libcore.io.Memory
// ---------------------------------------------------------------------------

/// Register every `libcore.io.Memory` native.
pub(crate) fn install_memory(vm: &mut Vm) -> VmResult<()> {
    let c = "Llibcore/io/Memory;";
    vm.register_native(c, "peekByte", "(J)B", peek_byte);
    vm.register_native(c, "peekByteArray", "(J[BII)V", peek_byte_array);
    vm.register_native(c, "peekCharArray", "(J[CIIZ)V", peek_char_array);
    vm.register_native(c, "peekDoubleArray", "(J[DIIZ)V", peek_double_array);
    vm.register_native(c, "peekFloatArray", "(J[FIIZ)V", peek_float_array);
    vm.register_native(c, "peekIntArray", "(J[IIIZ)V", peek_int_array);
    vm.register_native(c, "peekIntNative", "(J)I", peek_int_native);
    vm.register_native(c, "peekLongArray", "(J[JIIZ)V", peek_long_array);
    vm.register_native(c, "peekLongNative", "(J)J", peek_long_native);
    vm.register_native(c, "peekShortArray", "(J[SIIZ)V", peek_short_array);
    vm.register_native(c, "peekShortNative", "(J)S", peek_short_native);
    vm.register_native(c, "pokeByte", "(JB)V", poke_byte);
    vm.register_native(c, "pokeByteArray", "(J[BII)V", poke_byte_array);
    vm.register_native(c, "pokeCharArray", "(J[CIIZ)V", poke_char_array);
    vm.register_native(c, "pokeDoubleArray", "(J[DIIZ)V", poke_double_array);
    vm.register_native(c, "pokeFloatArray", "(J[FIIZ)V", poke_float_array);
    vm.register_native(c, "pokeIntArray", "(J[IIIZ)V", poke_int_array);
    vm.register_native(c, "pokeIntNative", "(JI)V", poke_int_native);
    vm.register_native(c, "pokeLongArray", "(J[JIIZ)V", poke_long_array);
    vm.register_native(c, "pokeLongNative", "(JJ)V", poke_long_native);
    vm.register_native(c, "pokeShortArray", "(J[SIIZ)V", poke_short_array);
    vm.register_native(c, "pokeShortNative", "(JS)V", poke_short_native);
    vm.register_native(
        c,
        "memmove",
        "(Ljava/lang/Object;ILjava/lang/Object;IJ)V",
        memmove,
    );
    Ok(())
}

fn peek_byte(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let addr = vm.arg_i64(ctx, 0);
    let v = read_i8(vm, addr)?;
    vm.ret_i32(ctx, v as i32);
    Ok(())
}

fn poke_byte(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let addr = vm.arg_i64(ctx, 0);
    let v = vm.arg_i32(ctx, 1) as i8;
    write_i8(vm, addr, v)
}

fn peek_int_native(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let v = read_i32(vm, vm.arg_i64(ctx, 0))?;
    vm.ret_i32(ctx, v);
    Ok(())
}

fn poke_int_native(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let addr = vm.arg_i64(ctx, 0);
    let v = vm.arg_i32(ctx, 1);
    write_i32(vm, addr, v)
}

fn peek_short_native(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let v = read_i16(vm, vm.arg_i64(ctx, 0))?;
    vm.ret_i32(ctx, v as i32);
    Ok(())
}

fn poke_short_native(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let addr = vm.arg_i64(ctx, 0);
    let v = vm.arg_i32(ctx, 1) as i16;
    write_i16(vm, addr, v)
}

fn peek_long_native(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let v = read_i64(vm, vm.arg_i64(ctx, 0))?;
    vm.ret_i64(ctx, v);
    Ok(())
}

fn poke_long_native(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let addr = vm.arg_i64(ctx, 0);
    let v = vm.arg_i64(ctx, 1);
    write_i64(vm, addr, v)
}

fn peek_byte_array(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let addr = vm.arg_i64(ctx, 0);
    let arr = vm.arg_ref(ctx, 1);
    let off = vm.arg_i32(ctx, 2).max(0) as usize;
    let count = vm.arg_i32(ctx, 3).max(0) as usize;
    let bytes = read_bytes_at(vm, addr, count)?;
    array_write_bytes(vm, arr, off, &bytes)
}

fn poke_byte_array(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let addr = vm.arg_i64(ctx, 0);
    let arr = vm.arg_ref(ctx, 1);
    let off = vm.arg_i32(ctx, 2).max(0) as usize;
    let count = vm.arg_i32(ctx, 3).max(0) as usize;
    let bytes = array_read_bytes(vm, arr, off, count)?;
    write_bytes_at(vm, addr, &bytes)
}

macro_rules! peek_array {
    ($name:ident, $ty:ty, $scale:expr) => {
        fn $name(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
            let addr = vm.arg_i64(ctx, 0);
            let arr = vm.arg_ref(ctx, 1);
            let off = vm.arg_i32(ctx, 2).max(0) as usize;
            let count = vm.arg_i32(ctx, 3);
            if count < 0 {
                return Err(vm.throw_new(
                    "Ljava/lang/IllegalArgumentException;",
                    Some("Unsafe.peek count is negative"),
                ));
            }
            let count = count as usize;
            let bytes = count.saturating_mul($scale);
            // Bound before allocating the host output vector (HONESTY_AUDIT.md
            // #14); the checked multiply prevents a wrap-around that would
            // pass the memory range check.
            vm.check_array_len("B", bytes)?;
            let swap = vm.arg_bool(ctx, 4);
            let raw = read_bytes_at(vm, addr, bytes)?;
            let mut vals: Vec<$ty> = Vec::with_capacity(count);
            for chunk in raw.chunks_exact($scale) {
                let mut w = [0u8; $scale];
                w.copy_from_slice(chunk);
                let v = <$ty>::from_le_bytes(w);
                vals.push(if swap { swap_bytes(v) } else { v });
            }
            array_write_as(vm, arr, off, &vals)
        }
    };
}

macro_rules! poke_array {
    ($name:ident, $ty:ty, $scale:expr) => {
        fn $name(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
            let addr = vm.arg_i64(ctx, 0);
            let arr = vm.arg_ref(ctx, 1);
            let off = vm.arg_i32(ctx, 2).max(0) as usize;
            let count = vm.arg_i32(ctx, 3);
            if count < 0 {
                return Err(vm.throw_new(
                    "Ljava/lang/IllegalArgumentException;",
                    Some("Unsafe.poke count is negative"),
                ));
            }
            let count = count as usize;
            let bytes = count.saturating_mul($scale);
            // Bound before allocating the host buffer (HONESTY_AUDIT.md #14).
            vm.check_array_len("B", bytes)?;
            let swap = vm.arg_bool(ctx, 4);
            let vals: Vec<$ty> = array_read_as(vm, arr, off, count)?;
            let mut raw = Vec::with_capacity(bytes);
            for v in vals {
                let v = if swap { swap_bytes(v) } else { v };
                raw.extend_from_slice(&v.to_le_bytes());
            }
            write_bytes_at(vm, addr, &raw)
        }
    };
}

peek_array!(peek_int_array, i32, 4);
poke_array!(poke_int_array, i32, 4);
peek_array!(peek_short_array, i16, 2);
poke_array!(poke_short_array, i16, 2);
peek_array!(peek_long_array, i64, 8);
poke_array!(poke_long_array, i64, 8);

peek_array!(peek_char_array, u16, 2);
poke_array!(poke_char_array, u16, 2);
peek_array!(peek_float_array, f32, 4);
poke_array!(poke_float_array, f32, 4);
peek_array!(peek_double_array, f64, 8);
poke_array!(poke_double_array, f64, 8);

/// Byte-order swap used by the `libcore.io.Memory` element-array natives.
trait SwapBytes: Copy {
    fn swapped(self) -> Self;
}

macro_rules! impl_swap_int {
    ($($ty:ty),*) => {$(
        impl SwapBytes for $ty {
            fn swapped(self) -> Self { <$ty>::swap_bytes(self) }
        }
    )*};
}

impl_swap_int!(i16, i32, i64, u16);

impl SwapBytes for f32 {
    fn swapped(self) -> Self {
        f32::from_bits(self.to_bits().swap_bytes())
    }
}

impl SwapBytes for f64 {
    fn swapped(self) -> Self {
        f64::from_bits(self.to_bits().swap_bytes())
    }
}

fn swap_bytes<T: SwapBytes>(v: T) -> T {
    v.swapped()
}

/// `memmove(Object dstBase, int dstOffset, Object srcBase, int srcOffset, long n)`.
///
/// Either base may be a heap `byte[]` or `null` (native synthetic address in
/// the offset field), matching the two address spaces the framework mixes.
fn memmove(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let dst_base = vm.arg_ref(ctx, 0);
    let dst_off = vm.arg_i32(ctx, 1);
    let src_base = vm.arg_ref(ctx, 2);
    let src_off = vm.arg_i32(ctx, 3);
    let len = vm.arg_i64(ctx, 4).max(0) as usize;

    let src = if src_base == NULL {
        read_bytes_at(vm, src_off as i64, len)?
    } else {
        array_read_bytes(vm, src_base, src_off.max(0) as usize, len)?
    };
    if dst_base == NULL {
        write_bytes_at(vm, dst_off as i64, &src)
    } else {
        array_write_bytes(vm, dst_base, dst_off.max(0) as usize, &src)
    }
}

// -- Java primitive-array byte access ---------------------------------------

fn array_len(a: &ArrayObj) -> usize {
    a.data.len()
}

fn elem_scale(d: &ArrayData) -> usize {
    match d {
        ArrayData::Bool(_) | ArrayData::Byte(_) => 1,
        ArrayData::Char(_) | ArrayData::Short(_) => 2,
        ArrayData::Int(_) | ArrayData::Float(_) | ArrayData::Ref(_) => 4,
        ArrayData::Long(_) | ArrayData::Double(_) => 8,
    }
}

fn elem_byte(d: &ArrayData, idx: usize, within: usize) -> u8 {
    match d {
        ArrayData::Bool(v) => (v[idx] as u8).to_le_bytes()[within],
        ArrayData::Byte(v) => (v[idx] as u8).to_le_bytes()[within],
        ArrayData::Char(v) => v[idx].to_le_bytes()[within],
        ArrayData::Short(v) => v[idx].to_le_bytes()[within],
        ArrayData::Int(v) => v[idx].to_le_bytes()[within],
        ArrayData::Long(v) => v[idx].to_le_bytes()[within],
        ArrayData::Float(v) => v[idx].to_le_bytes()[within],
        ArrayData::Double(v) => v[idx].to_le_bytes()[within],
        ArrayData::Ref(v) => v[idx].to_le_bytes()[within],
    }
}

fn set_elem_byte(d: &mut ArrayData, idx: usize, within: usize, byte: u8) {
    macro_rules! set {
        ($v:expr, $ty:ty) => {{
            let mut w = $v.to_le_bytes();
            w[within] = byte;
            $v = <$ty>::from_le_bytes(w);
        }};
    }
    match d {
        ArrayData::Bool(v) => {
            let mut w = (v[idx] as u8).to_le_bytes();
            w[within] = byte;
            v[idx] = u8::from_le_bytes(w) != 0;
        }
        ArrayData::Byte(v) => set!(v[idx], i8),
        ArrayData::Char(v) => set!(v[idx], u16),
        ArrayData::Short(v) => set!(v[idx], i16),
        ArrayData::Int(v) => set!(v[idx], i32),
        ArrayData::Long(v) => set!(v[idx], i64),
        ArrayData::Float(v) => set!(v[idx], f32),
        ArrayData::Double(v) => set!(v[idx], f64),
        ArrayData::Ref(v) => {
            let mut w = v[idx].to_le_bytes();
            w[within] = byte;
            v[idx] = u32::from_le_bytes(w);
        }
    }
}

fn array_byte_len(a: &ArrayObj) -> usize {
    array_len(a) * elem_scale(&a.data)
}

/// Read `len` little-endian bytes from a Java primitive array starting at
/// byte offset `off` (relative to the array data, not `arrayBaseOffset`).
pub(crate) fn array_read_bytes(
    vm: &mut Vm,
    arr: Handle,
    off: usize,
    len: usize,
) -> VmResult<Vec<u8>> {
    let total = match vm.heap.get(arr) {
        Some(Object::Array(a)) => array_byte_len(a),
        _ => {
            return Err(VmError::Internal(
                "native memory: expected a primitive array".into(),
            ))
        }
    };
    let end = off.saturating_add(len);
    if end > total {
        return Err(vm.throw_new(
            "Ljava/lang/ArrayIndexOutOfBoundsException;",
            Some(&format!(
                "native memory: array range {off}..{end} exceeds length {total}"
            )),
        ));
    }
    let a = match vm.heap.get(arr) {
        Some(Object::Array(a)) => a,
        _ => unreachable!(),
    };
    if let ArrayData::Byte(v) = &a.data {
        return Ok(v[off..end].iter().map(|b| *b as u8).collect());
    }
    let scale = elem_scale(&a.data);
    let mut out = vec![0u8; len];
    for (i, slot) in out.iter_mut().enumerate() {
        let bo = off + i;
        *slot = elem_byte(&a.data, bo / scale, bo % scale);
    }
    Ok(out)
}

/// Write `bytes` little-endian into a Java primitive array at byte offset `off`.
pub(crate) fn array_write_bytes(
    vm: &mut Vm,
    arr: Handle,
    off: usize,
    bytes: &[u8],
) -> VmResult<()> {
    let a = match vm.heap.get_mut(arr) {
        Some(Object::Array(a)) => a,
        _ => {
            return Err(VmError::Internal(
                "native memory: expected a primitive array".into(),
            ))
        }
    };
    let total = array_byte_len(a);
    let end = off.saturating_add(bytes.len());
    if end > total {
        return Err(vm.throw_new(
            "Ljava/lang/ArrayIndexOutOfBoundsException;",
            Some(&format!(
                "native memory: array range {off}..{end} exceeds length {total}"
            )),
        ));
    }
    if let ArrayData::Byte(v) = &mut a.data {
        for (dst, src) in v[off..end].iter_mut().zip(bytes) {
            *dst = *src as i8;
        }
        return Ok(());
    }
    let scale = elem_scale(&a.data);
    for (i, b) in bytes.iter().enumerate() {
        let bo = off + i;
        set_elem_byte(&mut a.data, bo / scale, bo % scale, *b);
    }
    Ok(())
}

/// Write typed values into a Java primitive array at element offset `off`.
fn array_write_as<T: ArrayElement>(
    vm: &mut Vm,
    arr: Handle,
    off: usize,
    vals: &[T],
) -> VmResult<()> {
    let total = match vm.heap.get(arr) {
        Some(Object::Array(a)) => array_len(a),
        _ => {
            return Err(VmError::Internal(
                "native memory: expected a primitive array".into(),
            ))
        }
    };
    if off
        .checked_add(vals.len())
        .map(|e| e > total)
        .unwrap_or(true)
    {
        return Err(vm.throw_new(
            "Ljava/lang/ArrayIndexOutOfBoundsException;",
            Some(&format!(
                "native memory: array range {off}..{} exceeds length {total}",
                off.saturating_add(vals.len())
            )),
        ));
    }
    let a = match vm.heap.get_mut(arr) {
        Some(Object::Array(a)) => a,
        _ => {
            return Err(VmError::Internal(
                "native memory: expected a primitive array".into(),
            ))
        }
    };
    T::store(&mut a.data, off, vals)
}

/// Read typed values from a Java primitive array at element offset `off`.
fn array_read_as<T: ArrayElement>(
    vm: &mut Vm,
    arr: Handle,
    off: usize,
    len: usize,
) -> VmResult<Vec<T>> {
    let total = match vm.heap.get(arr) {
        Some(Object::Array(a)) => array_len(a),
        _ => {
            return Err(VmError::Internal(
                "native memory: expected a primitive array".into(),
            ))
        }
    };
    if off.checked_add(len).map(|e| e > total).unwrap_or(true) {
        return Err(vm.throw_new(
            "Ljava/lang/ArrayIndexOutOfBoundsException;",
            Some(&format!(
                "native memory: array range {off}..{} exceeds length {total}",
                off.saturating_add(len)
            )),
        ));
    }
    let a = match vm.heap.get(arr) {
        Some(Object::Array(a)) => a,
        _ => unreachable!(),
    };
    T::load(&a.data, off, len)
}

trait ArrayElement: Copy {
    fn load(d: &ArrayData, off: usize, len: usize) -> VmResult<Vec<Self>>;
    fn store(d: &mut ArrayData, off: usize, vals: &[Self]) -> VmResult<()>;
}

macro_rules! impl_array_element {
    ($ty:ty, $variant:ident, $conv_load:expr, $conv_store:expr) => {
        impl ArrayElement for $ty {
            fn load(d: &ArrayData, off: usize, len: usize) -> VmResult<Vec<Self>> {
                match d {
                    ArrayData::$variant(v) => {
                        let mut out = Vec::with_capacity(len);
                        for x in &v[off..off + len] {
                            out.push($conv_load(*x));
                        }
                        Ok(out)
                    }
                    _ => Err(VmError::Internal(
                        "native memory: array element type mismatch".into(),
                    )),
                }
            }

            fn store(d: &mut ArrayData, off: usize, vals: &[Self]) -> VmResult<()> {
                match d {
                    ArrayData::$variant(v) => {
                        for (slot, x) in v[off..off + vals.len()].iter_mut().zip(vals) {
                            *slot = $conv_store(*x);
                        }
                        Ok(())
                    }
                    _ => Err(VmError::Internal(
                        "native memory: array element type mismatch".into(),
                    )),
                }
            }
        }
    };
}

impl_array_element!(i32, Int, |x: i32| x, |x: i32| x);
impl_array_element!(i16, Short, |x: i16| x, |x: i16| x);
impl_array_element!(i64, Long, |x: i64| x, |x: i64| x);
impl_array_element!(u16, Char, |x: u16| x, |x: u16| x);
impl_array_element!(f32, Float, |x: f32| x, |x: f32| x);
impl_array_element!(f64, Double, |x: f64| x, |x: f64| x);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn region_roundtrip_and_bounds() {
        reclaim_all();
        let a = alloc(16, false).expect("alloc");
        set_i32(a, 0x0102_0304).unwrap();
        assert_eq!(get_i32(a).unwrap(), 0x0102_0304);
        set_i64(a + 8, -1).unwrap();
        assert_eq!(get_i64(a + 8).unwrap(), -1);
        assert_eq!(get_i8(a).unwrap(), 0x04);
        // Out of bounds is a structured error, not a panic.
        assert_eq!(get_i32(a + 16), Err(MemError::OutOfBounds));
        assert_eq!(get_i32(a + 0xffff_fff0_i64), Err(MemError::OutOfBounds));
        // A freed region is a structured error.
        assert!(free(a));
        assert_eq!(get_i32(a), Err(MemError::NoRegion));
    }

    #[test]
    fn readonly_and_null_errors() {
        reclaim_all();
        let a = alloc_with(vec![1, 2, 3, 4], true).expect("alloc");
        assert_eq!(read_bytes(a, 4).unwrap(), vec![1, 2, 3, 4]);
        assert!(write_bytes(a, &[9]).is_err());
        assert_eq!(get_i32(0), Err(MemError::Null));
        assert_eq!(get_i32(1i64 << 40), Err(MemError::NoRegion));
    }

    #[test]
    fn cap_is_enforced() {
        reclaim_all();
        assert_eq!(total_bytes(), 0);
        let cap = cap_bytes();
        // A single oversized allocation is rejected without touching the host.
        let big = alloc(cap + 1, false);
        assert_eq!(big, Err(MemError::CapExceeded));
    }

    #[test]
    fn snapshot_and_restore_regions_round_trip() {
        reclaim_all();
        let a = alloc_with(vec![9, 8, 7, 6], true).expect("alloc");
        let b = alloc(4, false).expect("alloc");
        let marker = 0xdead_beefu32 as i32;
        set_i32(b, marker).unwrap();
        let saved = snapshot_regions();
        assert_eq!(saved.len(), 2);
        assert!(
            saved.windows(2).all(|w| w[0].id < w[1].id),
            "regions must be sorted by id for a deterministic snapshot"
        );

        // Simulate a fresh process: drop every region, then re-establish the
        // snapshot's table. The synthetic addresses must resolve again.
        reclaim_all();
        assert_eq!(snapshot_regions().len(), 0);
        restore_regions(&saved);
        assert_eq!(read_bytes(a, 4).unwrap(), vec![9, 8, 7, 6]);
        assert_eq!(get_i32(b).unwrap(), marker);
        assert!(
            write_bytes(a, &[0]).is_err(),
            "the read-only flag must survive a restore"
        );
        assert_eq!(snapshot_regions(), saved, "restore is exact");

        // Restoring the same table again is a no-op, not a duplication.
        restore_regions(&saved);
        assert_eq!(snapshot_regions(), saved);

        // A fresh allocation must not reuse a restored region id.
        let c = alloc(1, false).expect("alloc");
        let cid = (c as u64 >> 32) as u32;
        assert!(!saved.iter().any(|r| r.id == cid));
    }

    #[test]
    fn file_backed_region_records_source_without_embedding_bytes() {
        reclaim_all();
        let src = RegionSource::capture(
            "/apex/com.android.i18n/etc/icu/icudt72l.dat".to_string(),
            4096,
            &[1, 2, 3, 4],
            28_431_696,
            1_700_000_000,
        );
        assert_eq!(src.length, 4);
        assert!(src.matches(&[1, 2, 3, 4], 28_431_696, 1_700_000_000));
        assert!(!src.matches(&[1, 2, 3, 5], 28_431_696, 1_700_000_000));
        assert!(!src.matches(&[1, 2, 3, 4], 99, 1_700_000_000));
        assert!(!src.matches(&[1, 2, 3, 4], 28_431_696, 42));

        let a = alloc_mapped(vec![1, 2, 3, 4], true, src.clone()).expect("alloc");
        let saved = snapshot_regions();
        assert_eq!(saved.len(), 1);
        assert_eq!(saved[0].source.as_ref(), Some(&src));
        assert!(
            saved[0].data.is_empty(),
            "a file-backed region must not embed its bytes in the snapshot"
        );

        // Simulate a fresh process: the caller re-reads/validates the file and
        // hands the bytes back, then the table is rebuilt with the source kept.
        reclaim_all();
        let mut entry = saved[0].clone();
        entry.data = vec![1, 2, 3, 4];
        restore_regions(&[entry]);
        assert_eq!(read_bytes(a, 4).unwrap(), vec![1, 2, 3, 4]);
        assert!(write_bytes(a, &[0]).is_err());
        assert_eq!(snapshot_regions()[0].source.as_ref(), Some(&src));
    }

    #[test]
    fn realloc_preserves_prefix() {
        reclaim_all();
        let a = alloc(4, false).unwrap();
        write_bytes(a, &[1, 2, 3, 4]).unwrap();
        let b = realloc(a, 8).unwrap();
        assert_eq!(b, a);
        assert_eq!(read_bytes(b, 8).unwrap(), vec![1, 2, 3, 4, 0, 0, 0, 0]);
    }
}
