//! The ELF/Bionic loader core: mapping, relocation, symbol binding, constructors
//! and teardown.

use std::collections::{HashMap, HashSet};
use std::os::raw::{c_char, c_int, c_void};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};

use crate::bionic::{self, Category, Shim};
use crate::dyninfo::{DynInfo, TlsSegment};
use crate::elf::*;

/// Architecture-neutral names for the dynamic relocation types the loader
/// implements. The ELF numbers differ per machine; the *semantics* (RELATIVE,
/// GLOB_DAT, JUMP_SLOT, ABS64, COPY, TLS) are the same, so the apply logic is
/// shared and only the numbers are `cfg`-selected. x86_64 is byte-for-byte
/// unchanged on the product host (W194-A).
mod rt {
    #[cfg(target_arch = "x86_64")]
    pub const ABS32: u32 = super::R_X86_64_32;
    #[cfg(target_arch = "x86_64")]
    pub const ABS32S: u32 = super::R_X86_64_32S;
    #[cfg(target_arch = "x86_64")]
    pub const ABS64: u32 = super::R_X86_64_64;
    #[cfg(target_arch = "x86_64")]
    pub const COPY: u32 = super::R_X86_64_COPY;
    #[cfg(target_arch = "x86_64")]
    pub const GLOB_DAT: u32 = super::R_X86_64_GLOB_DAT;
    #[cfg(target_arch = "x86_64")]
    pub const IRELATIVE: u32 = super::R_X86_64_IRELATIVE;
    #[cfg(target_arch = "x86_64")]
    pub const JUMP_SLOT: u32 = super::R_X86_64_JUMP_SLOT;
    #[cfg(target_arch = "x86_64")]
    pub const NONE: u32 = super::R_X86_64_NONE;
    #[cfg(target_arch = "x86_64")]
    pub const PC32: u32 = super::R_X86_64_PC32;
    #[cfg(target_arch = "x86_64")]
    pub const RELATIVE: u32 = super::R_X86_64_RELATIVE;
    #[cfg(target_arch = "x86_64")]
    pub const TLS_DTPMOD: u32 = super::R_X86_64_TLS_DTPMOD;
    #[cfg(target_arch = "x86_64")]
    pub const TLS_DTPOFF: u32 = super::R_X86_64_TLS_DTPOFF;
    #[cfg(target_arch = "x86_64")]
    pub const TLS_TPOFF: u32 = super::R_X86_64_TLS_TPOFF;
    #[cfg(target_arch = "x86_64")]
    pub const TLSDESC: u32 = super::R_X86_64_TLSDESC;

    #[cfg(target_arch = "aarch64")]
    pub const ABS64: u32 = super::R_AARCH64_ABS64;
    #[cfg(target_arch = "aarch64")]
    pub const COPY: u32 = super::R_AARCH64_COPY;
    #[cfg(target_arch = "aarch64")]
    pub const GLOB_DAT: u32 = super::R_AARCH64_GLOB_DAT;
    #[cfg(target_arch = "aarch64")]
    pub const IRELATIVE: u32 = super::R_AARCH64_IRELATIVE;
    #[cfg(target_arch = "aarch64")]
    pub const JUMP_SLOT: u32 = super::R_AARCH64_JUMP_SLOT;
    #[cfg(target_arch = "aarch64")]
    pub const NONE: u32 = super::R_AARCH64_NONE;
    #[cfg(target_arch = "aarch64")]
    pub const RELATIVE: u32 = super::R_AARCH64_RELATIVE;
    #[cfg(target_arch = "aarch64")]
    pub const TLS_DTPMOD: u32 = super::R_AARCH64_TLS_DTPMOD64;
    #[cfg(target_arch = "aarch64")]
    pub const TLS_DTPOFF: u32 = super::R_AARCH64_TLS_DTPREL64;
    #[cfg(target_arch = "aarch64")]
    pub const TLS_TPOFF: u32 = super::R_AARCH64_TLS_TPREL64;
    #[cfg(target_arch = "aarch64")]
    pub const TLSDESC: u32 = super::R_AARCH64_TLSDESC;
}
use crate::error::{LibraryMissing, LoaderError, Result};
use crate::report::{LibraryReport, Report, TableReport};
use crate::{LoaderConfig, MissingPolicy};

/// Global registry of all loaded guest objects (used by `dl_iterate_phdr` and
/// `dlsym(RTLD_DEFAULT, ...)`).
pub(crate) static OBJECTS: Mutex<Vec<Arc<NativeLibrary>>> = Mutex::new(Vec::new());

/// The loader that `dlopen`/`android_dlopen_ext` should use.
pub(crate) static CURRENT_LOADER: Mutex<Option<Weak<LoaderInner>>> = Mutex::new(None);

/// Name of a system library satisfied by the bionic shim table rather than a
/// file on disk.
/// Whether a `DT_NEEDED`/soname is a plain file-name component.
///
/// `search_paths` is the only resolution root, so absolute paths and any
/// separator-bearing or `.`/`..` name are refused before a host path can be
/// joined (W67A #4). This mirrors `bionic::dl::resolve_library` but applies to
/// guest-controlled ELF content, not just an explicit `dlopen` argument.
fn is_safe_dep_name(need: &str) -> bool {
    !need.is_empty()
        && !need.contains('/')
        && !need.contains('\\')
        && need != "."
        && need != ".."
        && !Path::new(need).is_absolute()
}

/// Whether `candidate` canonicalises to a path still under `dir`. A symlink
/// placed in a search root cannot be used to escape it.
fn contained_in(candidate: &Path, dir: &Path) -> bool {
    let Ok(real) = std::fs::canonicalize(candidate) else {
        return false;
    };
    match std::fs::canonicalize(dir) {
        Ok(root) => real.starts_with(&root),
        Err(_) => false,
    }
}

pub(crate) fn is_shim_system_lib(name: &str) -> bool {
    matches!(
        name,
        "libc.so"
            | "libm.so"
            | "libdl.so"
            | "liblog.so"
            | "libandroid.so"
            | "libz.so"
            | "libEGL.so"
            | "libGLESv2.so"
            | "libGLESv1_CM.so"
            | "libOpenSLES.so"
            | "libvulkan.so"
            | "libjnigraphics.so"
            | "libstdc++.so"
            | "libcutils.so"
            | "libnativewindow.so"
            | "libsync.so"
            | "libui.so"
            | "libutils.so"
    )
}

/// A parsed relocation.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Reloc {
    pub offset: u64,
    pub r_type: u32,
    pub sym: u32,
    pub addend: i64,
    /// True for REL/RELR-format relocations whose addend is stored in place at
    /// the target (rather than in the relocation entry).
    pub addend_in_place: bool,
}

