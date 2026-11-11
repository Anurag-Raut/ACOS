//! `artspike` — load real ART's `libart.so` through `acos-native-loader`,
//! create a JavaVM with the exported `JNI_CreateJavaVM`, and run a DEX.
//!
//! Steps are independently addressable so a failure names the exact gap:
//!   `--step load`    map libart.so + its DT_NEEDED closure, print the report
//!   `--step create`  call JNI_CreateJavaVM
//!   `--step run`     FindClass + CallStaticVoidMethodA("main", String[])
//!   `--step natives` RegisterNatives on a class from `--dex`, then run it
//!
//! Everything here is *research scratch*: trusted local files only.

use std::os::raw::{c_char, c_int, c_void};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use acos_native_loader::{Loader, LoaderConfig, MissingPolicy};

// --- JNI ABI ---------------------------------------------------------------

type JInt = i32;
type JBoolean = u8;

#[repr(C)]
struct JavaVMOption {
    option_string: *const c_char,
    extra_info: *mut c_void,
}

#[repr(C)]
struct JavaVMInitArgs {
    version: JInt,
    n_options: JInt,
    options: *mut JavaVMOption,
    ignore_unrecognized: JBoolean,
}

const JNI_VERSION_1_6: JInt = 0x0001_0006;

type FnCreateJavaVM =
    unsafe extern "C" fn(*mut *mut c_void, *mut *mut c_void, *mut c_void) -> JInt;

// JNINativeInterface_ indices (standard JNI + the 4 leading reserved slots).
const ENV_GET_VERSION: usize = 4;
const ENV_FIND_CLASS: usize = 6;
const ENV_EXCEPTION_OCCURRED: usize = 15;
const ENV_EXCEPTION_DESCRIBE: usize = 16;
const ENV_EXCEPTION_CLEAR: usize = 17;
const ENV_THROW_NEW: usize = 14;
const ENV_GET_STATIC_METHOD_ID: usize = 113;
const ENV_CALL_STATIC_VOID_METHOD_A: usize = 143;
const ENV_NEW_STRING_UTF: usize = 167;
const ENV_NEW_OBJECT_ARRAY: usize = 172;
const ENV_REGISTER_NATIVES: usize = 215;
// JNIInvokeInterface_ indices.
const VM_DESTROY: usize = 4;
const VM_GET_ENV: usize = 7;

unsafe fn env_fn(env: *mut c_void, idx: usize) -> *const c_void {
    let table = *(env as *const *const *const c_void);
    *table.add(idx)
}

macro_rules! jni {
    ($env:expr, $idx:expr, $ty:ty) => {{
        let f = unsafe { env_fn($env, $idx) };
        assert!(!f.is_null(), "null JNI slot {}", $idx);
        unsafe { std::mem::transmute::<*const c_void, $ty>(f) }
    }};
}

#[repr(C)]
struct JNINativeMethod {
    name: *const c_char,
    signature: *const c_char,
    fn_ptr: *mut c_void,
}

fn cstring(s: &str) -> std::ffi::CString {
    std::ffi::CString::new(s).unwrap_or_default()
}

// --- log sink --------------------------------------------------------------

struct StderrLog;
impl acos_native_loader::LogSink for StderrLog {
    fn log(&self, priority: i32, tag: &str, message: &str) {
        eprintln!("[art {priority}] {tag}: {message}");
    }
}

// --- main ------------------------------------------------------------------

struct Opts {
    step: String,
    sysroot: PathBuf,
    dex: Option<PathBuf>,
    class: String,
    method: String,
    extra_options: Vec<String>,
    confine: bool,
    lazy: bool,
    mem_gib: u64,
    bootclasspath: Option<String>,
    image: Option<String>,
    no_image: bool,
    xtrace: bool,
    natives: bool,
}

