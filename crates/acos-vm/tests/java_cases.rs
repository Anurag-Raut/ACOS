//! JVM-referenced edge-case tests that need a real DEX. The Java sources live
//! in `tests/java/`; this test compiles them with the host `javac`/`d8` (build
//! tools only, never the execution path) and skips loudly if they are absent.

use std::path::{Path, PathBuf};
use std::process::Command;

use acos_dex::DexFile;
use acos_vm::{Vm, VmError, VmOptions};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn compile_case(name: &str) -> Option<PathBuf> {
    let root = repo_root();
    let out = root.join("target/tmp/acos-java-cases").join(name);
    let dex = out.join("classes.dex");
    if dex.is_file() {
        return Some(dex);
    }
    let javac = which("javac")?;
    let sdk = std::env::var("ANDROID_HOME").unwrap_or_else(|_| "/home/Qui/Android/Sdk".to_string());
    let d8 = PathBuf::from(format!("{sdk}/build-tools/35.0.0/d8"));
    let android_jar = PathBuf::from(format!("{sdk}/platforms/android-34/android.jar"));
    if !d8.is_file() || !android_jar.is_file() {
        return None;
    }
    let src = root
        .join("crates/acos-vm/tests/java")
        .join(format!("{name}.java"));
    if !src.is_file() {
        return None;
    }
    let classes = out.join("classes");
    std::fs::create_dir_all(&classes).ok()?;
    let status = Command::new(javac)
        .args(["--release", "11", "-encoding", "UTF-8", "-d"])
        .arg(&classes)
        .arg(&src)
        .status()
        .ok()?;
    if !status.success() {
        return None;
    }
    let mut class_files: Vec<PathBuf> = std::fs::read_dir(&classes)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().map(|x| x == "class").unwrap_or(false))
        .collect();
    class_files.sort();
    let status = Command::new(d8)
        .arg("--lib")
        .arg(&android_jar)
        .args(["--min-api", "26", "--output"])
        .arg(&out)
        .args(&class_files)
        .status()
        .ok()?;
    if !status.success() || !dex.is_file() {
        return None;
    }
    Some(dex)
}

fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let p = dir.join(name);
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

fn run(dex: &Path, main: &str, heap_max: usize) -> (i32, String, Option<VmError>) {
    let outcome = run_full(dex, main, heap_max);
    (outcome.exit_code, outcome.stdout, outcome.vm_error)
}

fn run_full(dex: &Path, main: &str, heap_max: usize) -> acos_vm::RunOutcome {
    let mut vm = Vm::new(VmOptions {
        heap_max_bytes: heap_max,
        gc_stress: false,
        ..VmOptions::default()
    })
    .unwrap();
    vm.add_dex(DexFile::parse(std::fs::read(dex).unwrap()).unwrap());
    vm.run(main, &[])
}

#[test]
fn stack_overflow_is_catchable() {
    let Some(dex) = acos_test_support::require_some(compile_case("StackOverflowMain"), "stack_overflow_is_catchable", "javac/d8 unavailable") else {
        return;
    };
    let (code, stdout, _) = run(&dex, "StackOverflowMain", 256 * 1024 * 1024);
    assert_eq!(code, 0);
    assert_eq!(stdout, "caught-stack-overflow\ndone\n");
}

#[test]
fn out_of_memory_is_catchable_with_small_heap() {
    let Some(dex) = acos_test_support::require_some(compile_case("OomMain"), "out_of_memory_is_catchable_with_small_heap", "javac/d8 unavailable") else {
        return;
    };
    let (code, stdout, _) = run(&dex, "OomMain", 4 * 1024 * 1024);
    assert_eq!(code, 0);
    assert_eq!(stdout, "caught-oom\ndone\n");
}

#[test]
fn unsupported_java_api_is_structured() {
    let Some(dex) = acos_test_support::require_some(compile_case("UnsupportedMain"), "unsupported_java_api_is_structured", "javac/d8 unavailable") else {
        return;
    };
    let (code, _stdout, err) = run(&dex, "UnsupportedMain", 256 * 1024 * 1024);
    assert_eq!(code, 70, "unsupported run must not succeed");
    match err {
        Some(VmError::UnsupportedApi { api, at }) => {
            assert!(
                api.contains("java/util/concurrent/Executors"),
                "api was {api}"
            );
            assert!(!at.is_empty(), "call site must be reported, got empty");
        }
        other => panic!("expected UNSUPPORTED_JAVA_API, got {other:?}"),
    }
}

/// The exception floor: a `<clinit>` failure must stop at the synchronous
/// Rust→Java call base and be rethrown into the outer Java frames (as
/// `ExceptionInInitializerError`), not unwind past it with a stale frame index.
#[test]
fn exception_floor_rethrows_at_the_rust_call_base() {
    let Some(dex) = acos_test_support::require_some(compile_case("ExceptionFloorMain"), "exception_floor_rethrows_at_the_rust_call_base", "javac/d8 unavailable") else {
        return;
    };
    let outcome = run_full(&dex, "ExceptionFloorMain", 256 * 1024 * 1024);
    assert!(
        outcome.vm_error.is_none(),
        "unexpected VM error: {:?}",
        outcome.vm_error
    );
    assert_eq!(
        outcome.exit_code, 0,
        "stdout:\n{}\nstderr:\n{}",
        outcome.stdout, outcome.stderr
    );
    assert_eq!(outcome.stdout, "eie:boom\nncdfe\ndone\n");
}

// ---------------------------------------------------------------------------
// W95-A React-Native blockers 3 & 4: static reflection and thread stack traces
// ---------------------------------------------------------------------------