/// How an import was satisfied.
#[derive(Clone, Debug)]
pub(crate) enum BoundVia {
    SelfObject,
    Object(#[allow(dead_code)] String),
    Forwarded,
    Shimmed,
    Missing,
}

/// One import binding record (for the report).
#[derive(Clone, Debug)]
pub(crate) struct ImportRecord {
    pub symbol: String,
    pub version: Option<String>,
    pub via: BoundVia,
}

/// A mapped, linked guest shared object.
pub struct NativeLibrary {
    pub name: String,
    pub path: Option<PathBuf>,
    pub id: usize,
    /// Load bias: `runtime_addr = bias + vaddr`.
    pub(crate) bias: usize,
    map_base: usize,
    map_len: usize,
    /// Program headers kept in memory for `dl_iterate_phdr`.
    phdrs: Box<[Phdr]>,
    pub(crate) dyninfo: DynInfo,
    pub(crate) tls_modid: Option<usize>,
    pub(crate) relocs: Vec<Reloc>,
    pub(crate) imports: Mutex<Vec<ImportRecord>>,
    deps: Mutex<Vec<Arc<NativeLibrary>>>,
    pub(crate) refs: AtomicUsize,
    initialized: AtomicBool,
    relocated: AtomicBool,
    protected: AtomicBool,
    loaded: AtomicBool,
    /// Whether `JNI_OnLoad` has already run for this mapping. bionic runs it
    /// once per loaded object, even when `System.loadLibrary` names it again.
    jni_on_load: AtomicBool,
    pub(crate) name_c: std::ffi::CString,
    /// Lazily built (address, NUL-terminated name) table for `dladdr`.
    dladdr_syms: Mutex<Option<Vec<(usize, std::ffi::CString)>>>,
}

// SAFETY: a `NativeLibrary` is immutable after load except for internal
// synchronised flags; the mapped pages are shared read-mostly code/data.
unsafe impl Send for NativeLibrary {}
unsafe impl Sync for NativeLibrary {}

impl NativeLibrary {
    /// Load bias (`dlpi_addr`).
    pub fn bias(&self) -> usize {
        self.bias
    }

    /// Runtime address for a virtual address in this object.
    pub fn runtime_addr(&self, vaddr: u64) -> usize {
        self.bias.wrapping_add(vaddr as usize)
    }

    /// Pointer to the in-memory program headers (for the unwinder).
    pub(crate) fn phdrs_ptr(&self) -> *const Phdr {
        self.phdrs.as_ptr()
    }

    pub(crate) fn phnum(&self) -> u16 {
        self.phdrs.len() as u16
    }

    fn find_defined(&self, name: &str) -> Option<Sym> {
        let idx = self.dyninfo.lookup(name)?;
        let s = *self.dyninfo.symbols.get(idx as usize)?;
        if s.is_undefined() || s.shndx == SHN_UNDEF {
            None
        } else {
            Some(s)
        }
    }

    /// Version-aware definition lookup.
    ///
    /// ELF symbol versioning semantics implemented here:
    ///
    /// * a reference with a *required* version (`VERNEED`, e.g. `LIBC`) binds to
    ///   a definition carrying that version, or to an unversioned definition
    ///   (which is how the bionic shim table and sibling guest objects look);
    /// * a definition of a *different* version never satisfies the reference;
    /// * a hidden (`VERSYM_HIDDEN`) definition is only reachable through an
    ///   explicit version, never by a plain name (`dlsym` skips it).
    fn find_defined_versioned(&self, name: &str, wanted: Option<&str>) -> Option<Sym> {
        let wanted = match wanted {
            None => return self.find_defined(name),
            Some(v) => v,
        };
        // A library can define several versions of one name, so scan the
        // dynamic symbol table rather than taking the hash-table hit.
        let mut unversioned: Option<Sym> = None;
        for (i, sym) in self.dyninfo.symbols.iter().enumerate() {
            if sym.is_undefined() || sym.shndx == SHN_UNDEF {
                continue;
            }
            if self.dyninfo.symbol_name(i) != Some(name) {
                continue;
            }
            if let Some((def, _default)) = self.dyninfo.defined_version(i) {
                if def == wanted && !self.dyninfo.is_hidden_version(i) {
                    return Some(*sym);
                }
                // Mismatched definition: keep looking.
                continue;
            }
            unversioned.get_or_insert(*sym);
        }
        unversioned
    }

    /// Nearest exported function/object name at or below `addr`.
    ///
    /// Returns a pointer to a NUL-terminated name owned by this object (stable
    /// for the object's lifetime) plus the symbol address.
    pub(crate) fn nearest_symbol(&self, addr: usize) -> Option<(*const c_char, usize)> {
        let mut guard = self.dladdr_syms.lock().ok()?;
        let table = guard.get_or_insert_with(|| {
            let mut v: Vec<(usize, std::ffi::CString)> = Vec::new();
            for (i, sym) in self.dyninfo.symbols.iter().enumerate() {
                if sym.is_undefined() || sym.value == 0 || sym.bind() == STB_LOCAL {
                    continue;
                }
                let t = sym.sym_type();
                if t != STT_FUNC && t != STT_OBJECT && t != STT_NOTYPE {
                    continue;
                }
                let Some(n) = self.dyninfo.symbol_name(i) else {
                    continue;
                };
                if let Ok(c) = std::ffi::CString::new(n) {
                    v.push((self.runtime_addr(sym.value), c));
                }
            }
            v.sort_by_key(|(a, _)| *a);
            v
        });
        let idx = table.partition_point(|(a, _)| *a <= addr);
        if idx == 0 {
            return None;
        }
        let (sym_addr, name) = &table[idx - 1];
        Some((name.as_ptr(), *sym_addr))
    }

    /// Runtime end address (one past the highest mapped byte).
    pub(crate) fn runtime_end(&self) -> usize {
        let end = self
            .phdrs
            .iter()
            .filter(|p| p.p_type == PT_LOAD && p.p_memsz > 0)
            .map(|p| p.p_vaddr + p.p_memsz)
            .max()
            .unwrap_or(0);
        self.bias.wrapping_add(end as usize)
    }

    /// Look up an exported symbol defined by this object.
    pub fn symbol(&self, name: &str) -> Option<*const c_void> {
        let s = self.find_defined(name)?;
        if s.sym_type() == STT_TLS {
            // TLS symbol: return the offset, not an address (the caller must
            // use `__tls_get_addr`).
            return Some(s.value as *const c_void);
        }
        Some(self.runtime_addr(s.value) as *const c_void)
    }

    /// Whether the object has already been through `JNI_OnLoad`.
    pub fn jni_on_load_done(&self) -> bool {
        self.jni_on_load.load(Ordering::Acquire)
    }

    /// Record that `JNI_OnLoad` has run for this mapping (idempotent).
    pub fn note_jni_on_load(&self) {
        self.jni_on_load.store(true, Ordering::Release);
    }

    /// Whether the object is still mapped.
    pub fn is_loaded(&self) -> bool {
        self.loaded.load(Ordering::Acquire)
    }

    /// Whether this object answers to `name` (its file name or its SONAME).
    /// SONAME de-duplication is what stops `libc++_shared.so` being mapped
    /// twice when one dependency requests the file name and another the
    /// `DT_SONAME`.
    pub(crate) fn matches_name(&self, name: &str) -> bool {
        self.name == name || self.dyninfo.soname.as_deref() == Some(name)
    }