fn main() {
    let mut o = Opts {
        step: "load".into(),
        sysroot: PathBuf::from("/home/Qui/Projects/acos/.cache/art-exp/sysroot"),
        dex: None,
        class: "LHello;".into(),
        method: "main".into(),
        extra_options: Vec::new(),
        confine: false,
        lazy: false,
        mem_gib: 24,
        bootclasspath: None,
        image: None,
        no_image: false,
        xtrace: false,
        natives: false,
    };
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--step" => {
                i += 1;
                o.step = args[i].clone();
            }
            "--sysroot" => {
                i += 1;
                o.sysroot = PathBuf::from(&args[i]);
            }
            "--dex" => {
                i += 1;
                o.dex = Some(PathBuf::from(&args[i]));
            }
            "--class" => {
                i += 1;
                o.class = args[i].clone();
            }
            "--method" => {
                i += 1;
                o.method = args[i].clone();
            }
            "--option" => {
                i += 1;
                o.extra_options.push(args[i].clone());
            }
            "--confine" => o.confine = true,
            "--lazy" => o.lazy = true,
            "--mem-gib" => {
                i += 1;
                o.mem_gib = args[i].parse().unwrap_or(24);
            }
            "--bootclasspath" => {
                i += 1;
                o.bootclasspath = Some(args[i].clone());
            }
            "--image" => {
                i += 1;
                o.image = Some(args[i].clone());
            }
            "--noimage" => o.no_image = true,
            "--xtrace" => o.xtrace = true,
            "--natives" => o.natives = true,
            other => {
                eprintln!("artspike: unknown argument {other}");
                std::process::exit(2);
            }
        }
        i += 1;
    }

    if o.confine {
        install_sandbox(&o);
    }

    let libart = find_lib(&o.sysroot, "libart.so").unwrap_or_else(|| {
        eprintln!("artspike: libart.so not found under {}", o.sysroot.display());
        std::process::exit(3);
    });
    eprintln!("artspike: libart = {}", libart.display());

    // B-LOADER: `/system`, `/apex`, ... must resolve into the extracted sysroot.
    acos_native_loader::bionic::art_gaps::set_sysroot(&o.sysroot);
    // The spike has no System.loadLibrary, so disable ART's eager preload of
    // the public NDK libraries (each one is a full dlopen of a bionic library).
    let pubs = workdir().join("etc/public.libraries.txt");
    let _ = std::fs::create_dir_all(pubs.parent().unwrap());
    let _ = std::fs::write(&pubs, "libz.so\n");
    acos_native_loader::bionic::art_gaps::set_path_override(
        "/system/etc/public.libraries.txt",
        pubs.to_string_lossy().to_string(),
    );

    let mut config = LoaderConfig::default()
        .with_search_path(&o.sysroot)
        .with_missing_policy(if o.lazy {
            MissingPolicy::LazyTrap
        } else {
            MissingPolicy::Strict
        })
        .with_log_sink(Arc::new(StderrLog));
    for dir in search_dirs(&o.sysroot) {
        config = config.with_search_path(dir);
    }
    config.require_confinement = o.confine;
    // Properties ART reads at startup.
    for (k, v) in [
        ("ro.build.version.sdk", "34"),
        ("ro.build.version.release", "14"),
        ("ro.product.cpu.abi", "x86_64"),
        ("ro.product.cpu.abilist", "x86_64"),
        ("ro.debuggable", "1"),
        ("ro.zygote", "zygote64"),
        ("ro.kernel.qemu", "1"),
        ("dalvik.vm.heapsize", "256m"),
        ("dalvik.vm.dex2oat-filter", "verify"),
        ("persist.device_config.runtime_native_boot.enable_uffd_gc", "false"),
    ] {
        config.system_properties.insert(k.into(), v.into());
    }

    let loader = Loader::new(config);
    let t0 = std::time::Instant::now();
    let handle = match loader.load_file(&libart) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("artspike: LOAD FAILED [{}] {e}", e.code());
            match &e {
                acos_native_loader::LoaderError::UnsupportedSymbols(list) => {
                    for l in list {
                        eprintln!("  missing in {}: {:?}", l.library, l.symbols);
                    }
                }
                acos_native_loader::LoaderError::DepNotFound {
                    needed,
                    requester,
                    searched,
                } => {
                    eprintln!("  needed {needed} by {requester}; searched {searched:?}");
                }
                _ => {}
            }
            std::process::exit(10);
        }
    };
    eprintln!(
        "artspike: loaded libart.so in {:?} (bias 0x{:x})",
        t0.elapsed(),
        handle.library().bias()
    );
    let report = loader.report();
    if o.xtrace {
        eprintln!("{}", report.to_json());
    }
    // bionic's linker namespaces pre-populate a "global group" that every
    // namespace can resolve against (zygote loads libcutils/libselinux before
    // any APEX library). Our loader has no such group, so preload the members
    // libart's palette/selinux tail imports from.
    for name in ["libcutils.so", "libselinux.so"] {
        if let Some(p) = find_lib(&o.sysroot, name) {
            match loader.load_file(&p) {
                Ok(_) => eprintln!("artspike: preloaded {name}"),
                Err(e) => eprintln!("artspike: preload {name} failed [{}] {e}", e.code()),
            }
        }
    }
    if std::env::var_os("AEXP_TRACE_MAPS").is_some() {
        for (name, base, len) in acos_native_loader::loader::loaded_objects() {
            eprintln!("artspike: map {name} 0x{base:x}+0x{len:x}");
        }
    }
    if o.step == "load" {
        println!("STEP load: OK");
        return;
    }

    let create = match handle.symbol("JNI_CreateJavaVM") {
        Some(p) => p,
        None => {
            eprintln!("artspike: libart.so does not export JNI_CreateJavaVM");
            std::process::exit(11);
        }
    };
    let create: FnCreateJavaVM = unsafe { std::mem::transmute(create) };
    eprintln!("artspike: JNI_CreateJavaVM at {create:p}");

    // --- VM options --------------------------------------------------------
    let mut options: Vec<String> = Vec::new();
    options.push("-Xint".into());
    options.push("-XjdwpProvider:none".into());
    options.push("-Xverify:none".into());
    let bcp = o.bootclasspath.clone().unwrap_or_else(|| detect_bootclasspath(&o.sysroot));
    let mut bcp = bcp;
    if let Some(dex) = &o.dex {
        // The spike has no DexClassLoader wiring yet: put the DEX on the boot
        // class path so FindClass can see it in `-Xnoimage-dex2oat` mode.
        bcp.push(':');
        bcp.push_str(&dex.to_string_lossy());
    }
    options.push(format!("-Xbootclasspath:{bcp}"));
    if let Some(img) = &o.image {
        options.push(format!("-Ximage:{img}"));
    } else if o.no_image {
        options.push("-Xnoimage-dex2oat".into());
    }
    options.push(format!("-Djava.io.tmpdir={}/run/tmp", workdir().display()));
    options.push(format!("-Djava.library.path={}/run/lib", workdir().display()));
    options.extend(o.extra_options.iter().cloned());
    eprintln!("artspike: options: {options:?}");

    let cstrings: Vec<std::ffi::CString> = options.iter().map(|s| cstring(s)).collect();
    let mut vm_opts: Vec<JavaVMOption> = cstrings
        .iter()
        .map(|c| JavaVMOption {
            option_string: c.as_ptr(),
            extra_info: std::ptr::null_mut(),
        })
        .collect();
    let mut init = JavaVMInitArgs {
        version: JNI_VERSION_1_6,
        n_options: vm_opts.len() as JInt,
        options: vm_opts.as_mut_ptr(),
        ignore_unrecognized: 0,
    };

    let mut vm: *mut c_void = std::ptr::null_mut();
    let mut env: *mut c_void = std::ptr::null_mut();
    let t1 = std::time::Instant::now();
    eprintln!("artspike: calling JNI_CreateJavaVM...");
    let rc = unsafe { create(&mut vm, &mut env, &mut init as *mut _ as *mut c_void) };
    eprintln!(
        "artspike: JNI_CreateJavaVM -> {rc} after {:?} (vm={vm:p} env={env:p})",
        t1.elapsed()
    );
    if rc != 0 || vm.is_null() || env.is_null() {
        eprintln!("artspike: CREATE FAILED");
        std::process::exit(12);
    }
    let version: unsafe extern "C" fn(*mut c_void) -> JInt =
        jni!(env, ENV_GET_VERSION, unsafe extern "C" fn(*mut c_void) -> JInt);
    eprintln!("artspike: JNIEnv version 0x{:x}", unsafe { version(env) });
    if o.step == "create" {
        println!("STEP create: OK");
        return;
    }

    // --- run a DEX ---------------------------------------------------------
    let Some(dex) = o.dex.clone() else {
        eprintln!("artspike: --step run needs --dex");
        std::process::exit(2);
    };
    // Hand the dex to ART by appending its directory to the class path.
    let dex_dir = dex.parent().unwrap_or(Path::new("."));
    let find_class: unsafe extern "C" fn(*mut c_void, *const c_char) -> *mut c_void =
        jni!(env, ENV_FIND_CLASS, unsafe extern "C" fn(*mut c_void, *const c_char) -> *mut c_void);
    let get_static: unsafe extern "C" fn(
        *mut c_void,
        *mut c_void,
        *const c_char,
        *const c_char,
    ) -> *mut c_void = jni!(
        env,
        ENV_GET_STATIC_METHOD_ID,
        unsafe extern "C" fn(*mut c_void, *mut c_void, *const c_char, *const c_char) -> *mut c_void
    );
    let call_static: unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void, *const i64) =
        jni!(
            env,
            ENV_CALL_STATIC_VOID_METHOD_A,
            unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void, *const i64)
        );
    let new_array: unsafe extern "C" fn(
        *mut c_void,
        JInt,
        *mut c_void,
        *mut c_void,
    ) -> *mut c_void = jni!(
        env,
        ENV_NEW_OBJECT_ARRAY,
        unsafe extern "C" fn(*mut c_void, JInt, *mut c_void, *mut c_void) -> *mut c_void
    );
    let exception_occurred: unsafe extern "C" fn(*mut c_void) -> *mut c_void = jni!(
        env,
        ENV_EXCEPTION_OCCURRED,
        unsafe extern "C" fn(*mut c_void) -> *mut c_void
    );
    let exception_describe: unsafe extern "C" fn(*mut c_void) =
        jni!(env, ENV_EXCEPTION_DESCRIBE, unsafe extern "C" fn(*mut c_void));
    let exception_clear: unsafe extern "C" fn(*mut c_void) =
        jni!(env, ENV_EXCEPTION_CLEAR, unsafe extern "C" fn(*mut c_void));

    let check = |what: &str| {
        let ex = unsafe { exception_occurred(env) };
        if !ex.is_null() {
            eprintln!("artspike: JNI exception after {what}:");
            unsafe { exception_describe(env) };
            unsafe { exception_clear(env) };
            return false;
        }
        true
    };

    // Append the dex directory to the class path via the system property ART
    // honours for `-Xbootclasspath` extras: use DexClassLoader through JNI is
    // heavier, so put the dex *file* on the boot class path through a
    // runtime-visible property is not possible; instead the spike's DEX is
    // copied next to the boot jars and named on the bootclasspath by the
    // caller. As a convenience, `--dex` sets `CLASSPATH` for ART.
    std::env::set_var("CLASSPATH", dex_dir);
    eprintln!("artspike: CLASSPATH={}", dex_dir.display());

    let cls_name = cstring(o.class.trim_start_matches('L').trim_end_matches(';'));
    let cls = unsafe { find_class(env, cls_name.as_ptr()) };
    if !check("FindClass") || cls.is_null() {
        eprintln!("artspike: FindClass({}) failed", o.class);
        std::process::exit(13);
    }
    eprintln!("artspike: FindClass({}) = {cls:p}", o.class);

    let m_name = cstring(&o.method);
    let m_sig = cstring("([Ljava/lang/String;)V");
    let mid = unsafe { get_static(env, cls, m_name.as_ptr(), m_sig.as_ptr()) };
    if !check("GetStaticMethodID") || mid.is_null() {
        eprintln!("artspike: GetStaticMethodID failed");
        std::process::exit(14);
    }

    let string_cls = unsafe { find_class(env, cstring("java/lang/String").as_ptr()) };
    let args_array = unsafe { new_array(env, 0, string_cls, std::ptr::null_mut()) };
    if !check("NewObjectArray") {
        std::process::exit(15);
    }
    let mut jargs: [i64; 1] = [args_array as i64];

    if o.natives {
        // Step 4: register a Rust-implemented native on the DEX class.
        unsafe extern "C" fn native_echo(env: *mut c_void, _this: *mut c_void) -> i32 {
            let get_version: unsafe extern "C" fn(*mut c_void) -> JInt =
                jni!(env, ENV_GET_VERSION, unsafe extern "C" fn(*mut c_void) -> JInt);
            let v = get_version(env);
            println!("[rust native] called on tid {} (env version 0x{v:x})", unsafe {
                libc::syscall(libc::SYS_gettid)
            });
            42
        }
        let n_name = cstring("nativeEcho");
        let n_sig = cstring("()I");
        let mut methods = [JNINativeMethod {
            name: n_name.as_ptr(),
            signature: n_sig.as_ptr(),
            fn_ptr: native_echo as *const () as *mut c_void,
        }];
        let register: unsafe extern "C" fn(
            *mut c_void,
            *mut c_void,
            *mut JNINativeMethod,
            JInt,
        ) -> JInt = jni!(
            env,
            ENV_REGISTER_NATIVES,
            unsafe extern "C" fn(*mut c_void, *mut c_void, *mut JNINativeMethod, JInt) -> JInt
        );
        let rrc = unsafe { register(env, cls, methods.as_mut_ptr(), 1) };
        eprintln!("artspike: RegisterNatives -> {rrc}");
        check("RegisterNatives");
    }

    eprintln!("artspike: calling {}.{}(String[])", o.class, o.method);
    let t2 = std::time::Instant::now();
    unsafe { call_static(env, cls, mid, jargs.as_ptr()) };
    eprintln!("artspike: main returned after {:?}", t2.elapsed());
    if !check("CallStaticVoidMethodA") {
        std::process::exit(16);
    }

    let destroy: unsafe extern "C" fn(*mut c_void) -> JInt =
        jni!(vm, VM_DESTROY, unsafe extern "C" fn(*mut c_void) -> JInt);
    let _ = destroy;
    println!("STEP run: OK");
}

