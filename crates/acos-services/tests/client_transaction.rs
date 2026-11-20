//! MPROC6: the host's `ClientTransaction` encoder.
//!
//! The bytes are checked against real Java in `client_transaction_java.rs`; here
//! the shapes are pinned on the Rust side: every item round-trips through the
//! parcel codec, an `Intent`'s extras survive, and the narrow ATMS call site
//! returns a delivery for the process that owns the activity (and nothing for a
//! launch whose process is not running).

use acos_parcel::BinderId;
use acos_services::ams::{IntentFacts, START_SUCCESS};
use acos_services::atms::AtmsState;
use acos_services::client_transaction::{
    self as ctx, activity_result_item, destroy_item, encode, intent, launch_transaction,
    new_intent_item, pause_item, process_configuration, referrer_intent, resume_item, stop_item,
    LaunchActivity, LaunchDelivery,
};
use acos_services::value::{Bundle, Parcelable, Value};

const PACKAGE: &str = "dev.acos.fixtures.multiactivity";
const SECOND: &str = "dev.acos.fixtures.multiactivity.SecondActivity";

fn apk() -> std::path::PathBuf {
    acos_services::workspace_path("fixtures/build/apps/multi-activity.apk")
        .expect("multi-activity fixture APK")
}

/// The intent the acceptance evidence uses: an explicit target with a string,
/// an int and a parcelable extra.
fn intent_with_extras() -> IntentFacts {
    let mut facts = IntentFacts::explicit(PACKAGE, SECOND);
    facts.extras.insert(
        "greeting".into(),
        acos_services::ams::Extra::String("hello".into()),
    );
    facts
        .extras
        .insert("count".into(), acos_services::ams::Extra::Int(7));
    facts
        .extras
        .insert("flag".into(), acos_services::ams::Extra::Bool(true));
    facts
}

fn plain_launch(intent: Parcelable) -> LaunchActivity {
    let config = process_configuration();
    let (app, activity) = installed();
    LaunchActivity::plain(
        intent,
        ctx::activity_info(&app, &activity),
        config.clone(),
        config,
        Some(PACKAGE.to_string()),
    )
}

fn decoded(bytes: &[u8], class: &str) -> Parcelable {
    ctx::decode(class, bytes).expect("decode")
}

/// The callbacks of a decoded `ClientTransaction` (the list the presence flag
/// guards; empty when the transaction is lifecycle-only).
fn callbacks(tx: &Parcelable) -> Vec<Parcelable> {
    let Parcelable::Ordered { values, .. } = tx else {
        panic!("not an ordered parcelable");
    };
    let Some(Value::Cond { present, values }) = values.last() else {
        panic!("no callbacks-presence flag");
    };
    if !present {
        return Vec::new();
    }
    let Some(Value::List(Some(items))) = values.first() else {
        panic!("no callback list");
    };
    items
        .iter()
        .map(|v| match v {
            Value::Parcelable(p) => p.clone(),
            other => panic!("callback is {other:?}"),
        })
        .collect()
}

#[test]
fn every_item_round_trips_through_the_codec() {
    let items = vec![
        (
            "launch",
            plain_launch(intent(&intent_with_extras()).unwrap()).to_parcelable(),
        ),
        ("resume", resume_item(2, true, false, false)),
        ("pause", pause_item(false, true, 0, false, false)),
        ("stop", stop_item(0x0400)),
        ("destroy", destroy_item(true, 0)),
        (
            "result",
            activity_result_item(vec![ctx::result_info(
                Some("who"),
                5,
                1,
                Some(intent(&intent_with_extras()).unwrap()),
            )]),
        ),
        (
            "new_intent",
            new_intent_item(
                vec![referrer_intent(
                    intent(&intent_with_extras()).unwrap(),
                    Some(PACKAGE),
                )],
                true,
            ),
        ),
    ];
    for (name, value) in items {
        let bytes = encode(&value).unwrap_or_else(|e| panic!("{name}: encode: {e}"));
        let class = value.class().expect("class").to_string();
        let back = decoded(&bytes, &class);
        assert_eq!(back.class(), Some(class.as_str()), "{name}: class");
        assert_eq!(
            encode(&back).expect("re-encode"),
            bytes,
            "{name}: decode/encode is not the wire identity"
        );
    }
}

