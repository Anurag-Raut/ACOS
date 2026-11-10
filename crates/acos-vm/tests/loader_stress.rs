//! Stress the lazy class loader with nested class initialisation.
//!
//! F-0008's `Build$VERSION.<clinit>` runs while `android.os.Build` is loaded on
//! demand, so the nest bridge `Build.-$$Nest$smgetString` is resolved from a
//! class that the lazy boot-classpath loader has just published. This suite
//! drives that path in every order it can and asserts the loader never exposes
//! a class whose methods are still missing.
//!
//! These tests skip loudly when `.cache/boot/android-34` is absent.

use std::path::{Path, PathBuf};

use acos_vm::vm::InvokeKind;
use acos_vm::{CoreLib, JavaValue, Vm, VmOptions};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn boot_available() -> bool {
    repo_root()
        .join(".cache/boot/android-34/BOOTCLASSPATH")
        .is_file()
}

fn real_vm(gc_stress: bool) -> Vm {
    Vm::new(VmOptions {
        heap_max_bytes: 1024 * 1024 * 1024,
        gc_stress,
        corelib: CoreLib::Real,
        boot_dir: None,
        trace_natives: false,
    })
    .expect("real-corelib VM (boot classpath must be present)")
}

/// `ensure_host_thread` is private, so prime the host thread with a real static
/// call before driving `initialize_class` directly.
fn prime_host_thread(vm: &mut Vm) {
    vm.call_static("java.lang.System", "currentTimeMillis", "()J", &[])
        .expect("System.currentTimeMillis primes the host thread");
}

/// Classes whose initialisation nests: `Build$VERSION.<clinit>` invokes
/// `Build.-$$Nest$smgetString`, and `Build.<clinit>` reads the same properties.
const NESTED_CLASSES: &[&str] = &[
    "android.os.Build",
    "android.os.Build$VERSION",
    "android.os.Build$VERSION_CODES",
    "android.os.Build$Partition",
    "android.os.SystemProperties",
    "android.text.TextUtils",
    "android.util.ArraySet",
    "java.util.Optional",
];

/// Other boot-classpath classes that force different lazy jars to load first.
/// These are only *loaded* (their `<clinit>` may need natives outside this
/// test's scope); loading is what exercises the lazy jar cursor.
const CROSS_JAR_CLASSES: &[&str] = &[
    "java.lang.String",
    "java.util.ArrayList",
    "com.android.icu.charset.CharsetICU",
    "com.android.okhttp.OkHttpClient",
    "org.apache.xml.serializer.SerializerBase",
    "com.android.internal.telephony.RIL",
    "android.graphics.Rect",
];

fn load_and_initialize(vm: &mut Vm, class: &str) -> u32 {
    let desc = format!("L{};", class.replace('.', "/"));
    let cid = vm
        .load_class(&desc)
        .unwrap_or_else(|e| panic!("load {class}: {e:?}"));
    vm.initialize_class(cid)
        .unwrap_or_else(|e| panic!("initialize {class}: {e:?}"));
    cid
}

/// The nest bridge must resolve on a `Build` that the lazy loader published
/// while `Build$VERSION.<clinit>` was already running.
fn assert_nest_bridge_resolves(vm: &mut Vm) {
    let build = vm
        .load_class("Landroid/os/Build;")
        .expect("android.os.Build loads");
    let mid = vm
        .find_method(
            build,
            "-$$Nest$smgetString",
            "(Ljava/lang/String;)Ljava/lang/String;",
            InvokeKind::Static,
        )
        .expect("Build.-$$Nest$smgetString resolves");
    assert!(mid != 0);
    // And the class carries its complete declared method set (`getString` is
    // private static, so it lives in the direct map, not the vtable).
    assert!(!vm.classes[build as usize].declared_methods.is_empty());
    assert!(vm.classes[build as usize].direct_map.contains_key(&(
        "getString".to_string(),
        "(Ljava/lang/String;)Ljava/lang/String;".to_string()
    )));
}

/// Loading `Build$VERSION` first must lazily pull framework.jar and leave both
/// halves of the nest complete.
#[test]
fn build_version_first_then_build() {
    if !boot_available() {
        eprintln!("SKIP build_version_first_then_build: .cache/boot/android-34 absent");
        return;
    }
    let mut vm = real_vm(false);
    prime_host_thread(&mut vm);
    load_and_initialize(&mut vm, "android.os.Build$VERSION");
    assert_nest_bridge_resolves(&mut vm);
    load_and_initialize(&mut vm, "android.os.Build");
    assert_nest_bridge_resolves(&mut vm);
    assert_eq!(
        vm.get_static_field("android.os.Build$VERSION", "SDK_INT")
            .unwrap(),
        JavaValue::Int(34)
    );
}

/// Loading `Build` first then `Build$VERSION` must take the other branch and
/// reach the same complete state.
#[test]
fn build_first_then_build_version() {
    if !boot_available() {
        eprintln!("SKIP build_first_then_build_version: .cache/boot/android-34 absent");
        return;
    }
    let mut vm = real_vm(false);
    prime_host_thread(&mut vm);
    load_and_initialize(&mut vm, "android.os.Build");
    load_and_initialize(&mut vm, "android.os.Build$VERSION");
    assert_nest_bridge_resolves(&mut vm);
    assert_eq!(
        vm.get_static_field("android.os.Build$VERSION", "SDK_INT")
            .unwrap(),
        JavaValue::Int(34)
    );
}

/// Rotate the load order through every position and run each rotation on a
/// fresh VM (the loader's lazy-jar cursor is per-VM state).
#[test]
fn varied_load_orders_are_atomic() {
    if !boot_available() {
        eprintln!("SKIP varied_load_orders_are_atomic: .cache/boot/android-34 absent");
        return;
    }
    let mut order: Vec<(&str, bool)> = NESTED_CLASSES.iter().map(|c| (*c, true)).collect();
    order.extend(CROSS_JAR_CLASSES.iter().map(|c| (*c, false)));
    for rotation in 0..order.len() {
        let mut vm = real_vm(rotation % 2 == 1);
        prime_host_thread(&mut vm);
        for step in 0..order.len() {
            let (class, initialize) = order[(rotation + step) % order.len()];
            if initialize {
                load_and_initialize(&mut vm, class);
            } else {
                let desc = format!("L{};", class.replace('.', "/"));
                vm.load_class(&desc)
                    .unwrap_or_else(|e| panic!("load {class}: {e:?}"));
            }
            // Every intermediate state must already expose the nest bridge.
            assert_nest_bridge_resolves(&mut vm);
        }
        assert_eq!(
            vm.get_static_field("android.os.Build$VERSION", "SDK_INT")
                .unwrap(),
            JavaValue::Int(34)
        );
    }
}

/// Re-initialising the same nested classes many times in one VM must stay
/// idempotent and keep the nest bridge resolvable (catches a partial class left
/// behind by a failed lazy load).
#[test]
fn repeated_nested_init_stays_complete() {
    if !boot_available() {
        eprintln!("SKIP repeated_nested_init_stays_complete: .cache/boot/android-34 absent");
        return;
    }
    let mut vm = real_vm(std::env::var_os("ACOS_GC_STRESS").is_some());
    prime_host_thread(&mut vm);
    for _ in 0..32 {
        for class in NESTED_CLASSES {
            load_and_initialize(&mut vm, class);
            assert_nest_bridge_resolves(&mut vm);
        }
    }
}
