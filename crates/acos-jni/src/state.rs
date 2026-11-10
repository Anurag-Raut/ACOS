//! Per-runtime JNI state: loaded libraries, reference frames, method IDs and
//! the thread-local attachment used by the C function table.
//!
//! ACOS runs green threads inside one host thread, so a single thread-local
//! attachment is enough for the supported model; `AttachCurrentThread` returns
//! the same environment and is documented as such. The state is reference
//! counted so guest code may re-enter the VM (a native calls Java which calls
//! another native) without holding a `RefCell` borrow across the call.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::ffi::CString;
use std::path::PathBuf;
use std::rc::Rc;

use acos_native_loader::{LibraryHandle, Loader, LoaderConfig, MissingPolicy};
use acos_vm::{RunOutcome, Vm};

use crate::error::JniError;

/// JNI version reported by `GetVersion` and accepted by `JNI_OnLoad`.
pub const JNI_VERSION_1_6: i32 = 0x0001_0006;
/// JNI version 1.4 (accepted for compatibility with older libraries).
pub const JNI_VERSION_1_4: i32 = 0x0001_0004;
pub const JNI_OK: i32 = 0;
pub const JNI_ERR: i32 = -1;
pub const JNI_EDETACHED: i32 = -2;
pub const JNI_EVERSION: i32 = -3;

pub const JNI_FALSE: u8 = 0;
pub const JNI_TRUE: u8 = 1;

pub const JNI_INVALID_REF_TYPE: i32 = 0;
pub const JNI_LOCAL_REF_TYPE: i32 = 1;
pub const JNI_GLOBAL_REF_TYPE: i32 = 2;
pub const JNI_WEAK_GLOBAL_REF_TYPE: i32 = 3;

/// Runtime configuration.
#[derive(Clone)]
pub struct JniConfig {
    /// Directories searched for `DT_NEEDED` libraries and `System.loadLibrary`
    /// names (an extracted APK `lib/<abi>/` directory, for example).
    pub search_paths: Vec<PathBuf>,
    /// Optional APK whose `lib/<abi>/` entries satisfy `System.loadLibrary`.
    pub apk: Option<PathBuf>,
    /// Preferred ABI directory inside [`JniConfig::apk`].
    pub abi: String,
    /// Missing native-import policy for the ELF loader.
    pub missing_policy: MissingPolicy,
    /// Optional guest `__android_log_*` sink.
    pub log_sink: Option<std::sync::Arc<dyn acos_native_loader::LogSink>>,
}

impl std::fmt::Debug for JniConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JniConfig")
            .field("search_paths", &self.search_paths)
            .field("apk", &self.apk)
            .field("abi", &self.abi)
            .field("missing_policy", &self.missing_policy)
            .field("log_sink", &self.log_sink.is_some())
            .finish()
    }
}

impl Default for JniConfig {
    fn default() -> Self {
        JniConfig {
            search_paths: Vec::new(),
            apk: None,
            abi: "x86_64".to_string(),
            missing_policy: MissingPolicy::Strict,
            log_sink: None,
        }
    }
}

impl JniConfig {
    pub fn with_search_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.search_paths.push(path.into());
        self
    }

    pub fn with_apk(mut self, path: impl Into<PathBuf>) -> Self {
        self.apk = Some(path.into());
        self
    }
}

/// A loaded guest shared object.
#[derive(Clone)]
pub struct LoadedLibrary {
    pub name: String,
    pub path: Option<String>,
    pub handle: LibraryHandle,
}

impl std::fmt::Debug for LoadedLibrary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoadedLibrary")
            .field("name", &self.name)
            .field("path", &self.path)
            .finish()
    }
}

/// A resolved `jmethodID`. Boxed so its address is stable for guest code.
#[derive(Clone, Debug)]
pub struct MethodInfo {
    /// Declaring class descriptor (`L...;`).
    pub class: String,
    pub name: String,
    pub sig: String,
    pub is_static: bool,
    pub param_descs: Vec<String>,
    pub ret_desc: String,
    /// One code byte per parameter for the C varargs shim.
    pub arg_codes: CString,
}

