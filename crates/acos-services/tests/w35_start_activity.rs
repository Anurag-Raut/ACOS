//! W35: `IActivityTaskManager.startActivity` decoded **host-side** off the real
//! argument layout, and the request recorded (component, action, flags).
//!
//! Before the W35 `Intent` codec the transaction could not decode at all:
//! `codec::read_args` reaches `parcelable::read_payload("android.content.Intent")`,
//! `spec_for` returns `None` for the class (it is not a flat field list), and the
//! host answered `UNSUPPORTED_PARCELABLE`. The rows below build the image's own
//! argument order (binder, package, featureId, **Intent**, resolvedType, resultTo,
//! resultWho, requestCode, flags, profilerInfo, bOptions) and assert that the
//! request now decodes and lands in `AmsState::pending_launches` — the request
//! the runtime would have to turn into a `ClientTransaction`. The second-Activity
//! *launch* still needs the stock `LaunchActivityItem` delivery in `acos-art`, so
//! that half keeps W27's structured `UNSUPPORTED_ANDROID_API`.

use acos_parcel::{BinderId, Parcel};
use acos_services::ams::{self, AmsState, DeclaredComponent, ManifestDb, PackageRecord, START_SUCCESS};
use acos_services::codec;
use acos_services::parcelable::{self, INTENT_CLASS};
use acos_services::{Bundle, Parcelable, Value};

const ATMS: &str = "android.app.IActivityTaskManager";
const PACKAGE: &str = "dev.acos.fixtures.multiactivity";
const CLASS: &str = "dev.acos.fixtures.multiactivity.SecondActivity";

fn manifest_db() -> ManifestDb {
    let mut db = ManifestDb::new();
    db.packages.push(PackageRecord {
        package: PACKAGE.into(),
        uid: 10_200,
        activities: vec![
            DeclaredComponent {
                package: PACKAGE.into(),
                class: format!("{PACKAGE}.MainActivity"),
                exported: true,
                launch_mode: 0,
                filters: vec![],
            },
            DeclaredComponent {
                package: PACKAGE.into(),
                class: CLASS.into(),
                exported: true,
                launch_mode: 0,
                filters: vec![],
            },
        ],
        services: vec![],
        receivers: vec![],
        providers: vec![],
    });
    db
}

/// A component-launch Intent, encoded by the wire codec.
fn launch_intent(action: &str, package: &str, class: &str, extras: bool) -> Parcelable {
    let mut bundle = Bundle::new();
    if extras {
        bundle.put("msg", Value::Str(Some("hello".into())));
    }
    let component = Parcelable::ordered(
        "android.content.ComponentName",
        vec![
            Value::Str(Some(package.into())),
            Value::Str(Some(class.into())),
        ],
    );
    Parcelable::ordered(
        INTENT_CLASS,
        vec![
            Value::Str(Some(action.into())),
            Value::Str(None),
            Value::Str(None),
            Value::Str(None),
            Value::Int(0),
            Value::Str(Some(PACKAGE.into())),
            Value::Parcelable(component),
            Value::Parcelable(Parcelable::Null),
            Value::StringList(Some(vec![Some(
                "android.intent.category.DEFAULT".into(),
            )])),
            Value::Parcelable(Parcelable::Null),
            Value::Parcelable(Parcelable::Null),
            Value::Int(-2),
            Value::Bundle(Some(bundle)),
            Value::Parcelable(Parcelable::Null),
        ],
    )
}

