//! `dl*` shims backed by this loader.
//!
//! `dl_iterate_phdr` is the critical one for C++ unwinding: the guest's static
//! libunwind walks it to locate `PT_GNU_EH_FRAME` for each loaded object. We
//! report every mapped guest library with its real program headers and load
//! bias.

use std::collections::HashMap;
use std::os::raw::{c_char, c_int, c_void};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use super::Shim;
use crate::elf::Phdr;
use crate::loader::{
    current_loader, dl_handle_lookup, dl_handle_remove, is_shim_system_lib,
    register_dl_handle, LibraryHandle, LoaderInner, NativeLibrary, OBJECTS,
};

/// Pseudo-`dlopen` handles for the bionic-provided system libraries
/// (`libandroid.so`, `liblog.so`, ...). They have no backing ELF; `dlsym` on
/// them resolves from the process-wide bionic shim table via [`super::resolve`].
///
/// Impeller's `GetProcTable` (the Flutter engine's `AChoreographer` branch)
/// opens `libandroid.so` and looks up `AChoreographer_*`; without this the
/// engine concludes the native vsync path is unavailable and falls back to the
/// Java delegate. The handle id sets bit 62 so it never collides with a real
/// loader handle (`next_dl_handle` counts from 0).
fn pseudo_handles() -> &'static Mutex<HashMap<usize, String>> {
    static P: OnceLock<Mutex<HashMap<usize, String>>> = OnceLock::new();
    P.get_or_init(|| Mutex::new(HashMap::new()))
}

const PSEUDO_HANDLE_BIT: usize = 1 << 62;
static NEXT_PSEUDO: AtomicUsize = AtomicUsize::new(1);

/// Matches glibc/bionic `struct dl_phdr_info`.
#[repr(C)]
pub struct DlPhdrInfo {
    pub dlpi_addr: u64,
    pub dlpi_name: *const c_char,
    pub dlpi_phdr: *const Phdr,
    pub dlpi_phnum: u16,
    pub dlpi_adds: u64,
    pub dlpi_subs: u64,
    pub dlpi_tls_modid: usize,
    pub dlpi_tls_data: *mut c_void,
}

/// `struct Dl_info`.
#[repr(C)]
pub struct DlInfo {
    pub dli_fname: *const c_char,
    pub dli_fbase: *mut c_void,
    pub dli_sname: *const c_char,
    pub dli_saddr: *mut c_void,
}

/// Callback type for `dl_iterate_phdr`.
pub type DlIterateCallback = unsafe extern "C" fn(*mut DlPhdrInfo, usize, *mut c_void) -> c_int;

/// `dl_iterate_phdr`.
///
/// # Safety
/// `cb` must be a valid callback; guest code calls it synchronously.
pub unsafe extern "C" fn dl_iterate_phdr(
    cb: Option<DlIterateCallback>,
    data: *mut c_void,
) -> c_int {
    let cb = match cb {
        Some(c) => c,
        None => return 0,
    };
    let objs: Vec<Arc<NativeLibrary>> = OBJECTS.lock().unwrap().clone();
    for lib in objs {
        let mut info = DlPhdrInfo {
            dlpi_addr: lib.bias() as u64,
            dlpi_name: lib.name_c.as_ptr(),
            dlpi_phdr: lib.phdrs_ptr(),
            dlpi_phnum: lib.phnum(),
            dlpi_adds: 0,
            dlpi_subs: 0,
            dlpi_tls_modid: lib.tls_modid.unwrap_or(0),
            dlpi_tls_data: std::ptr::null_mut(),
        };
        let rc = cb(&mut info, std::mem::size_of::<DlPhdrInfo>(), data);
        if rc != 0 {
            return rc;
        }
    }
    0
}

fn cstr(p: *const c_char) -> String {
    if p.is_null() {
        String::new()
    } else {
        // SAFETY: caller-provided NUL-terminated string.
        unsafe { std::ffi::CStr::from_ptr(p).to_string_lossy().into_owned() }
    }
}

