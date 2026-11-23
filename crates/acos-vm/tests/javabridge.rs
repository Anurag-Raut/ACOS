//! Acceptance tests for the Rust ↔ Java object bridge (`acos_vm::javabridge`).
//!
//! These run against the **real** Android 14 boot classpath (`--corelib real`)
//! and are skipped loudly when `.cache/boot/android-34` is absent. Every
//! scenario is executed normally and under GC stress (a precise collection
//! before every allocation), so a missing root fails the test.
//!
//! The boot-classpath search is the same as `tests/dexprogs.rs`: the default
//! path walks up from the crate directory to the repository root.

use std::path::{Path, PathBuf};

use acos_vm::{BridgeError, CoreLib, JavaValue, Vm, VmOptions};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn boot_available() -> bool {
    repo_root()
        .join(".cache/boot/android-34/BOOTCLASSPATH")
        .is_file()
}

fn real_vm(gc_stress: bool) -> Vm {
    // Honour `ACOS_GC_STRESS=1` as well, so the whole suite can be run under
    // the same stress switch the differential runner uses.
    let gc_stress = gc_stress || std::env::var_os("ACOS_GC_STRESS").is_some();
    Vm::new(VmOptions {
        heap_max_bytes: 1024 * 1024 * 1024,
        gc_stress,
        corelib: CoreLib::Real,
        boot_dir: None,
        trace_natives: false,
    })
    .expect("real-corelib VM (boot classpath must be present)")
}

macro_rules! require_boot {
    () => {
        if !boot_available() {
            acos_test_support::require_prereq(false, "real_vm", "javabridge test: .cache/boot/android-34 is absent (run tools/fetch/extract-bootclasspath.sh)");
            return;
        }
    };
}

/// A handle that is never live; used only to exercise the reference type check.
const NOT_A_HANDLE: u32 = 0xFFFF_FFFE;