/// A resolved `jfieldID`.
#[derive(Clone, Debug)]
pub struct FieldInfo {
    pub class: String,
    pub name: String,
    pub desc: String,
    pub is_static: bool,
}

/// A buffered `Get*ArrayElements` / `GetString*Chars` allocation.
pub enum Buffer {
    Utf8(Box<[u8]>),
    Utf16(Box<[u16]>),
    Bool(Vec<u8>),
    Byte(Vec<i8>),
    Char(Vec<u16>),
    Short(Vec<i16>),
    Int(Vec<i32>),
    Long(Vec<i64>),
    Float(Vec<f32>),
    Double(Vec<f64>),
}

impl std::fmt::Debug for Buffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Buffer({})", self.kind())
    }
}

impl Buffer {
    pub fn kind(&self) -> &'static str {
        match self {
            Buffer::Utf8(_) => "utf8",
            Buffer::Utf16(_) => "utf16",
            Buffer::Bool(_) => "boolean[]",
            Buffer::Byte(_) => "byte[]",
            Buffer::Char(_) => "char[]",
            Buffer::Short(_) => "short[]",
            Buffer::Int(_) => "int[]",
            Buffer::Long(_) => "long[]",
            Buffer::Float(_) => "float[]",
            Buffer::Double(_) => "double[]",
        }
    }
}

/// A JNI local frame.
#[derive(Default, Debug)]
pub struct LocalFrame {
    pub refs: Vec<u32>,
    /// `PushLocalFrame` capacity. `0` means the frame is unbounded (the base
    /// frame and frames pushed by `PushLocalFrame(0)`); a positive value makes
    /// the frame overflow once that many live locals exist.
    pub cap: usize,
}

/// Cap on a single `PushLocalFrame`/`EnsureLocalCapacity` request. Larger
/// requests are rejected with `OutOfMemoryError`, matching the JNI contract
/// for an unsatisfiable capacity.
pub const MAX_LOCAL_CAPACITY: i32 = 1_000_000;

/// A pending Java exception plus whether this module added the GC root.
#[derive(Debug)]
pub struct PendingException {
    pub handle: u32,
    pub rooted: bool,
}

/// Per-VM-thread JNI state.
#[derive(Default, Debug)]
pub struct ThreadJni {
    pub frames: Vec<LocalFrame>,
    pub pending: Option<PendingException>,
    pub error: Option<JniError>,
    pub buffers: HashMap<usize, Buffer>,
    /// References returned to Java by the previous native call. The Java frame
    /// roots them; this keeps them alive across the hand-off without leaking.
    pub return_roots: Vec<u32>,
    /// Set when a JNI local exceeded the capacity of its `PushLocalFrame`.
    pub frame_overflow: bool,
}

impl ThreadJni {
    fn ensure_frame(&mut self) {
        if self.frames.is_empty() {
            self.frames.push(LocalFrame::default());
        }
    }
}

/// Shared runtime state.
pub struct State {
    pub loader: Loader,
    pub config: JniConfig,
    pub env_slot: usize,
    pub vm_slot: usize,
    pub libs: Vec<LoadedLibrary>,
    pub loaded: HashMap<String, String>,
    pub natives: HashMap<(String, String, String), usize>,
    pub methods: Vec<Box<MethodInfo>>,
    pub fields: Vec<Box<FieldInfo>>,
    pub globals: Vec<u32>,
    /// Weak global references. They are *not* GC roots; a referent may be
    /// collected while one is held, which clears the reference.
    pub weak_globals: Vec<u32>,
    /// `NewDirectByteBuffer` buffers: `jobject` handle -> (region address,
    /// capacity in bytes).
    pub direct_buffers: HashMap<u32, (i64, i64)>,
    pub threads: HashMap<usize, ThreadJni>,
    /// DEX `native` declarations discovered by [`JniRuntime::scan_natives`].
    pub dex_natives: HashSet<(String, String, String)>,
    /// `(class, name) -> number of overloads` for JNI symbol mangling.
    pub overloads: HashMap<(String, String), usize>,
    /// Resolved guest symbol cache.
    pub resolved: HashMap<(String, String, String), usize>,
    /// The last VM thread that entered a JNI slot (for diagnostics).
    pub last_thread: usize,
}

