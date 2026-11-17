//! **T-42 (proof of concept): VM snapshot / restore.**
//!
//! This module serialises the *runtime* state of a [`Vm`] into a versioned,
//! fingerprint-checked byte stream and rebuilds a `Vm` from it. It is the
//! measured proof of concept behind [ADR-0010] (snapshot/restore as the
//! start/reset strategy) and the T-42 backlog item.
//!
//! ## What is snapshotted
//!
//! * the **loaded class set** — by descriptor, plus each class's initialisation
//!   state and static-field slots. Method bodies, vtables, field layouts and
//!   native bindings are **re-linked lazily from the same boot DEX files** on
//!   restore (the fingerprint in the header guarantees they are the same files),
//!   so no code is copied into the snapshot;
//! * the whole **heap arena** (instances, arrays, strings, `Class` mirrors and
//!   builtin payloads), preserving **handle indices** so object identity and
//!   `identityHashCode` survive;
//! * **interned strings** (identity matters: `intern()` returns the same
//!   object), the **string hash** cache, the integer cache, **native roots**,
//!   pre-allocated fatal errors, the main `ThreadGroup`, the thread table, the
//!   throwable stack table, output/exit state and the heap/metric counters.
//!
//! ## What is *not* snapshotted
//!
//! Green-thread Java frames, monitors, native converter handles and open file
//! descriptors are **preconditions**: snapshotting refuses (structured
//! `SNAPSHOT_PRECONDITION`) while they are live. The T-43 work item lists the
//! remaining per-process state (pid/tid, RNG, clock, properties, resources,
//! binder identity, fds) that must be re-seeded after restore. The header
//! carries a locale and a clock anchor so the re-seed has somewhere to start.
//!
//! ## Copy-light design
//!
//! The crate is `#![forbid(unsafe_code)]`, so `mmap(MAP_PRIVATE)` / copy-on-write
//! of the arena is not available here. The restore path therefore *memcpys* the
//! arena through [`Object`] decode; `examples/snapshot_probe.rs` measures that
//! decode against a raw `memcpy` of the same bytes as the honest
//! lower bound a CoW mapping would approach.
//!
//! [ADR-0010]: ../../../../DECISIONS.md

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use acos_dex::DexFile;
use borsh::{BorshDeserialize, BorshSerialize};

use crate::class::{Class, ClassId, InitState, Method, TypeId, TypeInfo};
use crate::error::{VmError, VmResult};
use crate::heap::{
    ArrayData, ArrayObj, BuiltinData, Handle, Heap, Object, StackTraceElementData, ThreadData, NULL,
};
use crate::javabridge::JavaValue;
use crate::natives::fwk_fonts::{FamilyRecordData, FontRecordData, FontState};
use crate::natives::gfx_text::{FaceRef, TextState, TypefaceRecordData};
use crate::vm::{CoreLib, ThreadRT, ThreadStatus, Vm, VmOptions};

/// File magic at offset 0 of every snapshot.
pub const SNAPSHOT_MAGIC: [u8; 8] = *b"ACOSSNAP";
/// Snapshot format version. Bump on any layout change; restore refuses to read
/// a stream whose version differs.
///
/// Version 3 adds the native-memory region table (W154-A): a mapped ICU `.dat`
/// `DirectByteBuffer` captured by a primed template otherwise restores to a
/// stale synthetic address in a fresh process. Version 4 records a file-backed
/// region's *source* (path/offset/hash/size/mtime) instead of its bytes and
/// re-reads it on restore (W160-A), so the boot cache no longer carries the
/// 28 MiB ICU mapping.
pub const SNAPSHOT_FORMAT_VERSION: u32 = 4;

/// Process-native identity recovered from a restored `Vm` (T-43).
#[derive(Clone, Debug, Default)]
pub struct RestoreSeed {
    /// Local Java `Binder` instances: `(binder id, heap handle)`.
    pub local_binders: Vec<(u64, Handle)>,
    /// Java `BinderProxy` instances: `(binder id, heap handle)`.
    pub binder_proxies: Vec<(u64, Handle)>,
    /// `android.os.MessageQueue.mPtr` values that need a live native looper.
    pub message_queue_ptrs: Vec<u64>,
}

/// Parsed snapshot header (also the tamper surface for mismatch tests).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SnapshotHeader {
    pub version: u32,
    pub corelib: CoreLib,
    pub trace_natives: bool,
    pub gc_stress: bool,
    pub heap_max_bytes: u64,
    /// Combined boot-DEX fingerprint (checksum + SHA-1 + size of every loaded
    /// provider, in order).
    pub boot_fingerprint: u64,
    pub property_hash: u64,
    pub locale: String,
    pub clock_anchor_millis: u64,
    pub boot_dir: Option<String>,
    pub class_count: u32,
    pub object_slots: u32,
}

/// Wall-clock-free timings for one restore, used by the measurement harness.
#[derive(Clone, Copy, Debug, Default)]
pub struct RestoreStats {
    /// Time spent copying the snapshot bytes into an owned buffer. This is the
    /// honest lower bound a `mmap(MAP_PRIVATE)`/CoW arena restore would
    /// approach (the crate forbids `unsafe`, so no real mapping is done).
    pub copy_nanos: u64,
    /// Time spent fully decoding the stream into a live [`Vm`].
    pub decode_nanos: u64,
    /// Bytes in the stream.
    pub bytes: u64,
}

// ---------------------------------------------------------------------------
// Process-global linked boot metadata (T-43)
// ---------------------------------------------------------------------------

/// The immutable, per-boot-classpath part of a linked VM class table.
///
/// Built once per `(boot fingerprint, ordered class descriptor list)` from a VM
/// whose classes are already fully linked (the framework template, or the first
/// restore of a given snapshot), and shared read-only by every later restore in
/// the process. A restore clones this image instead of re-parsing DEX
/// `class_data`, re-formatting signatures, re-interned strings, rebuilding
/// vtables/interface tables and re-decoding method bodies for hundreds of boot
/// classes.
///
/// Only **layout** is shared: `statics`, `init` state, `class_obj` handles, the
/// heap, the interner and every inline-cache/quickening side vector stay
/// per-VM, so two VMs restored from the same image never share mutable state.
/// The `MethodCode` bodies inside are already process-shared `Arc`s and are a
/// pure function of the DEX bytes.
pub(crate) struct BootImage {
    pub(crate) classes: Vec<Class>,
    pub(crate) methods: Vec<Method>,
    pub(crate) types: Vec<TypeInfo>,
    pub(crate) type_by_desc: HashMap<String, TypeId>,
    pub(crate) class_by_desc: HashMap<String, ClassId>,
    pub(crate) loaded_array_elems: HashSet<String>,
    pub(crate) builtin_alloc: HashMap<ClassId, crate::corelib::BuiltinAlloc>,
    pub(crate) c_object: ClassId,
    pub(crate) c_class: ClassId,
    pub(crate) c_string: ClassId,
    pub(crate) c_string_builder: ClassId,
    pub(crate) c_throwable: ClassId,
    pub(crate) c_integer: ClassId,
    pub(crate) c_long: ClassId,
    pub(crate) c_thread: ClassId,
    pub(crate) c_print_stream: ClassId,
    pub(crate) c_system: ClassId,
    pub(crate) dex_files: Vec<Arc<DexFile>>,
    /// The source path of every provider, index-aligned with `dex_files`, so a
    /// serialized image can re-open the identical list in a fresh process.
    pub(crate) dex_paths: Vec<Option<PathBuf>>,
    pub(crate) boot_dexs: Vec<usize>,
    pub(crate) boot_lazy_jars: Vec<String>,
    pub(crate) boot_dir: Option<PathBuf>,
}

impl BootImage {
    fn capture(vm: &Vm) -> BootImage {
        BootImage {
            classes: vm.classes.clone(),
            methods: vm.methods.clone(),
            types: vm.types.clone(),
            type_by_desc: vm.type_by_desc.clone(),
            class_by_desc: vm.class_by_desc.clone(),
            loaded_array_elems: vm.loaded_array_elems.clone(),
            builtin_alloc: vm.builtin_alloc.clone(),
            c_object: vm.c_object,
            c_class: vm.c_class,
            c_string: vm.c_string,
            c_string_builder: vm.c_string_builder,
            c_throwable: vm.c_throwable,
            c_integer: vm.c_integer,
            c_long: vm.c_long,
            c_thread: vm.c_thread,
            c_print_stream: vm.c_print_stream,
            c_system: vm.c_system,
            dex_files: vm.dexs.iter().map(|d| d.file.clone()).collect(),
            dex_paths: vm.dexs.iter().map(|d| d.source.clone()).collect(),
            boot_dexs: vm.boot_dexs.clone(),
            boot_lazy_jars: vm.boot_lazy_jars.clone(),
            boot_dir: vm.boot_dir.clone(),
        }
    }

    fn same_layout(&self, class_desc: &[String]) -> bool {
        self.classes.len() == class_desc.len()
            && self
                .classes
                .iter()
                .zip(class_desc)
                .all(|(c, d)| c.descriptor == *d)
    }

