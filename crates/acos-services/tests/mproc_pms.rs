//! MPROC0 step 1: one session, more than one installed package.
//!
//! Before this change `PmsState` held exactly one package: every query for a
//! second package answered the wire's `null` (`NameNotFoundException` on the
//! client), and `queryIntentActivities` answered the launcher of *the* package
//! regardless of the intent. These tests install a second APK into the same
//! session and assert the per-package answers the framework's
//! `createPackageContext` path needs: `getApplicationInfo` (uid, `sourceDir`),
//! `getPackageInfo` (activities, version), `getPackageUid`,
//! `getPackagesForUid`, `resolveContentProvider`, and the multi-package
//! `queryIntentActivities`/`resolveIntent`.

use std::sync::{Arc, Mutex};

use acos_services::host::Wiring;
use acos_services::parcelable::{INTENT_ACTION, INTENT_CATEGORIES, INTENT_PACKAGE};
use acos_services::pms::{
    cached_intent_activities, PmsService, PmsState, FIXTURE_UID, GET_META_DATA,
};
use acos_services::value::{Parcelable, Value};
use acos_services::ServiceObject;

const PRIMARY: &str = "fixtures/build/apps/hello-views.apk";
const EXTRA: &str = "fixtures/build/apps/counter.apk";

fn apk(rel: &str) -> std::path::PathBuf {
    let path = acos_services::workspace_path(rel).unwrap_or_else(|| std::path::PathBuf::from(rel));
    assert!(
        path.is_file(),
        "{rel} is not built; run tools/build/build-apps.sh (fixtures/build is gitignored)"
    );
    path
}

/// The session state: the app under test plus one extra installed APK, exactly
/// what `acos-art launch --extra-apk` installs before the guest boots.
fn state() -> PmsState {
    let mut state = PmsState::from_apk(apk(PRIMARY)).expect("primary installs");
    state
        .install_extra(apk(EXTRA), Some("/data/app/counter.apk"))
        .expect("extra installs");
    state
}

fn pms(state: PmsState) -> PmsService {
    PmsService::new(Arc::new(Mutex::new(state)), Wiring::default())
}

fn method(name: &str) -> &'static acos_aidl::MethodInfo {
    acos_aidl::lookup_by_name("android.content.pm.IPackageManager", name).expect("AIDL entry")
}

fn call(pms: &mut PmsService, name: &str, args: &[Value]) -> Vec<Value> {
    pms.call("android.content.pm.IPackageManager", method(name), args)
        .expect("supported method")
        .reply
}

/// A decoded `android.content.Intent` parcelable, the shape `ams_java` writes:
/// `mAction`, `mData`, `mType`, `mIdentifier`, `mFlags`, `mPackage`,
/// `mComponent`, `mSourceBounds`, `mCategories`, `mSelector`, `mClipData`,
/// `mContentUserHint`, `mExtras`, `mOriginalIntent`.
fn intent(action: &str, categories: &[&str]) -> Value {
    let mut fields: Vec<Value> = (0..14).map(|_| Value::Str(None)).collect();
    fields[INTENT_ACTION] = Value::Str(Some(action.to_string()));
    fields[4] = Value::Int(0); // mFlags
    fields[7] = Value::Parcelable(Parcelable::Null); // mSourceBounds
    fields[INTENT_CATEGORIES] = Value::StringArray(Some(
        categories.iter().map(|c| Some((*c).to_string())).collect(),
    ));
    for index in [9, 10, 13] {
        fields[index] = Value::Parcelable(Parcelable::Null);
    }
    fields[11] = Value::Int(-1); // mContentUserHint
    fields[12] = Value::Parcelable(Parcelable::Null); // mExtras
    Value::Parcelable(Parcelable::ordered("android.content.Intent", fields))
}