    fn deps_snapshot(&self) -> Vec<Arc<NativeLibrary>> {
        self.deps.lock().map(|g| g.clone()).unwrap_or_default()
    }
}

/// Shared loader state.
pub struct LoaderInner {
    config: LoaderConfig,
    libs: Mutex<Vec<Arc<NativeLibrary>>>,
    next_object_id: AtomicUsize,
    next_dl_handle: AtomicUsize,
    dl_handles: Mutex<HashMap<usize, Arc<NativeLibrary>>>,
}

/// Public loader handle.
#[derive(Clone)]
pub struct Loader {
    inner: Arc<LoaderInner>,
}

/// Handle to a loaded library.
#[derive(Clone)]
pub struct LibraryHandle {
    pub(crate) lib: Arc<NativeLibrary>,
}

impl LibraryHandle {
    /// Library name.
    pub fn name(&self) -> &str {
        &self.lib.name
    }

    /// Look up an exported symbol (`JNI_OnLoad`, `Java_*`, ...).
    pub fn symbol(&self, name: &str) -> Option<*const c_void> {
        self.lib.symbol(name)
    }

    /// The underlying library object.
    pub fn library(&self) -> &Arc<NativeLibrary> {
        &self.lib
    }
}

impl Loader {
    /// Wrap an existing inner state (used by the `dl*` shims).
    pub(crate) fn from_inner(inner: Arc<LoaderInner>) -> Loader {
        Loader { inner }
    }

    /// Create a loader with the given configuration.
    pub fn new(config: LoaderConfig) -> Loader {
        crate::testjni::set_log_sink(config.log_sink.clone());
        if !config.system_properties.is_empty() {
            bionic::shims::set_system_properties(config.system_properties.clone());
        }
        let inner = Arc::new(LoaderInner {
            config,
            libs: Mutex::new(Vec::new()),
            next_object_id: AtomicUsize::new(1),
            next_dl_handle: AtomicUsize::new(1),
            dl_handles: Mutex::new(HashMap::new()),
        });
        *CURRENT_LOADER.lock().unwrap() = Some(Arc::downgrade(&inner));
        Loader { inner }
    }

    /// Load a shared object from a file.
    pub fn load(&self, path: &Path, name: &str) -> Result<LibraryHandle> {
        let bytes = std::fs::read(path)?;
        self.load_bytes_named(&bytes, Some(path.to_path_buf()), name)
    }

    /// Find an object that is already mapped under one of `names` (file name,
    /// `DT_SONAME` or full path) and take a reference to it.
    ///
    /// This is bionic `dlopen`'s "already loaded" rule: an object that is in the
    /// process is never mapped a second time. `System.loadLibrary` is `dlopen`
    /// plus `JNI_OnLoad`, so it must consult the same registry (`OBJECTS`);
    /// otherwise a JNI load of an object the loader mapped itself produces a
    /// second copy of its code and data, and the registered natives point into
    /// the copy the VM never runs.
    pub fn find_loaded(&self, names: &[&str]) -> Option<LibraryHandle> {
        let found = OBJECTS
            .lock()
            .unwrap()
            .iter()
            .find(|lib| {
                names.iter().any(|name| {
                    if name.is_empty() {
                        return false;
                    }
                    lib.matches_name(name)
                        || lib.path.as_deref() == Some(Path::new(name))
                        || lib
                            .path
                            .as_deref()
                            .and_then(Path::file_name)
                            .and_then(|f| f.to_str())
                            == Some(*name)
                })
            })
            .cloned()?;
        found.refs.fetch_add(1, Ordering::SeqCst);
        Some(LibraryHandle { lib: found })
    }

    /// Load a shared object from bytes (no file path, e.g. extracted from an APK).
    pub fn load_bytes(&self, bytes: &[u8], name: &str) -> Result<LibraryHandle> {
        self.load_bytes_named(bytes, None, name)
    }

    /// Load from a file, defaulting the display name to the file name.
    pub fn load_file(&self, path: &Path) -> Result<LibraryHandle> {
        let name = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown.so")
            .to_string();
        self.load(path, &name)
    }

    fn load_bytes_named(
        &self,
        bytes: &[u8],
        path: Option<PathBuf>,
        name: &str,
    ) -> Result<LibraryHandle> {
        // ADR-0022: mapping `PROT_EXEC` guest code in-process is only allowed
        // once this process reports an active confinement. Product paths leave
        // `require_confinement` on; trusted tests opt out explicitly.
        if self.inner.config.require_confinement && !acos_sandbox::confinement_active() {
            return Err(LoaderError::ConfinementRequired);
        }
        let root = self.load_object(bytes, path, name)?;
        // Recursively load DT_NEEDED dependencies (pre-order registration gives
        // the ELF global search order: root, then deps breadth/depth-first).
        self.load_deps(&root)?;
        // Final symbol binding pass, now that the whole graph is mapped. This
        // also makes forward references and symbol interposition work.
        self.relocate_graph()?;
        self.protect_and_irelative()?;
        self.run_init(&root);
        Ok(LibraryHandle { lib: root })
    }

    fn load_object(
        &self,
        bytes: &[u8],
        path: Option<PathBuf>,
        name: &str,
    ) -> Result<Arc<NativeLibrary>> {
        let elf = Elf::parse(bytes).map_err(|e| LoaderError::ElfMalformed(e.to_string()))?;
        if elf.header.e_machine != HOST_ELF_MACHINE {
            return Err(LoaderError::WrongAbi {
                found: abi_name(elf.header.e_machine),
                detail: format!("e_machine={}", elf.header.e_machine),
            });
        }
        let dyninfo =
            DynInfo::parse(&elf).map_err(|e| LoaderError::ElfMalformed(e.to_string()))?;
        let relocs = self.parse_relocations(bytes, &elf, &dyninfo)?;
        let (map_base, map_len, bias) = self.map_segments(bytes, &elf, name)?;

        let id = self.inner.next_object_id.fetch_add(1, Ordering::SeqCst);
        let tls_modid = dyninfo.tls.map(|t| {
            let init = tls_init_image(bytes, &elf, t);
            crate::tls::register_module(t.memsz as usize, t.align as usize, init)
        });

        let name_c = std::ffi::CString::new(name).unwrap_or_default();
        let lib = Arc::new(NativeLibrary {
            name: name.to_string(),
            path,
            id,
            bias,
            map_base,
            map_len,
            phdrs: elf.phdrs.clone().into_boxed_slice(),
            dyninfo,
            tls_modid,
            relocs,
            imports: Mutex::new(Vec::new()),
            deps: Mutex::new(Vec::new()),
            refs: AtomicUsize::new(1),
            initialized: AtomicBool::new(false),
            relocated: AtomicBool::new(false),
            protected: AtomicBool::new(false),
            loaded: AtomicBool::new(true),
            jni_on_load: AtomicBool::new(false),
            name_c,
            dladdr_syms: Mutex::new(None),
        });

        // Pre-order registration (root before deps) fixes the search order.
        self.inner.libs.lock().unwrap().push(lib.clone());
        OBJECTS.lock().unwrap().push(lib.clone());
        Ok(lib)
    }

    fn load_deps(&self, lib: &Arc<NativeLibrary>) -> Result<()> {
        let needed = lib.dyninfo.needed.clone();
        for need in needed {
            if is_shim_system_lib(&need) {
                continue;
            }
            let dep = match self.find_and_load_dep(&need, &lib.name)? {
                Some(d) => d,
                None => continue,
            };
            dep.refs.fetch_add(1, Ordering::SeqCst);
            lib.deps.lock().unwrap().push(dep);
        }
        Ok(())
    }