    /// Serialize the fully linked boot image (class tables, linked methods with
    /// their decoded bodies, type tables and the provider list) into a compact
    /// borsh stream. This is the payload the on-disk boot cache persists so a
    /// cold process clones the image instead of re-parsing and re-linking the
    /// boot classpath.
    pub(crate) fn to_bytes(&self) -> Vec<u8> {
        let data = SerializedBootImage {
            classes: self.classes.clone(),
            methods: self.methods.clone(),
            types: self.types.clone(),
            type_by_desc: self.type_by_desc.clone(),
            class_by_desc: self.class_by_desc.clone(),
            loaded_array_elems: self.loaded_array_elems.clone(),
            builtin_alloc: self.builtin_alloc.clone(),
            c_object: self.c_object,
            c_class: self.c_class,
            c_string: self.c_string,
            c_string_builder: self.c_string_builder,
            c_throwable: self.c_throwable,
            c_integer: self.c_integer,
            c_long: self.c_long,
            c_thread: self.c_thread,
            c_print_stream: self.c_print_stream,
            c_system: self.c_system,
            boot_dexs: self.boot_dexs.clone(),
            boot_lazy_jars: self.boot_lazy_jars.clone(),
            boot_dir: self
                .boot_dir
                .as_ref()
                .map(|p| p.to_string_lossy().into_owned()),
            dex_paths: self
                .dex_paths
                .iter()
                .map(|p| p.as_ref().map(|p| p.to_string_lossy().into_owned()))
                .collect(),
            // Parsed id/string/type/method pools, one per provider. A cold
            // process rebuilds each `DexFile` from these plus a plain re-read
            // of the raw bytes instead of re-running `DexFile::parse`.
            // Measured a net loss (W131-A: +4 ms boot, +21 MB), so they are
            // opt-in via `ACOS_BOOT_CACHE_DEX_TABLES=1`.
            dex_tables: if std::env::var_os("ACOS_BOOT_CACHE_DEX_TABLES").is_none() {
                Vec::new()
            } else {
                self.dex_files.iter().map(|f| Some(f.to_tables())).collect()
            },
        };
        borsh::to_vec(&data).expect("in-memory borsh serialization cannot fail")
    }

    /// Rebuild a [`BootImage`] from [`BootImage::to_bytes`]. The provider list
    /// is re-opened from the recorded paths through the process-global DEX
    /// cache (one parse per process); any failure is a structured error so the
    /// caller can fall back to a real boot.
    pub(crate) fn from_bytes(bytes: &[u8]) -> VmResult<BootImage> {
        crate::profile::note_alloc("boot_image_serialized", bytes.len() as u64);
        let data: SerializedBootImage = borsh::from_slice(bytes)
            .map_err(|e| VmError::SnapshotFormat(format!("linked boot image: {e}")))?;
        // The cache payload is untrusted input. Every provider path must
        // resolve inside the configured boot directory, so a poisoned entry
        // cannot make the host `read`/parse an arbitrary file as DEX. A failure
        // is a structured error (the caller falls back to a fresh boot), never
        // a panic or an out-of-tree open.
        let boot_root = match data.boot_dir.as_deref() {
            Some(dir) => Some(std::fs::canonicalize(dir).map_err(|e| {
                VmError::SnapshotFormat(format!(
                    "linked boot image boot_dir {dir:?} is not resolvable: {e}"
                ))
            })?),
            None => None,
        };
        let dex_paths = data.dex_paths;
        let mut dex_tables = data.dex_tables.into_iter();
        let mut dex_tables_used = 0u64;
        let mut dex_files = Vec::with_capacity(dex_paths.len());
        for path in &dex_paths {
            let Some(path) = path else {
                return Err(VmError::SnapshotFormat(
                    "linked boot image contains a non-boot DEX provider".to_string(),
                ));
            };
            if let Some(root) = &boot_root {
                let resolved = std::fs::canonicalize(path).map_err(|e| {
                    VmError::SnapshotFormat(format!(
                        "linked boot image provider {path:?} is not resolvable: {e}"
                    ))
                })?;
                if !resolved.starts_with(root) {
                    return Err(VmError::SnapshotFormat(format!(
                        "linked boot image provider {path:?} is outside the boot dir"
                    )));
                }
            }
            let tables = dex_tables.next().flatten();
            let tables = if std::env::var_os("ACOS_BOOT_CACHE_DEX_TABLES").is_none() {
                None
            } else {
                tables
            };
            if tables.is_some() {
                dex_tables_used += 1;
            }
            let file = acos_dex::cache::boot_cache()
                .get_or_load_with_tables(Path::new(path), tables)
                .map_err(VmError::Load)?;
            dex_files.push(file);
        }
        crate::profile::set_note("dex_tables_used", dex_tables_used);
        crate::profile::set_note("dex_providers", dex_files.len() as u64);
        Ok(BootImage {
            classes: data.classes,
            methods: data.methods,
            types: data.types,
            type_by_desc: data.type_by_desc,
            class_by_desc: data.class_by_desc,
            loaded_array_elems: data.loaded_array_elems,
            builtin_alloc: data.builtin_alloc,
            c_object: data.c_object,
            c_class: data.c_class,
            c_string: data.c_string,
            c_string_builder: data.c_string_builder,
            c_throwable: data.c_throwable,
            c_integer: data.c_integer,
            c_long: data.c_long,
            c_thread: data.c_thread,
            c_print_stream: data.c_print_stream,
            c_system: data.c_system,
            dex_files,
            dex_paths: dex_paths
                .into_iter()
                .map(|p| p.map(PathBuf::from))
                .collect(),
            boot_dexs: data.boot_dexs,
            boot_lazy_jars: data.boot_lazy_jars,
            boot_dir: data.boot_dir.map(PathBuf::from),
        })
    }
}

/// Wire form of [`BootImage`] for the on-disk cache. `dex_files` is deliberately
/// excluded: `Arc<DexFile>` is a process-local parsed object, so the provider
/// list travels as paths and is re-opened through `acos_dex::shared_dex`.
#[derive(BorshSerialize, BorshDeserialize)]
struct SerializedBootImage {
    classes: Vec<Class>,
    methods: Vec<Method>,
    types: Vec<TypeInfo>,
    type_by_desc: HashMap<String, TypeId>,
    class_by_desc: HashMap<String, ClassId>,
    loaded_array_elems: HashSet<String>,
    builtin_alloc: HashMap<ClassId, crate::corelib::BuiltinAlloc>,
    c_object: ClassId,
    c_class: ClassId,
    c_string: ClassId,
    c_string_builder: ClassId,
    c_throwable: ClassId,
    c_integer: ClassId,
    c_long: ClassId,
    c_thread: ClassId,
    c_print_stream: ClassId,
    c_system: ClassId,
    boot_dexs: Vec<usize>,
    boot_lazy_jars: Vec<String>,
    boot_dir: Option<String>,
    dex_paths: Vec<Option<String>>,
    /// Serialized parsed pools, parallel to `dex_paths`. `None` is a legacy /
    /// non-boot entry and falls back to a full parse.
    dex_tables: Vec<Option<acos_dex::DexTables>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct BootKey {
    fingerprint: u64,
    corelib: u8,
    /// Order-sensitive hash of the loaded class descriptors: a snapshot with a
    /// different (or differently ordered) class set must not reuse an image.
    classes: u64,
}

/// Bounded LRU-ish list of boot images. The limit keeps the process-global
/// metadata bounded when a process snapshots several different class sets.
const BOOT_IMAGE_SLOTS: usize = 4;

type ImageCache = Mutex<Vec<(BootKey, Arc<BootImage>)>>;

static BOOT_IMAGES: OnceLock<ImageCache> = OnceLock::new();

fn image_cache() -> &'static ImageCache {
    BOOT_IMAGES.get_or_init(|| Mutex::new(Vec::new()))
}

fn class_list_hash<'a>(descs: impl Iterator<Item = &'a str>) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    let mut n = 0u64;
    for d in descs {
        h ^= d.len() as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
        for b in d.as_bytes() {
            h ^= *b as u64;
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
        n += 1;
    }
    h ^= n;
    h
}

fn lookup_image(key: BootKey) -> Option<Arc<BootImage>> {
    image_cache()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .rev()
        .find(|(k, _)| *k == key)
        .map(|(_, img)| Arc::clone(img))
}

fn remember_image(key: BootKey, image: BootImage) {
    let mut cache = image_cache().lock().unwrap_or_else(|e| e.into_inner());
    if cache.iter().any(|(k, _)| *k == key) {
        return;
    }
    if cache.len() >= BOOT_IMAGE_SLOTS {
        cache.remove(0);
    }
    cache.push((key, Arc::new(image)));
}

/// Number of boot images currently resident in this process (diagnostics).
#[must_use]
pub fn boot_image_count() -> usize {
    image_cache()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .len()
}

/// Drop every boot image. Existing restores keep their cloned tables alive.
pub fn clear_boot_images() {
    image_cache()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clear();
}

// ---------------------------------------------------------------------------
// Compact little-endian cursor writer / reader
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Writer {
    buf: Vec<u8>,
}

impl Writer {
    fn u8(&mut self, v: u8) {
        self.buf.push(v);
    }
    fn bool(&mut self, v: bool) {
        self.buf.push(v as u8);
    }
    fn u16(&mut self, v: u16) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    fn u32(&mut self, v: u32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    fn i32(&mut self, v: i32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    fn u64(&mut self, v: u64) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    fn handle(&mut self, v: Handle) {
        self.u32(v);
    }
    fn i64(&mut self, v: i64) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }
    fn raw(&mut self, v: &[u8]) {
        self.buf.extend_from_slice(v);
    }
    fn str(&mut self, s: &str) {
        self.u32(s.len() as u32);
        self.raw(s.as_bytes());
    }
}

struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(buf: &'a [u8]) -> Reader<'a> {
        Reader { buf, pos: 0 }
    }

    fn need(&self, n: usize) -> VmResult<()> {
        if self.pos + n > self.buf.len() {
            return Err(VmError::SnapshotFormat(format!(
                "unexpected end of snapshot at offset {} (need {n} bytes, have {})",
                self.pos,
                self.buf.len() - self.pos
            )));
        }
        Ok(())
    }

    fn u8(&mut self) -> VmResult<u8> {
        self.need(1)?;
        let v = self.buf[self.pos];
        self.pos += 1;
        Ok(v)
    }
    fn bool(&mut self) -> VmResult<bool> {
        Ok(self.u8()? != 0)
    }
    fn u16(&mut self) -> VmResult<u16> {
        self.need(2)?;
        let v = u16::from_le_bytes([self.buf[self.pos], self.buf[self.pos + 1]]);
        self.pos += 2;
        Ok(v)
    }
    fn u32(&mut self) -> VmResult<u32> {
        self.need(4)?;
        let mut b = [0u8; 4];
        b.copy_from_slice(&self.buf[self.pos..self.pos + 4]);
        self.pos += 4;
        Ok(u32::from_le_bytes(b))
    }
    fn i32(&mut self) -> VmResult<i32> {
        Ok(self.u32()? as i32)
    }
    fn u64(&mut self) -> VmResult<u64> {
        self.need(8)?;
        let mut b = [0u8; 8];
        b.copy_from_slice(&self.buf[self.pos..self.pos + 8]);
        self.pos += 8;
        Ok(u64::from_le_bytes(b))
    }
    fn i64(&mut self) -> VmResult<i64> {
        Ok(self.u64()? as i64)
    }
    fn str(&mut self) -> VmResult<String> {
        let n = self.u32()? as usize;
        self.need(n)?;
        let s = std::str::from_utf8(&self.buf[self.pos..self.pos + n])
            .map_err(|_| VmError::SnapshotFormat("invalid UTF-8 string".into()))?
            .to_string();
        self.pos += n;
        Ok(s)
    }
    fn handle(&mut self) -> VmResult<Handle> {
        self.u32()
    }
    fn remaining(&self) -> &'a [u8] {
        &self.buf[self.pos..]
    }
}