#[test]
fn get_application_info_is_per_package() {
    let mut pms = pms(state());
    let extra = call(
        &mut pms,
        "getApplicationInfo",
        &[
            Value::Str(Some("dev.acos.fixtures.counter".into())),
            Value::Long(0),
        ],
    );
    let Value::TypedObject(info) = &extra[0] else {
        panic!("expected a typed ApplicationInfo");
    };
    // `ApplicationInfo` writes `packageName` first, then the `PackageItemInfo`
    // fields; `sourceDir` is the path the framework opens with `ApkAssets`.
    assert_eq!(
        info.field(2).and_then(Value::as_str),
        Some("dev.acos.fixtures.counter")
    );
    assert_eq!(
        info.field(24).and_then(Value::as_str),
        Some("/data/app/counter.apk"),
        "the extra APK publishes the guest path, not the host file"
    );
    let uid = info.field(45).and_then(Value::as_int).expect("uid field");
    assert_ne!(uid, FIXTURE_UID, "a second package gets its own uid");

    // The primary still answers as itself.
    let primary = call(
        &mut pms,
        "getApplicationInfo",
        &[
            Value::Str(Some("dev.acos.fixtures.helloviews".into())),
            Value::Long(0),
        ],
    );
    let Value::TypedObject(info) = &primary[0] else {
        panic!("expected a typed ApplicationInfo");
    };
    assert_eq!(info.field(45).and_then(Value::as_int), Some(FIXTURE_UID));
}

#[test]
fn get_package_info_answers_the_second_package_activities() {
    let mut pms = pms(state());
    let reply = call(
        &mut pms,
        "getPackageInfo",
        &[
            Value::Str(Some("dev.acos.fixtures.counter".into())),
            Value::Long(GET_META_DATA),
        ],
    );
    let Value::TypedObject(info) = &reply[0] else {
        panic!("expected a typed PackageInfo");
    };
    assert_eq!(
        info.field(0).and_then(Value::as_str),
        Some("dev.acos.fixtures.counter")
    );
    assert_eq!(
        info.field(4).and_then(Value::as_str),
        Some("1.0"),
        "the second package's versionName, not the primary's"
    );
    // The nested `applicationInfo` is the *second* package's (sourceDir is the
    // guest path), which is what `createPackageContext` opens.
    let nested = format!("{:?}", info.field(9).expect("applicationInfo"));
    assert!(
        nested.contains("/data/app/counter.apk"),
        "PackageInfo.applicationInfo names the extra APK: {nested}"
    );
}

#[test]
fn unknown_package_is_the_wire_null_not_an_error() {
    let mut pms = pms(state());
    for name in ["getApplicationInfo", "getPackageInfo"] {
        let reply = call(
            &mut pms,
            name,
            &[
                Value::Str(Some("com.example.absent".into())),
                Value::Long(0),
            ],
        );
        let Value::TypedObject(p) = &reply[0] else {
            panic!("{name} must answer a typed object");
        };
        assert!(matches!(p, Parcelable::Null), "{name} answers null");
    }
}

#[test]
fn uids_are_per_package() {
    let mut pms = pms(state());
    let extra_uid = call(
        &mut pms,
        "getPackageUid",
        &[
            Value::Str(Some("dev.acos.fixtures.counter".into())),
            Value::Int(0),
        ],
    );
    let primary_uid = call(
        &mut pms,
        "getPackageUid",
        &[
            Value::Str(Some("dev.acos.fixtures.helloviews".into())),
            Value::Int(0),
        ],
    );
    assert_ne!(extra_uid[0], primary_uid[0]);
    let Value::Int(uid) = extra_uid[0] else {
        panic!("uid is an int");
    };
    let packages = call(&mut pms, "getPackagesForUid", &[Value::Int(uid)]);
    assert_eq!(
        packages[0],
        Value::StringArray(Some(vec![Some("dev.acos.fixtures.counter".into())]))
    );
}

#[test]
fn query_intent_activities_matches_every_installed_package() {
    let mut pms = pms(state());
    let reply = call(
        &mut pms,
        "queryIntentActivities",
        &[
            intent(
                "android.intent.action.MAIN",
                &["android.intent.category.LAUNCHER"],
            ),
            Value::Str(None),
            Value::Long(0),
            Value::Int(0),
        ],
    );
    let Value::TypedObject(Parcelable::Null) = &reply[0] else {
        panic!("the generic reply is a placeholder for the materialiser");
    };
    let matches = cached_intent_activities();
    let packages: Vec<&str> = matches
        .iter()
        .map(|m| m.activity.package_name.as_str())
        .collect();
    assert!(
        packages.contains(&"dev.acos.fixtures.helloviews"),
        "the primary package's launcher matches: {packages:?}"
    );
    assert!(
        packages.contains(&"dev.acos.fixtures.counter"),
        "the extra package's launcher matches: {packages:?}"
    );
    // Every match carries its own package's application record, so the
    // materialised `ActivityInfo.applicationInfo` names the right APK.
    let extra = matches
        .iter()
        .find(|m| m.activity.package_name == "dev.acos.fixtures.counter")
        .expect("counter matched");
    assert_eq!(extra.app.source_dir, "/data/app/counter.apk");
    assert_eq!(
        extra.declared.class,
        "dev.acos.fixtures.counter.MainActivity"
    );
}