#[test]
fn a_client_transaction_carries_its_lifecycle_request_and_callbacks() {
    let launch = plain_launch(intent(&intent_with_extras()).unwrap());
    let tx = launch_transaction(
        &launch,
        BinderId(0x1234),
        resume_item(2, true, false, false),
    );
    let bytes = encode(&tx).expect("encode transaction");
    let back = decoded(&bytes, ctx::CLIENT_TRANSACTION);
    let Parcelable::Ordered { values, .. } = &back else {
        panic!("not a ClientTransaction");
    };
    // The image's shape: activity-token presence + binder, the named lifecycle
    // request, the callbacks-presence flag (+ list when non-empty).
    assert_eq!(
        values[0],
        Value::Cond {
            present: true,
            values: vec![Value::Binder(Some(BinderId(0x1234)))],
        },
        "mActivityToken"
    );
    assert_eq!(
        values[1],
        Value::NamedParcelable(resume_item(2, true, false, false)),
        "mLifecycleStateRequest"
    );
    let Value::Cond { present, .. } = &values[2] else {
        panic!("the callbacks-presence flag is not a conditional");
    };
    assert!(*present, "a transaction with a callback writes the list");
    let callbacks = callbacks(&back);
    assert_eq!(callbacks.len(), 1);
    assert_eq!(callbacks[0].class(), Some(ctx::LAUNCH_ACTIVITY_ITEM));
}

#[test]
fn intent_extras_and_the_component_survive_the_encoder() {
    let facts = intent_with_extras();
    let value = intent(&facts).expect("encode intent");
    let bytes = encode(&value).expect("encode");
    let back = ctx::decode(ctx::INTENT, &bytes).expect("decode");
    let Parcelable::Ordered { values, .. } = &back else {
        panic!("not an Intent");
    };
    assert_eq!(
        values[acos_services::parcelable::INTENT_ACTION],
        Value::Str(None),
        "an explicit intent carries no action"
    );
    match &values[acos_services::parcelable::INTENT_COMPONENT] {
        Value::Parcelable(Parcelable::Ordered { values, .. }) => {
            assert_eq!(values[0], Value::Str(Some(PACKAGE.into())));
            assert_eq!(values[1], Value::Str(Some(SECOND.into())));
        }
        other => panic!("component is {other:?}"),
    }
    let Some(Value::Bundle(Some(bundle))) = values.get(acos_services::parcelable::INTENT_EXTRAS)
    else {
        panic!("no extras bundle");
    };
    assert_eq!(
        bundle.get("greeting"),
        Some(&Value::Str(Some("hello".into())))
    );
    assert_eq!(bundle.get("count"), Some(&Value::Int(7)));
    assert_eq!(bundle.get("flag"), Some(&Value::Bool(true)));
}

#[test]
fn an_extra_without_a_recorded_value_is_refused() {
    let mut facts = IntentFacts::explicit(PACKAGE, SECOND);
    facts
        .extras
        .insert("bitmap".into(), acos_services::ams::Extra::Present);
    let err = intent(&facts).expect_err("a present-only extra cannot be reproduced");
    assert!(
        matches!(
            err,
            acos_services::ServiceError::UnsupportedParcelable { .. }
        ),
        "structured refusal, got {err:?}"
    );
}

// ---------------------------------------------------------------------------
// the narrow ATMS call site
// ---------------------------------------------------------------------------

/// An ATMS with `PACKAGE`'s APK installed and its process `tag` attached.
fn atms_with_process(tag: u32) -> AtmsState {
    let mut atms = AtmsState::new();
    atms.ams
        .manifests
        .add_apk(apk(), acos_services::pms::FIXTURE_UID)
        .expect("seed the APK component table");
    atms.process_started(tag, PACKAGE, acos_services::pms::FIXTURE_UID, 0);
    atms.attach_application_for(tag, 0x1000, 7, PACKAGE);
    atms.process_mut(tag).expect("process").attached = true;
    atms
}

fn installed() -> (
    acos_services::pms::AppInfo,
    acos_services::pms::ActivityInfo,
) {
    let state = acos_services::pms::PmsState::from_apk(apk()).expect("PMS state");
    let record = state.lookup(Some(PACKAGE)).expect("installed package");
    let activity = record
        .package
        .activities
        .iter()
        .find(|a| a.name == SECOND)
        .expect("SecondActivity")
        .clone();
    (record.app.clone(), activity)
}