    fn find_and_load_dep(
        &self,
        need: &str,
        requester: &str,
    ) -> Result<Option<Arc<NativeLibrary>>> {
        // A guest library's `DT_NEEDED` is untrusted content: only a plain
        // SONAME-style name may be resolved, and only inside `search_paths`.
        if !is_safe_dep_name(need) {
            return Err(LoaderError::UnsafeDependency {
                needed: need.to_string(),
                requester: requester.to_string(),
            });
        }
        // Already loaded? De-duplicate by file name or `DT_SONAME`.
        if let Some(existing) = self
            .inner
            .libs
            .lock()
            .unwrap()
            .iter()
            .find(|l| l.matches_name(need))
            .cloned()
        {
            return Ok(Some(existing));
        }
        for dir in &self.inner.config.search_paths {
            let candidate = dir.join(need);
            if candidate.is_file() {
                if !contained_in(&candidate, dir) {
                    return Err(LoaderError::UnsafeDependency {
                        needed: need.to_string(),
                        requester: requester.to_string(),
                    });
                }
                let bytes = std::fs::read(&candidate)?;
                let lib = self.load_object(&bytes, Some(candidate), need)?;
                self.load_deps(&lib)?;
                return Ok(Some(lib));
            }
        }
        Err(LoaderError::DepNotFound {
            needed: need.to_string(),
            requester: requester.to_string(),
            searched: self.inner.config.search_paths.clone(),
        })
    }

    // --- relocation parsing -------------------------------------------------

    fn parse_relocations(
        &self,
        bytes: &[u8],
        elf: &Elf<'_>,
        dyni: &DynInfo,
    ) -> Result<Vec<Reloc>> {
        let mut out = Vec::new();
        if let Some((va, size)) = dyni.rela {
            self.read_rela(elf, va, size, &mut out)?;
        }
        if let Some((va, size)) = dyni.jmprel {
            if dyni.jmprel_is_rela {
                self.read_rela(elf, va, size, &mut out)?;
            } else {
                self.read_rel(elf, va, size, &mut out)?;
            }
        }
        if let Some((va, size)) = dyni.rel {
            self.read_rel(elf, va, size, &mut out)?;
        }
        if let Some((va, size, android)) = dyni.relr {
            self.read_relr(elf, va, size, android, &mut out)?;
        }
        if let Some((va, size, has_addend)) = dyni.android_packed {
            self.read_android_packed(bytes, elf, va, size, has_addend, &mut out)?;
        }
        Ok(out)
    }

    fn read_rela(&self, elf: &Elf<'_>, va: u64, size: u64, out: &mut Vec<Reloc>) -> Result<()> {
        let off = elf
            .vaddr_to_offset(va)
            .ok_or_else(|| malformed("relocation table not in PT_LOAD"))?;
        let count = (size / 24) as usize;
        for i in 0..count {
            let p = off + i * 24;
            let offset = rd_u64_checked(elf.data, p)?;
            let info = rd_u64_checked(elf.data, p + 8)?;
            let addend = rd_u64_checked(elf.data, p + 16)? as i64;
            out.push(Reloc {
                offset,
                r_type: (info & 0xffff_ffff) as u32,
                sym: (info >> 32) as u32,
                addend,
                addend_in_place: false,
            });
        }
        Ok(())
    }

    fn read_rel(&self, elf: &Elf<'_>, va: u64, size: u64, out: &mut Vec<Reloc>) -> Result<()> {
        let off = elf
            .vaddr_to_offset(va)
            .ok_or_else(|| malformed("relocation table not in PT_LOAD"))?;
        let count = (size / 16) as usize;
        for i in 0..count {
            let p = off + i * 16;
            let offset = rd_u64_checked(elf.data, p)?;
            let info = rd_u64_checked(elf.data, p + 8)?;
            out.push(Reloc {
                offset,
                r_type: (info & 0xffff_ffff) as u32,
                sym: (info >> 32) as u32,
                addend: 0,
                addend_in_place: true,
            });
        }
        Ok(())
    }

    fn read_relr(
        &self,
        elf: &Elf<'_>,
        va: u64,
        size: u64,
        _android: bool,
        out: &mut Vec<Reloc>,
    ) -> Result<()> {
        let off = elf
            .vaddr_to_offset(va)
            .ok_or_else(|| malformed("RELR table not in PT_LOAD"))?;
        let end = off + size as usize;
        let data = elf
            .data
            .get(off..end)
            .ok_or_else(|| malformed("RELR table exceeds file"))?;
        let mut where_: u64 = 0;
        let mut i = 0usize;
        while i + 8 <= data.len() {
            let entry = u64::from_le_bytes(data[i..i + 8].try_into().unwrap());
            i += 8;
            if entry & 1 == 0 {
                where_ = entry;
                out.push(Reloc {
                    offset: where_,
                    r_type: rt::RELATIVE,
                    sym: 0,
                    addend: 0,
                    addend_in_place: true,
                });
                where_ = where_.wrapping_add(8);
            } else {
                let bits = entry >> 1;
                for b in 0..63 {
                    if bits & (1u64 << b) != 0 {
                        out.push(Reloc {
                            offset: where_.wrapping_add(b * 8),
                            r_type: rt::RELATIVE,
                            sym: 0,
                            addend: 0,
                            addend_in_place: true,
                        });
                    }
                }
                where_ = where_.wrapping_add(63 * 8);
            }
        }
        Ok(())
    }

    /// Decode an Android APS2 packed REL/RELA section.
    ///
    /// The Android relocation packer only packs `*_RELATIVE` relocations, so a
    /// section contains a single group. The observed layout (NDK 27 / LLD) is:
    /// `APS2`, `uleb reloc_count`, `uleb reserved`, `uleb group_size`,
    /// `uleb group_flags`, `uleb relocation_info`, then the first relocation as
    /// `sleb offset` + `sleb addend`, and each following one as `uleb offset_delta`
    /// + `sleb addend_delta`.
    ///
    /// Multi-group sections are rejected rather than mis-decoded.
    fn read_android_packed(
        &self,
        _bytes: &[u8],
        elf: &Elf<'_>,
        va: u64,
        size: u64,
        has_addend: bool,
        out: &mut Vec<Reloc>,
    ) -> Result<()> {
        let off = elf
            .vaddr_to_offset(va)
            .ok_or_else(|| malformed("android reloc table not in PT_LOAD"))?;
        let end = off
            .checked_add(size as usize)
            .ok_or_else(|| malformed("android reloc size overflow"))?;
        let data = elf
            .data
            .get(off..end)
            .ok_or_else(|| malformed("android reloc table exceeds file"))?;
        let mut cur = 0usize;
        if data.len() >= 4 && &data[0..4] == b"APS2" {
            cur = 4;
        }
        let reloc_count = read_leb128(data, &mut cur)?;
        let _reserved = read_leb128(data, &mut cur)?;
        // One packed section may contain several groups (RELATIVE, SYMBOLIC,
        // CUSTOM). The loop below walks them all; the previous implementation
        // rejected anything with more than one group.
        let mut remaining = reloc_count as usize;
        let mut groups = 0usize;
        while remaining > 0 {
            let group_size = match read_leb128(data, &mut cur) {
                Ok(v) => v as usize,
                Err(_) => break,
            };
            let group_flags = read_leb128(data, &mut cur)?;
            let info = read_leb128(data, &mut cur)?;
            if group_size == 0 {
                break;
            }
            // Group flags: 1 = RELATIVE, 2 = SYMBOLIC, 4 = ANDROID custom.
            let group_relative = group_flags & 1 != 0;
            let group_symbolic = group_flags & 2 != 0;
            let r_type = (info & 0xffff_ffff) as u32;
            let group_sym_idx = (info >> 32) as u32;
            if !group_relative && !group_symbolic {
                return Err(malformed(
                    "Android packed relocation group flags 0x0 are unsupported",
                ));
            }
            let mut offset: i64 = 0;
            let mut addend: i64 = 0;
            for i in 0..group_size {
                if i == 0 {
                    offset = read_sleb128(data, &mut cur)?;
                    if has_addend {
                        addend = read_sleb128(data, &mut cur)?;
                    }
                } else {
                    offset += read_leb128(data, &mut cur)? as i64;
                    if has_addend {
                        addend += read_sleb128(data, &mut cur)?;
                    }
                }
                out.push(Reloc {
                    offset: offset as u64,
                    r_type,
                    sym: if group_symbolic { group_sym_idx } else { 0 },
                    addend,
                    addend_in_place: !has_addend,
                });
            }
            remaining = remaining.saturating_sub(group_size);
            groups += 1;
        }
        if groups == 0 {
            return Err(malformed("Android packed relocation section has no groups"));
        }
        Ok(())
    }

