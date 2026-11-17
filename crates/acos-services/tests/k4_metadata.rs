//! K3d: manifest `<meta-data>` as the `PackageItemInfo.metaData` `Bundle`, and
//! the `GET_META_DATA` (0x80) gate the platform puts on it.
//!
//! Transcribed from `.cache/aosp/base` (tag android-14.0.0_r75):
//!
//! * `PackageParser.parseMetaData` stores `android:resource` as
//!   `putInt(name, resourceId)` and `android:value` as `putString`/`putInt`/
//!   `putBoolean`/`putFloat` — the four value types ACOS's manifest model
//!   carries.
//! * `PackageParser.generateApplicationInfo`/`generateActivityInfo`/
//!   `generateServiceInfo`/`generateProviderInfo` (via `copyNeeded`) copy the
//!   parsed `Bundle` into the reply **only** when the caller passes
//!   `PackageManager.GET_META_DATA`; otherwise the parcelled field is `null`.
//!
//! The bytes are checked here, not the intent: each reply is encoded with the
//! same writer the host uses on the wire and read back with the same reader, and
//! the Bundle is written/read through `write_bundle`/`read_bundle`.

use std::sync::{Arc, Mutex};

use acos_services::parcelable::{read_bundle, read_payload, write_bundle, write_payload};
use acos_services::pms::{
    AppInfo, MetaDataEntry, MetaDataValue, PmsService, PmsState, ProviderInfo, GET_META_DATA,
};
use acos_services::value::{Bundle, Parcelable, Value};
use acos_services::{ServiceObject, Wiring};

fn method(name: &str) -> &'static acos_aidl::MethodInfo {
    acos_aidl::lookup_by_name("android.content.pm.IPackageManager", name).expect("AIDL entry")
}

/// `PackageManager.GET_META_DATA` in the API-34 image.
const GET_META_DATA_IMAGE: i64 = 0x80;

/// The entries `PackageParser.parseMetaData` would build for this manifest
/// fragment:
///
/// ```xml
/// <meta-data android:name="io.sentry.dsn" android:value="https://k@example/1"/>
/// <meta-data android:name="io.sentry.auto-init" android:value="false"/>
/// <meta-data android:name="android.app.shortcuts" android:resource="@xml/shortcuts"/>
/// <meta-data android:name="retries" android:value="3"/>
/// <meta-data android:name="scale" android:value="0.5"/>
/// ```
fn manifest_meta_data() -> Vec<MetaDataEntry> {
    vec![
        MetaDataEntry {
            name: "io.sentry.dsn".into(),
            value: MetaDataValue::String("https://k@example/1".into()),
        },
        MetaDataEntry {
            name: "io.sentry.auto-init".into(),
            value: MetaDataValue::Boolean(false),
        },
        MetaDataEntry {
            name: "android.app.shortcuts".into(),
            value: MetaDataValue::Reference(0x7f0b_0001),
        },
        MetaDataEntry {
            name: "retries".into(),
            value: MetaDataValue::Int(3),
        },
        MetaDataEntry {
            name: "scale".into(),
            value: MetaDataValue::Float(0.5),
        },
    ]
}

/// The `Bundle` those entries must become: `putString`, `putBoolean`,
/// `putInt(name, resourceId)`, `putInt`, `putFloat`.
fn expected_bundle() -> Bundle {
    let mut bundle = Bundle::new();
    bundle.put(
        "io.sentry.dsn",
        Value::Str(Some("https://k@example/1".into())),
    );
    bundle.put("io.sentry.auto-init", Value::Bool(false));
    bundle.put("android.app.shortcuts", Value::Int(0x7f0b_0001));
    bundle.put("retries", Value::Int(3));
    bundle.put("scale", Value::Float(0.5));
    bundle
}

fn state_with_meta_data() -> PmsState {
    let mut state = PmsState::new();
    state.primary_mut().app.meta_data = manifest_meta_data();
    state.primary_mut().package.application.meta_data = manifest_meta_data();
    let package_name = state.app().package_name.clone();
    let process_name = state.app().process_name.clone();
    state.primary_mut().providers.push(ProviderInfo {
        name: "androidx.startup.InitializationProvider".into(),
        package_name,
        authority: "dev.acos.fixtures.helloviews.androidx-startup".into(),
        process_name,
        read_permission: String::new(),
        write_permission: String::new(),
        exported: false,
        grant_uri_permissions: false,
        init_order: 0,
        meta_data: manifest_meta_data(),
    });
    state
}

fn pms(state: PmsState) -> PmsService {
    PmsService::new(Arc::new(Mutex::new(state)), Wiring::default())
}