/// The `IActivityTaskManager.startActivity` request parcel the guest's proxy
/// writes, in the image's argument order.
fn start_activity_parcel(intent: &Parcelable, request_code: i32, flags: i32) -> Parcel {
    let mut parcel = Parcel::new();
    parcel
        .write_interface_token(ATMS)
        .expect("interface token");
    parcel.write_strong_binder_opt(Some(BinderId(0x4000)));
    parcel.write_string16(PACKAGE).expect("callingPackage");
    parcel.write_string16("feature").expect("callingFeatureId");
    parcel.write_int32(1); // Intent: writeTypedObject presence
    parcelable::write_intent(&mut parcel, intent).expect("the Intent writes");
    parcel.write_string16("text/plain").expect("resolvedType");
    parcel.write_strong_binder_opt(None);
    parcel.write_string16("who").expect("resultWho");
    parcel.write_int32(request_code); // requestCode
    parcel.write_int32(flags); // startFlags
    parcel.write_int32(0); // no ProfilerInfo
    parcel.write_int32(0); // no bOptions
    parcel
}

#[test]
fn the_start_activity_request_decodes_and_is_recorded() {
    let method = acos_aidl::lookup_by_name(ATMS, "startActivity").expect("the image's table");
    let intent = launch_intent(
        "android.intent.action.MAIN",
        PACKAGE,
        CLASS,
        true,
    );
    let mut parcel = start_activity_parcel(&intent, 7, 0);
    parcel.set_data_position(0);
    // The binder token prefix (strict-mode policy, work source, header, then the
    // descriptor String16) is what the peer strips before the args.
    for _ in 0..3 {
        let _ = parcel.read_int32().expect("token prefix word");
    }
    assert_eq!(
        parcel.read_string16().expect("token"),
        Some(ATMS.into()),
        "the request carries the interface token first"
    );
    let args = codec::read_args(&mut parcel, &method.args, ATMS, "startActivity")
        .expect("the Intent argument decodes host-side");
    assert!(
        matches!(&args[3], Value::TypedObject(Parcelable::Ordered { class, .. }) if class == INTENT_CLASS),
        "arg 3 is the Intent, got {:?}",
        args[3]
    );

    let mut state = AmsState::new();
    state.manifests = manifest_db();
    let reply = ams::dispatch(&mut state, ATMS, method, &args)
        .expect("startActivity is served")
        .expect("an ATMS method");
    assert_eq!(
        reply.reply,
        vec![Value::Int(START_SUCCESS)],
        "stock START_SUCCESS for a component launch"
    );
    let launches = state.take_pending_launches();
    assert_eq!(launches.len(), 1, "exactly one launch was recorded");
    let launch = &launches[0];
    assert_eq!(launch.package, PACKAGE);
    assert_eq!(launch.class, CLASS, "component recorded");
    assert_eq!(
        launch.intent.action.as_deref(),
        Some("android.intent.action.MAIN"),
        "action recorded"
    );
    assert_eq!(launch.request_code, 7, "requestCode recorded");
    assert_eq!(
        launch.intent.extras.get("msg"),
        Some(&acos_services::ams::Extra::String("hello".into())),
        "the extras Bundle survived the codec"
    );
}

#[test]
fn a_flag_the_host_does_not_honour_stays_a_structured_refusal() {
    // `startActivity` decodes, but a launch contract the host cannot honour is
    // still `UNSUPPORTED_ANDROID_API` (0x0000_0004 = FLAG_ACTIVITY_NO_ANIMATION).
    let method = acos_aidl::lookup_by_name(ATMS, "startActivity").expect("the image's table");
    let intent = launch_intent("android.intent.action.MAIN", PACKAGE, CLASS, false);
    let mut parcel = start_activity_parcel(&intent, 0, 0x0000_0004);
    parcel.set_data_position(0);
    for _ in 0..3 {
        parcel.read_int32().expect("token prefix word");
    }
    parcel.read_string16().expect("token");
    let args = codec::read_args(&mut parcel, &method.args, ATMS, "startActivity")
        .expect("decodes");
    let mut state = AmsState::new();
    state.manifests = manifest_db();
    let err = ams::dispatch(&mut state, ATMS, method, &args)
        .expect_err("an unhonoured flag is refused");
    assert!(
        matches!(err, acos_services::ServiceError::UnsupportedAndroidApi { .. }),
        "{err:?}"
    );
    assert!(state.take_pending_launches().is_empty());
}