    // --- mapping ------------------------------------------------------------

    fn map_segments(&self, bytes: &[u8], elf: &Elf<'_>, name: &str) -> Result<(usize, usize, usize)> {
        let page = page_size() as u64;
        let loads: Vec<&Phdr> = elf.phdrs.iter().filter(|p| p.p_type == PT_LOAD).collect();
        if loads.is_empty() {
            return Err(malformed("no PT_LOAD segments"));
        }
        let mut min = u64::MAX;
        let mut max = 0u64;
        for p in &loads {
            if p.p_memsz == 0 {
                continue;
            }
            min = min.min(align_down(p.p_vaddr, page));
            max = max.max(align_up(
                p.p_vaddr
                    .checked_add(p.p_memsz)
                    .ok_or_else(|| malformed("segment overflow"))?,
                page,
            ));
        }
        if min == u64::MAX || max <= min {
            return Err(malformed("degenerate segment layout"));
        }
        let len = (max - min) as usize;
        // SAFETY: anonymous private reservation.
        let base = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                len,
                libc::PROT_NONE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        if base == libc::MAP_FAILED {
            return Err(LoaderError::InitFailed(format!(
                "mmap reservation failed for {name}"
            )));
        }
        let base = base as usize;

        for p in &loads {
            if p.p_memsz == 0 {
                continue;
            }
            let seg_start = align_down(p.p_vaddr, page);
            let seg_end = align_up(p.p_vaddr + p.p_memsz, page);
            let dest = base + (seg_start - min) as usize;
            let seg_len = (seg_end - seg_start) as usize;
            // Map RW during relocation/link; final protections are applied
            // after relocations (W^X: never RWX).
            // SAFETY: MAP_FIXED over our reservation.
            let rc = unsafe {
                libc::mmap(
                    dest as *mut c_void,
                    seg_len,
                    libc::PROT_READ | libc::PROT_WRITE,
                    libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_FIXED,
                    -1,
                    0,
                )
            };
            if rc == libc::MAP_FAILED {
                return Err(LoaderError::InitFailed(format!("mmap segment failed for {name}")));
            }
            if p.p_filesz > 0 {
                let file_start = usize::try_from(p.p_offset)
                    .map_err(|_| malformed("segment file offset overflow"))?;
                let file_end = file_start
                    .checked_add(p.p_filesz as usize)
                    .ok_or_else(|| malformed("segment file size overflow"))?;
                let src = bytes
                    .get(file_start..file_end)
                    .ok_or_else(|| malformed("segment exceeds file"))?;
                let dst_off = (p.p_vaddr - seg_start) as usize;
                // SAFETY: src and dest are valid for `p_filesz` bytes; the
                // reservation has room for the whole segment.
                unsafe {
                    std::ptr::copy_nonoverlapping(src.as_ptr(), (dest + dst_off) as *mut u8, src.len());
                }
            }
        }
        let bias = base - min as usize;
        Ok((base, len, bias))
    }

    // --- symbol binding & relocation ---------------------------------------

    fn resolve_global(&self, name: &str, version: Option<&str>) -> Option<ResolveResult> {
        for lib in self.inner.libs.lock().unwrap().iter() {
            if let Some(s) = lib.find_defined_versioned(name, version) {
                return Some(ResolveResult::Object(lib.clone(), s));
            }
        }
        // The bionic table is unversioned; a versioned reference (`foo@LIBC`)
        // is satisfied by it because bionic's own libc exports exactly those
        // default versions.
        bionic::resolve(name).map(ResolveResult::Bionic)
    }

    fn relocate_graph(&self) -> Result<()> {
        let libs: Vec<Arc<NativeLibrary>> = self.inner.libs.lock().unwrap().clone();
        let mut missing: Vec<LibraryMissing> = Vec::new();
        for lib in &libs {
            if lib.relocated.swap(true, Ordering::SeqCst) {
                continue;
            }
            let lib_missing = self.apply_non_irelative(lib)?;
            if !lib_missing.is_empty() {
                missing.push(LibraryMissing {
                    library: lib.name.clone(),
                    symbols: lib_missing,
                });
            }
        }
        if !missing.is_empty() {
            match self.inner.config.missing_policy {
                MissingPolicy::Strict => return Err(LoaderError::UnsupportedSymbols(missing)),
                MissingPolicy::LazyTrap => { /* traps were already bound */ }
            }
        }
        Ok(())
    }

    fn apply_non_irelative(&self, lib: &Arc<NativeLibrary>) -> Result<Vec<String>> {
        let mut missing = Vec::new();
        let mut seen_missing: HashSet<String> = HashSet::new();
        let mut records: Vec<ImportRecord> = Vec::new();
        for r in &lib.relocs {
            if r.r_type == rt::IRELATIVE || r.r_type == rt::NONE {
                continue;
            }
            let name = lib.dyninfo.symbol_name(r.sym as usize).map(|s| s.to_string());
            let needs_symbol = Self::reloc_needs_symbol(r.r_type);
            if needs_symbol {
                let display = name.clone().unwrap_or_else(|| "<null>".into());
                let req_version = lib.dyninfo.symbol_version(r.sym as usize).map(str::to_string);
                let resolved = name
                    .as_deref()
                    .and_then(|n| self.resolve_global(n, req_version.as_deref()));
                match resolved {
                    Some(ResolveResult::Object(ref l, s)) => {
                        records.push(ImportRecord {
                            symbol: display.clone(),
                            version: lib.dyninfo.symbol_version(r.sym as usize).map(String::from),
                            via: if Arc::ptr_eq(l, lib) {
                                BoundVia::SelfObject
                            } else {
                                BoundVia::Object(l.name.clone())
                            },
                        });
                        // record kept above
                        let _ = s;
                    }
                    Some(ResolveResult::Bionic(shim)) => {
                        records.push(ImportRecord {
                            symbol: display.clone(),
                            version: None,
                            via: match shim.category {
                                Category::Forwarded => BoundVia::Forwarded,
                                Category::Shimmed => BoundVia::Shimmed,
                            },
                        });
                    }
                    None => {
                        // Weak undefined symbols bind to 0.
                        let weak = lib
                            .dyninfo
                            .symbols
                            .get(r.sym as usize)
                            .map(|s| s.bind() == STB_WEAK)
                            .unwrap_or(false);
                        if weak {
                            continue;
                        }
                        if seen_missing.insert(display.clone()) {
                            missing.push(display.clone());
                        }
                        records.push(ImportRecord {
                            symbol: display.clone(),
                            version: None,
                            via: BoundVia::Missing,
                        });
                    }
                }
            }
            self.apply_one(lib, r)?;
        }
        *lib.imports.lock().unwrap() = records;
        Ok(missing)
    }