impl std::fmt::Debug for State {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JniState")
            .field("libs", &self.libs)
            .field("natives", &self.natives.len())
            .field("methods", &self.methods.len())
            .field("fields", &self.fields.len())
            .field("globals", &self.globals.len())
            .field("weak_globals", &self.weak_globals.len())
            .field("direct_buffers", &self.direct_buffers.len())
            .field("dex_natives", &self.dex_natives.len())
            .finish()
    }
}

impl State {
    pub fn thread_mut(&mut self, t: usize) -> &mut ThreadJni {
        self.threads.entry(t).or_default()
    }

    pub fn thread(&self, t: usize) -> Option<&ThreadJni> {
        self.threads.get(&t)
    }

    pub fn find_library(&self, name: &str) -> Option<&LoadedLibrary> {
        self.libs.iter().find(|l| l.name == name)
    }
}

// --- thread-local attachment ------------------------------------------------

thread_local! {
    static RUNTIME: RefCell<Option<Rc<RefCell<State>>>> = const { RefCell::new(None) };
    static VM_PTR: Cell<*mut Vm> = const { Cell::new(std::ptr::null_mut()) };
}

pub(crate) fn runtime_state() -> Option<Rc<RefCell<State>>> {
    RUNTIME.with(|r| r.borrow().clone())
}

pub(crate) fn current_vm() -> *mut Vm {
    VM_PTR.with(Cell::get)
}

/// Install the current VM pointer for JNI calls made on this host thread.
///
/// # Safety
///
/// `vm` must stay valid until the next call to `set_vm`/`clear_vm`. The runtime
/// sets it from `&mut Vm` for the duration of the owning operation.
pub(crate) unsafe fn set_vm(vm: *mut Vm) {
    VM_PTR.with(|c| c.set(vm));
}

pub(crate) fn clear_vm() {
    VM_PTR.with(|c| c.set(std::ptr::null_mut()));
}

// --- environment / JavaVM slots --------------------------------------------

/// C-visible `JNIEnv`: a pointer to a slot holding the function-table pointer.
#[repr(C)]
#[derive(Debug)]
pub struct EnvSlot {
    pub table: *const usize,
    pub magic: u64,
}

/// C-visible `JavaVM`: a pointer to a slot holding the invoke-table pointer.
#[repr(C)]
#[derive(Debug)]
pub struct VmSlot {
    pub table: *const usize,
    pub magic: u64,
}

const ENV_MAGIC: u64 = 0x4143_4f53_4a4e_4931; // "ACOSJNI1"
const VM_MAGIC: u64 = 0x4143_4f53_4a56_4d31; // "ACOSJVM1"

/// The JNI runtime. One per VM process (ACOS is in-process).
pub struct JniRuntime {
    pub(crate) inner: Rc<RefCell<State>>,
    env: Box<EnvSlot>,
    vm: Box<VmSlot>,
}

impl std::fmt::Debug for JniRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JniRuntime")
            .field("state", &self.inner.borrow())
            .finish()
    }
}

impl Drop for JniRuntime {
    fn drop(&mut self) {
        // If this runtime is still the thread attachment, drop it before the
        // state/env storage goes away so later JNI calls see "no VM" instead of
        // a dangling pointer.
        let current = runtime_state()
            .map(|r| Rc::ptr_eq(&r, &self.inner))
            .unwrap_or(false);
        if current {
            clear_vm();
            RUNTIME.with(|r| *r.borrow_mut() = None);
        }
    }
}