#[test]
fn a_launch_for_a_running_process_is_delivered_as_a_client_transaction() {
    let mut atms = atms_with_process(3);
    let mut facts = intent_with_extras();
    facts.action = Some("android.intent.action.MAIN".into());
    let code = atms
        .ams
        .start_activity(&facts, 0, 0, 0)
        .expect("startActivity resolves the declared target");
    assert_eq!(code, START_SUCCESS);
    let token = atms.ams.pending_launches[0].token;

    let (app, activity) = installed();
    let delivery = atms
        .launch_delivery(token, &app, &activity, BinderId(0xabc))
        .expect("encode");
    let LaunchDelivery::Ready(schedule) = delivery else {
        panic!("expected a ready delivery, got {delivery:?}");
    };
    assert_eq!(
        schedule.process_tag, 3,
        "the process that owns the activity"
    );
    assert_eq!(schedule.method, "scheduleTransaction");

    let tx = decoded(&schedule.parcel, ctx::CLIENT_TRANSACTION);
    let Parcelable::Ordered { values, .. } = &tx else {
        panic!("not a ClientTransaction");
    };
    assert_eq!(
        values[0],
        Value::Cond {
            present: true,
            values: vec![Value::Binder(Some(BinderId(0xabc)))],
        }
    );
    assert_eq!(
        values[1],
        Value::NamedParcelable(resume_item(2, true, false, false)),
        "a launch with andResume=true resumes in the same transaction"
    );
    let callbacks = callbacks(&tx);
    assert_eq!(callbacks.len(), 1);
    let Parcelable::Ordered { values, .. } = &callbacks[0] else {
        panic!("not a LaunchActivityItem");
    };
    // mIntent is the first field: the extras the caller sent must be there.
    let Value::TypedObject(intent) = &values[0] else {
        panic!("mIntent is not a typed object");
    };
    let Parcelable::Ordered {
        values: intent_fields,
        ..
    } = intent
    else {
        panic!("not an Intent");
    };
    let Some(Value::Bundle(Some(bundle))) =
        intent_fields.get(acos_services::parcelable::INTENT_EXTRAS)
    else {
        panic!("the intent has no extras after the encode");
    };
    assert_eq!(
        bundle.get("greeting"),
        Some(&Value::Str(Some("hello".into())))
    );
    assert_eq!(bundle.get("count"), Some(&Value::Int(7)));
    assert_eq!(bundle.get("flag"), Some(&Value::Bool(true)));
    assert_eq!(
        intent_fields[acos_services::parcelable::INTENT_ACTION],
        Value::Str(Some("android.intent.action.MAIN".into()))
    );
}

#[test]
fn a_launch_without_a_running_process_or_record_is_not_delivered() {
    let (app, activity) = installed();
    // Installed but no process attached for the package.
    let mut atms = AtmsState::new();
    atms.ams
        .manifests
        .add_apk(apk(), acos_services::pms::FIXTURE_UID)
        .expect("seed the APK component table");
    atms.ams
        .start_activity(&IntentFacts::explicit(PACKAGE, SECOND), 0, 0, 0)
        .expect("startActivity");
    let token = atms.ams.pending_launches[0].token;
    assert_eq!(
        atms.launch_delivery(token, &app, &activity, BinderId(0xabc))
            .expect("encode"),
        LaunchDelivery::ProcessNotRunning
    );

    // No recorded launch for this token.
    let atms = atms_with_process(0);
    assert_eq!(
        atms.launch_delivery(0xdead, &app, &activity, BinderId(0xabc))
            .expect("encode"),
        LaunchDelivery::NoPendingLaunch
    );
}

#[test]
fn the_process_configuration_matches_the_bind_application_values() {
    let config = process_configuration();
    let bytes = encode(&config).expect("encode");
    let Parcelable::Ordered { values, .. } =
        ctx::decode(ctx::CONFIGURATION, &bytes).expect("decode")
    else {
        panic!("not a Configuration");
    };
    assert_eq!(values[0], Value::Float(1.0), "fontScale");
    assert_eq!(values[11], Value::Int(1), "orientation PORTRAIT");
    assert_eq!(
        values[18],
        Value::Int(match acos_services::display::display_override() {
            Some(o) => o.density_dpi,
            None => 420,
        }),
        "densityDpi"
    );
    // `LocaleList` is the en-US singleton the bind path sets.
    let Value::TypedObject(Parcelable::Ordered {
        values: locales, ..
    }) = &values[3]
    else {
        panic!("mLocaleList is not a LocaleList");
    };
    assert_eq!(locales[0], Value::Str8(Some("en-US".into())));
}