// --- helpers ---------------------------------------------------------------

fn workdir() -> PathBuf {
    let p = PathBuf::from("/home/Qui/Projects/acos/.cache/art-exp/run");
    let _ = std::fs::create_dir_all(p.join("tmp"));
    let _ = std::fs::create_dir_all(p.join("lib"));
    p
}

fn find_lib(root: &Path, name: &str) -> Option<PathBuf> {
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.file_name().and_then(|s| s.to_str()) == Some(name) {
                // Prefer the ART apex copy when several exist.
                if p.to_string_lossy().contains("com.android.art") {
                    return Some(p);
                }
            }
        }
    }
    // second pass: any copy
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.file_name().and_then(|s| s.to_str()) == Some(name) {
                return Some(p);
            }
        }
    }
    None
}

/// Every directory under the sysroot that contains at least one `.so`, with the
/// ART apex searched first (bionic linker namespace order).
fn search_dirs(root: &Path) -> Vec<PathBuf> {
    let mut seen = std::collections::BTreeSet::new();
    let mut dirs = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        let mut has_so = false;
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().and_then(|s| s.to_str()) == Some("so") {
                has_so = true;
            }
        }
        if has_so && seen.insert(d.clone()) {
            dirs.push(d);
        }
    }
    dirs.sort_by_key(|d| {
        let s = d.to_string_lossy().to_string();
        let rank = if s.contains("apex/com.android.art/lib64") {
            0
        } else if s.contains("apex/com.android.runtime/lib64") {
            1
        } else if s.contains("apex/") && s.ends_with("lib64") {
            2
        } else {
            3
        };
        (rank, s)
    });
    dirs
}