/// `dlopen` (and `android_dlopen_ext`). Returns a non-zero handle id or 0.
///
/// # Safety
/// `filename` must be a valid NUL-terminated string (or NULL).
pub unsafe extern "C" fn dlopen(filename: *const c_char, _flags: c_int) -> *mut c_void {
    let inner = match current_loader() {
        Some(i) => i,
        None => return std::ptr::null_mut(),
    };
    let name = cstr(filename);
    // A bionic system library (`libandroid.so`, `liblog.so`, ...) has no APK
    // file: it is backed by the process-wide shim table. Hand out a pseudo
    // handle so the engine's `fml::NativeLibrary` lookups (`AChoreographer_*`)
    // succeed; `dlsym` resolves the symbol from the shim table.
    if is_shim_system_lib(&name) {
        let id = NEXT_PSEUDO.fetch_add(1, Ordering::SeqCst) | PSEUDO_HANDLE_BIT;
        pseudo_handles()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id, name);
        return id as *mut c_void;
    }
    let loader = crate::loader::Loader::from_inner(inner.clone());
    // HONESTY_AUDIT #1: only resolve inside the configured (extracted APK)
    // roots. An absolute host path is refused unless the loader is explicitly
    // in trusted-test mode.
    let trace = std::env::var_os("AEXP_TRACE_DL").is_some();
    let Some(path) = resolve_library(&inner, &name) else {
        if trace {
            eprintln!("[acos-dl] dlopen({name:?}) -> unresolved name");
        }
        return std::ptr::null_mut();
    };
    // Already mapped? Bump its reference count and reuse it (bionic returns a
    // handle to the existing mapping, it does not map a second copy).
    if let Some(existing) = OBJECTS
        .lock()
        .unwrap()
        .iter()
        .find(|l| l.matches_name(&name) || l.matches_name(&path.to_string_lossy()))
        .cloned()
    {
        existing.refs.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let id = register_dl_handle(&inner, existing);
        return id as *mut c_void;
    }
    let handle = match loader.load(&path, &name) {
        Ok(h) => h,
        Err(e) => {
            if trace {
                eprintln!("[acos-dl] dlopen({name:?}) from {} -> FAILED [{}] {e}", path.display(), e.code());
                if let crate::LoaderError::UnsupportedSymbols(list) = &e {
                    for l in list {
                        eprintln!("   missing in {}: {:?}", l.library, l.symbols);
                    }
                }
            }
            return std::ptr::null_mut();
        }
    };
    let id = register_dl_handle(&inner, handle.lib.clone());
    id as *mut c_void
}

/// `android_dlopen_ext(filename, flags, extinfo)`.
///
/// # Safety
/// See [`dlopen`].
pub unsafe extern "C" fn android_dlopen_ext(
    filename: *const c_char,
    flags: c_int,
    _extinfo: *const c_void,
) -> *mut c_void {
    dlopen(filename, flags)
}

/// Alias used internally by some bionic builds.
///
/// # Safety
/// See [`dlopen`].
pub unsafe extern "C" fn __loader_dlopen(
    filename: *const c_char,
    flags: c_int,
    _caller: *const c_void,
) -> *mut c_void {
    dlopen(filename, flags)
}

/// `dlsym`.
///
/// # Safety
/// `symbol` must be a valid NUL-terminated string.
pub unsafe extern "C" fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void {
    let name = cstr(symbol);
    if handle.is_null() {
        // RTLD_DEFAULT: search all loaded objects, then the bionic table.
        let objs = OBJECTS.lock().unwrap().clone();
        for lib in objs {
            if let Some(p) = lib.symbol(&name) {
                return p as *mut c_void;
            }
        }
        return super::resolve(&name)
            .map(|s| s.ptr as *mut c_void)
            .unwrap_or(std::ptr::null_mut());
    }
    if handle as isize == -1 {
        // B-LOADER PATCH: RTLD_NEXT. Within this loader the "next" object in
        // the lookup order is the bionic shim table — that is where the real
        // bionic libc implementation of `sigaction64`/`sigprocmask64` lives.
        // libsigchain resolves its predecessor this way at startup; returning
        // NULL there makes ART install a broken signal chain.
        return super::resolve(&name)
            .map(|s| s.ptr as *mut c_void)
            .unwrap_or(std::ptr::null_mut());
    }
    if (handle as usize) & PSEUDO_HANDLE_BIT != 0 {
        // A system-library pseudo handle: resolve from the bionic shim table.
        return super::resolve(&name)
            .map(|s| s.ptr as *mut c_void)
            .unwrap_or(std::ptr::null_mut());
    }
    let inner = match current_loader() {
        Some(i) => i,
        None => return std::ptr::null_mut(),
    };
    match dl_handle_lookup(&inner, handle as usize) {
        Some(lib) => {
            let p = lib.symbol(&name);
            if p.is_none() && std::env::var_os("AEXP_TRACE_DL").is_some() {
                eprintln!("[acos-dl] dlsym(handle {}, {name:?}) -> not found", handle as usize);
            }
            p.map(|p| p as *mut c_void).unwrap_or(std::ptr::null_mut())
        }
        None => {
            if std::env::var_os("AEXP_TRACE_DL").is_some() {
                eprintln!("[acos-dl] dlsym(handle {}, {name:?}) -> unknown handle", handle as usize);
            }
            std::ptr::null_mut()
        }
    }
}