impl JniRuntime {
    /// Create a runtime with the given configuration.
    pub fn new(config: JniConfig) -> JniRuntime {
        let loader = Loader::new(LoaderConfig {
            search_paths: config.search_paths.clone(),
            missing_policy: config.missing_policy,
            log_sink: config.log_sink.clone(),
            system_properties: HashMap::new(),
        });
        let env = Box::new(EnvSlot {
            table: crate::table::native_table_ptr(),
            magic: ENV_MAGIC,
        });
        let vm = Box::new(VmSlot {
            table: crate::table::invoke_table_ptr(),
            magic: VM_MAGIC,
        });
        let state = State {
            loader,
            config,
            env_slot: &*env as *const EnvSlot as usize,
            vm_slot: &*vm as *const VmSlot as usize,
            libs: Vec::new(),
            loaded: HashMap::new(),
            natives: HashMap::new(),
            methods: Vec::new(),
            fields: Vec::new(),
            globals: Vec::new(),
            weak_globals: Vec::new(),
            direct_buffers: HashMap::new(),
            threads: HashMap::new(),
            dex_natives: HashSet::new(),
            overloads: HashMap::new(),
            resolved: HashMap::new(),
            last_thread: 0,
        };
        JniRuntime {
            inner: Rc::new(RefCell::new(state)),
            env,
            vm,
        }
    }

    /// `JNIEnv*` handed to guest native code.
    pub fn env(&self) -> *mut std::os::raw::c_void {
        &*self.env as *const EnvSlot as *mut std::os::raw::c_void
    }

    /// `JavaVM*` handed to `JNI_OnLoad`.
    pub fn java_vm(&self) -> *mut std::os::raw::c_void {
        &*self.vm as *const VmSlot as *mut std::os::raw::c_void
    }

    /// Attach the runtime to `vm`: install the JNI host, register the
    /// library-loading natives, and bind every non-boot DEX `native` method to
    /// the JNI dispatcher.
    pub fn attach(&self, vm: &mut Vm) -> Result<(), JniError> {
        RUNTIME.with(|r| *r.borrow_mut() = Some(self.inner.clone()));
        // Do not retain a raw `Vm` pointer here: the caller may move the value
        // after `attach` returns. Native dispatch and [`JniRuntime::run`] bind
        // the pointer at the final address; direct table users call
        // [`JniRuntime::bind`] once the VM is in place.
        clear_vm();
        acos_vm::natives::jni_glue::set_host(acos_vm::natives::jni_glue::Host {
            load_library: crate::load::host_load_library,
            call_native: crate::dispatch::host_call_native,
        });
        acos_vm::natives::jni_glue::install(vm).map_err(|e| JniError::NativeFault {
            message: e.to_string(),
        })?;
        self.scan_natives(vm);
        // A base local frame exists for direct table use outside a native call.
        let t = vm.current_thread;
        self.inner.borrow_mut().thread_mut(t).ensure_frame();
        Ok(())
    }

    /// Bind the runtime's JNI table to the final address of `vm`.
    ///
    /// `attach` deliberately does not keep a raw VM pointer (the value may be
    /// moved out of the calling stack frame). Call this once after moving the VM
    /// into its owning struct, or use [`JniRuntime::run`] which binds for you.
    pub fn bind(&self, vm: &mut Vm) {
        // SAFETY: `vm` is borrowed here; the pointer is used by JNI calls made
        // on this host thread until the runtime is dropped or re-bound.
        unsafe { set_vm(vm as *mut Vm) };
        let t = vm.current_thread;
        self.inner.borrow_mut().thread_mut(t).ensure_frame();
    }

    /// Detach the runtime from the calling host thread.
    pub fn detach(&self) {
        clear_vm();
        acos_vm::natives::jni_glue::clear_host();
        RUNTIME.with(|r| *r.borrow_mut() = None);
    }