// ---------------------------------------------------------------------------
// Fingerprints
// ---------------------------------------------------------------------------

/// FNV-1a over the loaded DEX provider fingerprints, in order.
fn boot_fingerprint(vm: &Vm) -> u64 {
    let dexs: Vec<Arc<DexFile>> = vm.dexs.iter().map(|d| d.file.clone()).collect();
    boot_fingerprint_parts(vm.boot_dir.as_deref(), &dexs)
}

/// [`boot_fingerprint`] from the header alone, so a restore can validate the
/// snapshot before constructing a `Vm` (T-43 fast path).
fn boot_fingerprint_parts(boot_dir: Option<&Path>, dexs: &[Arc<DexFile>]) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    let mut mix = |b: u8| {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    };
    // Hash the boot classpath *definition* (the `BOOTCLASSPATH` jar list and
    // each jar's size), not the dynamically loaded `vm.dexs`: lazy providers are
    // pulled in on demand by the framework boot, so hashing `vm.dexs` would make
    // a framework snapshot invalid against a fresh `Vm::new`. The list is the
    // stable identity of the boot image; content changes still surface through
    // the class-layout checks in `restore`.
    if let Some(dir) = boot_dir {
        if let Ok(text) = std::fs::read_to_string(dir.join("BOOTCLASSPATH")) {
            for line in text.lines() {
                let jar = line.trim();
                for b in jar.as_bytes() {
                    mix(*b);
                }
                mix(b'\n');
                if let Ok(meta) =
                    std::fs::metadata(dir.join("dex").join(jar.trim_end_matches(".jar")))
                {
                    for b in meta.len().to_le_bytes() {
                        mix(b);
                    }
                }
            }
        }
    }
    // Bootstrap has no on-disk boot classpath: fall back to the loaded providers.
    if boot_dir.is_none() {
        for dex in dexs {
            let bytes = dex.bytes();
            let header = dex.header();
            for b in header.checksum.to_le_bytes() {
                mix(b);
            }
            for b in header.signature {
                mix(b);
            }
            for b in (bytes.len() as u64).to_le_bytes() {
                mix(b);
            }
        }
    }
    h
}

/// The boot-classpath fingerprint for a boot directory, computed the same way
/// [`boot_fingerprint`] computes it from a live `Vm` (the `BOOTCLASSPATH` jar
/// list and each jar's on-disk identity).
///
/// Exposed so the on-disk boot-image cache (T-43b) can key an entry without
/// constructing a `Vm`. It is a **cache key**, not a security token: the
/// snapshot stream is re-validated by [`Vm::restore`] on every load.
#[must_use]
pub fn boot_dir_fingerprint(boot_dir: Option<&Path>) -> u64 {
    boot_fingerprint_parts(boot_dir, &[])
}

/// Hash of the property table that a snapshot is valid for.
///
/// ACOS has no guest `SystemProperties` table yet, so the hash covers the
/// configuration the VM actually consults: corelib, boot dir, locale and the
/// `ACOS_*` environment (sorted, so it is order-independent and deterministic).
fn property_hash(vm: &Vm) -> u64 {
    property_hash_parts(vm.corelib, vm.boot_dir.as_deref(), &vm.snapshot_locale)
}