    fn resolved_addr(&self, lib: &Arc<NativeLibrary>, sym: u32) -> Result<ResolvedValue> {
        let name = match lib.dyninfo.symbol_name(sym as usize) {
            Some(n) => n.to_string(),
            None => return Ok(ResolvedValue::WeakNull),
        };
        let version = lib
            .dyninfo
            .symbol_version(sym as usize)
            .map(str::to_string);
        match self.resolve_global(&name, version.as_deref()) {
            Some(ResolveResult::Object(l, s)) => {
                if s.sym_type() == STT_TLS {
                    let modid = l
                        .tls_modid
                        .ok_or_else(|| malformed("TLS symbol in library without PT_TLS"))?;
                    Ok(ResolvedValue::Tls {
                        modid,
                        value: s.value,
                    })
                } else if s.shndx == SHN_ABS {
                    Ok(ResolvedValue::Abs(s.value as usize))
                } else {
                    Ok(ResolvedValue::Abs(l.bias.wrapping_add(s.value as usize)))
                }
            }
            Some(ResolveResult::Bionic(shim)) => Ok(ResolvedValue::Abs(shim.ptr as usize)),
            None => {
                if self.inner.config.missing_policy == MissingPolicy::LazyTrap {
                    let addr = bionic::trap::stub_for(&name, &lib.name);
                    Ok(ResolvedValue::Abs(addr))
                } else {
                    let weak = lib
                        .dyninfo
                        .symbols
                        .get(sym as usize)
                        .map(|s| s.bind() == STB_WEAK)
                        .unwrap_or(false);
                    if weak {
                        Ok(ResolvedValue::WeakNull)
                    } else {
                        Ok(ResolvedValue::Abs(0))
                    }
                }
            }
        }
    }

    fn apply_one(&self, lib: &Arc<NativeLibrary>, r: &Reloc) -> Result<()> {
        let target_addr = lib.bias.wrapping_add(r.offset as usize);
        if target_addr < lib.map_base || target_addr >= lib.map_base.saturating_add(lib.map_len) {
            return Err(LoaderError::ElfMalformed(format!(
                "{}: relocation offset 0x{:x} (type {}) outside mapping [0x{:x}, 0x{:x})",
                lib.name,
                r.offset,
                r.r_type,
                lib.map_base,
                lib.map_base.saturating_add(lib.map_len)
            )));
        }
        let target = target_addr as *mut u64;
        match r.r_type {
            rt::NONE => {}
            rt::RELATIVE => {
                // SAFETY: target is inside a writable data segment.
                unsafe {
                    if r.addend_in_place {
                        // REL/RELR: addend lives in the target slot.
                        *target = (*target).wrapping_add(lib.bias as u64);
                    } else {
                        *target = (lib.bias as u64).wrapping_add(r.addend as u64);
                    }
                }
            }
            rt::GLOB_DAT | rt::JUMP_SLOT => {
                match self.resolved_addr(lib, r.sym)? {
                    ResolvedValue::Abs(a) => unsafe { *target = a as u64 },
                    ResolvedValue::Tls { modid, value } => unsafe {
                        *target = (modid as u64).wrapping_add(value)
                    },
                    ResolvedValue::WeakNull => unsafe { *target = 0 },
                }
            }
            rt::ABS64 => {
                let add = r.addend as u64;
                match self.resolved_addr(lib, r.sym)? {
                    ResolvedValue::Abs(a) => unsafe { *target = (a as u64).wrapping_add(add) },
                    ResolvedValue::Tls { .. } => {
                        return Err(LoaderError::UnsupportedRelocation {
                            kind: r.r_type,
                            library: lib.name.clone(),
                            offset: r.offset,
                        })
                    }
                    ResolvedValue::WeakNull => unsafe { *target = add },
                }
            }
            #[cfg(target_arch = "x86_64")]
            rt::PC32 | rt::ABS32 | rt::ABS32S => {
                let place = lib.bias.wrapping_add(r.offset as usize) as u64;
                let s = match self.resolved_addr(lib, r.sym)? {
                    ResolvedValue::Abs(a) => a as u64,
                    _ => 0,
                };
                let value = if r.r_type == rt::PC32 {
                    s.wrapping_add(r.addend as u64).wrapping_sub(place)
                } else {
                    s.wrapping_add(r.addend as u64)
                };
                // SAFETY: 32-bit relocation writes into mapped storage.
                unsafe {
                    *(target as *mut u32) = value as u32;
                }
            }
            rt::COPY => {
                let s = match self.resolved_addr(lib, r.sym)? {
                    ResolvedValue::Abs(a) => a,
                    _ => return Ok(()),
                };
                let size = lib
                    .dyninfo
                    .symbols
                    .get(r.sym as usize)
                    .map(|s| s.size as usize)
                    .unwrap_or(0);
                if size > 0 {
                    // SAFETY: both source and destination are valid for `size`.
                    unsafe {
                        std::ptr::copy_nonoverlapping(s as *const u8, target as *mut u8, size);
                    }
                }
            }
            rt::TLS_DTPMOD => {
                let modid = match self.resolved_addr(lib, r.sym)? {
                    ResolvedValue::Tls { modid, .. } => modid,
                    _ => 0,
                };
                unsafe { *target = modid as u64 };
            }
            rt::TLS_DTPOFF => {
                let value = match self.resolved_addr(lib, r.sym)? {
                    ResolvedValue::Tls { value, .. } => value,
                    ResolvedValue::Abs(a) => a as u64,
                    _ => 0,
                };
                unsafe { *target = value.wrapping_add(r.addend as u64) };
            }
            rt::TLS_TPOFF | rt::TLSDESC => {
                return Err(LoaderError::UnsupportedRelocation {
                    kind: r.r_type,
                    library: lib.name.clone(),
                    offset: r.offset,
                })
            }
            other => {
                return Err(LoaderError::UnsupportedRelocation {
                    kind: other,
                    library: lib.name.clone(),
                    offset: r.offset,
                })
            }
        }
        Ok(())
    }

    /// Whether satisfying this relocation type requires resolving a symbol.
    fn reloc_needs_symbol(t: u32) -> bool {
        match t {
            rt::GLOB_DAT | rt::JUMP_SLOT | rt::ABS64 | rt::COPY | rt::TLS_DTPMOD
            | rt::TLS_DTPOFF | rt::TLS_TPOFF => true,
            #[cfg(target_arch = "x86_64")]
            rt::PC32 | rt::ABS32 | rt::ABS32S => true,
            _ => false,
        }
    }