fn real_boot_dir() -> PathBuf {
    repo_root().join(".cache/boot/android-34")
}

/// Run a case against the real Android 14 boot classpath (where
/// `java.lang.reflect.Field` and `dalvik.system.VMStack` exist).
fn run_real(dex: &Path, main: &str, gc_stress: bool, ir: bool) -> acos_vm::RunOutcome {
    acos_vm::exec::set_enabled(Some(ir));
    let mut vm = Vm::new(VmOptions {
        heap_max_bytes: 256 * 1024 * 1024,
        gc_stress,
        corelib: acos_vm::CoreLib::Real,
        boot_dir: Some(real_boot_dir()),
        ..VmOptions::default()
    })
    .unwrap();
    vm.add_dex(DexFile::parse(std::fs::read(dex).unwrap()).unwrap());
    let outcome = vm.run(main, &[]);
    acos_vm::exec::set_enabled(None);
    outcome
}

fn real_corelib_available() -> bool {
    real_boot_dir().join("BOOTCLASSPATH").is_file()
}

/// W88-A blocker 3: `Field.set` on a static field must initialise the class
/// first, so `<clinit>` cannot clobber the value a later compiled `getstatic`
/// observes. Checked on both tiers and under GC stress.
#[test]
fn reflection_static_set_survives_compiled_getstatic() {
    let Some(dex) = acos_test_support::require_some(compile_case("ReflectStaticInitMain"), "reflection_static_set_survives_compiled_getstatic", "javac/d8 unavailable") else {
        return;
    };
    if !real_corelib_available() {
        acos_test_support::require_prereq(false, "reflection_static_set_survives_compiled_getstatic", "reflection_static_set_survives_compiled_getstatic: real boot classpath absent");
        return;
    }
    for (ir, stress) in [(true, false), (false, false), (true, true), (false, true)] {
        let outcome = run_real(&dex, "ReflectStaticInitMain", stress, ir);
        assert_eq!(
            outcome.exit_code, 0,
            "ir={ir} gc_stress={stress}: {}",
            outcome.stderr
        );
        assert_eq!(
            outcome.stdout, "true\ntrue\ntrue\ntrue\n",
            "ir={ir} gc_stress={stress}"
        );
    }
}

/// W88-A blocker 4: `Thread.getStackTrace()` / `Throwable.getStackTrace()` must
/// return real frames under the real corelib. An empty array made Kotlin's
/// `Intrinsics.createParameterIsNullExceptionMessage` index `[0]` and throw a
/// spurious `ArrayIndexOutOfBoundsException`.
#[test]
fn thread_stack_traces_are_non_empty_under_real_corelib() {
    let Some(dex) = compile_case("VmStackMain") else {
        acos_test_support::require_prereq(false, "thread_stack_traces_are_non_empty_under_real_corelib", "thread_stack_traces_are_non_empty_under_real_corelib: javac/d8 unavailable");
        return;
    };
    if !real_corelib_available() {
        acos_test_support::require_prereq(false, "thread_stack_traces_are_non_empty_under_real_corelib", "thread_stack_traces_are_non_empty_under_real_corelib: real boot classpath absent");
        return;
    }
    for (ir, stress) in [(true, false), (false, false), (true, true)] {
        let outcome = run_real(&dex, "VmStackMain", stress, ir);
        assert_eq!(
            outcome.exit_code, 0,
            "ir={ir} gc_stress={stress}: {}",
            outcome.stderr
        );
        assert_eq!(
            outcome.stdout, "true\ntrue\ntrue\n",
            "ir={ir} gc_stress={stress}"
        );
    }
}

// ---------------------------------------------------------------------------
// W123-A: Android 14 `AtomicReference` is `VarHandle`-backed
// ---------------------------------------------------------------------------

/// Every `AtomicReference` accessor in the Android 14 core library is a
/// signature-polymorphic `VarHandle` call (opcode `invoke-polymorphic`), which
/// aborted the React Native bridge at `HermesExecutorFactory.create`
/// (`AtomicReference.compareAndSet pc=2`, W120-A). The VM now supplies the
/// `value` reference storage directly in `natives/vmrt.rs`; this pins the whole
/// surface, including GC stress (the field is a traced reference).
#[test]
fn atomic_reference_varhandle_surface_under_real_corelib() {
    let Some(dex) = compile_case("AtomicReferenceMain") else {
        acos_test_support::require_prereq(false, "atomic_reference_varhandle_surface_under_real_corelib", "atomic_reference_varhandle_surface_under_real_corelib: javac/d8 unavailable");
        return;
    };
    if !real_corelib_available() {
        acos_test_support::require_prereq(false, "atomic_reference_varhandle_surface_under_real_corelib", "atomic_reference_varhandle_surface_under_real_corelib: real boot classpath absent");
        return;
    }
    let expected = "get=a\ngetAndSet=a\nafter=b\ncas-ok=true\ncas-fail=false\nval=c\n\
                    weak=true\nfinal=e\nnull=true\ncas-null=true\nn=z\n";
    for (ir, stress) in [(true, false), (false, false), (true, true), (false, true)] {
        let outcome = run_real(&dex, "AtomicReferenceMain", stress, ir);
        assert!(
            outcome.vm_error.is_none(),
            "ir={ir} gc_stress={stress}: {:?}",
            outcome.vm_error
        );
        assert_eq!(
            outcome.exit_code, 0,
            "ir={ir} gc_stress={stress}: {}",
            outcome.stderr
        );
        assert_eq!(outcome.stdout, expected, "ir={ir} gc_stress={stress}");
    }
}