/// [`property_hash`] from the snapshot header plus the environment, so a
/// restore can refuse a mismatch before constructing a `Vm`.
fn property_hash_parts(corelib: CoreLib, boot_dir: Option<&Path>, locale: &str) -> u64 {
    let mut props: Vec<(String, String)> = std::env::vars()
        .filter(|(k, _)| k.starts_with("ACOS_"))
        .collect();
    props.sort();
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    let mix = |h: &mut u64, s: &str| {
        for b in s.as_bytes() {
            *h ^= *b as u64;
            *h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
        *h ^= 0xff;
        *h = h.wrapping_mul(0x0000_0100_0000_01b3);
    };
    let core = match corelib {
        CoreLib::Bootstrap => "bootstrap",
        CoreLib::Real => "real",
    };
    mix(&mut h, core);
    mix(&mut h, locale);
    if let Some(d) = boot_dir {
        mix(&mut h, &d.to_string_lossy());
    }
    for (k, v) in &props {
        mix(&mut h, k);
        mix(&mut h, v);
    }
    h
}

// ---------------------------------------------------------------------------
// Preconditions
// ---------------------------------------------------------------------------

impl Vm {
    /// True when `frame` is the host-bridge sentinel base frame (the only frame
    /// a quiescent host thread should carry at snapshot time).
    fn is_sentinel_frame(&self, frame: &crate::frame::Frame) -> bool {
        let Some(m) = self.methods.get(frame.method as usize) else {
            return false;
        };
        m.class == self.c_object && m.name == "<init>" && m.sig == "()V"
    }

    /// Enforce the T-42 snapshot preconditions and return a structured error
    /// naming the first violation.
    pub fn snapshot_preconditions(&self) -> VmResult<()> {
        if self.pending_exception != NULL {
            return Err(VmError::SnapshotPrecondition(
                "a pending Java exception is live (handle != 0)".into(),
            ));
        }
        if self.threads.len() > 1 {
            return Err(VmError::SnapshotPrecondition(format!(
                "{} green threads are live; only the main thread may exist",
                self.threads.len()
            )));
        }
        for (i, t) in self.threads.iter().enumerate() {
            if !t.frames.is_empty() {
                // The host thread always carries one sentinel base frame
                // (`javabridge::ensure_host_thread`); it is a host bridge
                // artifact, not Java execution state, and the snapshot stores no
                // frames. Allow it when it is the only frame.
                let sentinel_only = t.frames.len() == 1 && self.is_sentinel_frame(&t.frames[0]);
                if !sentinel_only {
                    return Err(VmError::SnapshotPrecondition(format!(
                        "thread {i} has {} live Java frames (running); snapshot requires a quiescent VM",
                        t.frames.len()
                    )));
                }
            }
            match t.status {
                ThreadStatus::Runnable | ThreadStatus::Finished => {}
                other => {
                    return Err(VmError::SnapshotPrecondition(format!(
                        "thread {i} is {other:?} (not runnable); snapshot requires a quiescent VM"
                    )))
                }
            }
            if t.wait_monitor.is_some() {
                return Err(VmError::SnapshotPrecondition(format!(
                    "thread {i} holds a wait monitor"
                )));
            }
            if t.wake_at.is_some() {
                return Err(VmError::SnapshotPrecondition(format!(
                    "thread {i} has a pending wake deadline"
                )));
            }
            if !t.joiners.is_empty() {
                return Err(VmError::SnapshotPrecondition(format!(
                    "thread {i} has {} joiner(s)",
                    t.joiners.len()
                )));
            }
        }
        if self.vfs.open_fd_count() != 0 {
            return Err(VmError::SnapshotPrecondition(format!(
                "{} file descriptor(s) are open; the snapshot stores no fd number",
                self.vfs.open_fd_count()
            )));
        }
        if !self.monitors.is_empty() {
            return Err(VmError::SnapshotPrecondition(format!(
                "{} monitor(s) are held",
                self.monitors.len()
            )));
        }
        // The stream stores class descriptors, not code: every DEX provider must
        // be one `Vm::restore` will re-load from the same boot files. An app DEX
        // would be silently missing after restore, so refuse it (T-43).
        match self.corelib {
            CoreLib::Bootstrap => {
                if !self.dexs.is_empty() {
                    return Err(VmError::SnapshotPrecondition(format!(
                        "{} application DEX image(s) are loaded; the T-42 snapshot only covers the core provider set",
                        self.dexs.len()
                    )));
                }
            }
            CoreLib::Real => {
                if self.dexs.len() != self.boot_dexs.len() {
                    return Err(VmError::SnapshotPrecondition(format!(
                        "{} non-boot DEX image(s) are loaded ({} boot, {} total); the T-42 snapshot only covers the core provider set",
                        self.dexs.len() - self.boot_dexs.len(),
                        self.boot_dexs.len(),
                        self.dexs.len()
                    )));
                }
            }
        }
        Ok(())
    }

    /// Serialise this VM into a versioned snapshot byte stream.
    ///
    /// The bytes are a pure function of the VM state (no wall clock, no HashMap
    /// iteration order): two snapshots of the same state are byte-equal.
    pub fn snapshot(&self) -> VmResult<Vec<u8>> {
        self.snapshot_preconditions()?;

        // T-43: record the linked boot metadata so every later `Vm::restore` in
        // this process can clone it instead of re-linking the boot classpath.
        if self.corelib == CoreLib::Real {
            let key = BootKey {
                fingerprint: boot_fingerprint(self),
                corelib: 1,
                classes: class_list_hash(self.classes.iter().map(|c| c.descriptor.as_str())),
            };
            if lookup_image(key).is_none() {
                remember_image(key, BootImage::capture(self));
            }
        }

        let mut w = Writer::default();
        w.raw(&SNAPSHOT_MAGIC);
        w.u32(SNAPSHOT_FORMAT_VERSION);
        w.u8(match self.corelib {
            CoreLib::Bootstrap => 0,
            CoreLib::Real => 1,
        });
        w.bool(self.native_trace.is_some());
        w.bool(self.gc_stress);
        w.u64(self.heap.max_bytes as u64);
        w.u64(boot_fingerprint(self));
        w.u64(property_hash(self));
        w.str(&self.snapshot_locale);
        w.u64(self.clock_anchor_millis);
        match &self.boot_dir {
            Some(d) => {
                w.bool(true);
                w.str(&d.to_string_lossy());
            }
            None => w.bool(false),
        }
        w.u32(self.classes.len() as u32);
        w.u32(self.heap.objects.len() as u32);

        // ---- heap counters / metrics ------------------------------------
        w.u64(self.heap.live_bytes as u64);
        w.u64(self.heap.peak_bytes as u64);
        w.u64(self.heap.allocations);
        w.u64(self.heap.gc_count);
        w.u64(self.heap.gc_nanos);
        w.u64(self.metrics.instructions);
        w.u64(self.metrics.invokes);
        w.u64(self.metrics.clinit_count);

        // ---- process roots / scalar state -------------------------------
        w.u64(self.next_converter);
        w.u32(self.next_thread_id);
        w.handle(self.oom_object);
        w.handle(self.stack_overflow_object);
        w.handle(self.main_thread_group);
        w.handle(self.stdout);
        w.handle(self.stderr);
        w.bool(self.stopping);
        match self.exit_request {
            Some(c) => {
                w.bool(true);
                w.i32(c);
            }
            None => w.bool(false),
        }
        w.u32(self.out_buf.len() as u32);
        w.raw(&self.out_buf);
        w.u32(self.err_buf.len() as u32);
        w.raw(&self.err_buf);

        // ---- classes (descriptor + init + statics) ----------------------
        for c in &self.classes {
            w.str(&c.descriptor);
            w.u8(init_state_tag(c.init));
            w.bool(c.is_primitive);
            w.bool(c.is_array);
            w.u32(c.statics.len() as u32);
            for &s in &c.statics {
                w.u32(s);
            }
            w.handle(c.class_obj);
        }

        // ---- heap arena (slot-preserving) -------------------------------
        for obj in &self.heap.objects {
            match obj {
                Some(o) => {
                    w.bool(true);
                    encode_object(&mut w, o);
                }
                None => w.bool(false),
            }
        }

        // ---- interned strings / roots / caches --------------------------
        w.u32(self.interned.len() as u32);
        for &h in &self.interned {
            w.handle(h);
        }
        w.u32(self.native_roots.len() as u32);
        for &h in &self.native_roots {
            w.handle(h);
        }

        // int_cache: sorted by key for determinism.
        let mut ints: Vec<(i32, Handle)> = self.int_cache.iter().map(|(k, v)| (*k, *v)).collect();
        ints.sort_by_key(|(k, _)| *k);
        w.u32(ints.len() as u32);
        for (k, v) in ints {
            w.i32(k);
            w.handle(v);
        }

        // string_hashes: sorted by handle for determinism.
        let mut sh: Vec<(Handle, i32)> = self.string_hashes.iter().map(|(k, v)| (*k, *v)).collect();
        sh.sort_by_key(|(k, _)| *k);
        w.u32(sh.len() as u32);
        for (k, v) in sh {
            w.handle(k);
            w.i32(v);
        }

        // throwable stacks: sorted by handle for determinism.
        let mut ts: Vec<(Handle, &Vec<crate::vm::StackEntry>)> =
            self.throwable_stacks.iter().map(|(k, v)| (*k, v)).collect();
        ts.sort_by_key(|(k, _)| *k);
        w.u32(ts.len() as u32);
        for (h, entries) in ts {
            w.handle(h);
            w.u32(entries.len() as u32);
            for e in entries {
                w.str(&e.class);
                w.str(&e.method);
                w.u32(e.line);
            }
        }

        // ---- threads (quiescent, no frames) -----------------------------
        w.u32(self.threads.len() as u32);
        for t in &self.threads {
            w.handle(t.object);
            w.u8(thread_status_tag(t.status));
        }

        // ---- font / typeface native tables (W115-A) ---------------------
        // The snapshot carries the *identity* of every native
        // `Font`/`FontFamily`/`Typeface` handle, not the resolved faces: a
        // restore rebuilds them lazily from the recorded file path + index.
        // This lets the template prime `Typeface.<clinit>` (which fills these
        // tables) without the restored process holding dangling native handles.
        // Bootstrap VMs never load the Android font stack; writing the live
        // process tables there would leak another template's handles into a
        // bootstrap snapshot. Only a real-corelib snapshot carries font state.
        if self.corelib == CoreLib::Real {
            let fonts = crate::natives::fwk_fonts::current_font_state();
            write_font_state(&mut w, &fonts);
            let text = crate::natives::gfx_text::current_text_state();
            write_text_state(&mut w, &text);
        } else {
            write_font_state(&mut w, &FontState::default());
            write_text_state(&mut w, &TextState::default());
        }

        // ---- ICU converter table (native charset id -> canonical name) ---
        // The native `NativeConverter` is a Java wrapper around an id; the
        // per-VM table is the only process state behind it. Priming `Typeface`
        // leaves one open (the `fonts.xml` parser's UTF-8 decoder), so the
        // table travels with the snapshot instead of refusing to snapshot.
        let mut convs: Vec<(u64, String)> = self
            .converters
            .iter()
            .map(|(k, v)| (*k, v.clone()))
            .collect();
        convs.sort_by_key(|(k, _)| *k);
        w.u32(convs.len() as u32);
        for (k, v) in convs {
            w.u64(k);
            w.str(&v);
        }

        // ---- regex registry (ICU PatternNative handles) -----------------
        // A primed `Typeface` leaves a compiled `FontVariationAxis` pattern in
        // the process registry; the restored Java `Pattern` holds its handle.
        // Carry the source + flags so a restore recompiles it.
        if self.corelib == CoreLib::Real {
            write_regex_state(&mut w, &crate::natives::icu_regex::current_regex_state());
        } else {
            write_regex_state(&mut w, &crate::natives::icu_regex::RegexState::default());
        }

        // ---- native-memory regions (W154-A) -----------------------------
        // `FileChannel.map0` copies VFS bytes into the process-global native
        // region table and hands Java a synthetic address. A template snapshot
        // can therefore hold `DirectByteBuffer`s (e.g. the ICU `.dat` reader
        // caches created while priming `Typeface.<clinit>`) whose addresses
        // name those regions. Carrying the regions lets a fresh process resolve
        // the same addresses instead of reading a stale/garbage region.
        write_native_regions(&mut w, &crate::natives::native_mem::snapshot_regions());

        Ok(w.buf)
    }

    /// Find the first heap instance whose class descriptor is `desc`.
    ///
    /// Used by the framework-init snapshot to recover the preserved
    /// `android.app.ActivityThread` handle without adding a VM field: handles
    /// are slot-preserving, so the handle found here is valid after restore.
    pub fn find_instance_of_class(&self, desc: &str) -> Option<Handle> {
        let class_id = *self.class_by_desc.get(desc)?;
        for (idx, slot) in self.heap.objects.iter().enumerate() {
            if let Some(Object::Instance { class, .. }) = slot {
                if *class == class_id {
                    return Some(idx as Handle);
                }
            }
        }
        None
    }

    /// Every heap instance whose class is `desc` or a subclass of it.
    ///
    /// Heap handles are slot-preserving across snapshot/restore, so the
    /// returned handles are valid after restore. Used by the T-43 re-seed to
    /// find the binder and looper objects a restored heap still references.
    #[must_use]
    pub fn instances_of_class(&self, desc: &str) -> Vec<Handle> {
        let Some(&root) = self.class_by_desc.get(desc) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for (idx, slot) in self.heap.objects.iter().enumerate() {
            let Some(Object::Instance { class, .. }) = slot else {
                continue;
            };
            let mut cur = Some(*class);
            while let Some(c) = cur {
                if c == root {
                    out.push(idx as Handle);
                    break;
                }
                cur = self.classes.get(c as usize).and_then(|k| k.super_class);
            }
        }
        out
    }

    /// Re-seed the process-native tables a restored heap still references and
    /// return the binder identity the runtime must import (T-43).
    ///
    /// The snapshot stores Java `Binder`/`BinderProxy` handles (the boot
    /// process's `mObject`/`mNativeData`), `MessageQueue.mPtr` and `Parcel`
    /// native handles, but not the native tables they index. This recreates a
    /// live looper for every restored `MessageQueue`, a fresh native parcel for
    /// every restored `Parcel`, and returns the binder ids for the runtime to
    /// seed into its transport.
    pub fn reseed_after_restore(&mut self) -> RestoreSeed {
        // Loopers: `nativePollOnce` would otherwise fail on the first park.
        for handle in self.instances_of_class("Landroid/os/MessageQueue;") {
            if let Ok(JavaValue::Long(ptr)) = self.get_field(handle, "mPtr") {
                if ptr > 0 {
                    let _ = crate::natives::looper::ensure_looper(ptr as u64);
                }
            }
        }
        // Parcels: a restored `Parcel` carries a boot-process handle into a
        // table that no longer exists; give it a live one (empty at rest).
        for handle in self.instances_of_class("Landroid/os/Parcel;") {
            let new = crate::natives::create_parcel_handle();
            let _ = self.set_field(handle, "mNativePtr", JavaValue::Long(new as i64));
        }

        let mut seed = RestoreSeed::default();
        for handle in self.instances_of_class("Landroid/os/Binder;") {
            if let Ok(JavaValue::Long(v)) = self.get_field(handle, "mObject") {
                if v > 0 {
                    seed.local_binders.push((v as u64, handle));
                }
            }
        }
        for handle in self.instances_of_class("Landroid/os/BinderProxy;") {
            if let Ok(JavaValue::Long(v)) = self.get_field(handle, "mNativeData") {
                if v > 0 {
                    seed.binder_proxies.push((v as u64, handle));
                }
            }
        }
        for handle in self.instances_of_class("Landroid/os/MessageQueue;") {
            if let Ok(JavaValue::Long(v)) = self.get_field(handle, "mPtr") {
                if v > 0 {
                    seed.message_queue_ptrs.push(v as u64);
                }
            }
        }
        seed.local_binders.sort_unstable_by_key(|(id, _)| *id);
        seed.binder_proxies.sort_unstable_by_key(|(id, _)| *id);
        seed.message_queue_ptrs.sort_unstable();
        seed.message_queue_ptrs.dedup();
        seed
    }

    /// Parse only the header of a snapshot (used for refusal tests and
    /// diagnostics).
    pub fn snapshot_header(bytes: &[u8]) -> VmResult<SnapshotHeader> {
        let mut r = Reader::new(bytes);
        read_header(&mut r)
    }

    /// Serialize the fully linked boot image (class tables, linked methods with
    /// decoded bodies, type tables and provider paths) captured from this VM.
    ///
    /// Only meaningful for `CoreLib::Real`: the builtin/bootstrap class set is
    /// constructed in code and is not re-opened from the boot classpath. Returns
    /// an empty vector otherwise so callers can simply skip persisting.
    pub fn boot_image_bytes(&self) -> VmResult<Vec<u8>> {
        if self.corelib != CoreLib::Real {
            return Ok(Vec::new());
        }
        Ok(BootImage::capture(self).to_bytes())
    }

    /// Restore a VM from [`Vm::snapshot`] bytes, installing the linked boot
    /// metadata in `image` (from [`Vm::boot_image_bytes`]) instead of re-linking
    /// the boot classpath.
    ///
    /// The image is registered under the snapshot's own boot key before the
    /// restore, so the normal `Vm::restore` image path takes it. A malformed
    /// image is a `Ok(None)`-style miss for the caller: this returns `Err`, and
    /// the caller falls back to a plain [`Vm::restore`] / fresh boot.
    pub fn restore_with_boot_image(snapshot: &[u8], image: &[u8]) -> VmResult<Vm> {
        let header = Vm::snapshot_header(snapshot)?;
        if header.corelib == CoreLib::Real && !image.is_empty() {
            let img = BootImage::from_bytes(image)?;
            // Key the image by its own descriptor list: `Vm::restore` derives
            // the same key from the snapshot's class list, so a tampered or
            // mismatched image simply misses and the restore re-links.
            let key = BootKey {
                fingerprint: header.boot_fingerprint,
                corelib: 1,
                classes: class_list_hash(img.classes.iter().map(|c| c.descriptor.as_str())),
            };
            remember_image(key, img);
        }
        Vm::restore(snapshot)
    }

    /// Restore a VM from [`Vm::snapshot`] bytes.
    ///
    /// The boot DEX set, property table and locale in the header must match this
    /// process; otherwise restore is refused with `SNAPSHOT_MISMATCH`. Class
    /// metadata is re-linked from the boot DEX files (no code is in the stream).
    pub fn restore(bytes: &[u8]) -> VmResult<Vm> {
        let (vm, _stats) = Vm::restore_with_stats(bytes)?;
        Ok(vm)
    }

    /// [`Vm::restore`] plus timings for the measurement harness.
    pub fn restore_with_stats(bytes: &[u8]) -> VmResult<(Vm, RestoreStats)> {
        let t_copy = std::time::Instant::now();
        // Copy-light lower bound: the whole stream is copied once here. A CoW
        // mmap would map instead of copy; the crate forbids `unsafe`, so this
        // memcpy is the measured surrogate.
        let owned = bytes.to_vec();
        let copy_nanos = t_copy.elapsed().as_nanos() as u64;
        let t_decode = std::time::Instant::now();

        let mut r = Reader::new(&owned);
        let header = read_header(&mut r)?;
        if header.version != SNAPSHOT_FORMAT_VERSION {
            return Err(VmError::SnapshotFormat(format!(
                "unsupported snapshot format version {} (expected {})",
                header.version, SNAPSHOT_FORMAT_VERSION
            )));
        }

        let opts = VmOptions {
            heap_max_bytes: header.heap_max_bytes as usize,
            gc_stress: header.gc_stress,
            corelib: header.corelib,
            boot_dir: header.boot_dir.as_ref().map(std::path::PathBuf::from),
            trace_natives: header.trace_natives,
        };

        // Strict fingerprint / metadata refusal before touching any state. The
        // header alone is enough here, so a refused restore never pays for a
        // `Vm` construction.
        let now_boot = boot_fingerprint_parts(header.boot_dir.as_deref().map(Path::new), &[]);
        if now_boot != header.boot_fingerprint {
            return Err(VmError::SnapshotMismatch(format!(
                "boot DEX fingerprint mismatch: snapshot {:016x}, boot {now_boot:016x}",
                header.boot_fingerprint
            )));
        }
        let now_locale = std::env::var("LANG").unwrap_or_else(|_| "en-US".to_string());
        if now_locale != header.locale {
            return Err(VmError::SnapshotMismatch(format!(
                "locale mismatch: snapshot {:?}, boot {:?}",
                header.locale, now_locale
            )));
        }
        let now_props = property_hash_parts(
            header.corelib,
            header.boot_dir.as_deref().map(Path::new),
            &now_locale,
        );
        if now_props != header.property_hash {
            return Err(VmError::SnapshotMismatch(format!(
                "property hash mismatch: snapshot {:016x}, boot {now_props:016x}",
                header.property_hash
            )));
        }

        // ---- heap counters / metrics (staged until the VM exists) --------
        let heap_live_bytes = r.u64()? as usize;
        let heap_peak_bytes = r.u64()? as usize;
        let heap_allocations = r.u64()?;
        let heap_gc_count = r.u64()?;
        let heap_gc_nanos = r.u64()?;
        let metric_instructions = r.u64()?;
        let metric_invokes = r.u64()?;
        let metric_clinit_count = r.u64()?;

        // ---- process roots / scalars (staged) ----------------------------
        let next_converter = r.u64()?;
        let next_thread_id = r.u32()?;
        let oom_object = r.handle()?;
        let stack_overflow_object = r.handle()?;
        let main_thread_group = r.handle()?;
        let stdout = r.handle()?;
        let stderr = r.handle()?;
        let stopping = r.bool()?;
        let exit_request = if r.bool()? { Some(r.i32()?) } else { None };
        let out_len = r.u32()? as usize;
        r.need(out_len)?;
        let out = r.remaining()[..out_len].to_vec();
        r.pos += out_len;
        let err_len = r.u32()? as usize;
        r.need(err_len)?;
        let err = r.remaining()[..err_len].to_vec();
        r.pos += err_len;

        // ---- classes -----------------------------------------------------
        let class_count = header.class_count as usize;
        let mut class_desc = Vec::with_capacity(class_count);
        let mut class_meta = Vec::with_capacity(class_count);
        for _ in 0..class_count {
            let desc = r.str()?;
            let init = tag_init_state(r.u8()?)?;
            let is_primitive = r.bool()?;
            let is_array = r.bool()?;
            let n = r.u32()? as usize;
            let mut statics = Vec::with_capacity(n);
            for _ in 0..n {
                statics.push(r.u32()?);
            }
            let class_obj = r.handle()?;
            class_desc.push(desc);
            class_meta.push((init, is_primitive, is_array, statics, class_obj));
        }

        let class_key = if header.corelib == CoreLib::Real {
            Some(BootKey {
                fingerprint: header.boot_fingerprint,
                corelib: 1,
                classes: class_list_hash(class_desc.iter().map(String::as_str)),
            })
        } else {
            None
        };

        // ---- build the VM: shared linked image (T-43) or a fresh relink ----
        let image = class_key
            .and_then(lookup_image)
            .filter(|img| img.same_layout(&class_desc));
        let mut vm = match &image {
            Some(img) => {
                // Cloning the shared linked boot metadata (field layouts,
                // vtables/interface tables, method tables, decoded bodies and
                // resolved provider list) replaces the per-restore re-link.
                let mut vm = Vm::empty(opts);
                vm.install_boot_image(img)?;
                vm
            }
            None => Vm::new(opts)?,
        };

        // Seed the per-VM state that the image deliberately does not carry.
        vm.heap.live_bytes = heap_live_bytes;
        vm.heap.peak_bytes = heap_peak_bytes;
        vm.heap.allocations = heap_allocations;
        vm.heap.gc_count = heap_gc_count;
        vm.heap.gc_nanos = heap_gc_nanos;
        vm.metrics.instructions = metric_instructions;
        vm.metrics.invokes = metric_invokes;
        vm.metrics.clinit_count = metric_clinit_count;
        vm.next_converter = next_converter;
        vm.next_thread_id = next_thread_id;
        vm.oom_object = oom_object;
        vm.stack_overflow_object = stack_overflow_object;
        vm.main_thread_group = main_thread_group;
        vm.stdout = stdout;
        vm.stderr = stderr;
        vm.stopping = stopping;
        vm.exit_request = exit_request;
        vm.out_buf = out;
        vm.err_buf = err;

        // Build the snapshot -> VM class id map. The image path is keyed by the
        // exact ordered descriptor list, so the mapping is the identity; the
        // relink path resolves/loads each descriptor.
        let mut old_to_new = Vec::with_capacity(class_count);
        if image.is_some() {
            old_to_new.extend(0..class_count as u32);
        } else {
            // Re-link any class the fresh VM does not already have, then build
            // the old -> new id map by descriptor.
            for desc in &class_desc {
                let id = match vm.find_class(desc) {
                    Some(id) => id,
                    None => vm.load_class(desc)?,
                };
                old_to_new.push(id);
            }
            for (i, desc) in class_desc.iter().enumerate() {
                let new = old_to_new[i];
                let got = vm.class_desc(new).to_string();
                if &got != desc {
                    return Err(VmError::SnapshotMismatch(format!(
                        "class id {i} maps to {got}, snapshot expected {desc}"
                    )));
                }
            }
        }

        // Static slots + init state (checked for layout equality).
        for (i, (init, _prim, _arr, statics, class_obj)) in class_meta.iter().enumerate() {
            let new = old_to_new[i] as usize;
            if vm.classes[new].statics.len() != statics.len() {
                return Err(VmError::SnapshotMismatch(format!(
                    "class {} static layout changed: snapshot {} slots, boot {}",
                    class_desc[i],
                    statics.len(),
                    vm.classes[new].statics.len()
                )));
            }
            vm.classes[new].statics.copy_from_slice(statics);
            vm.classes[new].init = *init;
            vm.classes[new].class_obj = *class_obj;
        }

        // First restore of this class set in this process: remember the linked
        // metadata so the next restore clones it (both paths end with the same
        // linked tables).
        if image.is_none() {
            if let Some(key) = class_key {
                if lookup_image(key).is_none() {
                    remember_image(key, BootImage::capture(&vm));
                }
            }
        }

        // ---- heap arena --------------------------------------------------
        let slots = header.object_slots as usize;
        let mut objects: Vec<Option<Object>> = Vec::with_capacity(slots);
        for i in 0..slots {
            if r.bool()? {
                objects.push(Some(decode_object(&mut r, &old_to_new)?));
            } else {
                objects.push(None);
                let _ = i;
            }
        }
        // Rebuild the free list from empty slots (slot 0 is the null handle).
        let mut free = Vec::new();
        for (i, o) in objects.iter().enumerate().skip(1) {
            if o.is_none() {
                free.push(i as u32);
            }
        }
        vm.heap.objects = objects;
        vm.heap.free = free;
        vm.heap.field_pool.clear();
        vm.heap.field_pool_words = 0;

        // ---- interned strings / roots / caches ---------------------------
        let interned_n = r.u32()? as usize;
        vm.interned.clear();
        for _ in 0..interned_n {
            vm.interned.push(r.handle()?);
        }
        vm.native_roots.clear();
        let roots_n = r.u32()? as usize;
        for _ in 0..roots_n {
            vm.native_roots.push(r.handle()?);
        }

        let int_n = r.u32()? as usize;
        vm.int_cache.clear();
        for _ in 0..int_n {
            let k = r.i32()?;
            let v = r.handle()?;
            vm.int_cache.insert(k, v);
        }

        let sh_n = r.u32()? as usize;
        vm.string_hashes.clear();
        for _ in 0..sh_n {
            let k = r.handle()?;
            let v = r.i32()?;
            vm.string_hashes.insert(k, v);
        }

        // Rebuild the canonical intern table from the ordered interned list, so
        // HashMap iteration order never enters the snapshot.
        vm.strings.clear();
        for &h in &vm.interned {
            if let Some(Object::String(units)) = vm.heap.get(h) {
                vm.strings.insert(units.clone(), h);
            }
        }

        let ts_n = r.u32()? as usize;
        vm.throwable_stacks.clear();
        for _ in 0..ts_n {
            let h = r.handle()?;
            let n = r.u32()? as usize;
            let mut entries = Vec::with_capacity(n);
            for _ in 0..n {
                entries.push(crate::vm::StackEntry {
                    class: r.str()?,
                    method: r.str()?,
                    line: r.u32()?,
                });
            }
            vm.throwable_stacks.insert(h, entries);
        }

        // ---- threads -----------------------------------------------------
        let thread_n = r.u32()? as usize;
        vm.threads.clear();
        for _ in 0..thread_n {
            let object = r.handle()?;
            let status = tag_thread_status(r.u8()?)?;
            let mut t = ThreadRT::new(object);
            t.status = status;
            vm.threads.push(t);
        }
        vm.current_thread = 0;

        // ---- font / typeface native tables (W115-A) ---------------------
        // Stage the restored tables; the first font/text native imports them.
        // The resolved FreeType/rustybuzz faces stay deferred until then, so a
        // restored launch that never touches text never loads the collection.
        let fonts = read_font_state(&mut r)?;
        if header.corelib == CoreLib::Real {
            crate::natives::fwk_fonts::stage_font_state(fonts);
        }
        let text = read_text_state(&mut r)?;
        if header.corelib == CoreLib::Real {
            crate::natives::gfx_text::stage_text_state(text);
        }
        let conv_n = r.u32()? as usize;
        vm.converters.clear();
        for _ in 0..conv_n {
            let k = r.u64()?;
            let v = r.str()?;
            vm.converters.insert(k, v);
        }
        let regex = read_regex_state(&mut r)?;
        if header.corelib == CoreLib::Real {
            crate::natives::icu_regex::stage_regex_state(regex);
        }

        // ---- native-memory regions (W154-A, source-recorded W160-A) ------
        // Re-establish the mapped regions the restored heap's direct buffers
        // point at. `read_native_regions` re-reads a file-backed region from
        // its recorded source and validates size/mtime/hash, failing closed on
        // any change. `restore_regions` is idempotent, so the template's first
        // restore (metadata recovery) and the launch restore share one table.
        let regions = read_native_regions(&mut r, &mut vm.vfs)?;
        crate::natives::native_mem::restore_regions(&regions);

        // Re-resolution caches refer to the *old* class/provider layout; drop
        // them so every access re-resolves through the descriptor tables. This
        // is what makes restore independent of DEX parse-order accidents. The
        // image path builds those provider contexts fresh, so they are already
        // empty (and clearing 9 DEX-sized vectors is pure overhead there).
        if image.is_none() {
            for dex in &mut vm.dexs {
                for s in &mut dex.string_cache {
                    *s = None;
                }
                for f in &mut dex.field_cache {
                    *f = None;
                }
                for m in &mut dex.method_cache {
                    *m = None;
                }
                for c in &mut dex.class_cache {
                    *c = None;
                }
                for v in &mut dex.virtual_cache {
                    *v = None;
                }
            }
        }
        vm.dispatch_cache.clear();
        vm.gc_threshold = (vm.heap.live_bytes * 2)
            .max(vm.heap.max_bytes / 16)
            .max(1 << 20);
        vm.metrics.classes_loaded = vm.classes.len();

        let decode_nanos = t_decode.elapsed().as_nanos() as u64;
        Ok((
            vm,
            RestoreStats {
                copy_nanos,
                decode_nanos,
                bytes: bytes.len() as u64,
            },
        ))
    }
}

fn read_header(r: &mut Reader<'_>) -> VmResult<SnapshotHeader> {
    r.need(8)?;
    if r.remaining()[..8] != SNAPSHOT_MAGIC {
        return Err(VmError::SnapshotFormat(
            "bad magic (not an ACOS snapshot)".into(),
        ));
    }
    r.pos += 8;
    let version = r.u32()?;
    let corelib = match r.u8()? {
        0 => CoreLib::Bootstrap,
        1 => CoreLib::Real,
        other => {
            return Err(VmError::SnapshotFormat(format!(
                "unknown corelib tag {other}"
            )))
        }
    };
    let trace_natives = r.bool()?;
    let gc_stress = r.bool()?;
    let heap_max_bytes = r.u64()?;
    let boot_fingerprint = r.u64()?;
    let property_hash = r.u64()?;
    let locale = r.str()?;
    let clock_anchor_millis = r.u64()?;
    let boot_dir = if r.bool()? { Some(r.str()?) } else { None };
    let class_count = r.u32()?;
    let object_slots = r.u32()?;
    Ok(SnapshotHeader {
        version,
        corelib,
        trace_natives,
        gc_stress,
        heap_max_bytes,
        boot_fingerprint,
        property_hash,
        locale,
        clock_anchor_millis,
        boot_dir,
        class_count,
        object_slots,
    })
}

// ---------------------------------------------------------------------------
// Font / typeface native table section (W115-A)
// ---------------------------------------------------------------------------

fn write_font_state(w: &mut Writer, state: &FontState) {
    w.u32(state.fonts.len() as u32);
    for f in &state.fonts {
        w.i64(f.handle);
        w.str(&f.path);
        w.u32(f.ttc_index);
        w.i32(f.weight);
        w.bool(f.italic);
        w.u32(f.axes.len() as u32);
        for (tag, value) in &f.axes {
            w.u32(*tag);
            w.u32(value.to_bits());
        }
        w.str(&f.locale_list);
        w.i32(f.source_id);
    }
    w.u32(state.families.len() as u32);
    for fam in &state.families {
        w.i64(fam.handle);
        w.u32(fam.fonts.len() as u32);
        for h in &fam.fonts {
            w.i64(*h);
        }
        w.str(&fam.lang_tags);
        w.i32(fam.variant);
    }
}

fn read_font_state(r: &mut Reader<'_>) -> VmResult<FontState> {
    let n = r.u32()? as usize;
    let mut fonts = Vec::with_capacity(n);
    for _ in 0..n {
        let handle = r.i64()?;
        let path = r.str()?;
        let ttc_index = r.u32()?;
        let weight = r.i32()?;
        let italic = r.bool()?;
        let axn = r.u32()? as usize;
        let mut axes = Vec::with_capacity(axn);
        for _ in 0..axn {
            axes.push((r.u32()?, f32::from_bits(r.u32()?)));
        }
        let locale_list = r.str()?;
        let source_id = r.i32()?;
        fonts.push(FontRecordData {
            handle,
            path,
            ttc_index,
            weight,
            italic,
            axes,
            locale_list,
            source_id,
        });
    }
    let n = r.u32()? as usize;
    let mut families = Vec::with_capacity(n);
    for _ in 0..n {
        let handle = r.i64()?;
        let fcount = r.u32()? as usize;
        let mut fs = Vec::with_capacity(fcount);
        for _ in 0..fcount {
            fs.push(r.i64()?);
        }
        let lang_tags = r.str()?;
        let variant = r.i32()?;
        families.push(FamilyRecordData {
            handle,
            fonts: fs,
            lang_tags,
            variant,
        });
    }
    Ok(FontState { fonts, families })
}

fn write_text_state(w: &mut Writer, state: &TextState) {
    w.i64(state.default_typeface);
    w.u32(state.typefaces.len() as u32);
    for t in &state.typefaces {
        w.i64(t.handle);
        match &t.family {
            Some(f) => {
                w.bool(true);
                w.str(f);
            }
            None => w.bool(false),
        }
        w.i32(t.weight);
        w.bool(t.italic);
        write_face_ref(w, t.face.as_ref());
    }
}

fn read_text_state(r: &mut Reader<'_>) -> VmResult<TextState> {
    let default_typeface = r.i64()?;
    let n = r.u32()? as usize;
    let mut typefaces = Vec::with_capacity(n);
    for _ in 0..n {
        let handle = r.i64()?;
        let family = if r.bool()? { Some(r.str()?) } else { None };
        let weight = r.i32()?;
        let italic = r.bool()?;
        let face = read_face_ref(r)?;
        typefaces.push(TypefaceRecordData {
            handle,
            family,
            weight,
            italic,
            face,
        });
    }
    Ok(TextState {
        typefaces,
        default_typeface,
    })
}

fn write_face_ref(w: &mut Writer, face: Option<&FaceRef>) {
    match face {
        Some(f) => {
            w.bool(true);
            w.str(&f.path);
            w.u32(f.face_index);
            w.u32(f.family_key as u32);
            w.u16(f.weight);
            w.bool(f.italic);
            w.u32(f.axes.len() as u32);
            for (tag, value) in &f.axes {
                w.str(tag);
                w.u32(value.to_bits());
            }
        }
        None => w.bool(false),
    }
}

fn read_face_ref(r: &mut Reader<'_>) -> VmResult<Option<FaceRef>> {
    if !r.bool()? {
        return Ok(None);
    }
    let path = r.str()?;
    let face_index = r.u32()?;
    let family_key = r.u32()? as usize;
    let weight = r.u16()?;
    let italic = r.bool()?;
    let n = r.u32()? as usize;
    let mut axes = Vec::with_capacity(n);
    for _ in 0..n {
        let tag = r.str()?;
        let value = f32::from_bits(r.u32()?);
        axes.push((tag, value));
    }
    Ok(Some(FaceRef {
        path,
        face_index,
        family_key,
        weight,
        italic,
        axes,
    }))
}

fn write_regex_state(w: &mut Writer, state: &crate::natives::icu_regex::RegexState) {
    w.u64(state.next);
    w.u32(state.patterns.len() as u32);
    for (h, source, flags) in &state.patterns {
        w.u64(*h);
        w.str(source);
        w.u32(*flags);
    }
}

fn read_regex_state(r: &mut Reader<'_>) -> VmResult<crate::natives::icu_regex::RegexState> {
    let next = r.u64()?;
    let n = r.u32()? as usize;
    let mut patterns = Vec::with_capacity(n);
    for _ in 0..n {
        let h = r.u64()?;
        let source = r.str()?;
        let flags = r.u32()?;
        patterns.push((h, source, flags));
    }
    Ok(crate::natives::icu_regex::RegexState { next, patterns })
}

/// Serialise the native-memory region table (W154-A; source-recorded W160-A).
///
/// Layout: `u32 count`, then per region `u32 id`, `u8 flags` (bit0 readonly,
/// bit1 has source). A file-backed region stores `str path`, `u64 offset`,
/// `u64 length`, `u64 hash`, `u64 file_size`, `i64 file_mtime` and **no
/// bytes**; an anonymous region stores `u32 len` + `len` raw bytes. Sorted by
/// id at capture so the stream is deterministic.
fn write_native_regions(w: &mut Writer, regions: &[crate::natives::native_mem::RegionEntry]) {
    w.u32(regions.len() as u32);
    for r in regions {
        w.u32(r.id);
        let mut flags = 0u8;
        if r.readonly {
            flags |= 1;
        }
        if r.source.is_some() {
            flags |= 2;
        }
        w.u8(flags);
        match &r.source {
            Some(s) => {
                w.str(&s.path);
                w.u64(s.offset);
                w.u64(s.length);
                w.u64(s.hash);
                w.u64(s.file_size);
                w.i64(s.file_mtime);
            }
            None => {
                w.u32(r.data.len() as u32);
                w.raw(&r.data);
            }
        }
    }
}

/// Read the native-region section and resolve every file-backed region by
/// re-reading its recorded source (W160-A). A missing or changed file fails
/// closed with [`VmError::SnapshotMismatch`], which the boot-cache caller
/// treats as a miss and cold-boots.
fn read_native_regions(
    r: &mut Reader<'_>,
    vfs: &mut crate::vfs::Vfs,
) -> VmResult<Vec<crate::natives::native_mem::RegionEntry>> {
    // A crafted count is bounded by the remaining bytes: each entry needs at
    // least 10 header bytes, so `count` can never exceed `len / 10` validly. Cap
    // at a sane ceiling and let `need` reject a short stream.
    const MAX_REGIONS: usize = 1 << 20;
    let n = r.u32()? as usize;
    if n > MAX_REGIONS {
        return Err(VmError::SnapshotFormat(format!(
            "native region count {n} too large"
        )));
    }
    let mut out = Vec::with_capacity(n.min(4096));
    for _ in 0..n {
        let id = r.u32()?;
        let flags = r.u8()?;
        if flags & !3 != 0 {
            return Err(VmError::SnapshotFormat(format!(
                "native region {id} has unknown flags {flags:#x}"
            )));
        }
        let readonly = flags & 1 != 0;
        if flags & 2 != 0 {
            let source = crate::natives::native_mem::RegionSource {
                path: r.str()?,
                offset: r.u64()?,
                length: r.u64()?,
                hash: r.u64()?,
                file_size: r.u64()?,
                file_mtime: r.i64()?,
            };
            if source.length > crate::natives::native_mem::cap_bytes() as u64 {
                return Err(VmError::SnapshotFormat(format!(
                    "native region {id} source length {} exceeds the native-memory cap",
                    source.length
                )));
            }
            let (data, size, mtime) = vfs
                .read_mapped_region(&source.path, source.offset, source.length as usize)
                .map_err(|e| {
                    VmError::SnapshotMismatch(format!(
                        "native region {id} source {:?} unreadable: {}",
                        source.path,
                        crate::vfs::errno_name(e)
                    ))
                })?;
            if !source.matches(&data, size, mtime) {
                return Err(VmError::SnapshotMismatch(format!(
                    "native region {id} source {:?} changed since the snapshot",
                    source.path
                )));
            }
            out.push(crate::natives::native_mem::RegionEntry {
                id,
                readonly,
                data,
                source: Some(source),
            });
        } else {
            let len = r.u32()? as usize;
            r.need(len)?;
            let data = r.remaining()[..len].to_vec();
            r.pos += len;
            out.push(crate::natives::native_mem::RegionEntry {
                id,
                readonly,
                data,
                source: None,
            });
        }
    }
    Ok(out)
}

fn init_state_tag(s: InitState) -> u8 {
    match s {
        InitState::NotInit => 0,
        InitState::InProgress => 1,
        InitState::Init => 2,
        InitState::Failed => 3,
    }
}

fn tag_init_state(t: u8) -> VmResult<InitState> {
    Ok(match t {
        0 => InitState::NotInit,
        1 => InitState::InProgress,
        2 => InitState::Init,
        3 => InitState::Failed,
        other => {
            return Err(VmError::SnapshotFormat(format!(
                "unknown InitState tag {other}"
            )))
        }
    })
}

fn thread_status_tag(s: ThreadStatus) -> u8 {
    match s {
        ThreadStatus::Runnable => 0,
        ThreadStatus::Running => 1,
        ThreadStatus::BlockedMonitor(_) => 2,
        ThreadStatus::Sleeping => 3,
        ThreadStatus::Waiting => 4,
        ThreadStatus::Parked => 5,
        ThreadStatus::Finished => 6,
    }
}

fn tag_thread_status(t: u8) -> VmResult<ThreadStatus> {
    Ok(match t {
        0 => ThreadStatus::Runnable,
        1 => ThreadStatus::Running,
        2 => ThreadStatus::BlockedMonitor(NULL),
        3 => ThreadStatus::Sleeping,
        4 => ThreadStatus::Waiting,
        5 => ThreadStatus::Parked,
        6 => ThreadStatus::Finished,
        other => {
            return Err(VmError::SnapshotFormat(format!(
                "unknown ThreadStatus tag {other}"
            )))
        }
    })
}

// ---------------------------------------------------------------------------
// Object encode / decode
// ---------------------------------------------------------------------------

const OBJ_INSTANCE: u8 = 0;
const OBJ_ARRAY: u8 = 1;
const OBJ_STRING: u8 = 2;
const OBJ_CLASS: u8 = 3;
const OBJ_BUILTIN: u8 = 4;

fn encode_object(w: &mut Writer, o: &Object) {
    match o {
        Object::Instance { class, fields } => {
            w.u8(OBJ_INSTANCE);
            w.u32(*class);
            w.u32(fields.len() as u32);
            for &f in fields {
                w.u32(f);
            }
        }
        Object::Array(a) => {
            w.u8(OBJ_ARRAY);
            w.str(&a.elem_desc);
            encode_array_data(w, &a.data);
        }
        Object::String(units) => {
            w.u8(OBJ_STRING);
            w.u32(units.len() as u32);
            for &u in units {
                w.u16(u);
            }
        }
        Object::Class(id) => {
            w.u8(OBJ_CLASS);
            w.u32(*id);
        }
        Object::Builtin { class, data } => {
            w.u8(OBJ_BUILTIN);
            w.u32(*class);
            encode_builtin(w, data);
        }
    }
}

fn encode_array_data(w: &mut Writer, d: &ArrayData) {
    match d {
        ArrayData::Bool(v) => {
            w.u8(0);
            w.u32(v.len() as u32);
            for &x in v {
                w.u8(x as u8);
            }
        }
        ArrayData::Byte(v) => {
            w.u8(1);
            w.u32(v.len() as u32);
            for &x in v {
                w.u8(x as u8);
            }
        }
        ArrayData::Char(v) => {
            w.u8(2);
            w.u32(v.len() as u32);
            for &x in v {
                w.u16(x);
            }
        }
        ArrayData::Short(v) => {
            w.u8(3);
            w.u32(v.len() as u32);
            for &x in v {
                w.u16(x as u16);
            }
        }
        ArrayData::Int(v) => {
            w.u8(4);
            w.u32(v.len() as u32);
            for &x in v {
                w.i32(x);
            }
        }
        ArrayData::Long(v) => {
            w.u8(5);
            w.u32(v.len() as u32);
            for &x in v {
                w.i64(x);
            }
        }
        ArrayData::Float(v) => {
            w.u8(6);
            w.u32(v.len() as u32);
            for &x in v {
                w.u32(x.to_bits());
            }
        }
        ArrayData::Double(v) => {
            w.u8(7);
            w.u32(v.len() as u32);
            for &x in v {
                w.u64(x.to_bits());
            }
        }
        ArrayData::Ref(v) => {
            w.u8(8);
            w.u32(v.len() as u32);
            for &x in v {
                w.u32(x);
            }
        }
    }
}

fn decode_array_data(r: &mut Reader<'_>) -> VmResult<ArrayData> {
    let tag = r.u8()?;
    let n = r.u32()? as usize;
    Ok(match tag {
        0 => {
            let mut v = Vec::with_capacity(n);
            for _ in 0..n {
                v.push(r.u8()? != 0);
            }
            ArrayData::Bool(v)
        }
        1 => {
            let mut v = Vec::with_capacity(n);
            for _ in 0..n {
                v.push(r.u8()? as i8);
            }
            ArrayData::Byte(v)
        }
        2 => {
            let mut v = Vec::with_capacity(n);
            for _ in 0..n {
                v.push(r.u16()?);
            }
            ArrayData::Char(v)
        }
        3 => {
            let mut v = Vec::with_capacity(n);
            for _ in 0..n {
                v.push(r.u16()? as i16);
            }
            ArrayData::Short(v)
        }
        4 => {
            let mut v = Vec::with_capacity(n);
            for _ in 0..n {
                v.push(r.i32()?);
            }
            ArrayData::Int(v)
        }
        5 => {
            let mut v = Vec::with_capacity(n);
            for _ in 0..n {
                v.push(r.i64()?);
            }
            ArrayData::Long(v)
        }
        6 => {
            let mut v = Vec::with_capacity(n);
            for _ in 0..n {
                v.push(f32::from_bits(r.u32()?));
            }
            ArrayData::Float(v)
        }
        7 => {
            let mut v = Vec::with_capacity(n);
            for _ in 0..n {
                v.push(f64::from_bits(r.u64()?));
            }
            ArrayData::Double(v)
        }
        8 => {
            let mut v = Vec::with_capacity(n);
            for _ in 0..n {
                v.push(r.u32()?);
            }
            ArrayData::Ref(v)
        }
        other => {
            return Err(VmError::SnapshotFormat(format!(
                "unknown ArrayData tag {other}"
            )))
        }
    })
}

const BUILTIN_STRING_BUILDER: u8 = 0;
const BUILTIN_LIST: u8 = 1;
const BUILTIN_HASH_MAP: u8 = 2;
const BUILTIN_TREE_MAP: u8 = 3;
const BUILTIN_ENTRY: u8 = 4;
const BUILTIN_ITERATOR: u8 = 5;
const BUILTIN_ENUM_SET: u8 = 6;
const BUILTIN_THREAD: u8 = 7;
const BUILTIN_STACK_TRACE: u8 = 8;

fn encode_builtin(w: &mut Writer, d: &BuiltinData) {
    match d {
        BuiltinData::StringBuilder(units) => {
            w.u8(BUILTIN_STRING_BUILDER);
            w.u32(units.len() as u32);
            for &u in units {
                w.u16(u);
            }
        }
        BuiltinData::List(v) => {
            w.u8(BUILTIN_LIST);
            encode_handles(w, v);
        }
        BuiltinData::HashMap(v) => {
            w.u8(BUILTIN_HASH_MAP);
            encode_handle_pairs(w, v);
        }
        BuiltinData::TreeMap(v) => {
            w.u8(BUILTIN_TREE_MAP);
            encode_handle_pairs(w, v);
        }
        BuiltinData::Entry { key, value } => {
            w.u8(BUILTIN_ENTRY);
            w.u32(*key);
            w.u32(*value);
        }
        BuiltinData::Iterator { items, index } => {
            w.u8(BUILTIN_ITERATOR);
            encode_handles(w, items);
            w.u32(*index as u32);
        }
        BuiltinData::EnumSet {
            enum_class,
            elements,
        } => {
            w.u8(BUILTIN_ENUM_SET);
            w.u32(*enum_class);
            encode_handles(w, elements);
        }
        BuiltinData::Thread(t) => {
            w.u8(BUILTIN_THREAD);
            w.u32(t.id);
            w.u32(t.name);
            w.u32(t.target);
            w.bool(t.alive);
            w.bool(t.daemon);
        }
        BuiltinData::StackTraceElement(d) => {
            w.u8(BUILTIN_STACK_TRACE);
            w.str(&d.class_name);
            w.str(&d.method_name);
            match &d.file_name {
                Some(f) => {
                    w.bool(true);
                    w.str(f);
                }
                None => w.bool(false),
            }
            w.i32(d.line);
        }
    }
}

fn encode_handles(w: &mut Writer, v: &[Handle]) {
    w.u32(v.len() as u32);
    for &h in v {
        w.u32(h);
    }
}

fn encode_handle_pairs(w: &mut Writer, v: &[(Handle, Handle)]) {
    w.u32(v.len() as u32);
    for (a, b) in v {
        w.u32(*a);
        w.u32(*b);
    }
}

fn decode_builtin(r: &mut Reader<'_>, remap: &[u32]) -> VmResult<BuiltinData> {
    let tag = r.u8()?;
    Ok(match tag {
        BUILTIN_STRING_BUILDER => {
            let n = r.u32()? as usize;
            let mut v = Vec::with_capacity(n);
            for _ in 0..n {
                v.push(r.u16()?);
            }
            BuiltinData::StringBuilder(v)
        }
        BUILTIN_LIST => BuiltinData::List(decode_handles(r)?),
        BUILTIN_HASH_MAP => BuiltinData::HashMap(decode_handle_pairs(r)?),
        BUILTIN_TREE_MAP => BuiltinData::TreeMap(decode_handle_pairs(r)?),
        BUILTIN_ENTRY => {
            let key = r.u32()?;
            let value = r.u32()?;
            BuiltinData::Entry { key, value }
        }
        BUILTIN_ITERATOR => {
            let items = decode_handles(r)?;
            let index = r.u32()? as usize;
            BuiltinData::Iterator { items, index }
        }
        BUILTIN_ENUM_SET => {
            let enum_class = r.u32()?;
            let elements = decode_handles(r)?;
            BuiltinData::EnumSet {
                enum_class: remap_class(remap, enum_class),
                elements,
            }
        }
        BUILTIN_THREAD => {
            let id = r.u32()?;
            let name = r.u32()?;
            let target = r.u32()?;
            let alive = r.bool()?;
            let daemon = r.bool()?;
            BuiltinData::Thread(ThreadData {
                id,
                name,
                target,
                alive,
                daemon,
            })
        }
        BUILTIN_STACK_TRACE => {
            let class_name = r.str()?;
            let method_name = r.str()?;
            let file_name = if r.bool()? { Some(r.str()?) } else { None };
            let line = r.i32()?;
            BuiltinData::StackTraceElement(Box::new(StackTraceElementData {
                class_name,
                method_name,
                file_name,
                line,
            }))
        }
        other => {
            return Err(VmError::SnapshotFormat(format!(
                "unknown BuiltinData tag {other}"
            )))
        }
    })
}

fn decode_handles(r: &mut Reader<'_>) -> VmResult<Vec<Handle>> {
    let n = r.u32()? as usize;
    let mut v = Vec::with_capacity(n);
    for _ in 0..n {
        v.push(r.u32()?);
    }
    Ok(v)
}

fn decode_handle_pairs(r: &mut Reader<'_>) -> VmResult<Vec<(Handle, Handle)>> {
    let n = r.u32()? as usize;
    let mut v = Vec::with_capacity(n);
    for _ in 0..n {
        let a = r.u32()?;
        let b = r.u32()?;
        v.push((a, b));
    }
    Ok(v)
}

fn remap_class(remap: &[u32], old: u32) -> u32 {
    remap.get(old as usize).copied().unwrap_or(old)
}

fn decode_object(r: &mut Reader<'_>, remap: &[u32]) -> VmResult<Object> {
    let tag = r.u8()?;
    Ok(match tag {
        OBJ_INSTANCE => {
            let class = remap_class(remap, r.u32()?);
            let n = r.u32()? as usize;
            let mut fields = Vec::with_capacity(n);
            for _ in 0..n {
                fields.push(r.u32()?);
            }
            Object::Instance { class, fields }
        }
        OBJ_ARRAY => {
            let elem_desc = r.str()?;
            let data = decode_array_data(r)?;
            Object::Array(ArrayObj { elem_desc, data })
        }
        OBJ_STRING => {
            let n = r.u32()? as usize;
            let mut units = Vec::with_capacity(n);
            for _ in 0..n {
                units.push(r.u16()?);
            }
            Object::String(units)
        }
        OBJ_CLASS => Object::Class(remap_class(remap, r.u32()?)),
        OBJ_BUILTIN => {
            let class = remap_class(remap, r.u32()?);
            let data = decode_builtin(r, remap)?;
            Object::Builtin { class, data }
        }
        other => {
            return Err(VmError::SnapshotFormat(format!(
                "unknown object tag {other}"
            )))
        }
    })
}

/// A helper used by tests: brute-force the byte offset of the property hash
/// field so a mismatch can be injected without a second serialiser.
pub fn header_property_hash_offset() -> usize {
    8    // magic
        + 4  // version
        + 1  // corelib
        + 1  // trace
        + 1  // gc_stress
        + 8  // heap_max
        + 8 // boot fingerprint
            // property hash starts here
}

/// A helper used by tests: build a `Heap` with the given slot count. Exposed
/// only to the crate's own tests to keep `heap.rs` free of snapshot concerns.
#[doc(hidden)]
pub fn empty_heap(max_bytes: usize) -> Heap {
    Heap::new(max_bytes)
}

/// Sort helper shared by the determinism test: canonical byte order of a map.
#[doc(hidden)]
pub fn sorted_handles(m: &HashMap<Handle, i32>) -> Vec<Handle> {
    let mut v: Vec<Handle> = m.keys().copied().collect();
    v.sort_unstable();
    v
}