    fn protect_and_irelative(&self) -> Result<()> {
        let libs: Vec<Arc<NativeLibrary>> = self.inner.libs.lock().unwrap().clone();
        let page = page_size() as u64;
        // Final segment protections.
        for lib in &libs {
            if lib.protected.load(Ordering::SeqCst) {
                continue;
            }
            for p in lib.phdrs.iter() {
                if p.p_type != PT_LOAD || p.p_memsz == 0 {
                    continue;
                }
                let seg_start = align_down(p.p_vaddr, page);
                let seg_end = align_up(p.p_vaddr + p.p_memsz, page);
                let addr = lib.map_base + (seg_start - self.min_vaddr(lib)) as usize;
                let len = (seg_end - seg_start) as usize;
                let prot = prot_from_flags(p.p_flags);
                if let Some(relro) = relro_range(lib, page) {
                    // Hold RELRO writable until IRELATIVE is applied.
                    if addr >= relro.0 && addr < relro.1 {
                        continue;
                    }
                }
                // SAFETY: address range is inside our mapping.
                unsafe {
                    if libc::mprotect(addr as *mut c_void, len, prot) != 0 {
                        return Err(LoaderError::InitFailed(format!(
                            "mprotect failed for {}",
                            lib.name
                        )));
                    }
                }
            }
        }
        // Apply IRELATIVE resolvers now that `.text` is executable.
        for lib in &libs {
            for r in &lib.relocs {
                if r.r_type != rt::IRELATIVE {
                    continue;
                }
                let resolver = lib.bias.wrapping_add(r.addend as usize);
                // SAFETY: resolver is an IRELATIVE target inside guest .text.
                let value: usize = unsafe {
                    let f: extern "C" fn() -> usize = std::mem::transmute(resolver);
                    f()
                };
                let target = (lib.bias.wrapping_add(r.offset as usize)) as *mut u64;
                // SAFETY: RELRO is still writable at this point.
                unsafe { *target = value as u64 };
            }
        }
        // Seal RELRO.
        for lib in &libs {
            if lib.protected.swap(true, Ordering::SeqCst) {
                continue;
            }
            if let Some((start, end)) = relro_range(lib, page) {
                // SAFETY: range is inside our mapping.
                unsafe {
                    libc::mprotect(start as *mut c_void, end - start, libc::PROT_READ);
                }
            }
        }
        Ok(())
    }

    fn min_vaddr(&self, lib: &NativeLibrary) -> u64 {
        lib.phdrs
            .iter()
            .filter(|p| p.p_type == PT_LOAD && p.p_memsz > 0)
            .map(|p| align_down(p.p_vaddr, page_size() as u64))
            .min()
            .unwrap_or(0)
    }

    // --- init / fini --------------------------------------------------------

    #[allow(clippy::only_used_in_recursion)]
    fn run_init(&self, lib: &Arc<NativeLibrary>) {
        if lib.initialized.swap(true, Ordering::SeqCst) {
            return;
        }
        for dep in lib.deps_snapshot() {
            self.run_init(&dep);
        }
        let d = &lib.dyninfo;
        if let Some((va, size)) = d.preinit_array {
            for i in 0..(size / 8) {
                let entry_addr = lib.runtime_addr(va + i * 8) as *const u64;
                // SAFETY: preinit_array is inside a mapped segment.
                let f = unsafe { *entry_addr };
                if f != 0 {
                    call_guest_addr(f as usize, &[]);
                }
            }
        }
        if d.init != 0 {
            call_guest(lib, d.init, &[]);
        }
        if let Some((va, size)) = d.init_array {
            for i in 0..(size / 8) {
                let entry_addr = lib.runtime_addr(va + i * 8) as *const u64;
                // SAFETY: init_array is inside a mapped segment.
                let f = unsafe { *entry_addr };
                if f != 0 {
                    call_guest_addr(f as usize, &[]);
                }
            }
        }
    }

    fn run_fini(&self, lib: &Arc<NativeLibrary>) {
        let d = &lib.dyninfo;
        if let Some((va, size)) = d.fini_array {
            let n = size / 8;
            for i in (0..n).rev() {
                let entry_addr = lib.runtime_addr(va + i * 8) as *const u64;
                // SAFETY: fini_array is inside a mapped segment.
                let f = unsafe { *entry_addr };
                if f != 0 {
                    call_guest_addr(f as usize, &[]);
                }
            }
        }
        if d.fini != 0 {
            call_guest(lib, d.fini, &[]);
        }
    }

    // --- unload -------------------------------------------------------------

    /// Unload a library handle. Runs destructors and unmaps when the last
    /// reference (loader + dependents) goes away.
    pub fn unload(&self, handle: &LibraryHandle) -> Result<()> {
        self.unload_lib(&handle.lib);
        Ok(())
    }

    fn unload_lib(&self, lib: &Arc<NativeLibrary>) {
        if lib.refs.fetch_sub(1, Ordering::SeqCst) != 1 {
            return;
        }
        if lib.initialized.load(Ordering::SeqCst) {
            self.run_fini(lib);
        }
        lib.loaded.store(false, Ordering::Release);
        self.inner.libs.lock().unwrap().retain(|l| l.id != lib.id);
        OBJECTS.lock().unwrap().retain(|l| l.id != lib.id);
        // SAFETY: mapping was created by `map_segments`.
        unsafe {
            libc::munmap(lib.map_base as *mut c_void, lib.map_len);
        }
        for dep in lib.deps_snapshot() {
            self.unload_lib(&dep);
        }
    }

    // --- reporting ----------------------------------------------------------