    /// Discover DEX `native` declarations and install the JNI dispatcher for
    /// app classes. Boot-classpath natives keep their Rust implementations.
    pub fn scan_natives(&self, vm: &mut Vm) {
        let boot: HashSet<usize> = vm.boot_dexs.iter().copied().collect();
        let mut found: Vec<(String, String, String)> = Vec::new();
        for (idx, dex) in vm.dexs.iter().enumerate() {
            if boot.contains(&idx) {
                continue;
            }
            let file = dex.file.clone();
            for def in file.class_defs() {
                let Ok(cdesc) = file.class_descriptor(def).map(str::to_string) else {
                    continue;
                };
                let Ok(Some(cd)) = file.class_data(def) else {
                    continue;
                };
                for m in cd.direct_methods.iter().chain(cd.virtual_methods.iter()) {
                    if m.access_flags & acos_dex::access::NATIVE == 0 {
                        continue;
                    }
                    let Ok(mr) = file.method_ref(m.method_idx) else {
                        continue;
                    };
                    let params: Vec<String> = mr.parameters.iter().map(|p| p.to_string()).collect();
                    let sig = format!("({}){}", params.concat(), mr.return_type);
                    found.push((cdesc.clone(), mr.name.to_string(), sig));
                }
            }
        }

        {
            let mut st = self.inner.borrow_mut();
            for (c, n, s) in &found {
                if st.dex_natives.insert((c.clone(), n.clone(), s.clone())) {
                    *st.overloads.entry((c.clone(), n.clone())).or_insert(0) += 1;
                }
            }
        }

        for (class, name, sig) in &found {
            let key = (class.clone(), name.clone(), sig.clone());
            if vm.natives.contains_key(&key) {
                continue; // a Rust core-library implementation wins
            }
            vm.register_native(class, name, sig, acos_vm::natives::jni_glue::bridge());
        }

        // Bind declarations in classes that were already loaded before attach.
        for mid in 0..vm.methods.len() {
            if !vm.methods[mid].is_native() {
                continue;
            }
            let class = vm.class_desc(vm.methods[mid].class).to_string();
            let key = (
                class,
                vm.methods[mid].name.clone(),
                vm.methods[mid].sig.clone(),
            );
            let st = self.inner.borrow();
            if st.dex_natives.contains(&key)
                && !matches!(vm.methods[mid].imp, acos_vm::class::MethodImpl::Native(_))
            {
                drop(st);
                vm.methods[mid].imp =
                    acos_vm::class::MethodImpl::Native(acos_vm::natives::jni_glue::bridge());
            }
        }
    }

    /// Convenience: attach, bind and run a DEX main class.
    pub fn run(&self, vm: &mut Vm, main: &str, args: &[String]) -> Result<RunOutcome, JniError> {
        self.attach(vm)?;
        self.bind(vm);
        Ok(vm.run(main, args))
    }

    /// Take (and clear) the structured JNI error recorded on this host thread.
    pub fn take_error(&self) -> Option<JniError> {
        let mut st = self.inner.borrow_mut();
        let thread = st.last_thread;
        st.threads.get_mut(&thread).and_then(|s| s.error.take())
    }

    /// Names and paths of loaded libraries (test/reporting aid).
    pub fn libraries(&self) -> Vec<(String, Option<String>)> {
        self.inner
            .borrow()
            .libs
            .iter()
            .map(|l| (l.name.clone(), l.path.clone()))
            .collect()
    }

    /// Registered `RegisterNatives` entries (test/reporting aid).
    pub fn registered_natives(&self) -> Vec<(String, String, String, usize)> {
        self.inner
            .borrow()
            .natives
            .iter()
            .map(|((c, n, s), p)| (c.clone(), n.clone(), s.clone(), *p))
            .collect()
    }
}