/// Write a `Parcelable` with the host writer and read it back with the host
/// reader — the same two halves the binder transport uses.
fn round_trip(value: &Parcelable) -> Parcelable {
    let mut parcel = acos_parcel::Parcel::new();
    write_payload(&mut parcel, value).expect("encode");
    parcel.set_data_position(0);
    read_payload(value.class().expect("class"), &mut parcel).expect("decode")
}

fn meta_data_field(reply: &Value, index: usize) -> Option<Bundle> {
    let parcelable = reply.as_parcelable().expect("typed object");
    match parcelable.field(index).expect("metaData slot") {
        Value::Bundle(bundle) => bundle.clone(),
        other => panic!("field {index} is not a Bundle: {other:?}"),
    }
}

/// Compare as a map: `BaseBundle.writeToParcelInner` emits entries in
/// `ArrayMap` order (ascending Java `hashCode`) and the reader appends them in
/// wire order, so the decoded `Bundle` is *hash* ordered, not
/// insertion-ordered. Android's own `Bundle.equals` is a `Map` comparison.
fn assert_bundle(actual: &Bundle, expected: &Bundle, what: &str) {
    assert_eq!(actual.len(), expected.len(), "{what}: entry count");
    for (key, value) in &expected.entries {
        assert_eq!(actual.get(key), Some(value), "{what}: key {key}");
    }
}

fn assert_meta_data(actual: Option<Bundle>, expected: Option<Bundle>, what: &str) {
    match (actual, expected) {
        (None, None) => {}
        (Some(actual), Some(expected)) => assert_bundle(&actual, &expected, what),
        (actual, expected) => {
            panic!("{what}: metaData {actual:?} != {expected:?}")
        }
    }
}

#[test]
fn the_flag_value_is_the_one_in_the_image() {
    assert_eq!(GET_META_DATA, GET_META_DATA_IMAGE);
}

#[test]
fn application_meta_data_follows_get_meta_data() {
    let mut pms = pms(state_with_meta_data());
    let package = pms_own_package();
    for (flags, expected) in [
        (0i64, None),
        // The other flags a client sends (`GET_ACTIVITIES`, ...) do not matter.
        (0x1 | 0x2 | 0x4, None),
        (GET_META_DATA, Some(expected_bundle())),
        (GET_META_DATA | 0x1, Some(expected_bundle())),
    ] {
        let reply = pms
            .call(
                "android.content.pm.IPackageManager",
                method("getApplicationInfo"),
                &[
                    Value::Str(Some(package.clone())),
                    Value::Long(flags),
                    Value::Int(0),
                ],
            )
            .expect("supported")
            .reply;
        assert_meta_data(
            meta_data_field(&reply[0], 7),
            expected,
            &format!("getApplicationInfo flags={flags:#x}: ApplicationInfo.metaData"),
        );
    }
}

#[test]
fn package_meta_data_carries_the_nested_application_bundle() {
    let mut pms = pms(state_with_meta_data());
    let package = pms_own_package();
    let reply = pms
        .call(
            "android.content.pm.IPackageManager",
            method("getPackageInfo"),
            &[
                Value::Str(Some(package)),
                Value::Long(GET_META_DATA),
                Value::Int(0),
            ],
        )
        .expect("supported")
        .reply;
    let info = reply[0].as_parcelable().expect("typed object");
    let Some(Value::Cond {
        present: true,
        values,
    }) = info.field(9)
    else {
        panic!("PackageInfo.applicationInfo conditional body");
    };
    let app = values[0].as_parcelable().expect("ApplicationInfo");
    assert_meta_data(
        match app.field(7).expect("metaData") {
            Value::Bundle(bundle) => bundle.clone(),
            other => panic!("not a Bundle: {other:?}"),
        },
        Some(expected_bundle()),
        "generatePackageInfo builds the ApplicationInfo with the same flags",
    );
}

