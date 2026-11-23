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

/// Extract the `ActivityInfo` parcelable from a `ResolveInfo` wire parcelable
/// returned by `IPackageManager.queryIntentActivities` / `resolveIntent`.
fn resolve_info_activity(ri: &Parcelable) -> &Parcelable {
    assert_eq!(ri.class(), Some("android.content.pm.ResolveInfo"));
    // ResolveInfo field 0 is the activityInfo branch (`writeInt(1)` + ActivityInfo).
    let Some(Value::Cond { present: true, values }) = ri.field(0) else {
        panic!("expected ResolveInfo with present activityInfo: {ri:?}");
    };
    let Some(Value::Parcelable(ai)) = values.first() else {
        panic!("expected ActivityInfo inside ResolveInfo branch: {values:?}");
    };
    assert_eq!(ai.class(), Some("android.content.pm.ActivityInfo"));
    ai
}

fn activity_app_info(ai: &Parcelable) -> &Parcelable {
    let Some(Value::Parcelable(app)) = ai.field(9) else {
        panic!("expected ApplicationInfo in ActivityInfo field 9: {ai:?}");
    };
    assert_eq!(app.class(), Some("android.content.pm.ApplicationInfo"));
    app
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
    // AOSP `IPackageManager.queryIntentActivities` returns a
    // `ParceledListSlice<ResolveInfo>` containing every matching activity across
    // installed packages.
    let Value::TypedObject(slice) = &reply[0] else {
        panic!("expected TypedObject(ParceledListSlice), got {:?}", reply[0]);
    };
    let items = acos_services::parcelable::parceled_list_items(slice)
        .expect("queryIntentActivities must return a ParceledListSlice");
    assert_eq!(
        items.len(),
        2,
        "expected exactly the primary and extra launcher activities, got {items:?}"
    );

    let mut wire_matches = Vec::new();
    for item in items {
        let Value::Parcelable(ri) = item else {
            panic!("expected ResolveInfo parcelable in slice, got {item:?}");
        };
        let ai = resolve_info_activity(ri);
        let app = activity_app_info(ai);
        let pkg = ai.field(1).and_then(Value::as_str).expect("activity packageName");
        let name = ai.field(0).and_then(Value::as_str).expect("activity name");
        let exported = ai.field(15).and_then(Value::as_int).expect("exported");
        let app_pkg = app.field(2).and_then(Value::as_str).expect("app packageName");
        let app_label_res = app.field(3).and_then(Value::as_int).expect("app labelRes");
        let app_label_lit = app.field(4).and_then(Value::as_str);
        let source_dir = app.field(24).and_then(Value::as_str).expect("app sourceDir");
        let uid = app.field(45).and_then(Value::as_int).expect("app uid");
        assert_eq!(pkg, app_pkg);
        assert_eq!(exported, 1, "{pkg}/{name} must be exported");
        assert!(
            app_label_res != 0 || app_label_lit.is_some(),
            "{pkg} must carry its manifest application label"
        );
        wire_matches.push((pkg, name, source_dir, uid, app_label_res));
    }

    let primary = wire_matches
        .iter()
        .find(|m| m.0 == "dev.acos.fixtures.helloviews")
        .expect("primary package hello-views in ParceledListSlice");
    assert_eq!(primary.1, "dev.acos.fixtures.helloviews.MainActivity");
    assert_eq!(primary.3, FIXTURE_UID);
    assert_ne!(primary.4, 0, "hello-views declares @string/app_name");

    let extra_wire = wire_matches
        .iter()
        .find(|m| m.0 == "dev.acos.fixtures.counter")
        .expect("extra package counter in ParceledListSlice");
    assert_eq!(extra_wire.1, "dev.acos.fixtures.counter.MainActivity");
    assert_eq!(extra_wire.2, "/data/app/counter.apk");
    assert_ne!(extra_wire.3, FIXTURE_UID, "extra package has its own uid");
    assert_ne!(extra_wire.4, 0, "counter declares @string/app_name");

    // The VM-path cache stays in sync with the wire reply.
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
    let reply = call(
        &mut pms,
        "queryIntentActivities",
        &[filtered, Value::Str(None), Value::Long(0), Value::Int(0)],
    );
    let Value::TypedObject(slice) = &reply[0] else {
        panic!("expected ParceledListSlice, got {:?}", reply[0]);
    };
    let items = acos_services::parcelable::parceled_list_items(slice)
        .expect("filtered queryIntentActivities returns ParceledListSlice");
    assert_eq!(items.len(), 1);
    let Value::Parcelable(ri) = &items[0] else {
        panic!("expected ResolveInfo item");
    };
    let ai = resolve_info_activity(ri);
    assert_eq!(
        ai.field(1).and_then(Value::as_str),
        Some("dev.acos.fixtures.counter")
    );
    assert_eq!(
        ai.field(0).and_then(Value::as_str),
        Some("dev.acos.fixtures.counter.MainActivity")
    );
    assert_eq!(
        activity_app_info(ai).field(24).and_then(Value::as_str),
        Some("/data/app/counter.apk")
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

    // An action nobody declares is a genuine no-match (empty slice, not null
    // and not the launcher).
    let empty_reply = call(
        &mut pms,
        "queryIntentActivities",
        &[
            intent("android.intent.action.NOT_DECLARED", &[]),
            Value::Str(None),
            Value::Long(0),
            Value::Int(0),
        ],
    );
    let Value::TypedObject(empty_slice) = &empty_reply[0] else {
        panic!("expected empty ParceledListSlice, got {:?}", empty_reply[0]);
    };
    assert!(
        acos_services::parcelable::parceled_list_items(empty_slice)
            .expect("no-match returns empty ParceledListSlice")
            .is_empty()
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
    let reply = call(
        &mut pms,
        "resolveIntent",
        &[
            Value::Parcelable(Parcelable::ordered("android.content.Intent", fields)),
            Value::Str(None),
            Value::Long(0),
            Value::Int(0),
        ],
    );
    let Value::TypedObject(ri) = &reply[0] else {
        panic!("expected ResolveInfo, got {:?}", reply[0]);
    };
    let ai = resolve_info_activity(ri);
    assert_eq!(
        ai.field(1).and_then(Value::as_str),
        Some("dev.acos.fixtures.counter")
    );
    assert_eq!(
        ai.field(0).and_then(Value::as_str),
        Some("dev.acos.fixtures.counter.MainActivity")
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
