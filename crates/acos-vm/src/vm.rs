//! The ACOS virtual machine: heap ownership, class loading, resolution, GC and
//! the public `run` entry point.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use acos_dex::{DexFile, EncodedValue};

use crate::class::{
    parse_signature, parse_type_list, Class, ClassId, ClassSpec, FieldDef, FieldSign, Insn,
    Method, MethodCode, MethodId, MethodImpl, ResolvedField, TryEntry, TypeId, TypeInfo, TypeKind,
};
use crate::error::{VmError, VmResult};
use crate::frame::Frame;
use crate::heap::{new_array_data, ArrayObj, BuiltinData, Handle, Heap, Object, NULL};
use crate::limits::HostGuard;

/// Per-DEX lazy caches. Indices are the DEX's own id indices.
#[derive(Debug)]
pub struct DexCtx {
    pub file: Arc<DexFile>,
    pub string_cache: Vec<Option<Handle>>,
    pub field_cache: Vec<Option<ResolvedField>>,
    pub method_cache: Vec<Option<MethodId>>,
    pub class_cache: Vec<Option<ClassId>>,
    /// A small polymorphic (receiver class -> method) inline cache per
    /// virtual/interface call site. Monomorphic sites stay on the first entry;
    /// polymorphic cycles that previously thrashed a one-entry cache (and so
    /// always fell through to the tuple-keyed `dispatch_cache` `HashMap`) hit
    /// one of four entries. Replacement is round-robin. The mapping is a pure
    /// function of (declared signature, receiver class), so it never needs
    /// invalidation.
    ///
    /// Stored boxed: one `VirtualCache` is 40 bytes, and the vector is sized to
    /// the whole DEX `method_ids` table even though only executed call sites
    /// ever populate an entry. The boxed slot keeps the dense O(1) index the
    /// interpreter's fast path relies on while paying 8 bytes of pointer per
    /// unused slot instead of 40 (W146-A).
    pub virtual_cache: Vec<Option<Box<VirtualCache>>>,
    /// Absolute path the provider was parsed from, when it came from the boot
    /// classpath. Captured into the serialized boot image so a cold process can
    /// re-open the identical provider list (index order is part of the
    /// `MethodImpl::Dex { dex }` contract).
    pub source: Option<std::path::PathBuf>,
}

/// A 4-way polymorphic inline cache for one call site.
#[derive(Clone, Copy, Debug)]
pub struct VirtualCache {
    entries: [(ClassId, MethodId); 4],
    len: u8,
    next: u8,
}

impl Default for VirtualCache {
    fn default() -> Self {
        VirtualCache::new()
    }
}

impl VirtualCache {
    #[inline(always)]
    pub fn new() -> VirtualCache {
        VirtualCache {
            entries: [(0, 0); 4],
            len: 0,
            next: 0,
        }
    }

    /// Look up a receiver class; linear scan over at most four entries.
    #[inline(always)]
    pub fn get(&self, c: ClassId) -> Option<MethodId> {
        let n = self.len as usize;
        let mut i = 0;
        while i < n {
            if self.entries[i].0 == c {
                return Some(self.entries[i].1);
            }
            i += 1;
        }
        None
    }

