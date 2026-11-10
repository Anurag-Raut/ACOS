//! W52-A: the runtime-side AMS bridge reads real `android.content.Intent`
//! objects and re-materialises them without ever hand-writing a parcelable's
//! bytes.
//!
//! The test builds a real `Intent` on the real Android 14 core library,
//! marshals it with the framework's own `Intent.writeToParcel`, reduces the
//! bytes with `Intent.CREATOR` + the real accessors (`getAction`,
//! `getComponent`, `getCategories`, `getExtras`), then re-materialises the
//! typed facts into a fresh `Intent` and proves the reduction round-trips.
//!
//! When the boot-classpath cache is absent the test skips loudly.

mod common;

use acos_parcel::Parcel;
use acos_runtime::ams_java;
use acos_services::ams::Extra;
use acos_vm::heap::{ArrayData, Handle, Object};
use acos_vm::javabridge::JavaValue;
use acos_vm::Vm;

fn as_obj(v: JavaValue) -> Handle {
    match v {
        JavaValue::Object(h) => h,
        other => panic!("expected object, got {}", other.kind()),
    }
}

fn byte_array(vm: &Vm, h: Handle) -> Vec<u8> {
    match vm.heap.get(h) {
        Some(Object::Array(a)) => match &a.data {
            ArrayData::Byte(v) => v.iter().map(|&b| b as u8).collect(),
            other => panic!("expected byte array, got {other:?}"),
        },
        _ => panic!("expected an array"),
    }
}

/// `intent.writeToParcel(new Parcel())`, marshalled to bytes.
fn intent_bytes(vm: &mut Vm, intent: Handle) -> Vec<u8> {
    let parcel = as_obj(
        vm.call_static("android.os.Parcel", "obtain", "()Landroid/os/Parcel;", &[])
            .expect("Parcel.obtain"),
    );
    vm.call_method(
        intent,
        "writeToParcel",
        "(Landroid/os/Parcel;I)V",
        &[JavaValue::Object(parcel), JavaValue::Int(0)],
    )
    .expect("writeToParcel");
    let bytes = as_obj(
        vm.call_method(parcel, "marshall", "()[B", &[])
            .expect("marshall"),
    );
    byte_array(vm, bytes)
}

/// Wrap marshalled `Intent` bytes in the AIDL `writeTypedObject` presence
/// marker and read them back through the real `CREATOR`.
fn facts_from_bytes(vm: &mut Vm, bytes: &[u8]) -> acos_services::ams::IntentFacts {
    let mut data = Parcel::new();
    data.write_int32(1);
    let mut tmp = Parcel::new();
    tmp.unmarshall(bytes);
    let len = tmp.data_size();
    data.append_from(&tmp, 0, len).expect("append intent payload");
    data.set_data_position(0);
    ams_java::read_intent_facts(vm, &mut data)
        .expect("read intent facts")
        .expect("present intent")
}