#[test]
fn a_bundle_extra_keeps_the_frameworks_array_map_order() {
    // `Parcel.readArrayMap` requires ascending Java `hashCode` order; the codec
    // sorts on write, so a bundle built in any order encodes identically.
    let mut a = Bundle::new();
    a.put("z", Value::Int(1));
    a.put("a", Value::Int(2));
    a.put("m", Value::Int(3));
    let mut b = Bundle::new();
    b.put("m", Value::Int(3));
    b.put("z", Value::Int(1));
    b.put("a", Value::Int(2));
    assert_eq!(
        encode(&ctx::intent_payload(
            None,
            None,
            0,
            None,
            None,
            &[],
            Some(a)
        ))
        .unwrap(),
        encode(&ctx::intent_payload(
            None,
            None,
            0,
            None,
            None,
            &[],
            Some(b)
        ))
        .unwrap()
    );
    // And the harness value itself round-trips with the three keys intact.
    let facts = intent_with_extras();
    let bytes = encode(&intent(&facts).unwrap()).unwrap();
    let Parcelable::Ordered { values, .. } = ctx::decode(ctx::INTENT, &bytes).unwrap() else {
        panic!("not an Intent");
    };
    let Some(Value::Bundle(Some(bundle))) = values.get(acos_services::parcelable::INTENT_EXTRAS)
    else {
        panic!("no extras");
    };
    assert_eq!(bundle.len(), 3);
}

/// MPROC7: `FLAG_ACTIVITY_SINGLE_TOP` to the resumed top activity is an
/// `onNewIntent`, never a second launch.
///
/// This pins the activity-manager decision (the peer turns
/// `pending_new_intents` into a `NewIntentItem` `ClientTransaction`): with the
/// flag, no `PendingLaunch` is recorded and the delivery is queued for the
/// existing activity token.
#[test]
fn single_top_to_the_top_activity_records_a_new_intent_not_a_launch() {
    const FLAG_ACTIVITY_SINGLE_TOP: i32 = 0x2000_0000;
    let mut atms = atms_with_process(1);
    let facts = IntentFacts::explicit(PACKAGE, SECOND);
    assert_eq!(
        atms.ams
            .start_activity(&facts, 0, 0, -1)
            .expect("startActivity"),
        START_SUCCESS
    );
    let placeholder = atms.ams.pending_launches[0].token;
    // The host delivered that launch with its own token (MPROC7).
    let host_token = 0x4000_0004d;
    atms.ams.take_launch(placeholder);
    atms.ams.bind_launch_token(placeholder, host_token);
    atms.ensure_activity(host_token, SECOND);
    atms.activity_resumed(host_token, true)
        .expect("the app resumed it");

    // The real caller's shape: `Instrumentation.execStartActivity` passes `0`
    // as the AIDL startFlags argument and the flag travels in the Intent.
    let mut with_flag = facts.clone();
    with_flag.flags = FLAG_ACTIVITY_SINGLE_TOP;
    assert_eq!(
        atms.ams
            .start_activity(&with_flag, 0, 0, -1)
            .expect("singleTop startActivity"),
        START_SUCCESS
    );
    assert!(
        atms.ams.pending_launches.is_empty(),
        "singleTop reuses the instance: no launch transaction"
    );
    assert!(
        atms.ams
            .pending_new_intents
            .iter()
            .any(|(token, _)| *token == host_token),
        "the onNewIntent is queued for the activity that is already on top"
    );
    let transaction = atms
        .take_new_intent_for_tag(1)
        .expect("NewIntentItem transaction for the process that owns the activity");

    assert_eq!(class_of(&transaction), ctx::CLIENT_TRANSACTION);
}

fn class_of(value: &Parcelable) -> &str {
    match value {
        Parcelable::Ordered { class, .. } => class.as_str(),
        _ => "",
    }
}
