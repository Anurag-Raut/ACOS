//! K3: the compatibility walls K3 fixed, each pinned to the bytes the image
//! writes.
//!
//! * **K3b** — an unknown package is a null `PackageInfo`/`ApplicationInfo` on
//!   the wire; the client (`ApplicationPackageManager`) turns that null into
//!   `PackageManager.NameNotFoundException`. ACOS used to send a parcel
//!   exception, so apps caught the wrong class (vlc, org.fdroid.fdroid).
//! * **K3c** — `publishContentProviders` carries a
//!   `List<ContentProviderHolder>` whose generic the decoded table lost (the
//!   proxy writes an `ArrayList`, so the register type is the erasure
//!   `java.util.List`). The override decodes the real element class.
//! * **K3c** — `BaseBundle.writeToParcelInner`'s empty-bundle fast path writes a
//!   single `0` int with no magic; reading it as a corrupt bundle killed
//!   `installContentProviders` after `onCreate`.

use acos_services::codec::read_args_override;
use acos_services::parcelable::{read_bundle, write_payload};
use acos_services::pms::{PmsService, PmsState, ProviderInfo};
use acos_services::value::{Bundle, Parcelable, Value};
use acos_services::{ServiceObject, Wiring};

fn method(interface: &str, name: &str) -> &'static acos_aidl::MethodInfo {
    acos_aidl::lookup_by_name(interface, name).expect("AIDL table entry")
}

fn pms() -> PmsService {
    let state = std::sync::Arc::new(std::sync::Mutex::new(PmsState::hello_views()));
    PmsService::new(state, Wiring::default())
}

#[test]
fn unknown_package_is_a_null_package_info_not_an_exception() {
    let mut pms = pms();
    for name in ["getPackageInfo", "getApplicationInfo"] {
        let reply = pms
            .call(
                "android.content.pm.IPackageManager",
                method("android.content.pm.IPackageManager", name),
                &[Value::Str(Some("com.google.android.gsf".into()))],
            )
            .unwrap_or_else(|e| panic!("{name} must answer, not fail: {e}"))
            .reply;
        assert_eq!(
            reply[0].as_parcelable(),
            Some(&Parcelable::Null),
            "{name} for an unknown package is null (the client raises NameNotFoundException)"
        );
    }
}

#[test]
fn the_known_package_still_answers() {
    let mut pms = pms();
    let own = PmsState::hello_views().package().package_name.clone();
    let reply = pms
        .call(
            "android.content.pm.IPackageManager",
            method("android.content.pm.IPackageManager", "getPackageInfo"),
            &[Value::Str(Some(own))],
        )
        .expect("supported")
        .reply;
    assert!(reply[0].as_parcelable().is_some_and(|p| !matches!(p, Parcelable::Null)));
}

#[test]
fn an_empty_bundle_is_one_zero_int_not_a_magic_error() {
    // `BaseBundle.writeToParcelInner`: `if (map == null || map.size() <= 0)
    // { parcel.writeInt(0); return; }`.
    let mut parcel = acos_parcel::Parcel::new();
    parcel.write_int32(0);
    parcel.set_data_position(0);
    let bundle = read_bundle(&mut parcel).expect("an empty bundle, not an error");
    assert_eq!(bundle, Some(Bundle::new()));
}

#[test]
fn publish_content_providers_decodes_the_typed_list() {
    let state = PmsState::hello_views();
    let provider = ProviderInfo {
        name: "androidx.startup.InitializationProvider".into(),
        package_name: state.package().package_name.clone(),
        authority: "dev.acos.fixtures.hello_views.androidx-startup".into(),
        process_name: state.package().package_name.clone(),
        read_permission: String::new(),
        write_permission: String::new(),
        exported: false,
        grant_uri_permissions: false,
        init_order: 0,
        meta_data: Vec::new(),
    };
    let holder = Parcelable::ordered(
        "android.app.ContentProviderHolder",
        vec![
            Value::Parcelable(provider.to_parcelable(state.app())),
            Value::Binder(None), // provider (IContentProvider): null in this test
            Value::Binder(None), // connection
            Value::Int(1),       // noReleaseNeeded
            Value::Int(1),       // mLocal
        ],
    );
    let mut parcel = acos_parcel::Parcel::new();
    parcel.write_strong_binder_opt(None); // IApplicationThread (null binder)
    parcel.write_int32(1); // list size
    parcel.write_int32(1); // writeTypedObject presence marker
    write_payload(&mut parcel, &holder).expect("holder encodes");
    parcel.set_data_position(0);

    let args = read_args_override("android.app.IActivityManager", 31, &mut parcel)
        .expect("the publishContentProviders override takes the request")
        .expect("the override owns this transaction");
    assert_eq!(args.len(), 2);
    let Some(Value::List(Some(holders))) = args.get(1) else {
        panic!("arg 1 is the holder list");
    };
    assert_eq!(holders.len(), 1);
    let info = holders[0]
        .as_parcelable()
        .and_then(|holder| holder.field(0))
        .and_then(Value::as_parcelable)
        .expect("the ProviderInfo payload");
    assert_eq!(
        info.field(0).and_then(Value::as_str),
        Some("androidx.startup.InitializationProvider")
    );
}