/// `dlclose`.
///
/// # Safety
/// `handle` must come from [`dlopen`] or be NULL.
pub unsafe extern "C" fn dlclose(handle: *mut c_void) -> c_int {
    if handle.is_null() || handle as isize == -1 {
        return 0;
    }
    if (handle as usize) & PSEUDO_HANDLE_BIT != 0 {
        pseudo_handles()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&(handle as usize));
        return 0;
    }
    let inner = match current_loader() {
        Some(i) => i,
        None => return -1,
    };
    if let Some(lib) = dl_handle_remove(&inner, handle as usize) {
        let loader = crate::loader::Loader::from_inner(inner);
        let _ = loader.unload(&LibraryHandle { lib });
    }
    0
}

/// `dlerror` — always reports "no error" in this minimal implementation.
///
/// # Safety
/// Always safe.
pub unsafe extern "C" fn dlerror() -> *mut c_char {
    std::ptr::null_mut()
}

/// `dladdr` — best-effort object/symbol lookup.
///
/// # Safety
/// `info` must point to a writable `Dl_info`.
pub unsafe extern "C" fn dladdr(addr: *const c_void, info: *mut DlInfo) -> c_int {
    if info.is_null() {
        return 0;
    }
    *info = DlInfo {
        dli_fname: std::ptr::null(),
        dli_fbase: std::ptr::null_mut(),
        dli_sname: std::ptr::null(),
        dli_saddr: std::ptr::null_mut(),
    };
    let objs = OBJECTS.lock().unwrap().clone();
    for lib in objs {
        let a = addr as usize;
        let start = lib.bias();
        if a >= start && a < lib.runtime_end() {
            (*info).dli_fname = lib.name_c.as_ptr();
            (*info).dli_fbase = start as *mut c_void;
            // Nearest exported symbol at or below `addr`.
            if let Some((name, addr)) = lib.nearest_symbol(a) {
                (*info).dli_sname = name;
                (*info).dli_saddr = addr as *mut c_void;
            }
            return 1;
        }
    }
    0
}

/// Resolve a `dlopen` name to a loader path within the allowed roots.
///
/// An absolute (or slash-containing) name is canonicalised and must live under
/// one of `search_paths`; otherwise it is refused (returns `None`). A bare
/// soname is only ever resolved through `search_paths`, never the host dynamic
/// loader. This is the path-confinement half of HONESTY_AUDIT #1; the process
/// sandbox (seccomp/landlock) is still required before native APK code is
/// trusted (see DECISIONS.md ADR-0018 note).
fn resolve_library(inner: &Arc<LoaderInner>, name: &str) -> Option<std::path::PathBuf> {
    let config = inner.config();
    let p = std::path::Path::new(name);
    if p.is_absolute() || name.contains('/') {
        let real = std::fs::canonicalize(p).ok()?;
        let allowed = config.search_paths.iter().any(|root| {
            std::fs::canonicalize(root)
                .map(|r| real.starts_with(&r))
                .unwrap_or(false)
        });
        if !allowed && !config.allow_host_paths {
            return None;
        }
        return Some(real);
    }
    for dir in &config.search_paths {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

macro_rules! shim {
    ($m:expr, $name:literal, $f:path) => {
        $m.insert(
            $name,
            Shim::shimmed($name, $f as *const () as *const c_void),
        );
    };
}

/// Add all `dl*` shims to the table.
pub fn install(m: &mut HashMap<&'static str, Shim>) {
    shim!(m, "dlopen", dlopen);
    shim!(m, "android_dlopen_ext", android_dlopen_ext);
    shim!(m, "__loader_dlopen", __loader_dlopen);
    shim!(m, "dlsym", dlsym);
    shim!(m, "dlclose", dlclose);
    shim!(m, "dlerror", dlerror);
    shim!(m, "dladdr", dladdr);
    shim!(m, "dl_iterate_phdr", dl_iterate_phdr);
}