#[test]
fn intent_facts_round_trip_through_the_real_creators() {
    let Some(mut rt) = common::new_runtime() else {
        return;
    };
    let vm = rt.vm_mut();

    let intent = vm
        .new_instance_ctor("android.content.Intent", "()V", &[])
        .expect("new Intent");
    let package = vm
        .new_string_rooted("dev.acos.fixtures.twoactivity")
        .expect("string");
    let class = vm
        .new_string_rooted("dev.acos.fixtures.twoactivity.DetailActivity")
        .expect("string");
    vm.call_method(
        intent,
        "setClassName",
        "(Ljava/lang/String;Ljava/lang/String;)Landroid/content/Intent;",
        &[JavaValue::Object(package), JavaValue::Object(class)],
    )
    .expect("setClassName");
    let key = vm.new_string_rooted("name").expect("string");
    let value = vm.new_string_rooted("Ada").expect("string");
    vm.call_method(
        intent,
        "putExtra",
        "(Ljava/lang/String;Ljava/lang/String;)Landroid/content/Intent;",
        &[JavaValue::Object(key), JavaValue::Object(value)],
    )
    .expect("putExtra String");
    let attempt = vm.new_string_rooted("attempt").expect("string");
    vm.call_method(
        intent,
        "putExtra",
        "(Ljava/lang/String;I)Landroid/content/Intent;",
        &[JavaValue::Object(attempt), JavaValue::Int(1)],
    )
    .expect("putExtra int");
    let flag = vm.new_string_rooted("flag").expect("string");
    vm.call_method(
        intent,
        "putExtra",
        "(Ljava/lang/String;Z)Landroid/content/Intent;",
        &[JavaValue::Object(flag), JavaValue::Boolean(true)],
    )
    .expect("putExtra bool");
    let category = vm
        .new_string_rooted("android.intent.category.LAUNCHER")
        .expect("string");
    vm.call_method(
        intent,
        "addCategory",
        "(Ljava/lang/String;)Landroid/content/Intent;",
        &[JavaValue::Object(category)],
    )
    .expect("addCategory");

    let bytes = intent_bytes(vm, intent);
    let facts = facts_from_bytes(vm, &bytes);
    assert_eq!(
        facts.component.as_ref().map(|(p, c)| (p.as_str(), c.as_str())),
        Some((
            "dev.acos.fixtures.twoactivity",
            "dev.acos.fixtures.twoactivity.DetailActivity"
        ))
    );
    assert_eq!(facts.extras.get("name"), Some(&Extra::String("Ada".into())));
    assert_eq!(facts.extras.get("attempt"), Some(&Extra::Int(1)));
    assert_eq!(facts.extras.get("flag"), Some(&Extra::Bool(true)));
    assert!(facts
        .categories
        .contains(&"android.intent.category.LAUNCHER".to_string()));

    // Re-materialise and re-read: the typed facts are lossless for the
    // String/int/boolean subset.
    let rebuilt = ams_java::materialize_intent(vm, &facts).expect("materialize Intent");
    let rebuilt_bytes = intent_bytes(vm, rebuilt);
    let facts2 = facts_from_bytes(vm, &rebuilt_bytes);
    assert_eq!(facts, facts2);
}

#[test]
fn component_name_reply_is_a_real_component_name() {
    let Some(mut rt) = common::new_runtime() else {
        return;
    };
    let vm = rt.vm_mut();
    let cn = ams_java::component_name(vm, "dev.acos.fixtures.svcprovider", "dev.acos.fixtures.svcprovider.EchoService")
        .expect("component name");
    let package = match vm
        .call_method(cn, "getPackageName", "()Ljava/lang/String;", &[])
        .expect("getPackageName")
    {
        JavaValue::Object(h) => vm.java_string_to_rust(h).expect("string"),
        other => panic!("getPackageName returned {}", other.kind()),
    };
    let class = match vm
        .call_method(cn, "getClassName", "()Ljava/lang/String;", &[])
        .expect("getClassName")
    {
        JavaValue::Object(h) => vm.java_string_to_rust(h).expect("string"),
        other => panic!("getClassName returned {}", other.kind()),
    };
    assert_eq!(package, "dev.acos.fixtures.svcprovider");
    assert_eq!(class, "dev.acos.fixtures.svcprovider.EchoService");
}

#[test]
fn unsupported_extra_type_is_a_structured_error() {
    let Some(mut rt) = common::new_runtime() else {
        return;
    };
    let vm = rt.vm_mut();
    let intent = vm
        .new_instance_ctor("android.content.Intent", "()V", &[])
        .expect("new Intent");
    // A `long` extra is outside the AMS typed subset: it must be refused, not
    // silently recorded as present.
    let key = vm.new_string_rooted("big").expect("string");
    vm.call_method(
        intent,
        "putExtra",
        "(Ljava/lang/String;J)Landroid/content/Intent;",
        &[JavaValue::Object(key), JavaValue::Long(7)],
    )
    .expect("putExtra long");
    let bytes = intent_bytes(vm, intent);
    let mut data = Parcel::new();
    data.write_int32(1);
    let mut tmp = Parcel::new();
    tmp.unmarshall(&bytes);
    let len = tmp.data_size();
    data.append_from(&tmp, 0, len).expect("append");
    data.set_data_position(0);
    let err = ams_java::read_intent_facts(vm, &mut data).expect_err("long extra must be refused");
    assert!(err.contains("unsupported type"), "{err}");
}