fn string_value(vm: &Vm, v: &JavaValue) -> String {
    match v {
        JavaValue::Object(h) => vm.java_string_to_rust(*h).expect("a String handle"),
        other => panic!("expected a String, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Rect
// ---------------------------------------------------------------------------

fn rect_scenarios(vm: &mut Vm) {
    // Real constructor `Rect(int,int,int,int)`.
    let rect = vm
        .new_instance_ctor(
            "android.graphics.Rect",
            "(IIII)V",
            &[
                JavaValue::Int(1),
                JavaValue::Int(2),
                JavaValue::Int(3),
                JavaValue::Int(4),
            ],
        )
        .expect("new Rect(1,2,3,4)");

    assert_eq!(vm.get_field(rect, "left").unwrap(), JavaValue::Int(1));
    assert_eq!(vm.get_field(rect, "top").unwrap(), JavaValue::Int(2));
    assert_eq!(vm.get_field(rect, "right").unwrap(), JavaValue::Int(3));
    assert_eq!(vm.get_field(rect, "bottom").unwrap(), JavaValue::Int(4));

    // Read back through real Java bytecode.
    assert_eq!(
        vm.call_method(rect, "width", "()I", &[]).unwrap(),
        JavaValue::Int(2)
    );
    assert_eq!(
        vm.call_method(rect, "height", "()I", &[]).unwrap(),
        JavaValue::Int(2)
    );
    assert_eq!(
        vm.call_method(rect, "centerX", "()I", &[]).unwrap(),
        JavaValue::Int(2)
    );
    assert_eq!(
        vm.call_method(rect, "centerY", "()I", &[]).unwrap(),
        JavaValue::Int(3)
    );
    let s = vm
        .call_method(rect, "toString", "()Ljava/lang/String;", &[])
        .unwrap();
    assert_eq!(string_value(vm, &s), "Rect(1, 2 - 3, 4)");

    // No-constructor allocation + field writes (the explicit "no ctor" path).
    let rect2 = vm
        .new_instance("android.graphics.Rect")
        .expect("alloc Rect");
    vm.set_field(rect2, "left", JavaValue::Int(10)).unwrap();
    vm.set_field(rect2, "top", JavaValue::Int(20)).unwrap();
    vm.set_field(rect2, "right", JavaValue::Int(40)).unwrap();
    vm.set_field(rect2, "bottom", JavaValue::Int(80)).unwrap();
    assert_eq!(
        vm.call_method(rect2, "width", "()I", &[]).unwrap(),
        JavaValue::Int(30)
    );
    assert_eq!(
        vm.call_method(rect2, "height", "()I", &[]).unwrap(),
        JavaValue::Int(60)
    );
    assert_eq!(
        vm.call_method(rect2, "centerY", "()I", &[]).unwrap(),
        JavaValue::Int(50)
    );
}

// ---------------------------------------------------------------------------
// ArrayList<String>
// ---------------------------------------------------------------------------

fn arraylist_scenarios(vm: &mut Vm) {
    let list = vm
        .new_instance_ctor("java.util.ArrayList", "()V", &[])
        .expect("new ArrayList()");
    let words = ["alpha", "beta", "gamma"];
    for w in words {
        let s = vm.new_string_rooted(w).unwrap();
        let added = vm
            .call_method(
                list,
                "add",
                "(Ljava/lang/Object;)Z",
                &[JavaValue::Object(s)],
            )
            .unwrap();
        assert_eq!(added, JavaValue::Boolean(true));
    }
    assert_eq!(
        vm.call_method(list, "size", "()I", &[]).unwrap(),
        JavaValue::Int(3)
    );
    for (i, w) in words.iter().enumerate() {
        let got = vm
            .call_method(
                list,
                "get",
                "(I)Ljava/lang/Object;",
                &[JavaValue::Int(i as i32)],
            )
            .unwrap();
        assert_eq!(string_value(vm, &got), *w);
    }
    let dumped = vm
        .call_method(list, "toString", "()Ljava/lang/String;", &[])
        .unwrap();
    assert_eq!(string_value(vm, &dumped), "[alpha, beta, gamma]");
}

// ---------------------------------------------------------------------------
// HashMap<String,String>
// ---------------------------------------------------------------------------

fn hashmap_scenarios(vm: &mut Vm) {
    let map = vm
        .new_instance_ctor("java.util.HashMap", "()V", &[])
        .expect("new HashMap()");
    let pairs = [("k1", "v1"), ("k2", "v2"), ("k3", "v3")];
    for (k, v) in pairs {
        let kh = vm.new_string_rooted(k).unwrap();
        let vh = vm.new_string_rooted(v).unwrap();
        vm.call_method(
            map,
            "put",
            "(Ljava/lang/Object;Ljava/lang/Object;)Ljava/lang/Object;",
            &[JavaValue::Object(kh), JavaValue::Object(vh)],
        )
        .unwrap();
    }
    assert_eq!(
        vm.call_method(map, "size", "()I", &[]).unwrap(),
        JavaValue::Int(3)
    );
    for (k, v) in pairs {
        let kh = vm.new_string_rooted(k).unwrap();
        let present = vm
            .call_method(
                map,
                "containsKey",
                "(Ljava/lang/Object;)Z",
                &[JavaValue::Object(kh)],
            )
            .unwrap();
        assert_eq!(present, JavaValue::Boolean(true));
        let got = vm
            .call_method(
                map,
                "get",
                "(Ljava/lang/Object;)Ljava/lang/Object;",
                &[JavaValue::Object(kh)],
            )
            .unwrap();
        assert_eq!(string_value(vm, &got), v);
    }
}

// ---------------------------------------------------------------------------
// ApplicationInfo
// ---------------------------------------------------------------------------

fn application_info_scenarios(vm: &mut Vm) {
    // The real no-arg constructor; it calls SystemClock.uptimeMillis(), so this
    // also proves the bridge runs a constructor with a native call inside.
    let ai = vm
        .new_instance_ctor("android.content.pm.ApplicationInfo", "()V", &[])
        .expect("new ApplicationInfo()");

    let package_name = vm.new_string_rooted("com.example.acos").unwrap();
    let class_name = vm.new_string_rooted("com.example.acos.Main").unwrap();
    let source_dir = vm
        .new_string_rooted("/data/app/com.example.acos/base.apk")
        .unwrap();
    let data_dir = vm
        .new_string_rooted("/data/user/0/com.example.acos")
        .unwrap();
    let native_dir = vm
        .new_string_rooted("/data/app/com.example.acos/lib/x86_64")
        .unwrap();
    let process_name = vm.new_string_rooted("com.example.acos").unwrap();
    let permission = vm.new_string_rooted("android.permission.INTERNET").unwrap();
    let task_affinity = vm.new_string_rooted("com.example.acos").unwrap();

    let string_fields: [(&str, u32); 8] = [
        ("packageName", package_name),
        ("name", class_name), // inherited from PackageItemInfo
        ("sourceDir", source_dir),
        ("publicSourceDir", source_dir),
        ("dataDir", data_dir),
        ("nativeLibraryDir", native_dir),
        ("processName", process_name),
        ("permission", permission),
    ];
    for (name, h) in string_fields {
        vm.set_field(ai, name, JavaValue::Object(h)).unwrap();
    }
    vm.set_field(ai, "className", JavaValue::Object(class_name))
        .unwrap();
    vm.set_field(ai, "taskAffinity", JavaValue::Object(task_affinity))
        .unwrap();

    let int_fields: [(&str, i32); 12] = [
        ("uid", 10192),
        ("targetSdkVersion", 34),
        ("minSdkVersion", 26),
        ("flags", 4), // ApplicationInfo.FLAG_HAS_CODE
        ("privateFlags", 0),
        ("theme", 0x7f01_0000),
        ("enabledSetting", 1),
        ("installLocation", 0),
        ("versionCode", 7),
        ("compileSdkVersion", 34),
        ("largestWidthLimitDp", 0),
        ("requiresSmallestWidthDp", 0),
    ];
    for (name, v) in int_fields {
        vm.set_field(ai, name, JavaValue::Int(v)).unwrap();
    }
    vm.set_field(ai, "enabled", JavaValue::Boolean(true))
        .unwrap();

    // Read every field back.
    for (name, h) in string_fields {
        assert_eq!(vm.get_field(ai, name).unwrap(), JavaValue::Object(h));
    }
    assert_eq!(
        vm.get_field(ai, "className").unwrap(),
        JavaValue::Object(class_name)
    );
    assert_eq!(
        vm.get_field(ai, "taskAffinity").unwrap(),
        JavaValue::Object(task_affinity)
    );
    for (name, v) in int_fields {
        assert_eq!(vm.get_field(ai, name).unwrap(), JavaValue::Int(v));
    }
    assert_eq!(
        vm.get_field(ai, "enabled").unwrap(),
        JavaValue::Boolean(true)
    );

    // Read back through Java method calls.
    let base = vm
        .call_method(ai, "getBaseCodePath", "()Ljava/lang/String;", &[])
        .unwrap();
    assert_eq!(
        string_value(vm, &base),
        "/data/app/com.example.acos/base.apk"
    );
    assert_eq!(
        vm.call_method(ai, "hasCode", "()Z", &[]).unwrap(),
        JavaValue::Boolean(true)
    );
    assert_eq!(
        vm.call_method(ai, "getGwpAsanMode", "()I", &[]).unwrap(),
        JavaValue::Int(-1)
    );
}

// ---------------------------------------------------------------------------
// Arrays and strings
// ---------------------------------------------------------------------------

fn array_and_string_scenarios(vm: &mut Vm) {
    // Primitive array.
    let ints = vm
        .new_array(
            "I",
            &[JavaValue::Int(10), JavaValue::Int(20), JavaValue::Int(30)],
        )
        .unwrap();
    assert_eq!(vm.array_length(ints).unwrap(), 3);
    assert_eq!(vm.get_array_element(ints, 0).unwrap(), JavaValue::Int(10));
    vm.set_array_element(ints, 1, JavaValue::Int(99)).unwrap();
    assert_eq!(vm.get_array_element(ints, 1).unwrap(), JavaValue::Int(99));

    // Wide primitives exercise the two-slot encoding.
    let longs = vm
        .new_array("J", &[JavaValue::Long(1 << 40), JavaValue::Long(-5)])
        .unwrap();
    assert_eq!(
        vm.get_array_element(longs, 0).unwrap(),
        JavaValue::Long(1 << 40)
    );
    assert_eq!(vm.get_array_element(longs, 1).unwrap(), JavaValue::Long(-5));
    // Wide element writes must preserve the high word.
    vm.set_array_element(longs, 1, JavaValue::Long(1 << 40))
        .unwrap();
    assert_eq!(
        vm.get_array_element(longs, 1).unwrap(),
        JavaValue::Long(1 << 40)
    );
    let doubles = vm
        .new_array("D", &[JavaValue::Double(1.5), JavaValue::Double(-0.25)])
        .unwrap();
    assert_eq!(
        vm.get_array_element(doubles, 1).unwrap(),
        JavaValue::Double(-0.25)
    );
    vm.set_array_element(doubles, 0, JavaValue::Double(-1234.5))
        .unwrap();
    assert_eq!(
        vm.get_array_element(doubles, 0).unwrap(),
        JavaValue::Double(-1234.5)
    );

    // Object array with component assignability checking.
    let a = vm.new_string_rooted("one").unwrap();
    let b = vm.new_string_rooted("two").unwrap();
    let strings = vm
        .new_array(
            "Ljava/lang/String;",
            &[JavaValue::Object(a), JavaValue::Object(b)],
        )
        .unwrap();
    let e0 = vm.get_array_element(strings, 0).unwrap();
    assert_eq!(string_value(vm, &e0), "one");
    let e1 = vm.get_array_element(strings, 1).unwrap();
    assert_eq!(string_value(vm, &e1), "two");
    let null_slot = vm
        .new_array("Ljava/lang/String;", &[JavaValue::Null])
        .unwrap();
    assert_eq!(vm.get_array_element(null_slot, 0).unwrap(), JavaValue::Null);

    // Empty (zeroed) arrays.
    let zeros = vm.new_array_empty("I", 4).unwrap();
    assert_eq!(vm.array_length(zeros).unwrap(), 4);
    assert_eq!(vm.get_array_element(zeros, 3).unwrap(), JavaValue::Int(0));

    // Strings: UTF-16 round trip including a surrogate pair.
    let text = "h\u{e9}llo \u{1f600} world";
    let h = vm.new_string_rooted(text).unwrap();
    assert_eq!(vm.java_string_to_rust(h).unwrap(), text);
}

// ---------------------------------------------------------------------------
// Static fields and static calls
// ---------------------------------------------------------------------------

fn static_scenarios(vm: &mut Vm) {
    // Static field read (encoded static value applied at class load).
    assert_eq!(
        vm.get_static_field("java.lang.Integer", "MAX_VALUE")
            .unwrap(),
        JavaValue::Int(i32::MAX)
    );

    // Static field write/read round trip. The field is a constant unrelated to
    // this test; restore it afterwards.
    let original = vm
        .get_static_field("android.content.pm.ApplicationInfo", "AUTO_REVOKE_ALLOWED")
        .unwrap();
    vm.set_static_field(
        "android.content.pm.ApplicationInfo",
        "AUTO_REVOKE_ALLOWED",
        JavaValue::Int(1234),
    )
    .unwrap();
    assert_eq!(
        vm.get_static_field("android.content.pm.ApplicationInfo", "AUTO_REVOKE_ALLOWED")
            .unwrap(),
        JavaValue::Int(1234)
    );
    vm.set_static_field(
        "android.content.pm.ApplicationInfo",
        "AUTO_REVOKE_ALLOWED",
        original,
    )
    .unwrap();

    // Static method call through the bridge.
    let v = vm
        .call_static(
            "java.lang.String",
            "valueOf",
            "(I)Ljava/lang/String;",
            &[JavaValue::Int(42)],
        )
        .unwrap();
    assert_eq!(string_value(vm, &v), "42");
    let abs = vm
        .call_static("java.lang.Math", "abs", "(I)I", &[JavaValue::Int(-7)])
        .unwrap();
    assert_eq!(abs, JavaValue::Int(7));
}

// ---------------------------------------------------------------------------
// Hard errors (never a coercion)
// ---------------------------------------------------------------------------

fn error_scenarios(vm: &mut Vm) {
    let rect = vm
        .new_instance_ctor(
            "android.graphics.Rect",
            "(IIII)V",
            &[
                JavaValue::Int(0),
                JavaValue::Int(0),
                JavaValue::Int(1),
                JavaValue::Int(1),
            ],
        )
        .unwrap();

    // Wrong field type: long/boolean/object into an `int`.
    for bad in [
        JavaValue::Long(1),
        JavaValue::Boolean(true),
        JavaValue::Object(NOT_A_HANDLE),
        JavaValue::Null,
        JavaValue::Float(1.0),
    ] {
        let err = vm.set_field(rect, "left", bad).expect_err("int field");
        assert!(
            matches!(err, BridgeError::FieldTypeMismatch { .. }),
            "got {err:?}"
        );
    }
    // Missing field.
    assert!(matches!(
        vm.set_field(rect, "noSuchField", JavaValue::Int(1))
            .expect_err("missing field"),
        BridgeError::FieldNotFound { .. }
    ));
    // Known class/field mismatch on a reference field: put an int into the
    // String-valued ApplicationInfo.packageName.
    let ai = vm
        .new_instance("android.content.pm.ApplicationInfo")
        .unwrap();
    assert!(matches!(
        vm.set_field(ai, "packageName", JavaValue::Int(1))
            .expect_err("int into String"),
        BridgeError::FieldTypeMismatch { .. }
    ));

    // Wrong array element type.
    let s = vm.new_string_rooted("x").unwrap();
    assert!(matches!(
        vm.new_array("I", &[JavaValue::Object(s)])
            .expect_err("String into int[]"),
        BridgeError::ArrayElementType { .. }
    ));
    assert!(matches!(
        vm.new_array("Ljava/lang/String;", &[JavaValue::Int(1)])
            .expect_err("int into String[]"),
        BridgeError::ArrayElementType { .. }
    ));

    // Missing class / method.
    assert!(matches!(
        vm.new_instance("does.not.Exist")
            .expect_err("missing class"),
        BridgeError::ClassNotFound(_)
    ));
    assert!(matches!(
        vm.call_method(rect, "noSuchMethod", "()V", &[])
            .expect_err("missing method"),
        BridgeError::MethodNotFound { .. }
    ));
    // Wrong argument count / type.
    assert!(matches!(
        vm.call_method(rect, "set", "(IIII)V", &[JavaValue::Int(1)])
            .expect_err("arg count"),
        BridgeError::ArgCount { .. }
    ));
    assert!(matches!(
        vm.call_method(
            rect,
            "set",
            "(IIII)V",
            &[
                JavaValue::Int(1),
                JavaValue::Int(2),
                JavaValue::Int(3),
                JavaValue::Long(4)
            ]
        )
        .expect_err("arg type"),
        BridgeError::ArgType { .. }
    ));

    // Missing field on a static read, and a static/instance mismatch.
    assert!(matches!(
        vm.get_static_field("java.lang.Integer", "noSuchStatic")
            .expect_err("missing static"),
        BridgeError::FieldNotFound { .. }
    ));
    assert!(matches!(
        vm.set_static_field("java.lang.Integer", "MAX_VALUE", JavaValue::Long(1))
            .expect_err("long into int static"),
        BridgeError::FieldTypeMismatch { .. }
    ));
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn rect_round_trips() {
    require_boot!();
    rect_scenarios(&mut real_vm(false));
}

#[test]
fn arraylist_round_trips() {
    require_boot!();
    arraylist_scenarios(&mut real_vm(false));
}

#[test]
fn hashmap_round_trips() {
    require_boot!();
    hashmap_scenarios(&mut real_vm(false));
}

#[test]
fn application_info_round_trips() {
    require_boot!();
    application_info_scenarios(&mut real_vm(false));
}

#[test]
fn arrays_and_strings_round_trip() {
    require_boot!();
    array_and_string_scenarios(&mut real_vm(false));
}

#[test]
fn static_fields_and_static_calls_round_trip() {
    require_boot!();
    static_scenarios(&mut real_vm(false));
}

#[test]
fn bridge_errors_are_hard() {
    require_boot!();
    error_scenarios(&mut real_vm(false));
}

/// The whole acceptance suite under a precise collection before every
/// allocation (`ACOS_GC_STRESS=1` semantics). Any unrooted handle fails here.
#[test]
fn all_scenarios_pass_under_gc_stress() {
    require_boot!();
    let mut vm = real_vm(true);
    rect_scenarios(&mut vm);
    arraylist_scenarios(&mut vm);
    hashmap_scenarios(&mut vm);
    application_info_scenarios(&mut vm);
    array_and_string_scenarios(&mut vm);
    static_scenarios(&mut vm);
    error_scenarios(&mut vm);
}