    /// Insert a resolution, evicting round-robin once full.
    #[inline]
    pub fn put(&mut self, c: ClassId, m: MethodId) {
        let n = self.len as usize;
        if n < self.entries.len() {
            self.entries[n] = (c, m);
            self.len = (n + 1) as u8;
        } else {
            let i = self.next as usize;
            self.entries[i] = (c, m);
            self.next = ((i + 1) % self.entries.len()) as u8;
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThreadStatus {
    Runnable,
    Running,
    BlockedMonitor(Handle),
    Sleeping,
    Waiting,
    /// Parked in a native blocking poll (`android.os.MessageQueue.nativePollOnce`).
    /// The scheduler treats it as not runnable; the looper pump wakes it.
    Parked,
    Finished,
}

#[derive(Debug)]
pub struct ThreadRT {
    pub object: Handle,
    pub frames: Vec<Frame>,
    pub status: ThreadStatus,
    pub joiners: Vec<usize>,
    pub wake_at: Option<Instant>,
    /// The object this thread is blocked in `Object.wait` on, if any.
    pub wait_monitor: Option<Handle>,
    /// Re-entry count of the monitor this thread released when it entered
    /// `Object.wait`; restored when it is notified and re-acquires the
    /// monitor. `0` when the thread is not waiting on a monitor.
    pub wait_saved_count: u32,
    /// Reusable register buffers for popped frames. Java calls happen in tight
    /// loops in real framework code; allocating a fresh `regs`/`refs` pair for
    /// every call dominated invoke cost. The pool is per-thread and capped.
    pub(crate) frame_pool: Vec<Frame>,
    /// A `LockSupport.unpark`/`Unsafe.unpark` permit waiting for this thread's
    /// next park (JDK `Parker` semantics). Set by `unpark`, consumed by `park`.
    pub park_permit: bool,
}

impl ThreadRT {
    /// Create an empty (frame-less) thread record. Public so snapshot tests can
    /// build a quiescent/live thread to exercise the preconditions.
    pub fn new(object: Handle) -> ThreadRT {
        ThreadRT {
            object,
            frames: Vec::new(),
            status: ThreadStatus::Runnable,
            joiners: Vec::new(),
            wake_at: None,
            wait_monitor: None,
            wait_saved_count: 0,
            frame_pool: Vec::new(),
            park_permit: false,
        }
    }

    pub fn top_frame(&self) -> Option<&Frame> {
        self.frames.last()
    }
}

/// VM execution counters and timings for the metrics block.
#[derive(Clone, Debug, Default)]
pub struct Metrics {
    pub instructions: u64,
    pub invokes: u64,
    pub allocations: u64,
    pub gc_count: u64,
    pub gc_ms: u128,
    pub heap_peak_bytes: usize,
    pub classes_loaded: usize,
    /// Number of `java.lang.Class` `<clinit>` bodies executed.
    pub clinit_count: u64,
}

/// A captured uncaught exception.
#[derive(Clone, Debug)]
pub struct Uncaught {
    pub class: String,
    pub message: Option<String>,
    pub stack: Vec<String>,
}

/// The result of running a program.
#[derive(Clone, Debug)]
pub struct RunOutcome {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub uncaught: Option<Uncaught>,
    pub metrics: Metrics,
    /// Present when the failure was a VM-level (non-Java) error.
    pub vm_error: Option<VmError>,
}

/// Which class provider backs the core library.
///
/// `Bootstrap` uses the Rust built-in `java.lang`/`java.util` subset (the
/// original ACOS path). `Real` loads the real Android 14 boot-classpath DEX as
/// the class provider and binds only true native methods from Rust.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CoreLib {
    Bootstrap,
    Real,
}

impl CoreLib {
    pub fn parse(s: &str) -> Option<CoreLib> {
        match s {
            "bootstrap" => Some(CoreLib::Bootstrap),
            "real" => Some(CoreLib::Real),
            _ => None,
        }
    }
}

/// Options controlling a VM run.
#[derive(Clone, Debug)]
pub struct VmOptions {
    pub heap_max_bytes: usize,
    pub gc_stress: bool,
    pub corelib: CoreLib,
    /// Directory holding `BOOTCLASSPATH` and `dex/<jar>/classes*.dex`.
    /// `None` means the default search path (see [`default_boot_dir`]).
    pub boot_dir: Option<PathBuf>,
    /// Record every native invocation for the native-diff harness.
    pub trace_natives: bool,
}

impl Default for VmOptions {
    fn default() -> Self {
        VmOptions {
            heap_max_bytes: 256 * 1024 * 1024,
            gc_stress: std::env::var_os("ACOS_GC_STRESS").is_some(),
            corelib: std::env::var("ACOS_CORELIB")
                .ok()
                .and_then(|s| CoreLib::parse(&s))
                .unwrap_or(CoreLib::Bootstrap),
            boot_dir: std::env::var_os("ACOS_BOOT_DIR").map(PathBuf::from),
            trace_natives: std::env::var_os("ACOS_TRACE_NATIVES").is_some(),
        }
    }
}

/// Resolve the boot-classpath cache directory.
///
/// Order: explicit path, `$ACOS_BOOT_DIR`, `<repo>/.cache/boot/android-34`
/// (walking up from the current directory), then `~/.cache/acos/boot/android-34`.
pub fn default_boot_dir(explicit: Option<&Path>) -> Option<PathBuf> {
    if let Some(p) = explicit {
        return Some(p.to_path_buf());
    }
    if let Some(p) = std::env::var_os("ACOS_BOOT_DIR") {
        return Some(PathBuf::from(p));
    }
    if let Ok(cwd) = std::env::current_dir() {
        let mut dir = Some(cwd.as_path());
        while let Some(d) = dir {
            let cand = d.join(".cache/boot/android-34");
            if cand.join("BOOTCLASSPATH").is_file() {
                return Some(cand);
            }
            dir = d.parent();
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        let cand = PathBuf::from(home).join(".cache/acos/boot/android-34");
        if cand.join("BOOTCLASSPATH").is_file() {
            return Some(cand);
        }
    }
    None
}

/// A recorded native invocation (native-diff / `--trace-natives`).
#[derive(Clone, Debug)]
pub struct NativeHit {
    pub class: String,
    pub method: String,
    pub signature: String,
    pub count: u64,
    pub first_order: u64,
}

/// Per-run native invocation recorder.
#[derive(Debug, Default)]
pub struct NativeTrace {
    pub hits: Vec<NativeHit>,
    index: HashMap<(String, String, String), usize>,
    order: u64,
}

impl NativeTrace {
    pub fn record(&mut self, class: &str, method: &str, sig: &str) {
        self.order += 1;
        let key = (class.to_string(), method.to_string(), sig.to_string());
        if let Some(&i) = self.index.get(&key) {
            self.hits[i].count += 1;
            return;
        }
        let i = self.hits.len();
        self.index.insert(key, i);
        self.hits.push(NativeHit {
            class: class.to_string(),
            method: method.to_string(),
            signature: sig.to_string(),
            count: 1,
            first_order: self.order,
        });
    }

    /// Emit `natives_*.tsv`-compatible columns.
    pub fn to_tsv(&self) -> String {
        let mut out = String::from("enters\texits\tfirst_order\tcall_site\tclass\tmethod\tsignature\n");
        for h in &self.hits {
            out.push_str(&format!(
                "{}\t{}\t{}\tprogram\t{}\t{}\t{}\n",
                h.count, h.count, h.first_order, h.class, h.method, h.signature
            ));
        }
        out
    }

    /// `class\tmethod\tsignature` for success counters, sorted by first order.
    pub fn classes(&self) -> Vec<(String, String, String)> {
        self.hits
            .iter()
            .map(|h| (h.class.clone(), h.method.clone(), h.signature.clone()))
            .collect()
    }
}

/// Structured boot-classpath failure.
pub fn boot_classpath_missing(dir: &Path, detail: &str) -> VmError {
    VmError::BootClasspathMissing(format!(
        "BOOT_CLASSPATH_MISSING: {detail} (looked in {})\ncreate it with: tools/extract-bootclasspath.sh (or set ACOS_BOOT_DIR)",
        dir.display()
    ))
}

/// A stack-trace entry captured for `printStackTrace`.
#[derive(Clone, Debug)]
pub struct StackEntry {
    pub class: String,
    pub method: String,
    pub line: u32,
}

#[derive(Debug)]
pub struct Vm {
    pub heap: Heap,
    pub dexs: Vec<DexCtx>,
    pub classes: Vec<Class>,
    pub class_by_desc: HashMap<String, ClassId>,
    /// Array element descriptors whose `[<elem>` class has already been loaded.
    /// `Vm::alloc` used to `format!("[{elem}")` and probe `class_by_desc` on
    /// **every** array allocation; array benchmarks allocate millions of
    /// arrays, so this keyed-by-`elem_desc` set removes the per-allocation
    /// `String` allocation and the `"["`-prefixed hash.
    pub(crate) loaded_array_elems: HashSet<String>,
    /// One-entry fast path for the `loaded_array_elems` probe above: a tight
    /// array-allocation loop almost always repeats the same element descriptor,
    /// so remembering the last one skips the string hash on the common case.
    /// Transient (not part of the heap snapshot).
    pub(crate) last_array_elem: Option<String>,
    /// Descriptors of DEX classes whose `load_dex_class` is on the stack.
    ///
    /// A class is only inserted into `class_by_desc` once its fields, methods
    /// and vtable are complete, so this set detects a re-entrant load of a
    /// class that is still being built (a cycle, or a bug) instead of handing
    /// back a partially linked class.
    loading_classes: HashSet<String>,
    pub types: Vec<TypeInfo>,
    pub type_by_desc: HashMap<String, TypeId>,
    pub methods: Vec<Method>,
    /// Keyed by `(class descriptor, method name, signature)` — the native
    /// registry used to bind true `native` DEX methods.
    pub natives: HashMap<(String, String, String), crate::class::NativeFn>,
    pub corelib: CoreLib,
    /// Boot-classpath DEX images (indices into `dexs`), in BOOTCLASSPATH order.
    pub boot_dexs: Vec<usize>,
    /// Boot-classpath jars not parsed at startup; parsed on first class miss.
    pub boot_lazy_jars: Vec<String>,
    pub boot_dir: Option<PathBuf>,
    pub native_trace: Option<NativeTrace>,
    pub strings: HashMap<Vec<u16>, Handle>,
    pub interned: Vec<Handle>,
    pub threads: Vec<ThreadRT>,
    pub current_thread: usize,
    pub next_thread_id: u32,
    pub native_roots: Vec<Handle>,
    pub throwable_stacks: HashMap<Handle, Vec<StackEntry>>,
    pub pending_exception: Handle,

    pub gc_stress: bool,
    pub(crate) gc_threshold: usize,

    /// Wall-clock anchor (unix milliseconds) recorded in snapshot metadata.
    /// Deterministic (0 unless explicitly seeded) so two snapshots of the same
    /// state are byte-equal; the T-43 re-seed hook sets it after restore.
    pub clock_anchor_millis: u64,
    /// Locale recorded in snapshot metadata (defaults to `$LANG` or `en-US`).
    pub snapshot_locale: String,

    // Pre-created fatal exceptions (cannot allocate when thrown).
    pub oom_object: Handle,
    pub stack_overflow_object: Handle,

    // Fast builtin class ids (filled during corelib install).
    pub c_object: ClassId,
    pub c_class: ClassId,
    pub c_string: ClassId,
    pub c_string_builder: ClassId,
    pub c_throwable: ClassId,
    pub c_integer: ClassId,
    pub c_long: ClassId,
    pub c_thread: ClassId,
    pub c_print_stream: ClassId,
    pub c_system: ClassId,

    pub out_buf: Vec<u8>,
    pub err_buf: Vec<u8>,
    pub exit_request: Option<i32>,
    pub stopping: bool,
    pub stdout: Handle,
    pub stderr: Handle,
    pub builtin_alloc: HashMap<ClassId, crate::corelib::BuiltinAlloc>,
    pub int_cache: HashMap<i32, Handle>,
    pub monitors: HashMap<Handle, (usize, u32)>,
    /// ICU converter handles: `jlong` -> canonical charset name (real mode).
    pub converters: HashMap<u64, String>,
    pub next_converter: u64,
    /// Shadow `Object::String` hash cache keyed by handle (real mode).
    pub string_hashes: HashMap<Handle, i32>,
    /// The process main `ThreadGroup` (real mode).
    pub main_thread_group: Handle,
    /// Global inline cache for `invoke-virtual` / `invoke-interface`: maps
    /// `(dex, method_ref index, receiver class)` to the resolved target. The
    /// mapping is a pure function of the declared signature and the receiver's
    /// dynamic class, so it never needs invalidation (classes are never
    /// unloaded). This removes the per-call `name`/`sig` string allocations and
    /// `HashMap<String,_>` probes from virtual/interface dispatch.
    pub(crate) dispatch_cache: HashMap<(u32, u32, ClassId), MethodId>,

    /// Per-VM quickening for the typed-IR tier: resolved field accesses indexed
    /// by the process-unique IR quick slot assigned at lowering. A side vector
    /// indexed by IR instruction, so a snapshot restore (which builds a fresh
    /// `Vm`) starts empty and never reuses a resolved offset from another
    /// layout; [`Vm::invalidate_ir_quickening`] drops it on a class change.
    pub(crate) ir_fields: Vec<crate::exec::FieldQuick>,

    pub metrics: Metrics,
    /// Execution histogram for the 256 opcode bytes (populated when
    /// `count_opcodes` is true).
    pub opcode_counts: [u64; 256],
    pub count_opcodes: bool,
    /// Maximum Java frames per thread before `StackOverflowError`.
    pub max_frames: usize,
    /// Hard host-side RSS and wall-clock ceilings (`ACOS_HOST_RSS_MAX` /
    /// `ACOS_WALL_MAX_SECS`), independent of the guest heap limit.
    pub host: HostGuard,
    /// The per-process virtual filesystem: sandboxed guest paths, the open-fd
    /// table and the per-session write quota.
    pub vfs: crate::vfs::Vfs,
}

impl Vm {
    /// A VM with all per-instance state initialised and **no core library
    /// installed**. `Vm::new` is this plus the corelib/boot-classpath bring-up;
    /// the snapshot image path installs the shared linked boot metadata
    /// instead of re-linking it (T-43).
    pub(crate) fn empty(options: VmOptions) -> Vm {
        let heap = Heap::new(options.heap_max_bytes);
        Vm {
            heap,
            dexs: Vec::new(),
            classes: Vec::new(),
            class_by_desc: HashMap::new(),
            loaded_array_elems: HashSet::new(),
            last_array_elem: None,
            loading_classes: HashSet::new(),
            types: Vec::new(),
            type_by_desc: HashMap::new(),
            methods: Vec::new(),
            natives: HashMap::new(),
            corelib: options.corelib,
            boot_dexs: Vec::new(),
            boot_lazy_jars: Vec::new(),
            boot_dir: None,
            native_trace: if options.trace_natives {
                Some(NativeTrace::default())
            } else {
                None
            },
            strings: HashMap::new(),
            interned: Vec::new(),
            threads: Vec::new(),
            current_thread: 0,
            next_thread_id: 1,
            native_roots: Vec::new(),
            throwable_stacks: HashMap::new(),
            pending_exception: NULL,
            gc_stress: options.gc_stress,
            gc_threshold: (options.heap_max_bytes / 16).max(1 << 20),
            clock_anchor_millis: 0,
            snapshot_locale: std::env::var("LANG").unwrap_or_else(|_| "en-US".to_string()),
            oom_object: NULL,
            stack_overflow_object: NULL,
            c_object: 0,
            c_class: 0,
            c_string: 0,
            c_string_builder: 0,
            c_throwable: 0,
            c_integer: 0,
            c_long: 0,
            c_thread: 0,
            c_print_stream: 0,
            c_system: 0,
            out_buf: Vec::new(),
            err_buf: Vec::new(),
            exit_request: None,
            stopping: false,
            stdout: NULL,
            stderr: NULL,
            builtin_alloc: HashMap::new(),
            int_cache: HashMap::new(),
            monitors: HashMap::new(),
            converters: HashMap::new(),
            next_converter: 1,
            string_hashes: HashMap::new(),
            main_thread_group: NULL,
            dispatch_cache: HashMap::new(),
            ir_fields: Vec::new(),
            metrics: Metrics::default(),
            opcode_counts: [0; 256],
            count_opcodes: false,
            max_frames: 2048,
            host: HostGuard::from_env(),
            vfs: crate::vfs::Vfs::new(crate::vfs::VfsConfig::from_env()),
        }
    }

    pub fn new(options: VmOptions) -> VmResult<Vm> {
        let corelib = options.corelib;
        let boot_dir = options.boot_dir.clone();
        // A fresh VM starts with a fresh native-region table. Regions backing a
        // previous VM on this host thread (e.g. the memory-mapped ICU `.dat`
        // captured while priming `Typeface.<clinit>`) must not leak into this
        // VM's snapshot; the restored heap only ever references regions this VM
        // allocated. `Vm::restore` does not clear: it re-establishes the
        // snapshot's own regions instead (W154-A).
        crate::natives::native_mem::reclaim_all();
        let mut vm = Vm::empty(options);
        match corelib {
            CoreLib::Bootstrap => {
                crate::corelib::install(&mut vm)?;
            }
            CoreLib::Real => {
                crate::natives::install(&mut vm)?;
                let dir = default_boot_dir(boot_dir.as_deref())
                    .ok_or_else(|| boot_classpath_missing(Path::new(".cache/boot/android-34"), "cache directory not found"))?;
                vm.vfs.set_boot_root(&dir);
                vm.load_boot_classpath(&dir)?;
                vm.crate_fast_classes()?;
            }
        }
        Ok(vm)
    }

    /// Install the process-shared linked boot metadata (T-43).
    ///
    /// The image carries the *immutable* part of a linked boot classpath
    /// (classes with field layouts/vtables/interface tables, methods with
    /// decoded bodies, types, provider list) captured once per boot-classpath
    /// fingerprint. The per-VM mutable state (statics, `init`, `class_obj`,
    /// inline caches, quickening) is reset here and re-seeded by the caller
    /// from the snapshot stream, so two VMs never share mutable state.
    pub(crate) fn install_boot_image(
        &mut self,
        image: &crate::snapshot::BootImage,
    ) -> VmResult<()> {
        if self.corelib == CoreLib::Real {
            // Same registry `Vm::new` installs; binding (not layout) state.
            crate::natives::install(self)?;
        }
        for file in &image.dex_files {
            self.add_shared_dex(file.clone());
        }
        self.boot_dexs = image.boot_dexs.clone();
        self.boot_lazy_jars = image.boot_lazy_jars.clone();
        self.boot_dir = image.boot_dir.clone();
        if let Some(dir) = &self.boot_dir {
            self.vfs.set_boot_root(dir);
        }
        self.classes = image.classes.clone();
        self.methods = image.methods.clone();
        self.types = image.types.clone();
        self.type_by_desc = image.type_by_desc.clone();
        self.class_by_desc = image.class_by_desc.clone();
        self.loaded_array_elems = image.loaded_array_elems.clone();
        self.last_array_elem = None;
        self.builtin_alloc = image.builtin_alloc.clone();
        self.c_object = image.c_object;
        self.c_class = image.c_class;
        self.c_string = image.c_string;
        self.c_string_builder = image.c_string_builder;
        self.c_throwable = image.c_throwable;
        self.c_integer = image.c_integer;
        self.c_long = image.c_long;
        self.c_thread = image.c_thread;
        self.c_print_stream = image.c_print_stream;
        self.c_system = image.c_system;
        // Approximate the per-restore clone cost of the linked image (the
        // `Arc<MethodCode>` bodies are shared by pointer, but the `Vec`s and
        // their inline `String`s are deep-cloned on every install). Diagnostic
        // only: a large number here is the multi-restore launch copying the
        // class/method tables once per `Vm::restore`.
        let clone_bytes = image.classes.len() * std::mem::size_of::<crate::class::Class>()
            + image.methods.len() * std::mem::size_of::<crate::class::Method>()
            + image.types.len() * std::mem::size_of::<crate::class::TypeInfo>();
        crate::profile::note_alloc("vm_install_boot_image", clone_bytes as u64);
        // A serialized image carries `NativeFn` placeholders; re-bind every one
        // from the registry `natives::install` just populated above. For an
        // in-process image this is a no-op (same function pointers). This runs
        // on *every* install, so both `Vm::restore` and the later
        // `RuntimeTemplate::restore_runtime` restore see usable natives.
        self.rebind_serialized_natives();
        Ok(())
    }

    /// Re-bind methods that were decoded from an on-disk linked boot image.
    ///
    /// A raw `NativeFn` cannot cross a process boundary, so a serialized
    /// `MethodImpl::Native` decodes to a placeholder. Every such method is
    /// matched against the freshly installed native registry
    /// `(class descriptor, name, signature)`; a miss (build skew) degrades to
    /// `Abstract` so a call fails structurally instead of entering the wrong
    /// native. This also re-applies the few VM-internal bytecode overrides that
    /// `bind_dex_native` installed at boot (e.g. the `String` constructors).
    pub(crate) fn rebind_serialized_natives(&mut self) {
        if self.corelib != CoreLib::Real {
            return;
        }
        for mid in 0..self.methods.len() {
            if !matches!(self.methods[mid].imp, MethodImpl::Native(_)) {
                continue;
            }
            let (class, name, sig) = {
                let m = &self.methods[mid];
                (m.class, m.name.clone(), m.sig.clone())
            };
            let desc = self.class_desc(class).to_string();
            self.methods[mid].imp = match self.natives.get(&(desc, name, sig)) {
                Some(&f) => MethodImpl::Native(f),
                None => MethodImpl::Abstract,
            };
        }
    }

    /// Load the real boot-classpath DEX images, in `BOOTCLASSPATH` order.
    ///
    /// Only `core-oj`, `core-libart` and `core-icu4j` are loaded for M1; the
    /// remaining jars are present on disk and parsed lazily when a class needs
    /// them (the current loader is eager-by-file, so this is explicit).
    pub fn load_boot_classpath(&mut self, dir: &Path) -> VmResult<()> {
        let bootlist = dir.join("BOOTCLASSPATH");
        let text = std::fs::read_to_string(&bootlist).map_err(|_| {
            boot_classpath_missing(dir, "BOOTCLASSPATH is not readable")
        })?;
        let jars: Vec<String> = text
            .lines()
            .map(|l| l.trim())
            .filter(|l| !l.is_empty())
            .map(|s| s.to_string())
            .collect();
        let m1_jars = ["core-oj.jar", "core-libart.jar", "core-icu4j.jar"];
        self.boot_dir = Some(dir.to_path_buf());
        self.boot_lazy_jars = jars
            .iter()
            .filter(|j| !m1_jars.contains(&j.as_str()))
            .cloned()
            .collect();
        for jar in &jars {
            let base = jar.trim_end_matches(".jar");
            // M1 loads the core trio eagerly; the rest stay lazy-on-disk.
            if !m1_jars.contains(&jar.as_str()) {
                continue;
            }
            let dexdir = dir.join("dex").join(base);
            let mut dexs: Vec<PathBuf> = std::fs::read_dir(&dexdir)
                .map_err(|_| boot_classpath_missing(&dexdir, "dex directory missing"))?
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| {
                    p.file_name()
                        .and_then(|n| n.to_str())
                        .map(|n| n.starts_with("classes") && n.ends_with(".dex"))
                        .unwrap_or(false)
                })
                .collect();
            dexs.sort();
            for p in dexs {
                // One parse per process, shared by every Vm (acos_dex::cache).
                let dex = acos_dex::cache::shared_dex(&p)
                    .map_err(VmError::Load)?;
                let idx = self.add_shared_dex(dex);
                self.dexs[idx].source = Some(p);
                self.boot_dexs.push(idx);
            }
        }
        if self.boot_dexs.is_empty() {
            return Err(boot_classpath_missing(dir, "no boot DEX images loaded"));
        }
        Ok(())
    }

    /// Parse one not-yet-loaded boot jar in search of `desc`.
    ///
    /// Called only when the eager providers (core-oj/core-libart/core-icu4j)
    /// and the app DEX cannot supply a class, so the 20 console programs never
    /// pay for the remaining 14 boot jars.
    fn try_load_lazy_boot_jar(&mut self, desc: &str) -> VmResult<bool> {
        let Some(dir) = self.boot_dir.clone() else {
            return Ok(false);
        };
        while let Some(jar) = self.boot_lazy_jars.first().cloned() {
            self.boot_lazy_jars.remove(0);
            let base = jar.trim_end_matches(".jar");
            let dexdir = dir.join("dex").join(base);
            let Ok(entries) = std::fs::read_dir(&dexdir) else {
                continue;
            };
            let mut dexs: Vec<PathBuf> = entries
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| {
                    p.file_name()
                        .and_then(|n| n.to_str())
                        .map(|n| n.starts_with("classes") && n.ends_with(".dex"))
                        .unwrap_or(false)
                })
                .collect();
            dexs.sort();
            let mut found = false;
            for p in dexs {
                let dex = acos_dex::cache::shared_dex(&p)
                    .map_err(VmError::Load)?;
                found |= dex.find_class_index(desc).is_some();
                let idx = self.add_shared_dex(dex);
                self.dexs[idx].source = Some(p);
                self.boot_dexs.push(idx);
            }
            if found {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// After loading the real boot classpath, cache the fundamental class ids
    /// and create the pre-allocated fatal errors.
    pub fn crate_fast_classes(&mut self) -> VmResult<()> {
        // Primitive types have no DEX class definition, so the VM provides
        // their mirror classes (`Class.getPrimitiveClass`/`TYPE`).
        for desc in ["Z", "B", "C", "S", "I", "J", "F", "D", "V"] {
            let id = self.def_root_class(
                desc,
                acos_dex::access::PUBLIC | acos_dex::access::FINAL | acos_dex::access::ABSTRACT,
            );
            self.classes[id as usize].is_primitive = true;
            self.link_class(id)?;
        }
        self.c_object = self.load_class("Ljava/lang/Object;")?;
        self.c_class = self.load_class("Ljava/lang/Class;")?;
        self.c_string = self.load_class("Ljava/lang/String;")?;
        self.c_string_builder = self
            .load_class("Ljava/lang/StringBuilder;")
            .or_else(|_| self.load_class("Ljava/lang/StringBuffer;"))?;
        self.c_throwable = self.load_class("Ljava/lang/Throwable;")?;
        self.c_thread = self.load_class("Ljava/lang/Thread;")?;
        self.c_system = self.load_class("Ljava/lang/System;")?;
        self.c_print_stream = self
            .load_class("Ljava/io/PrintStream;")
            .unwrap_or(0);
        // A process ThreadGroup for the main thread and its children.
        if let Ok(tg) = self.load_class("Ljava/lang/ThreadGroup;") {
            let group = self.alloc_instance(tg)?;
            self.native_roots.push(group);
            let name_h = self.new_string("main")?;
            self.set_instance_field_by_name(group, "name", name_h);
            self.set_instance_field_by_name(group, "maxPriority", 10);
            let threads = self.alloc(Object::Array(crate::heap::ArrayObj {
                elem_desc: "Ljava/lang/Thread;".to_string(),
                data: crate::heap::ArrayData::Ref(Vec::new()),
            }))?;
            self.set_instance_field_by_name(group, "threads", threads);
            self.native_roots.pop();
            self.main_thread_group = group;
        }
        // Pre-allocated fatal errors.
        self.int_cache.clear();
        if self.c_throwable != 0 {
            let oom = self.load_class("Ljava/lang/OutOfMemoryError;").ok();
            let so = self.load_class("Ljava/lang/StackOverflowError;").ok();
            if let Some(cid) = oom {
                self.oom_object = self.alloc_raw_no_gc(Object::Instance {
                    class: cid,
                    fields: vec![0; self.classes[cid as usize].instance_slot_count as usize],
                })?;
            }
            if let Some(cid) = so {
                self.stack_overflow_object = self.alloc_raw_no_gc(Object::Instance {
                    class: cid,
                    fields: vec![0; self.classes[cid as usize].instance_slot_count as usize],
                })?;
            }
        }
        Ok(())
    }

    // ---- types -----------------------------------------------------------

    pub fn intern_type(&mut self, desc: &str) -> TypeId {
        if let Some(&id) = self.type_by_desc.get(desc) {
            return id;
        }
        let id = self.types.len() as TypeId;
        self.types.push(TypeInfo {
            descriptor: desc.to_string(),
            kind: TypeKind::from_descriptor(desc),
        });
        self.type_by_desc.insert(desc.to_string(), id);
        id
    }

    pub fn type_kind(&self, id: TypeId) -> TypeKind {
        self.types[id as usize].kind
    }

    pub fn type_desc(&self, id: TypeId) -> &str {
        &self.types[id as usize].descriptor
    }

    pub fn type_is_ref(&self, id: TypeId) -> bool {
        self.types[id as usize].kind.is_ref()
    }

    // ---- class construction ---------------------------------------------

    /// Create a class record without publishing it in `class_by_desc`.
    ///
    /// `load_dex_class` builds a class through this path so a partially loaded
    /// class is never visible to `load_class`; the descriptor is inserted only
    /// once its fields, methods and vtable are complete.
    fn create_class(&mut self, spec: ClassSpec) -> ClassId {
        let id = self.classes.len() as ClassId;
        let mut class = Class::new(spec.descriptor.clone());
        class.access = spec.access;
        class.super_class = spec.super_class;
        class.interfaces = spec.interfaces;
        class.is_interface = spec.is_interface;
        class.is_builtin = spec.is_builtin;
        if let Some(sup) = spec.super_class {
            class.instance_slot_count = self.classes[sup as usize].instance_slot_count;
            class.instance_refs = self.classes[sup as usize].instance_refs.clone();
        }
        self.classes.push(class);
        id
    }

    pub fn add_class(&mut self, spec: ClassSpec) -> ClassId {
        if let Some(&id) = self.class_by_desc.get(&spec.descriptor) {
            return id;
        }
        let id = self.create_class(spec);
        let desc = self.class_desc(id).to_string();
        self.class_by_desc.insert(desc, id);
        id
    }

    pub fn class_desc(&self, id: ClassId) -> &str {
        &self.classes[id as usize].descriptor
    }

    pub fn find_class(&self, desc: &str) -> Option<ClassId> {
        self.class_by_desc.get(desc).copied()
    }

    /// Add an instance field to a class and assign its absolute slot offset.
    pub fn add_instance_field(&mut self, class: ClassId, name: &str, desc: &str, access: u32) {
        let is_ref = self.intern_type(desc);
        let is_ref = self.type_is_ref(is_ref);
        let kind = TypeKind::from_descriptor(desc);
        let width = crate::class::type_width(kind);
        let offset = self.classes[class as usize].instance_slot_count;
        let def = FieldDef {
            name: name.to_string(),
            desc: desc.to_string(),
            access,
            class,
            offset,
            is_ref,
            width,
        };
        self.classes[class as usize]
            .declared_instance_fields
            .push(def);
        for _ in 0..width {
            self.classes[class as usize].instance_refs.push(false);
        }
        if is_ref {
            let n = self.classes[class as usize].instance_refs.len();
            self.classes[class as usize].instance_refs[n - 1] = true;
        }
        self.classes[class as usize].instance_slot_count += width;
    }

    /// Add a static field and reserve its static slot.
    pub fn add_static_field(&mut self, class: ClassId, name: &str, desc: &str, access: u32) {
        let tid = self.intern_type(desc);
        let is_ref = self.type_is_ref(tid);
        let kind = TypeKind::from_descriptor(desc);
        let width = crate::class::type_width(kind);
        let offset = self.classes[class as usize].statics.len() as u32;
        let def = FieldDef {
            name: name.to_string(),
            desc: desc.to_string(),
            access,
            class,
            offset,
            is_ref,
            width,
        };
        self.classes[class as usize].declared_static_fields.push(def);
        for _ in 0..width {
            self.classes[class as usize].statics.push(0);
            self.classes[class as usize].static_refs.push(false);
        }
        if is_ref {
            let n = self.classes[class as usize].static_refs.len();
            self.classes[class as usize].static_refs[n - 1] = true;
        }
    }

    /// Register a native method (builtin classes).
    pub fn add_native(
        &mut self,
        class: ClassId,
        name: &str,
        sig: &str,
        access: u32,
        func: crate::class::NativeFn,
    ) -> MethodId {
        let desc = self.class_desc(class).to_string();
        self.natives
            .insert((desc, name.to_string(), sig.to_string()), func);
        let (params, ret) = parse_signature(sig).expect("builtin signature must be valid");
        let params: Vec<TypeId> = params.iter().map(|p| self.intern_type(p)).collect();
        let ret = self.intern_type(&ret);
        self.add_method(class, name, sig, params, ret, access, MethodImpl::Native(func))
    }

    /// Register a true native implementation for a real DEX class.
    pub fn register_native(
        &mut self,
        class_desc: &str,
        name: &str,
        sig: &str,
        func: crate::class::NativeFn,
    ) {
        self.natives
            .insert((class_desc.to_string(), name.to_string(), sig.to_string()), func);
    }

    /// Bind a loaded DEX method to its Rust implementation, if one is
    /// registered under `(class, name, signature)`.
    ///
    /// This normally binds true `native` methods. It also lets the VM override
    /// a handful of bytecode methods whose storage is VM-internal (the real
    /// `java.lang.String` constructors, which ART itself does not execute as
    /// bytecode when called from `StringLatin1`/`StringUTF16`).
    fn bind_dex_native(&mut self, mid: MethodId) {
        let (class, name, sig) = {
            let m = &self.methods[mid as usize];
            (
                m.class,
                m.name.clone(),
                m.sig.clone(),
            )
        };
        let desc = self.class_desc(class).to_string();
        // Bind any registered implementation. This covers true `native`
        // methods and the small set of VM-internal overrides (real
        // `java.lang.String` constructors, `AtomicInteger` storage, ...) whose
        // DEX bodies are stubs or use unsupported reflection.
        if let Some(&f) = self.natives.get(&(desc, name, sig)) {
            self.methods[mid as usize].imp = MethodImpl::Native(f);
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn add_dex_method(
        &mut self,
        class: ClassId,
        name: &str,
        sig: &str,
        params: Vec<TypeId>,
        ret: TypeId,
        access: u32,
        dex: u32,
        method_idx: u32,
        code: Option<Arc<MethodCode>>,
    ) -> MethodId {
        self.add_method(
            class,
            name,
            sig,
            params,
            ret,
            access,
            MethodImpl::Dex {
                dex,
                method_idx,
                code,
            },
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn add_method(
        &mut self,
        class: ClassId,
        name: &str,
        sig: &str,
        params: Vec<TypeId>,
        ret: TypeId,
        access: u32,
        imp: MethodImpl,
    ) -> MethodId {
        let is_static = access & acos_dex::access::STATIC != 0;
        let mut ins_size = if is_static { 0 } else { 1 };
        let mut ins_ref = Vec::new();
        if !is_static {
            ins_ref.push(true);
        }
        for &p in &params {
            let kind = self.type_kind(p);
            let w = crate::class::type_width(kind);
            ins_size += w;
            for i in 0..w {
                ins_ref.push(i == 0 && kind.is_ref());
            }
        }
        let id = self.methods.len() as MethodId;
        self.methods.push(Method {
            class,
            name: name.to_string(),
            sig: sig.to_string(),
            params,
            ret,
            access,
            imp,
            ins_size,
            ins_ref,
        });
        self.classes[class as usize].declared_methods.push(id);
        let key = (name.to_string(), sig.to_string());
        let is_direct = is_static
            || access & acos_dex::access::PRIVATE != 0
            || access & acos_dex::access::CONSTRUCTOR != 0
            || name == "<init>"
            || name == "<clinit>";
        if is_direct {
            self.classes[class as usize].direct_map.insert(key, id);
        } else {
            self.classes[class as usize].virtual_map.insert(key, id);
        }
        id
    }

    /// Build the vtable / interface default tables for a class.
    pub fn link_class(&mut self, id: ClassId) -> VmResult<()> {
        let __prof = crate::profile::start();
        // Start from the superclass tables.
        let (mut vtable, mut vindex, mut vmap) = if let Some(sup) =
            self.classes[id as usize].super_class
        {
            let s = &self.classes[sup as usize];
            (s.vtable.clone(), s.vtable_index.clone(), s.virtual_map.clone())
        } else {
            (Vec::new(), HashMap::new(), HashMap::new())
        };

        // Declared virtual methods override/add slots.
        let declared = self.classes[id as usize].declared_methods.clone();
        for mid in &declared {
            let m = &self.methods[*mid as usize];
            let is_direct = m.is_static()
                || m.access & acos_dex::access::PRIVATE != 0
                || m.access & acos_dex::access::CONSTRUCTOR != 0
                || m.name == "<init>"
                || m.name == "<clinit>";
            if is_direct {
                continue;
            }
            let key = (m.name.clone(), m.sig.clone());
            match vindex.get(&key) {
                Some(&slot) => vtable[slot] = *mid,
                None => {
                    vindex.insert(key.clone(), vtable.len());
                    vtable.push(*mid);
                }
            }
            vmap.insert(key, *mid);
            if m.is_abstract() {
                self.classes[id as usize].abstract_methods.insert((m.name.clone(), m.sig.clone()));
            }
        }

        // Merge interface default methods.
        let interfaces = self.classes[id as usize].interfaces.clone();
        let mut defaults: Vec<((String, String), MethodId)> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for iface in &interfaces {
            self.collect_defaults(*iface, &mut defaults, &mut seen);
        }
        for (key, mid) in defaults {
            match vindex.get(&key) {
                Some(&slot) => {
                    let existing = vtable[slot];
                    if self.methods[existing as usize].is_abstract() {
                        vtable[slot] = mid;
                        vmap.insert(key, mid);
                    }
                }
                None => {
                    vindex.insert(key.clone(), vtable.len());
                    vtable.push(mid);
                    vmap.insert(key, mid);
                }
            }
        }

        let c = &mut self.classes[id as usize];
        c.vtable = vtable;
        c.vtable_index = vindex;
        c.virtual_map = vmap;
        crate::profile::record_class_link(self, id, __prof);
        Ok(())
    }

    fn collect_defaults(
        &self,
        iface: ClassId,
        out: &mut Vec<((String, String), MethodId)>,
        seen: &mut std::collections::HashSet<(String, String)>,
    ) {
        let c = &self.classes[iface as usize];
        for (key, mid) in &c.virtual_map {
            if seen.contains(key) {
                continue;
            }
            let m = &self.methods[*mid as usize];
            if !m.is_abstract() {
                seen.insert(key.clone());
                out.push((key.clone(), *mid));
            }
        }
        for sup in &c.interfaces {
            self.collect_defaults(*sup, out, seen);
        }
    }

    // ---- loading ---------------------------------------------------------

    /// Load a class by descriptor, lazily, from the DEX providers then builtins.
    pub fn load_class(&mut self, desc: &str) -> VmResult<ClassId> {
        if let Some(&id) = self.class_by_desc.get(desc) {
            return Ok(id);
        }
        if desc.starts_with('[') {
            return self.load_array_class(desc);
        }
        if desc.starts_with('L') && desc.ends_with(';') {
            for di in 0..self.dexs.len() {
                // The shared class index avoids decoding every class descriptor
                // on each miss (and is built once per parsed file, not per Vm).
                if let Some(def_idx) = self.dexs[di].file.find_class_index(desc) {
                    return self.load_dex_class(di as u32, def_idx);
                }
            }
            // Last resort: parse the remaining boot jars one at a time.
            if self.try_load_lazy_boot_jar(desc)? {
                return self.load_class(desc);
            }
        }
        Err(VmError::unsupported_api(desc.to_string(), String::new()))
    }

    fn load_array_class(&mut self, desc: &str) -> VmResult<ClassId> {
        let object = self.load_class("Ljava/lang/Object;")?;
        let id = self.add_class(ClassSpec {
            descriptor: desc.to_string(),
            access: acos_dex::access::PUBLIC | acos_dex::access::FINAL,
            super_class: Some(object),
            interfaces: Vec::new(),
            is_interface: false,
            is_builtin: false,
        });
        self.classes[id as usize].is_array = true;
        self.link_class(id)?;
        Ok(id)
    }

    /// Load `desc` from one specific DEX provider (the `DexFile` cookie's
    /// provider), as `DexFile.defineClassNative` must.
    ///
    /// An already loaded class is returned as-is: ART's `defineClassNative`
    /// also returns the previously defined `Class` for a repeat request. A
    /// class that this provider does not declare is reported as unsupported so
    /// the caller can return null (the Java `DexFile.defineClass` wrapper then
    /// records a `NoClassDefFoundError`).
    pub(crate) fn load_class_from_dex(&mut self, dex_idx: u32, desc: &str) -> VmResult<ClassId> {
        if let Some(&id) = self.class_by_desc.get(desc) {
            return Ok(id);
        }
        let def_idx = self.dexs[dex_idx as usize]
            .file
            .find_class_index(desc)
            .ok_or_else(|| VmError::unsupported_api(desc.to_string(), String::new()))?;
        self.load_dex_class(dex_idx, def_idx)
    }

    fn load_dex_class(&mut self, dex_idx: u32, def_idx: usize) -> VmResult<ClassId> {
        let file = self.dexs[dex_idx as usize].file.clone();
        let def = &file.class_defs()[def_idx];
        let desc = file.class_descriptor(def)?.to_string();
        if let Some(&id) = self.class_by_desc.get(&desc) {
            return Ok(id);
        }
        // A class currently being loaded must not be re-entered: the caller
        // would observe a class whose methods/vtable are still incomplete
        // (e.g. resolving `Landroid/os/Build;->-$$Nest$...` while the host class
        // is mid-load). Class hierarchies are acyclic, so a re-entry here is a
        // cycle/bug and is reported rather than yielding a partial class.
        if !self.loading_classes.insert(desc.clone()) {
            return Err(VmError::Load(format!("cyclic class load: {desc}")));
        }
        let result = self.load_dex_class_body(&file, dex_idx, def_idx, &desc);
        self.loading_classes.remove(&desc);
        result
    }

    fn load_dex_class_body(
        &mut self,
        file: &Arc<DexFile>,
        dex_idx: u32,
        def_idx: usize,
        desc: &str,
    ) -> VmResult<ClassId> {
        let __prof = crate::profile::start();
        let def = &file.class_defs()[def_idx];
        let is_interface = def.access_flags & acos_dex::access::INTERFACE != 0;
        let id = self.create_class(ClassSpec {
            descriptor: desc.to_string(),
            access: def.access_flags,
            super_class: None,
            interfaces: Vec::new(),
            is_interface,
            is_builtin: false,
        });
        self.classes[id as usize].dex = Some(dex_idx);
        self.classes[id as usize].def_idx = Some(def_idx);

        // Superclass + interfaces.
        if let Some(sup) = file.superclass_descriptor(def)?.map(|s| s.to_string()) {
            let sup_id = self.load_class(&sup)?;
            self.classes[id as usize].super_class = Some(sup_id);
            self.classes[id as usize].instance_slot_count =
                self.classes[sup_id as usize].instance_slot_count;
            self.classes[id as usize].instance_refs =
                self.classes[sup_id as usize].instance_refs.clone();
        }
        let mut ifaces = Vec::new();
        for &ti in &def.interfaces {
            let d = file.type_descriptor(ti)?.to_string();
            ifaces.push(self.load_class(&d)?);
        }
        self.classes[id as usize].interfaces = ifaces;
        self.classes[id as usize].source_file =
            file.source_file(def)?.map(|s| s.to_string());

        // Static field initial values (applied in order before <clinit>).
        let static_values = file.static_values(def)?.unwrap_or_default();

        // Fields.
        let class_data = file.class_data(def)?;
        if let Some(cd) = &class_data {
            for f in &cd.static_fields {
                let fr = file.field_ref(f.field_idx)?;
                let name = fr.name.to_string();
                let fdesc = fr.type_.to_string();
                self.add_static_field(id, &name, &fdesc, f.access_flags);
            }
            for f in &cd.instance_fields {
                let fr = file.field_ref(f.field_idx)?;
                let name = fr.name.to_string();
                let fdesc = fr.type_.to_string();
                self.add_instance_field(id, &name, &fdesc, f.access_flags);
            }
            // Apply encoded static values by position.
            let statics: Vec<FieldDef> =
                self.classes[id as usize].declared_static_fields.clone();
            for (i, val) in static_values.iter().enumerate() {
                if i >= statics.len() {
                    break;
                }
                let f = &statics[i];
                let raw = self.encoded_to_slots(dex_idx, val, f)?;
                let off = f.offset as usize;
                for (j, slot) in raw.into_iter().enumerate() {
                    self.classes[id as usize].statics[off + j] = slot;
                }
            }

            // Methods.
            for m in cd.direct_methods.iter().chain(cd.virtual_methods.iter()) {
                self.add_dex_method_from(file.clone(), dex_idx, id, m.method_idx, m.access_flags, m.code_off)?;
            }
        }

        self.link_class(id)?;
        // Publish only after fields, methods and vtable are complete so that no
        // caller can observe a partially linked class.
        self.class_by_desc.insert(desc.to_string(), id);
        self.metrics.classes_loaded = self.classes.len();
        crate::profile::record_class_parse(desc, __prof);
        Ok(id)
    }

    fn encoded_to_slots(
        &mut self,
        dex: u32,
        v: &EncodedValue,
        field: &FieldDef,
    ) -> VmResult<Vec<u32>> {
        let _ = field;
        Ok(match v {
            EncodedValue::Boolean(b) => vec![*b as u32],
            EncodedValue::Byte(b) => vec![*b as i32 as u32],
            EncodedValue::Short(s) => vec![*s as i32 as u32],
            EncodedValue::Char(c) => vec![*c as u32],
            EncodedValue::Int(i) => vec![*i as u32],
            EncodedValue::Long(l) => vec![*l as u32, (*l >> 32) as u32],
            EncodedValue::Float(f) => vec![*f],
            EncodedValue::Double(d) => vec![*d as u32, (*d >> 32) as u32],
            EncodedValue::String(s) => {
                // The string id belongs to the DEX that declared this class, not
                // necessarily `dexs[0]` (lazy boot-classpath jars load later).
                let text = self.dexs[dex as usize].file.string(*s)?.to_string();
                let h = self.intern_str(&text)?;
                vec![h]
            }
            EncodedValue::Null => vec![0],
            other => {
                return Err(VmError::Load(format!(
                    "unsupported static value {other:?}"
                )))
            }
        })
    }

    fn add_dex_method_from(
        &mut self,
        file: Arc<DexFile>,
        dex_idx: u32,
        class: ClassId,
        method_idx: u32,
        access: u32,
        code_off: u32,
    ) -> VmResult<()> {
        let mr = file.method_ref(method_idx)?;
        let name = mr.name.to_string();
        let ret = mr.return_type.to_string();
        let param_descs: Vec<String> = mr.parameters.iter().map(|s| s.to_string()).collect();
        let sig = format!("({}){}", param_descs.concat(), ret);
        let params: Vec<TypeId> = param_descs.iter().map(|d| self.intern_type(d)).collect();
        let ret_id = self.intern_type(&ret);
        let code = if code_off != 0 {
            let ci = file.code_item(code_off)?;
            // Decoded bodies are a pure function of the DEX bytes, so share one
            // copy per process instead of one per session.
            let code = crate::codecache::get_or_decode(&file, code_off, || {
                self.decode_method(&file, &ci)
            })?;
            // Intern the method's string constants eagerly so that later
            // `const-string` never has to allocate (critical after OOM).
            for insn in &code.insns {
                if insn.op == 0x1a || insn.op == 0x1b {
                    self.dex_string(dex_idx, insn.index)?;
                }
            }
            Some(code)
        } else {
            None
        };
        let mid = self.add_dex_method(
            class, &name, &sig, params, ret_id, access, dex_idx, method_idx, code,
        );
        self.bind_dex_native(mid);
        Ok(())
    }

    /// Intern a DEX string constant by index (cached per DEX).
    pub(crate) fn dex_string(&mut self, dex: u32, index: u32) -> VmResult<Handle> {
        if let Some(h) = self.dexs[dex as usize].string_cache[index as usize] {
            return Ok(h);
        }
        let s = self.dexs[dex as usize].file.string(index)?.to_string();
        let h = self.intern_str(&s)?;
        self.dexs[dex as usize].string_cache[index as usize] = Some(h);
        Ok(h)
    }

    fn decode_method(&self, file: &DexFile, ci: &acos_dex::CodeItem) -> VmResult<MethodCode> {
        let raw = &ci.insns;
        let mut insns = Vec::new();
        let mut pc_to_idx = vec![u32::MAX; raw.len() + 1];
        let mut i = 0usize;
        while i < raw.len() {
            if acos_dex::payload_kind(raw, i).is_some() {
                let len = acos_dex::payload_len(raw, i)
                    .map_err(|e| VmError::Load(format!("payload: {e}")))?;
                i += len;
                continue;
            }
            let ins = acos_dex::decode(raw, i)
                .map_err(|e| VmError::Load(format!("decode: {e}")))?;
            pc_to_idx[i] = insns.len() as u32;
            insns.push(Insn {
                op: ins.opcode.byte(),
                size: ins.size,
                regs: ins.regs,
                reg_count: ins.reg_count,
                literal: ins.literal,
                index: ins.index,
                index2: ins.index2,
                target: ins.target,
            });
            i += ins.size as usize;
        }

        let mut tries = Vec::new();
        for t in &ci.tries {
            let h = &ci.handlers[t.handler_index];
            let mut typed = Vec::new();
            for p in &h.handlers {
                let d = file.type_descriptor(p.type_idx)?.to_string();
                typed.push((d, p.addr));
            }
            tries.push(TryEntry {
                start: t.start_addr,
                end: t.start_addr + t.insn_count as u32,
                typed,
                catch_all: h.catch_all_addr,
            });
        }
        let line_table = file.line_table(ci)?;
        Ok(MethodCode {
            registers_size: ci.registers_size,
            ins_size: ci.ins_size,
            outs_size: ci.outs_size,
            insns,
            pc_to_idx,
            raw: raw.clone(),
            tries,
            line_table,
            source_file: None,
        })
    }

    /// Register a DEX image as a class provider.
    pub fn add_dex(&mut self, file: DexFile) -> usize {
        self.add_shared_dex(Arc::new(file))
    }

    /// Register an already-parsed, process-shared DEX image as a class
    /// provider. The per-VM caches stay per-VM; only the immutable parse is
    /// shared.
    pub fn add_shared_dex(&mut self, file: Arc<DexFile>) -> usize {
        let strings = file.strings().len();
        let fields = file.field_ids().len();
        let methods = file.method_ids().len();
        let types = file.type_ids().len();
        let idx = self.dexs.len();
        self.dexs.push(DexCtx {
            file,
            string_cache: vec![None; strings],
            field_cache: vec![None; fields],
            method_cache: vec![None; methods],
            virtual_cache: vec![None; methods],
            class_cache: vec![None; types],
            source: None,
        });
        idx
    }

    // ---- allocation / strings -------------------------------------------

    /// Allocate an object, running GC when required and throwing a Java
    /// `OutOfMemoryError` if the heap cannot be satisfied.
    pub fn alloc(&mut self, obj: Object) -> VmResult<Handle> {
        // Host ceiling: independent of the guest heap, checked on every
        // allocation (throttled internally).
        self.host.check()?;
        if let Object::Array(a) = &obj {
            if self.last_array_elem.as_deref() != Some(a.elem_desc.as_str()) {
                if !self.loaded_array_elems.contains(a.elem_desc.as_str()) {
                    let desc = format!("[{}", a.elem_desc);
                    self.load_class(&desc)?;
                    self.loaded_array_elems.insert(a.elem_desc.clone());
                }
                self.last_array_elem = Some(a.elem_desc.clone());
            }
        }
        if self.gc_stress || self.heap.live_bytes >= self.gc_threshold {
            self.gc();
        }
        if self.heap.live_bytes > self.heap.max_bytes {
            return Err(VmError::JavaException(self.oom_object));
        }
        Ok(self.heap.alloc_raw(obj))
    }

    pub fn alloc_instance(&mut self, class: ClassId) -> VmResult<Handle> {
        let slots = self.classes[class as usize].instance_slot_count as usize;
        let fields = self.heap.take_field_vec(slots);
        self.alloc(Object::Instance { class, fields })
    }

    /// Validate a guest array length before building its backing store.
    ///
    /// Rejects against both the guest heap cap and the hard per-array cap, and
    /// throws a real Java `OutOfMemoryError` (never a host abort). Guest-sized
    /// `vec![..; len]`/`with_capacity(len)` must go through this first
    /// (`HONESTY_AUDIT.md` #2).
    pub fn check_array_len(&mut self, elem_desc: &str, len: usize) -> VmResult<()> {
        if crate::heap::array_len_fits(elem_desc, len, self.heap.max_bytes) {
            return Ok(());
        }
        Err(self.throw_new(
            "Ljava/lang/OutOfMemoryError;",
            Some(&format!(
                "array length {len} of {elem_desc} exceeds the heap cap ({} bytes)",
                self.heap.max_bytes
            )),
        ))
    }

    pub fn new_string_utf16(&mut self, units: Vec<u16>) -> VmResult<Handle> {
        self.alloc(Object::String(units))
    }

    pub fn new_string(&mut self, s: &str) -> VmResult<Handle> {
        let units: Vec<u16> = s.encode_utf16().collect();
        self.new_string_utf16(units)
    }

    pub fn intern_str(&mut self, s: &str) -> VmResult<Handle> {
        let units: Vec<u16> = s.encode_utf16().collect();
        if let Some(&h) = self.strings.get(&units) {
            return Ok(h);
        }
        let h = self.alloc_raw_no_gc(Object::String(units.clone()))?;
        self.strings.insert(units, h);
        self.interned.push(h);
        Ok(h)
    }

    fn alloc_raw_no_gc(&mut self, obj: Object) -> VmResult<Handle> {
        self.host.check()?;
        if self.heap.live_bytes > self.heap.max_bytes {
            return Err(VmError::JavaException(self.oom_object));
        }
        Ok(self.heap.alloc_raw(obj))
    }

    pub fn str_units(&self, h: Handle) -> Option<&[u16]> {
        match self.heap.get(h) {
            Some(Object::String(u)) => Some(u),
            _ => None,
        }
    }

    pub fn string_to_rust(&self, h: Handle) -> Option<String> {
        match self.heap.get(h) {
            Some(Object::String(u)) => Some(String::from_utf16_lossy(u)),
            Some(Object::Builtin {
                data: BuiltinData::StringBuilder(u),
                ..
            }) => Some(String::from_utf16_lossy(u)),
            _ => None,
        }
    }

    // ---- GC --------------------------------------------------------------

    pub fn gc(&mut self) {
        let start = Instant::now();
        let n = self.heap.objects.len();
        // Generation bitmap + reused worklist: a fresh `vec![false; n]` and a
        // fresh stack per collection cost an allocation and a full `memset`
        // each time. The generation counter marks live slots without clearing;
        // it is reset only on wrap.
        let mut gen = self.heap.gc_gen.wrapping_add(1);
        if gen == 0 {
            self.heap.gc_mark.fill(0);
            gen = 1;
        }
        self.heap.gc_gen = gen;
        if self.heap.gc_mark.len() < n {
            self.heap.gc_mark.resize(n, 0);
        }
        let mut stack: Vec<Handle> = std::mem::take(&mut self.heap.gc_stack);

        for t in &self.threads {
            if t.object != NULL {
                stack.push(t.object);
            }
            if let Some(w) = t.wait_monitor {
                stack.push(w);
            }
            for f in &t.frames {
                for (i, &r) in f.refs.iter().enumerate() {
                    if r {
                        stack.push(f.regs[i]);
                    }
                }
            }
        }
        for c in &self.classes {
            for (i, &r) in c.static_refs.iter().enumerate() {
                if r {
                    stack.push(c.statics[i]);
                }
            }
            if c.class_obj != NULL {
                stack.push(c.class_obj);
            }
        }
        stack.extend(self.interned.iter().copied());
        stack.extend(self.int_cache.values().copied());
        stack.extend(self.native_roots.iter().copied());
        stack.push(self.pending_exception);
        stack.push(self.oom_object);
        stack.push(self.stack_overflow_object);
        stack.push(self.main_thread_group);

        while let Some(h) = stack.pop() {
            if h == NULL || h as usize >= n || self.heap.gc_mark[h as usize] == gen {
                continue;
            }
            self.heap.gc_mark[h as usize] = gen;
            match &self.heap.objects[h as usize] {
                Some(Object::Array(a)) => {
                    if let crate::heap::ArrayData::Ref(v) = &a.data {
                        for &e in v {
                            stack.push(e);
                        }
                    }
                }
                Some(Object::Instance { class, fields }) => {
                    if let Some(c) = self.classes.get(*class as usize) {
                        for (i, &is_ref) in c.instance_refs.iter().enumerate() {
                            if is_ref {
                                stack.push(fields[i]);
                            }
                        }
                    }
                }
                Some(Object::Builtin { data, .. }) => match data {
                    BuiltinData::List(v) | BuiltinData::Iterator { items: v, .. } => {
                        stack.extend(v.iter().copied())
                    }
                    BuiltinData::HashMap(v) | BuiltinData::TreeMap(v) => {
                        for (k, val) in v {
                            stack.push(*k);
                            stack.push(*val);
                        }
                    }
                    BuiltinData::EnumSet { elements, .. } => {
                        stack.extend(elements.iter().copied())
                    }
                    BuiltinData::Entry { key, value } => {
                        stack.push(*key);
                        stack.push(*value);
                    }
                    BuiltinData::Thread(t) => {
                        stack.push(t.name);
                        stack.push(t.target);
                    }
                    _ => {}
                },
                _ => {}
            }
        }

        let mut i = 1usize;
        while i < self.heap.objects.len() {
            if let Some(obj) = self.heap.objects[i].as_ref() {
                if self.heap.gc_mark[i] != gen {
                    // Only strings are indexed in `string_hashes`; probing the
                    // map for every dead slot was a wasted hash lookup per
                    // collected object.
                    if matches!(obj, Object::String(_)) {
                        self.string_hashes.remove(&(i as u32));
                    }
                    self.heap.free_slot(i as u32);
                }
            }
            i += 1;
        }
        self.heap.gc_stack = stack;
        self.heap.gc_stack.clear();

        self.heap.gc_count += 1;
        self.heap.gc_nanos += start.elapsed().as_nanos() as u64;
        self.metrics.gc_count = self.heap.gc_count;
        crate::profile::record_gc(start);
        self.gc_threshold = (self.heap.live_bytes * 2)
            .max(self.heap.max_bytes / 16)
            .max(1 << 20);
    }

    // ---- class objects ---------------------------------------------------

    pub fn class_object(&mut self, id: ClassId) -> VmResult<Handle> {
        let existing = self.classes[id as usize].class_obj;
        if existing != NULL {
            return Ok(existing);
        }
        let class_id = self.c_class;
        let h = self.alloc(Object::Class(id))?;
        // Object::Class does not carry the instance class; record for getClass.
        if let Some(Object::Class(_)) = self.heap.get(h) {
            // nothing
        }
        let _ = class_id;
        self.classes[id as usize].class_obj = h;
        Ok(h)
    }

    /// The represented `ClassId` of a `java.lang.Class` instance.
    pub fn class_id_of_class_object(&self, h: Handle) -> Option<ClassId> {
        match self.heap.get(h)? {
            Object::Class(id) => Some(*id),
            _ => None,
        }
    }

    pub fn class_of_object(&self, h: Handle) -> Option<ClassId> {
        match self.heap.get(h)? {
            Object::Instance { class, .. } => Some(*class),
            Object::Array(a) => {
                let desc = format!("[{}", a.elem_desc);
                self.class_by_desc.get(&desc).copied()
            }
            Object::String(_) => Some(self.c_string),
            Object::Class(_) => Some(self.c_class),
            Object::Builtin { class, .. } => Some(*class),
        }
    }

    // ---- type checks -----------------------------------------------------

    pub fn is_assignable(&mut self, from: ClassId, to: ClassId) -> VmResult<bool> {
        if from == to {
            return Ok(true);
        }
        let to_desc = self.class_desc(to).to_string();
        if to_desc == "Ljava/lang/Object;" {
            return Ok(true);
        }
        let from_desc = self.class_desc(from).to_string();
        if from_desc.starts_with('[') || to_desc.starts_with('[') {
            return self.array_assignable(&from_desc, &to_desc);
        }
        // Walk superclass chain and interfaces.
        let mut c = Some(from);
        let mut seen = std::collections::HashSet::new();
        while let Some(cur) = c {
            if !seen.insert(cur) {
                break;
            }
            if cur == to {
                return Ok(true);
            }
            let ifaces = self.classes[cur as usize].interfaces.clone();
            for i in ifaces {
                if self.is_assignable(i, to)? {
                    return Ok(true);
                }
            }
            c = self.classes[cur as usize].super_class;
        }
        Ok(false)
    }

    fn array_assignable(&mut self, from: &str, to: &str) -> VmResult<bool> {
        if from == "Ljava/lang/Object;" {
            return Ok(false);
        }
        if to == "Ljava/lang/Cloneable;" || to == "Ljava/io/Serializable;" {
            return Ok(true);
        }
        let from_arr = from.starts_with('[');
        let to_arr = to.starts_with('[');
        if !from_arr || !to_arr {
            return Ok(false);
        }
        let fc = &from[1..];
        let tc = &to[1..];
        let fprim = !fc.starts_with('L') && !fc.starts_with('[');
        let tprim = !tc.starts_with('L') && !tc.starts_with('[');
        if fprim || tprim {
            return Ok(fc == tc);
        }
        if tc == "Ljava/lang/Object;" {
            return Ok(true);
        }
        let fcid = self.load_class(fc)?;
        let tcid = self.load_class(tc)?;
        self.is_assignable(fcid, tcid)
    }

    // ---- field resolution ------------------------------------------------

    /// Drop the typed-IR field quickening side table. Called by any class
    /// (re)definition that can change a field's resolved offset/width/sign, and
    /// implicitly unnecessary after a snapshot restore because restore builds a
    /// fresh `Vm` (empty table). The lowered programs themselves stay valid:
    /// they only hold the DEX instruction and its quick slot, so the next
    /// `exec::quicken` pass re-resolves each slot against the new layout before
    /// the tier executes it.
    pub fn invalidate_ir_quickening(&mut self) {
        self.ir_fields.clear();
        // Frames with a cached IR program must re-resolve their field slots.
        for t in &mut self.threads {
            for f in &mut t.frames {
                f.ir_quickened = false;
            }
        }
    }

    pub fn resolve_field(&mut self, dex: u32, field_idx: u32) -> VmResult<ResolvedField> {
        if let Some(rf) = self.dexs[dex as usize].field_cache[field_idx as usize] {
            return Ok(rf);
        }
        let (class_desc, name, fdesc) = {
            let f = self.dexs[dex as usize].file.field_ref(field_idx)?;
            (f.class.to_string(), f.name.to_string(), f.type_.to_string())
        };
        let cid = self.load_class(&class_desc)?;
        let sign = FieldSign::of_desc(&fdesc);
        let mut c = Some(cid);
        let mut found: Option<ResolvedField> = None;
        while let Some(cur) = c {
            {
                let class = &self.classes[cur as usize];
                if let Some(f) = class
                    .declared_instance_fields
                    .iter()
                    .find(|f| f.name == name && f.desc == fdesc)
                {
                    found = Some(ResolvedField {
                        class: cur,
                        offset: f.offset,
                        is_static: false,
                        is_ref: f.is_ref,
                        width: f.width,
                        sign,
                    });
                    break;
                }
                if let Some(f) = class
                    .declared_static_fields
                    .iter()
                    .find(|f| f.name == name && f.desc == fdesc)
                {
                    found = Some(ResolvedField {
                        class: cur,
                        offset: f.offset,
                        is_static: true,
                        is_ref: f.is_ref,
                        width: f.width,
                        sign,
                    });
                    break;
                }
            }
            c = self.classes[cur as usize].super_class;
        }
        let rf = found.ok_or_else(|| {
            VmError::Load(format!("field not found: {class_desc}.{name}:{fdesc}"))
        })?;
        self.dexs[dex as usize].field_cache[field_idx as usize] = Some(rf);
        Ok(rf)
    }

    // ---- method resolution ----------------------------------------------

    pub fn resolve_method_ref(
        &mut self,
        dex: u32,
        method_idx: u32,
        kind: InvokeKind,
    ) -> VmResult<MethodId> {
        if kind != InvokeKind::Virtual && kind != InvokeKind::Interface {
            if let Some(m) = self.dexs[dex as usize].method_cache[method_idx as usize] {
                return Ok(m);
            }
        }
        let (class_desc, name, sig) = {
            let r = self.dexs[dex as usize].file.method_ref(method_idx)?;
            (
                r.class.to_string(),
                r.name.to_string(),
                format!("({}){}", r.parameters.concat(), r.return_type),
            )
        };
        let cid = self.load_class(&class_desc)?;
        let mid = self.find_method(cid, &name, &sig, kind)?;
        if kind != InvokeKind::Virtual && kind != InvokeKind::Interface {
            self.dexs[dex as usize].method_cache[method_idx as usize] = Some(mid);
        }
        Ok(mid)
    }

    pub fn find_method(
        &mut self,
        class: ClassId,
        name: &str,
        sig: &str,
        kind: InvokeKind,
    ) -> VmResult<MethodId> {
        let key = (name.to_string(), sig.to_string());
        // Direct lookup in the class's declared maps.
        if let Some(&m) = self.classes[class as usize].direct_map.get(&key) {
            return Ok(m);
        }
        if kind == InvokeKind::Virtual || kind == InvokeKind::Interface || kind == InvokeKind::Super {
            if let Some(&m) = self.classes[class as usize].virtual_map.get(&key) {
                return Ok(m);
            }
        }
        // Inherited.
        if let Some(sup) = self.classes[class as usize].super_class {
            return self.find_method(sup, name, sig, kind);
        }
        Err(VmError::UnsupportedApi {
            api: format!("{}.{}{}", self.class_desc(class), name, sig),
            at: String::new(),
        })
    }

    /// Dynamic dispatch used by invoke-virtual / invoke-interface.
    pub fn dispatch(&mut self, receiver: Handle, name: &str, sig: &str) -> VmResult<MethodId> {
        let class = self
            .class_of_object(receiver)
            .or_else(|| self.array_class(receiver))
            .ok_or_else(|| VmError::Internal(format!("no class for receiver {receiver}")))?;
        let key = (name.to_string(), sig.to_string());
        if let Some(&m) = self.classes[class as usize].virtual_map.get(&key) {
            return Ok(m);
        }
        if let Some(sup) = self.classes[class as usize].super_class {
            return self.dispatch_in(sup, name, sig);
        }
        Err(VmError::UnsupportedApi {
            api: format!("{}.{name}{sig}", self.class_desc(class)),
            at: String::new(),
        })
    }

    fn dispatch_in(&mut self, class: ClassId, name: &str, sig: &str) -> VmResult<MethodId> {
        let key = (name.to_string(), sig.to_string());
        if let Some(&m) = self.classes[class as usize].virtual_map.get(&key) {
            return Ok(m);
        }
        if let Some(sup) = self.classes[class as usize].super_class {
            return self.dispatch_in(sup, name, sig);
        }
        Err(VmError::UnsupportedApi {
            api: format!("{}.{name}{sig}", self.class_desc(class)),
            at: String::new(),
        })
    }

    /// Resolve the class for an array handle by descriptor.
    pub fn array_class(&mut self, h: Handle) -> Option<ClassId> {
        let desc = match self.heap.get(h)? {
            Object::Array(a) => format!("[{}", a.elem_desc),
            _ => return None,
        };
        self.class_by_desc.get(&desc).copied()
    }

    pub fn ensure_array_class(&mut self, h: Handle) -> VmResult<ClassId> {
        if let Some(c) = self.array_class(h) {
            return Ok(c);
        }
        let desc = match self.heap.get(h) {
            Some(Object::Array(a)) => format!("[{}", a.elem_desc),
            _ => return Err(VmError::Internal("ensure_array_class on non-array".into())),
        };
        self.load_class(&desc)
    }

    // ---- exception helpers ----------------------------------------------

    pub fn new_throwable(&mut self, desc: &str, message: Option<&str>) -> VmResult<Handle> {
        let cid = self.load_class(desc)?;
        let h = self.builtin_instance(cid)?;
        self.native_roots.push(h);
        let result = (|| -> VmResult<()> {
            if let Some(msg) = message {
                let mh = self.new_string(msg)?;
                self.native_roots.push(mh);
                self.set_field_by_name(cid, h, "detailMessage", mh)?;
                self.native_roots.pop();
            }
            // Real JVMs fill the stack trace at construction time. VM-thrown
            // exceptions bypass the Java `Throwable` constructor, so
            // `nativeFillInStackTrace` never runs and `getStackTrace()` would
            // return an empty array. Populate `backtrace` here, while `h` is
            // rooted (the token allocation can trigger a GC).
            let thread = self.current_thread;
            if thread < self.threads.len() && !self.threads[thread].frames.is_empty() {
                let bt = crate::natives::lang::capture_backtrace(self, thread)?;
                self.native_roots.push(bt);
                self.set_instance_field_by_name(h, "backtrace", bt);
                self.native_roots.pop();
            }
            Ok(())
        })();
        self.native_roots.pop();
        result?;
        Ok(h)
    }

    /// Read an instance field by name, walking the superclass chain.
    pub fn get_instance_field_by_name(&self, obj: Handle, name: &str) -> Option<u32> {
        let class = self.class_of_object(obj)?;
        let mut c = Some(class);
        while let Some(cur) = c {
            if let Some(f) = self.classes[cur as usize]
                .declared_instance_fields
                .iter()
                .find(|f| f.name == name)
            {
                let off = f.offset as usize;
                return match self.heap.get(obj) {
                    Some(Object::Instance { fields, .. }) => fields.get(off).copied(),
                    Some(Object::String(u)) => match name {
                        "count" => Some((u.len() as u32) << 1),
                        "hash" => Some(0),
                        _ => None,
                    },
                    _ => None,
                };
            }
            c = self.classes[cur as usize].super_class;
        }
        None
    }

    /// Write an instance field by name (superclass chain included).
    pub fn set_instance_field_by_name(&mut self, obj: Handle, name: &str, value: u32) {
        if let Some(class) = self.class_of_object(obj) {
            let mut c = Some(class);
            while let Some(cur) = c {
                let found = self.classes[cur as usize]
                    .declared_instance_fields
                    .iter()
                    .find(|f| f.name == name)
                    .map(|f| f.offset as usize);
                if let Some(off) = found {
                    if let Some(Object::Instance { fields, .. }) = self.heap.get_mut(obj) {
                        if off < fields.len() {
                            fields[off] = value;
                        }
                    }
                    return;
                }
                c = self.classes[cur as usize].super_class;
            }
        }
    }

    fn set_field_by_name(
        &mut self,
        class: ClassId,
        obj: Handle,
        name: &str,
        value: Handle,
    ) -> VmResult<()> {
        let mut c = Some(class);
        while let Some(cur) = c {
            let f = self.classes[cur as usize]
                .declared_instance_fields
                .iter()
                .find(|f| f.name == name)
                .cloned();
            if let Some(f) = f {
                if let Some(Object::Instance { fields, .. }) = self.heap.get_mut(obj) {
                    fields[f.offset as usize] = value;
                    return Ok(());
                }
            }
            c = self.classes[cur as usize].super_class;
        }
        Ok(())
    }

    pub fn throw_new(&mut self, desc: &str, message: Option<&str>) -> VmError {
        crate::profile::record_exception();
        let thread = self.current_thread;
        match self.new_throwable(desc, message) {
            Ok(h) => {
                // Real JVMs fill in the stack at construction. Capture it here
                // for VM-thrown exceptions (natives and interpreter help paths)
                // so a `throw_new` NPE is diagnosable, not a frame-less cause.
                if thread < self.threads.len() && !self.threads[thread].frames.is_empty() {
                    self.capture_stack(h, thread);
                }
                VmError::JavaException(h)
            }
            Err(_) => VmError::JavaException(self.oom_object),
        }
    }

    pub fn capture_stack(&mut self, throwable: Handle, thread: usize) {
        // Real JVMs capture the stack once, at construction. An exception that
        // is caught and re-thrown (`athrow`) must keep its original frames, so
        // never overwrite an existing capture with a shallower post-unwind
        // stack. VM-thrown exceptions already capture in `throw_new`.
        if self.throwable_stacks.contains_key(&throwable) {
            return;
        }
        let mut entries = Vec::new();
        if let Some(t) = self.threads.get(thread) {
            for f in t.frames.iter().rev() {
                let class = self.class_desc_of_id(f.class);
                let method = self
                    .methods
                    .get(f.method as usize)
                    .map(|m| m.name.clone())
                    .unwrap_or_default();
                let line = match &self.methods[f.method as usize].imp {
                    MethodImpl::Dex { code: Some(code), .. } => code.line_for_pc(f.pc),
                    _ => 0,
                };
                entries.push(StackEntry { class, method, line });
            }
        }
        self.throwable_stacks.insert(throwable, entries);
    }

    pub fn class_desc_of_id(&self, id: ClassId) -> String {
        let desc = &self.classes[id as usize].descriptor;
        descriptor_to_dotted(desc)
    }

    // ---- public entry ----------------------------------------------------

    pub fn run(&mut self, main_class: &str, args: &[String]) -> RunOutcome {
        self.run_with(main_class, args, false)
    }

    /// [`Vm::run`] with deterministic virtual-time looper pumping: when every
    /// green thread is parked on a future message deadline the guest clock is
    /// advanced to it instead of sleeping. Used by tests and by callers that
    /// want future-dated framework work to run without real-time waits.
    pub fn run_virtual(&mut self, main_class: &str, args: &[String]) -> RunOutcome {
        self.run_with(main_class, args, true)
    }

    fn run_with(&mut self, main_class: &str, args: &[String], virtual_time: bool) -> RunOutcome {
        match self.run_program(main_class, args, virtual_time) {
            Ok(mut outcome) => {
                outcome.metrics = self.metrics_block();
                outcome
            }
            Err(e) => {
                let mut m = self.metrics_block();
                m.classes_loaded = self.classes.len();
                RunOutcome {
                    exit_code: 70,
                    stdout: String::from_utf8_lossy(&self.out_buf).into_owned(),
                    stderr: format!("acos-vm error: {}: {}\n", e.code(), e),
                    uncaught: None,
                    metrics: m,
                    vm_error: Some(e),
                }
            }
        }
    }

    fn run_program(
        &mut self,
        main_class: &str,
        args: &[String],
        virtual_time: bool,
    ) -> VmResult<RunOutcome> {
        // Arm the watchdog for the actual run (boot parsing happens in
        // `Vm::new`); the host RSS ceiling is already live.
        self.host.arm_wall();
        let desc = format!("L{};", main_class.replace('.', "/"));
        let cid = self.load_class(&desc)?;
        let main_sig = "([Ljava/lang/String;)V";
        let mid = self.find_method(cid, "main", main_sig, InvokeKind::Static)?;

        // Main thread + thread object.
        let thread_obj = self.make_thread("main", NULL)?;
        self.threads.clear();
        self.threads.push(ThreadRT::new(thread_obj));
        self.current_thread = 0;

        // Build the args String[].
        let arr = self.alloc(Object::Array(ArrayObj {
            elem_desc: "Ljava/lang/String;".to_string(),
            data: crate::heap::ArrayData::Ref(vec![NULL; args.len()]),
        }))?;
        self.native_roots.push(arr);
        for (i, a) in args.iter().enumerate() {
            let h = self.new_string(a)?;
            if let Some(Object::Array(ArrayObj {
                data: crate::heap::ArrayData::Ref(v),
                ..
            })) = self.heap.get_mut(arr)
            {
                v[i] = h;
            }
        }
        // Initialize the class, then push main.
        self.initialize_class(cid)?;
        crate::interp::push_frame_values(self, mid, NULL, &[arr as u64])?;
        self.native_roots.pop();

        // The interpreter returns when no green thread is runnable. If the
        // remaining threads are parked in a looper, block the host on the
        // process epoll set and then resume them (or, in virtual time, advance
        // the guest clock to their deadline). `pump` returns false when there
        // is nothing left to wait for.
        self.run_scheduler(virtual_time)?;

        // Collect an uncaught exception if any.
        if self.pending_exception != NULL {
            let h = self.pending_exception;
            self.pending_exception = NULL;
            let uncaught = self.describe_uncaught(h);
            let mut stderr = String::new();
            stderr.push_str(&format!("Exception in thread \"main\" {}\n", uncaught.1));
            for line in &uncaught.2 {
                stderr.push_str(line);
                stderr.push('\n');
            }
            return Ok(RunOutcome {
                exit_code: 1,
                stdout: String::from_utf8_lossy(&self.out_buf).into_owned(),
                stderr,
                uncaught: Some(uncaught.0),
                metrics: Metrics::default(),
                vm_error: None,
            });
        }

        Ok(RunOutcome {
            exit_code: self.exit_code(),
            stdout: String::from_utf8_lossy(&self.out_buf).into_owned(),
            stderr: String::from_utf8_lossy(&self.err_buf).into_owned(),
            uncaught: None,
            metrics: Metrics::default(),
            vm_error: None,
        })
    }

    /// Run the interpreter until no green thread is runnable, pumping parked
    /// loopers back to life.
    ///
    /// With `virtual_time == false` this is the real-time scheduler used by
    /// [`Vm::run`]: a fully parked VM blocks the host on the process `epoll`
    /// set until a wake or the earliest message deadline.
    ///
    /// With `virtual_time == true` an all-idle VM instead advances the guest
    /// monotonic clock to the next due message and wakes that looper, so
    /// future-dated framework work (a `Choreographer` `doFrame` at the next
    /// vsync) runs deterministically without a real-time sleep. The iteration
    /// count is bounded so a self-rescheduling message cannot spin forever;
    /// the host wall watchdog (`vm.host`) still applies on real time.
    pub fn run_scheduler(&mut self, virtual_time: bool) -> VmResult<()> {
        /// Upper bound on pump turns per scheduler entry. A normal frame is a
        /// handful of turns; a repeating timer is capped here and the host
        /// wall budget stops it after that.
        const MAX_PUMP_TURNS: u32 = 100_000;
        self.run_scheduler_max(virtual_time, MAX_PUMP_TURNS)
    }

    /// [`Vm::run_scheduler`] with an explicit turn budget.
    ///
    /// An interaction pump must not chase a repeating framework timer (for
    /// example the `EditText` cursor blink) for 100k virtual-time turns: the
    /// event and its immediate repaint are handled in the first turns, and the
    /// rest is a periodic timer that never goes idle. `max_turns` bounds that.
    pub fn run_scheduler_max(&mut self, virtual_time: bool, max_turns: u32) -> VmResult<()> {
        let mut turns: u32 = 0;
        loop {
            crate::interp::run(self)?;
            // Host safe point: a broker (the JNI crate) may have queued work
            // from native OS threads. Service it here, on the owner thread,
            // while no Rust borrow of the VM is live elsewhere.
            crate::natives::safe_point::pump(self);
            if self.pending_exception != NULL || self.exit_request.is_some() {
                break;
            }
            self.host.check()?;
            let progressed = if virtual_time {
                crate::natives::looper::pump_virtual(self)?
            } else {
                crate::natives::looper::pump(self)?
            };
            if !progressed {
                break;
            }
            turns += 1;
            if turns >= max_turns {
                break;
            }
        }
        Ok(())
    }

    /// Exit code requested by `System.exit`, if any.
    pub fn exit_code(&self) -> i32 {
        self.exit_request.unwrap_or(0)
    }

    fn describe_uncaught(&mut self, h: Handle) -> (Uncaught, String, Vec<String>) {
        let class = self
            .class_of_object(h)
            .map(|c| self.class_desc_of_id(c))
            .unwrap_or_else(|| "java.lang.Throwable".to_string());
        let message = self.throwable_message(h);
        let mut stack = Vec::new();
        if let Some(entries) = self.throwable_stacks.get(&h) {
            for e in entries {
                stack.push(format!("\tat {}.{}({})", e.class, e.method, e.line));
            }
        }
        let display = match &message {
            Some(m) => format!("{class}: {m}"),
            None => class.clone(),
        };
        let uncaught = Uncaught {
            class: class.clone(),
            message: message.clone(),
            stack: stack.clone(),
        };
        (uncaught, display, stack)
    }

    pub fn throwable_message(&self, h: Handle) -> Option<String> {
        let class = self.class_of_object(h)?;
        let mut c = Some(class);
        while let Some(cur) = c {
            if let Some(f) = self.classes[cur as usize]
                .declared_instance_fields
                .iter()
                .find(|f| f.name == "detailMessage")
            {
                if let Some(Object::Instance { fields, .. }) = self.heap.get(h) {
                    let mh = fields[f.offset as usize];
                    if mh == NULL {
                        return None;
                    }
                    return self.string_to_rust(mh);
                }
            }
            c = self.classes[cur as usize].super_class;
        }
        None
    }

    /// The direct cause of `h`, if any. API 34's
    /// `ExceptionInInitializerError` overrides `getCause()` to read its own
    /// `exception` field; every other `Throwable` uses the inherited `cause`
    /// field. Diagnostics follow this chain so a wrapper like
    /// `ExceptionInInitializerError` never hides the real `<clinit>` failure.
    pub fn throwable_cause(&self, h: Handle) -> Option<Handle> {
        let class = self.class_of_object(h)?;
        let field = if self.class_desc_of_id(class) == "java.lang.ExceptionInInitializerError" {
            "exception"
        } else {
            "cause"
        };
        match self.get_instance_field_by_name(h, field) {
            // `Throwable` initialises `cause` to `this`; `getCause()` treats
            // that as "no cause". Mirror it so the chain terminates.
            Some(v) if v != NULL && v != h => Some(v),
            _ => None,
        }
    }

    pub fn make_thread(&mut self, name: &str, target: Handle) -> VmResult<Handle> {
        let name_h = self.intern_str(name)?;
        let id = self.next_thread_id;
        self.next_thread_id += 1;
        if self.corelib == CoreLib::Real {
            let obj = self.alloc_instance(self.c_thread)?;
            // Any further allocation (including the `blockerLock` below) may run
            // a GC; keep the in-construction thread reachable until it is
            // published into `threads`.
            self.native_roots.push(obj);
            self.set_instance_field_by_name(obj, "name", name_h);
            self.set_instance_field_by_name(obj, "priority", 5);
            self.set_instance_field_by_name(obj, "daemon", 0);
            self.set_instance_field_by_name(obj, "tid", id);
            // `Thread.blockerLock` is a field initializer (`new Object()`) that
            // a VM-constructed green thread never runs. `Thread.blockedOn`
            // monitor-enters it (`AbstractInterruptibleChannel.begin`), so
            // without it every FileChannel operation NPEs.
            if self.c_object != 0 {
                let lock = self.alloc_instance(self.c_object)?;
                self.set_instance_field_by_name(obj, "blockerLock", lock);
            }
            if self.main_thread_group != NULL {
                self.set_instance_field_by_name(obj, "group", self.main_thread_group);
            }
            if target != NULL {
                self.set_instance_field_by_name(obj, "target", target);
            }
            self.native_roots.pop();
            return Ok(obj);
        }
        let obj = self.alloc(Object::Builtin {
            class: self.c_thread,
            data: BuiltinData::Thread(crate::heap::ThreadData {
                id,
                name: name_h,
                target,
                alive: true,
                daemon: false,
            }),
        })?;
        Ok(obj)
    }

    pub fn metrics_block(&self) -> Metrics {
        Metrics {
            instructions: self.metrics.instructions,
            invokes: self.metrics.invokes,
            allocations: self.heap.allocations,
            gc_count: self.heap.gc_count,
            gc_ms: self.heap.gc_nanos as u128 / 1_000_000,
            heap_peak_bytes: self.heap.peak_bytes,
            classes_loaded: self.classes.len(),
            clinit_count: self.metrics.clinit_count,
        }
    }

    /// Record that a class initialiser is about to run.
    pub fn note_clinit(&mut self) {
        self.metrics.clinit_count += 1;
    }
}

/// Which resolution rule an `invoke-*` uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InvokeKind {
    Virtual,
    Super,
    Direct,
    Static,
    Interface,
}

/// Render a descriptor as a dotted binary name (`Ljava/lang/String;` ->
/// `java.lang.String`).
pub fn descriptor_to_dotted(desc: &str) -> String {
    if let Some(rest) = desc.strip_prefix('L') {
        if let Some(inner) = rest.strip_suffix(';') {
            return inner.replace('/', ".");
        }
    }
    if desc.starts_with('[') {
        return array_descriptor_to_name(desc);
    }
    desc.to_string()
}

pub fn array_descriptor_to_name(desc: &str) -> String {
    let mut dims = 0;
    let mut rest = desc;
    while let Some(r) = rest.strip_prefix('[') {
        dims += 1;
        rest = r;
    }
    let base = match rest.as_bytes().first() {
        Some(b'Z') => "boolean".to_string(),
        Some(b'B') => "byte".to_string(),
        Some(b'C') => "char".to_string(),
        Some(b'S') => "short".to_string(),
        Some(b'I') => "int".to_string(),
        Some(b'J') => "long".to_string(),
        Some(b'F') => "float".to_string(),
        Some(b'D') => "double".to_string(),
        _ => {
            let dotted = descriptor_to_dotted(rest);
            let part = dotted.rsplit('.').next().unwrap_or(&dotted);
            match part.rfind('$') {
                Some(i) => part[i + 1..].to_string(),
                None => part.to_string(),
            }
        }
    };
    let mut s = base;
    for _ in 0..dims {
        s.push_str("[]");
    }
    s
}

/// Build a runtime array from a list of element handles.
pub fn make_ref_array(desc: &str, elements: Vec<Handle>) -> VmResult<Object> {
    Ok(Object::Array(ArrayObj {
        elem_desc: desc.to_string(),
        data: crate::heap::ArrayData::Ref(elements),
    }))
}

/// Validate an array component descriptor.
pub fn simple_array_object(elem: &str, len: usize) -> VmResult<Object> {
    Ok(Object::Array(ArrayObj {
        elem_desc: elem.to_string(),
        data: new_array_data(elem, len)?,
    }))
}

/// Parse a list of string descriptors (re-export for corelib).
pub fn parse_types(s: &str) -> VmResult<Vec<String>> {
    parse_type_list(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::heap::{ArrayData, ArrayObj, Object};

    fn ref_array() -> Object {
        Object::Array(ArrayObj {
            elem_desc: "Ljava/lang/Object;".to_string(),
            data: ArrayData::Ref(vec![NULL]),
        })
    }

    #[test]
    fn gc_collects_unreachable_cycles() {
        let mut vm = Vm::new(VmOptions::default()).unwrap();
        let a = vm.alloc(ref_array()).unwrap();
        let b = vm.alloc(ref_array()).unwrap();
        if let Some(Object::Array(ArrayObj {
            data: ArrayData::Ref(v),
            ..
        })) = vm.heap.get_mut(a)
        {
            v[0] = b;
        }
        if let Some(Object::Array(ArrayObj {
            data: ArrayData::Ref(v),
            ..
        })) = vm.heap.get_mut(b)
        {
            v[0] = a;
        }
        let before = vm.heap.live_bytes;
        vm.gc();
        assert!(!vm.heap.is_live(a), "cycle should be collected");
        assert!(!vm.heap.is_live(b), "cycle should be collected");
        assert!(vm.heap.live_bytes < before);
    }

    #[test]
    fn native_roots_survive_collection() {
        let mut vm = Vm::new(VmOptions::default()).unwrap();
        let h = vm.alloc(ref_array()).unwrap();
        vm.native_roots.push(h);
        let _junk = vm.alloc(ref_array()).unwrap();
        vm.gc();
        assert!(vm.heap.is_live(h), "native root must be traced");
        vm.native_roots.pop();
    }

    #[test]
    fn out_of_memory_throws_java_error() {
        let mut vm = Vm::new(VmOptions {
            heap_max_bytes: 64 * 1024,
            gc_stress: false,
            ..VmOptions::default()
        })
        .unwrap();
        let mut result = None;
        for _ in 0..100_000 {
            match vm.alloc(Object::String(vec![0u16; 128])) {
                Ok(h) => vm.native_roots.push(h),
                Err(e) => {
                    result = Some(e);
                    break;
                }
            }
        }
        match result {
            Some(VmError::JavaException(h)) => assert_eq!(h, vm.oom_object),
            other => panic!("expected OutOfMemoryError, got {other:?}"),
        }
    }

    #[test]
    fn unsupported_api_reports_exact_site() {
        let e = VmError::unsupported_api(
            "Ljava/util/concurrent/Executors;->newFixedThreadPool(I)Ljava/util/concurrent/ExecutorService;",
            "LMain;->main([Ljava/lang/String;)V pc=3",
        );
        assert_eq!(e.code(), "UNSUPPORTED_JAVA_API");
        assert!(e.to_string().contains("newFixedThreadPool"));
    }
}