#[test]
fn provider_meta_data_follows_get_meta_data_on_every_provider_reply() {
    let mut pms = pms(state_with_meta_data());
    let component = Parcelable::ordered(
        "android.content.ComponentName",
        vec![
            Value::Str(Some(pms_own_package())),
            Value::Str(Some("androidx.startup.InitializationProvider".into())),
        ],
    );
    // `getProviderInfo(ComponentName, long flags, int userId)`.
    for (flags, expected) in [(0i64, None), (GET_META_DATA, Some(expected_bundle()))] {
        let reply = pms
            .call(
                "android.content.pm.IPackageManager",
                method("getProviderInfo"),
                &[
                    Value::Parcelable(component.clone()),
                    Value::Long(flags),
                    Value::Int(0),
                ],
            )
            .expect("supported")
            .reply;
        assert_meta_data(
            meta_data_field(&reply[0], 6),
            expected,
            &format!("getProviderInfo flags={flags:#x}"),
        );
    }
    // `resolveContentProvider(String authority, long flags, int userId)` — the
    // call `FileProvider` makes to read `FILE_PROVIDER_PATHS`.
    let authority = Value::Str(Some("dev.acos.fixtures.helloviews.androidx-startup".into()));
    for (flags, expected) in [(0i64, None), (GET_META_DATA, Some(expected_bundle()))] {
        let reply = pms
            .call(
                "android.content.pm.IPackageManager",
                method("resolveContentProvider"),
                &[authority.clone(), Value::Long(flags), Value::Int(0)],
            )
            .expect("supported")
            .reply;
        assert_meta_data(
            meta_data_field(&reply[0], 6),
            expected,
            &format!("resolveContentProvider flags={flags:#x}"),
        );
    }
}

#[test]
fn a_component_without_meta_data_replies_null_even_with_the_flag() {
    let mut state = state_with_meta_data();
    state.primary_mut().providers[0].meta_data.clear();
    let mut pms = pms(state);
    let reply = pms
        .call(
            "android.content.pm.IPackageManager",
            method("resolveContentProvider"),
            &[
                Value::Str(Some("dev.acos.fixtures.helloviews.androidx-startup".into())),
                Value::Long(GET_META_DATA),
                Value::Int(0),
            ],
        )
        .expect("supported")
        .reply;
    assert_eq!(
        meta_data_field(&reply[0], 6),
        None,
        "there is nothing for the platform to copy, so the field stays null"
    );
}

/// The provider's `metaData` bytes survive the parcel writer and reader with
/// every value type `parseMetaData` stores (string, int, boolean, float, and an
/// `android:resource` reference id — an int).
#[test]
fn provider_meta_data_survives_the_parcel_round_trip() {
    let state = state_with_meta_data();
    let provider = state.primary().providers[0].clone();
    let encoded = provider.to_parcelable_with_flags(state.app(), GET_META_DATA);
    let decoded = round_trip(&encoded);
    // The decoded `Bundle` is in `ArrayMap` (hash) order, which is what the
    // writer emits, so a second encode/read cycle is a fixed point.
    assert_eq!(
        round_trip(&decoded),
        decoded,
        "the wire form is stable across a second round trip"
    );
    match decoded.field(6) {
        Some(Value::Bundle(Some(bundle))) => {
            assert_bundle(bundle, &expected_bundle(), "ProviderInfo.metaData")
        }
        other => panic!("ProviderInfo.metaData: {other:?}"),
    }
}

/// `BaseBundle.writeToParcelInner`/`readFromParcelInner` round-trip for the
/// metadata `Bundle` itself, including the `ArrayMap` key ordering the writer
/// applies and the empty-bundle fast path.
#[test]
fn a_meta_data_bundle_round_trips_through_the_bundle_codec() {
    let mut parcel = acos_parcel::Parcel::new();
    write_bundle(&mut parcel, Some(&expected_bundle())).expect("write");
    parcel.set_data_position(0);
    let decoded = read_bundle(&mut parcel).expect("read").expect("present");
    assert_bundle(&decoded, &expected_bundle(), "metadata Bundle round trip");

    let mut empty = acos_parcel::Parcel::new();
    write_bundle(&mut empty, Some(&Bundle::new())).expect("write empty");
    empty.set_data_position(0);
    assert_eq!(read_bundle(&mut empty).expect("read"), Some(Bundle::new()));
}

/// `PmsState::from_apk` carries the manifest's `<application>` and component
/// metadata into the typed state (the same model PMS answers from).
#[test]
fn from_apk_carries_the_manifest_meta_data() {
    let path = acos_services::workspace_path("fixtures/build/apps/service-provider.apk")
        .unwrap_or_else(|| std::path::PathBuf::from("fixtures/build/apps/service-provider.apk"));
    if !path.is_file() {
        eprintln!("skip from_apk_carries_the_manifest_meta_data: fixture APK not built");
        return;
    }
    let state = PmsState::from_apk(&path).expect("from_apk");
    // The fixture declares no `<meta-data>`; the model must not invent any.
    assert!(state.app().meta_data.is_empty());
    assert!(state
        .primary()
        .providers
        .iter()
        .all(|p| p.meta_data.is_empty()));
}

fn pms_own_package() -> String {
    AppInfo::hello_views_fallback(std::path::Path::new("hello-views.apk")).package_name
}