#[test]
fn query_intent_activities_honours_the_intent() {
    let mut pms = pms(state());
    // A package-filtered intent sees only that package.
    let mut filtered = intent(
        "android.intent.action.MAIN",
        &["android.intent.category.LAUNCHER"],
    );
    if let Value::Parcelable(p) = &mut filtered {
        let mut fields = p.fields().expect("ordered").to_vec();
        fields[INTENT_PACKAGE] = Value::Str(Some("dev.acos.fixtures.counter".into()));
        *p = Parcelable::ordered("android.content.Intent", fields);
    }
    call(
        &mut pms,
        "queryIntentActivities",
        &[filtered, Value::Str(None), Value::Long(0), Value::Int(0)],
    );
    let matches = cached_intent_activities();
    assert_eq!(
        matches.len(),
        1,
        "{:?}",
        matches
            .iter()
            .map(|m| &m.activity.package_name)
            .collect::<Vec<_>>()
    );
    assert_eq!(
        matches[0].activity.package_name,
        "dev.acos.fixtures.counter"
    );

    // An action nobody declares is a genuine no-match (empty, not the launcher).
    call(
        &mut pms,
        "queryIntentActivities",
        &[
            intent("android.intent.action.NOT_DECLARED", &[]),
            Value::Str(None),
            Value::Long(0),
            Value::Int(0),
        ],
    );
    assert!(cached_intent_activities().is_empty());
}

#[test]
fn resolve_intent_returns_one_match_for_an_explicit_component() {
    let mut pms = pms(state());
    let component = Value::Parcelable(Parcelable::ordered(
        "android.content.ComponentName",
        vec![
            Value::Str(Some("dev.acos.fixtures.counter".into())),
            Value::Str(Some("dev.acos.fixtures.counter.MainActivity".into())),
        ],
    ));
    let mut fields: Vec<Value> = (0..14).map(|_| Value::Str(None)).collect();
    fields[4] = Value::Int(0);
    fields[6] = component;
    fields[7] = Value::Parcelable(Parcelable::Null);
    fields[8] = Value::StringArray(None);
    fields[11] = Value::Int(-1);
    fields[12] = Value::Parcelable(Parcelable::Null);
    call(
        &mut pms,
        "resolveIntent",
        &[
            Value::Parcelable(Parcelable::ordered("android.content.Intent", fields)),
            Value::Str(None),
            Value::Long(0),
            Value::Int(0),
        ],
    );
    let matches = cached_intent_activities();
    assert_eq!(matches.len(), 1);
    assert_eq!(
        matches[0].activity.name,
        "dev.acos.fixtures.counter.MainActivity"
    );
}

#[test]
fn resolve_content_provider_searches_every_package() {
    let mut pms = pms(state());
    // The extra APK declares no provider; the primary's fixture authority is
    // still answered, and an unknown authority is null (never the primary's
    // provider by accident).
    let reply = call(
        &mut pms,
        "resolveContentProvider",
        &[
            Value::Str(Some("dev.acos.fixtures.helloviews.absent".into())),
            Value::Long(GET_META_DATA),
            Value::Int(0),
        ],
    );
    let Value::TypedObject(p) = &reply[0] else {
        panic!("typed provider reply");
    };
    assert!(matches!(p, Parcelable::Null));
}

#[test]
fn a_duplicate_install_is_a_structured_error() {
    let mut state = state();
    let error = state
        .install_extra(apk(EXTRA), Some("/data/app/counter.apk"))
        .expect_err("installing the same package twice is refused");
    assert!(format!("{error}").contains("already installed"), "{error}");
}