fn detect_bootclasspath(root: &Path) -> String {
    let want = ["core-oj.jar", "core-libart.jar", "core-icu4j.jar"];
    let mut found: Vec<PathBuf> = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if let Some(f) = p.file_name().and_then(|s| s.to_str()) {
                if want.contains(&f) {
                    found.push(p);
                }
            }
        }
    }
    found.sort();
    // de-duplicate by file name, keeping the shortest (apex javalib) path.
    let mut seen = std::collections::BTreeSet::new();
    let mut out = Vec::new();
    for p in &found {
        if let Some(f) = p.file_name().and_then(|s| s.to_str()) {
            if seen.insert(f.to_string()) {
                out.push(p.to_string_lossy().to_string());
            }
        }
    }
    eprintln!("artspike: bootclasspath auto-detected: {out:?}");
    out.join(":")
}

fn install_sandbox(o: &Opts) {
    use acos_sandbox::{ConfineProfile, Layer, RlimitProfile};
    let mut profile = ConfineProfile {
        read_only_paths: vec![
            o.sysroot.clone(),
            PathBuf::from("/proc"),
            PathBuf::from("/sys"),
            PathBuf::from("/usr/lib"),
            PathBuf::from("/lib"),
        ],
        read_write_paths: vec![workdir()],
        required: vec![
            Layer::NoNewPrivs,
            Layer::CloseFds,
            Layer::SanitizeEnv,
            Layer::Rlimits,
            Layer::Landlock,
            Layer::Seccomp,
        ],
        keep_fds: vec![0, 1, 2],
        env: vec![],
        rlimits: RlimitProfile {
            address_space_bytes: Some(o.mem_gib * 1024 * 1024 * 1024),
            nofile: Some(4096),
            nproc: Some(8192),
            cpu_seconds: Some(600),
            core: 0,
            file_size_bytes: Some(2 * 1024 * 1024 * 1024),
            stack_bytes: Some(256 * 1024 * 1024),
            memlock_bytes: None,
        },
        ..Default::default()
    };
    profile
        .env
        .push(("ANDROID_ROOT".into(), o.sysroot.join("system").to_string_lossy().into()));
    profile.env.push((
        "ANDROID_ART_ROOT".into(),
        o.sysroot
            .join("system/apex/com.android.art")
            .to_string_lossy()
            .into(),
    ));
    match acos_sandbox::confine(&profile) {
        Ok(rep) => eprintln!(
            "artspike: confinement active={} report={:?}",
            acos_sandbox::confinement_active(),
            rep.layers.iter().map(|l| (l.layer, l.status)).collect::<Vec<_>>()
        ),
        Err(e) => {
            eprintln!("artspike: CONFINE FAILED: {e}");
            std::process::exit(4);
        }
    }
}