    /// Build a structured report of the current loader state.
    pub fn report(&self) -> Report {
        let libs = self.inner.libs.lock().unwrap().clone();
        let (forwarded, shimmed) = bionic::counts();
        let libraries = libs
            .iter()
            .map(|lib| {
                let imports = lib.imports.lock().unwrap().clone();
                let mut fwd = 0;
                let mut shm = 0;
                let mut missing = Vec::new();
                let mut versioned = 0;
                for i in &imports {
                    if i.version.is_some() {
                        versioned += 1;
                    }
                    match &i.via {
                        BoundVia::Forwarded => fwd += 1,
                        BoundVia::Shimmed => shm += 1,
                        BoundVia::Missing => missing.push(i.symbol.clone()),
                        BoundVia::Object(_) | BoundVia::SelfObject => {}
                    }
                }
                LibraryReport {
                    name: lib.name.clone(),
                    path: lib.path.as_ref().map(|p| p.display().to_string()),
                    imports_total: imports.len(),
                    imports_forwarded: fwd,
                    imports_shimmed: shm,
                    imports_missing: missing,
                    imports_versioned: versioned,
                    relocations: lib.relocs.len(),
                    tls: lib.tls_modid.is_some(),
                    deps: lib.dyninfo.needed.clone(),
                }
            })
            .collect();
        Report {
            bionic_table: TableReport {
                forwarded,
                shimmed,
                total: forwarded + shimmed,
            },
            libraries,
        }
    }
}

enum ResolveResult {
    Object(Arc<NativeLibrary>, Sym),
    Bionic(Shim),
}

enum ResolvedValue {
    Abs(usize),
    Tls { modid: usize, value: u64 },
    WeakNull,
}

// --- helpers ----------------------------------------------------------------

fn abi_name(machine: u16) -> String {
    match machine {
        183 => "aarch64 (arm64-v8a)".to_string(),
        40 => "arm (armeabi-v7a)".to_string(),
        3 => "i386 (x86)".to_string(),
        62 => "x86_64".to_string(),
        other => format!("unknown ELF machine {other}"),
    }
}

fn malformed(msg: &str) -> LoaderError {
    LoaderError::ElfMalformed(msg.to_string())
}

fn page_size() -> usize {
    static PS: OnceLock<usize> = OnceLock::new();
    *PS.get_or_init(|| {
        // SAFETY: sysconf(_SC_PAGESIZE) is always safe.
        let v = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        if v > 0 {
            v as usize
        } else {
            4096
        }
    })
}

fn align_down(x: u64, a: u64) -> u64 {
    if a == 0 {
        x
    } else {
        x & !(a - 1)
    }
}

fn align_up(x: u64, a: u64) -> u64 {
    if a == 0 {
        x
    } else {
        align_down(x + a - 1, a)
    }
}

fn prot_from_flags(flags: u32) -> c_int {
    let mut prot = 0;
    if flags & PF_R != 0 {
        prot |= libc::PROT_READ;
    }
    if flags & PF_W != 0 {
        prot |= libc::PROT_WRITE;
    }
    if flags & PF_X != 0 {
        prot |= libc::PROT_EXEC;
    }
    if prot == 0 {
        prot = libc::PROT_READ;
    }
    prot
}

fn relro_range(lib: &NativeLibrary, page: u64) -> Option<(usize, usize)> {
    let min = lib
        .phdrs
        .iter()
        .filter(|p| p.p_type == PT_LOAD && p.p_memsz > 0)
        .map(|p| align_down(p.p_vaddr, page))
        .min()?;
    lib.phdrs
        .iter()
        .find(|p| p.p_type == PT_GNU_RELRO)
        .map(|p| {
            let start = lib.map_base + (align_down(p.p_vaddr, page) - min) as usize;
            let end = lib.map_base + (align_up(p.p_vaddr + p.p_memsz, page) - min) as usize;
            (start, end)
        })
}

fn rd_u64_checked(data: &[u8], off: usize) -> Result<u64> {
    let b = data
        .get(off..off + 8)
        .ok_or_else(|| malformed("read out of bounds"))?;
    Ok(u64::from_le_bytes(b.try_into().unwrap()))
}

fn read_leb128(data: &[u8], cur: &mut usize) -> Result<u64> {
    let mut result = 0u64;
    let mut shift = 0;
    loop {
        let byte = *data
            .get(*cur)
            .ok_or_else(|| malformed("truncated leb128"))?;
        *cur += 1;
        result |= ((byte & 0x7f) as u64) << shift;
        if byte & 0x80 == 0 {
            break;
        }
        shift += 7;
        if shift > 63 {
            return Err(malformed("leb128 too long"));
        }
    }
    Ok(result)
}

fn read_sleb128(data: &[u8], cur: &mut usize) -> Result<i64> {
    let mut result = 0i64;
    let mut shift = 0;
    let mut byte;
    loop {
        byte = *data
            .get(*cur)
            .ok_or_else(|| malformed("truncated sleb128"))?;
        *cur += 1;
        result |= ((byte & 0x7f) as i64) << shift;
        shift += 7;
        if byte & 0x80 == 0 {
            break;
        }
        if shift > 63 {
            return Err(malformed("sleb128 too long"));
        }
    }
    if shift < 64 && byte & 0x40 != 0 {
        result |= -1i64 << shift;
    }
    Ok(result)
}

fn tls_init_image(bytes: &[u8], elf: &Elf<'_>, tls: TlsSegment) -> Vec<u8> {
    if tls.filesz == 0 {
        return Vec::new();
    }
    let off = match elf.vaddr_to_offset(tls.vaddr) {
        Some(o) => o,
        None => return Vec::new(),
    };
    let end = off.saturating_add(tls.filesz as usize);
    bytes.get(off..end).map(|s| s.to_vec()).unwrap_or_default()
}

fn call_guest(_lib: &Arc<NativeLibrary>, vaddr: u64, _args: &[usize]) {
    call_guest_addr(_lib.runtime_addr(vaddr), _args);
}

fn call_guest_addr(addr: usize, _args: &[usize]) {
    // SAFETY: address is a guest function with the standard C ABI taking no
    // arguments for the init/fini entry points we call here.
    unsafe {
        let f: extern "C" fn() = std::mem::transmute(addr);
        f();
    }
}

/// Access the loader that should service `dlopen`.
pub(crate) fn current_loader() -> Option<Arc<LoaderInner>> {
    CURRENT_LOADER
        .lock()
        .ok()
        .and_then(|g| g.as_ref().and_then(|w| w.upgrade()))
}

/// Register a `dlopen` handle and return its integer id.
pub(crate) fn register_dl_handle(inner: &Arc<LoaderInner>, lib: Arc<NativeLibrary>) -> usize {
    let id = inner.next_dl_handle.fetch_add(1, Ordering::SeqCst);
    inner.dl_handles.lock().unwrap().insert(id, lib);
    id
}

/// Resolve a `dlopen` handle.
pub(crate) fn dl_handle_lookup(inner: &Arc<LoaderInner>, id: usize) -> Option<Arc<NativeLibrary>> {
    inner.dl_handles.lock().unwrap().get(&id).cloned()
}

/// Remove a `dlopen` handle.
pub(crate) fn dl_handle_remove(inner: &Arc<LoaderInner>, id: usize) -> Option<Arc<NativeLibrary>> {
    inner.dl_handles.lock().unwrap().remove(&id)
}

/// Used by `Loader` to expose config for the report / tests.
impl LoaderInner {
    pub(crate) fn config(&self) -> &LoaderConfig {
        &self.config
    }
}

#[cfg(test)]
mod tests {
    use super::{contained_in, is_safe_dep_name};

    #[test]
    fn rejects_unsafe_dt_needed_names() {
        // W67A #4: guest ELF content must not be able to name a host path.
        for bad in [
            "/lib/x86_64-linux-gnu/libc.so.6",
            "/etc/passwd",
            "../libevil.so",
            "sub/lib.so",
            "..",
            ".",
            "",
            "a\\b.so",
        ] {
            assert!(!is_safe_dep_name(bad), "{bad:?} must be rejected");
        }
        for ok in ["libtestdep.so", "libc++_shared.so", "libfoo.so.1"] {
            assert!(is_safe_dep_name(ok), "{ok:?} must be accepted");
        }
    }

    #[test]
    fn containment_follows_symlinks_out_of_a_root() {
        let dir = std::env::temp_dir().join(format!(
            "acos-native-loader-containment-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("root")).unwrap();
        std::fs::write(dir.join("outside.so"), b"x").unwrap();
        std::fs::write(dir.join("root/inside.so"), b"x").unwrap();
        std::os::unix::fs::symlink(dir.join("outside.so"), dir.join("root/escape.so")).unwrap();

        assert!(contained_in(&dir.join("root/inside.so"), &dir.join("root")));
        assert!(!contained_in(&dir.join("root/escape.so"), &dir.join("root")));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
