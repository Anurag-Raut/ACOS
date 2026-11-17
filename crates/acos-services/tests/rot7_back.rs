//! ROT7: the **server** half of the framework's own back path, API 34.
//!
//! `Activity.onBackPressed()` never finishes an Activity by itself. It calls
//! `ActivityClient.onBackPressed(mToken, new RequestFinishCallback(this))`
//! (`IActivityClientController.onBackPressed`, code 62, one-way), and
//! `ActivityClientController.onBackPressed` is the side that decides: it
//! resolves the `ActivityRecord` and asks the client to finish through the
//! callback it was handed. The callback's `requestFinish()` posts
//! `Activity.finishAfterTransition()` → `Activity.finish()` →
//! `IActivityClientController.finishActivity`, whose `true` is what flips
//! `Activity.mFinished`; `onPause`/`onSaveInstanceState`/`onStop`/`onDestroy`
//! are then `ClientTransaction`s the server sends.
//!
//! Rows:
//! * a back press on an adopted guest token records the press and queues
//!   exactly one reverse `IRequestFinishCallback.requestFinish` to the client
//!   binder the transaction carried — it never fabricates the finish itself;
//! * the delivery uses the **image's** transaction code (1, one-way), so the
//!   framework's own `IRequestFinishCallback.Stub` is what receives it;
//! * the client's `finishActivity` after that resolves and flips `finished`
//!   (`mFinished` on the guest), which is the trigger the lifecycle needs;
//! * a zero or unknown token still errors, and a null callback is a bad
//!   request rather than a silent finish (`an_unknown_or_zero_token_still_errors`
//!   in `w38_activity_token.rs` stays untouched).

use std::sync::{Arc, Mutex};

use acos_parcel::BinderId;
use acos_services::atms::{ActivityLifecycle, AtmsInterface, AtmsService, AtmsState};
use acos_services::host::{CallResult, ServiceObject, Wiring};
use acos_services::value::Value;

/// The peer's `JAVA_TAG`: a binder owned by the guest JVM (`acos-binder-peer`).
const GUEST_TAG: u64 = 1 << 32;

fn attached() -> Arc<Mutex<AtmsState>> {
    let mut state = AtmsState::new();
    state.attach_application(0x1000, 7, "dev.acos.fixtures.counter");
    state.finish_attach_application(7).expect("finish attach");
    Arc::new(Mutex::new(state))
}

fn controller(state: &Arc<Mutex<AtmsState>>) -> AtmsService {
    AtmsService::new(
        AtmsInterface::ClientController,
        Arc::clone(state),
        Wiring::default(),
    )
}

fn method(name: &str) -> acos_aidl::MethodInfo {
    acos_aidl::lookup_by_name("android.app.IActivityClientController", name)
        .unwrap_or_else(|| panic!("IActivityClientController.{name} is in the image tables"))
        .clone()
}

fn call(service: &mut AtmsService, name: &str, args: &[Value]) -> acos_services::Result<CallResult> {
    service.call(
        "android.app.IActivityClientController",
        &method(name),
        args,
    )
}

fn binder(id: u64) -> Value {
    Value::Binder(Some(BinderId(id)))
}

/// Launch the guest's real activity token the way `ResumeActivityItem` does.
fn resume(service: &mut AtmsService, token: u64) {
    call(service, "activityResumed", &[binder(token), Value::Bool(true)])
        .expect("activityResumed adopts a guest-tagged token");
}

#[test]
fn a_back_press_asks_the_client_to_finish() {
    let state = attached();
    let mut service = controller(&state);
    let token = GUEST_TAG | 0x123;
    let callback = GUEST_TAG | 0x456;
    resume(&mut service, token);

    let result = call(&mut service, "onBackPressed", &[binder(token), binder(callback)])
        .expect("onBackPressed is handled");
    assert!(result.reply.is_empty(), "onBackPressed is one-way: no reply values");

    // Exactly one reverse call, to the *client's* callback binder, in the
    // image's own interface/transaction shape.
    assert_eq!(result.deliveries.len(), 1, "one requestFinish delivery");
    let delivery = &result.deliveries[0];
    assert_eq!(delivery.target, BinderId(callback), "the callback the client sent");
    assert_eq!(delivery.interface, "android.app.IRequestFinishCallback");
    assert_eq!(delivery.method, "requestFinish");
    assert_eq!(delivery.code, 1, "decoded from the image's $Stub");
    let info = acos_aidl::lookup_by_name("android.app.IRequestFinishCallback", "requestFinish")
        .expect("interface is decoded");
    assert!(info.oneway, "the framework declares it one-way");
    assert!(info.args.is_empty(), "requestFinish takes no arguments");

    // Nothing is fabricated: the press is recorded, the Activity is *not*
    // finished and no lifecycle transition was emitted for it.
    let locked = state.lock().unwrap();
    let record = locked
        .activities
        .iter()
        .find(|a| a.token == token)
        .expect("record for the adopted token");
    assert!(record.back_pressed, "the press is recorded on the ActivityRecord");
    assert!(!record.finished, "the host does not finish it: the client does");
    assert_eq!(record.lifecycle, ActivityLifecycle::Uncreated);
    assert!(
        !locked
            .events
            .iter()
            .any(|e| e.state == acos_protocol::LifecycleState::Destroyed),
        "a back press is not a destroy"
    );
    assert!(
        locked.pending_reverse_calls.is_empty(),
        "the queue is drained by the reply"
    );
}

#[test]
fn the_client_finish_after_the_callback_flips_finished() {
    // The AOSP sequence end to end: the framework's back path asks the client
    // to finish, the client's `Activity.finish()` then reports `finishActivity`
    // and only *that* reply (`true`) marks the Activity finished.
    let state = attached();
    let mut service = controller(&state);
    let token = GUEST_TAG | 0x81;
    resume(&mut service, token);
    call(
        &mut service,
        "onBackPressed",
        &[binder(token), binder(GUEST_TAG | 0x82)],
    )
    .expect("onBackPressed");

    let reply = call(
        &mut service,
        "finishActivity",
        &[
            binder(token),
            Value::Int(0),
            Value::TypedObject(acos_services::value::Parcelable::Null),
            Value::Int(0),
        ],
    )
    .expect("finishActivity resolves for the adopted token");
    assert_eq!(
        reply.reply,
        vec![Value::Bool(true)],
        "the client sets `mFinished` only on `true`"
    );
    let locked = state.lock().unwrap();
    let record = locked
        .activities
        .iter()
        .find(|a| a.token == token)
        .expect("record stays until the destroy transaction");
    assert!(record.finished, "Activity.finish() marked it finished");
    // The destroy itself is still the server's ClientTransaction, delivered by
    // the guest's transaction executor; the host only recorded the finish.
    assert_eq!(
        locked.lifecycle_of(token),
        Some(ActivityLifecycle::Uncreated),
        "no lifecycle transition is invented by finishActivity"
    );
}

#[test]
fn back_on_an_unknown_or_zero_token_still_errors() {
    let state = attached();
    let mut service = controller(&state);
    let callback = GUEST_TAG | 0x99;
    for token in [0u64, 0x77] {
        let err = call(&mut service, "onBackPressed", &[binder(token), binder(callback)])
            .expect_err("an arbitrary number is not an activity");
        assert!(
            err.to_string().contains("no activity for token"),
            "token {token:#x}: {err}"
        );
    }
    let locked = state.lock().unwrap();
    assert!(
        locked.pending_reverse_calls.is_empty(),
        "a rejected press never queues a callback"
    );
    assert!(
        locked.activities.iter().all(|a| !a.back_pressed),
        "no record was marked"
    );
}

#[test]
fn back_without_a_finish_callback_is_a_bad_request() {
    let state = attached();
    let mut service = controller(&state);
    let token = GUEST_TAG | 0xb1;
    resume(&mut service, token);
    let err = call(
        &mut service,
        "onBackPressed",
        &[binder(token), Value::Binder(None)],
    )
    .expect_err("a null IRequestFinishCallback must not become a finish");
    assert!(
        err.to_string().contains("null IRequestFinishCallback"),
        "{err}"
    );
    let locked = state.lock().unwrap();
    assert!(locked.pending_reverse_calls.is_empty());
    assert!(!locked.activities.iter().any(|a| a.back_pressed));
}

#[test]
fn a_second_activity_below_stays_untouched_by_the_press() {
    // `back` on the top Activity of a two-Activity task: the callback finishes
    // the *top* one; the record below it is not modified here (the guest's
    // destroy+resume transactions return to it).
    let state = attached();
    let mut service = controller(&state);
    let main = GUEST_TAG | 0xc1;
    let second = GUEST_TAG | 0xc2;
    resume(&mut service, main);
    service.call(
        "android.app.IActivityClientController",
        &method("activityResumed"),
        &[binder(second), Value::Bool(true)],
    )
    .expect("second adopts");

    call(
        &mut service,
        "onBackPressed",
        &[binder(second), binder(GUEST_TAG | 0xc3)],
    )
    .expect("back on the top activity");
    let locked = state.lock().unwrap();
    let top = locked.activities.iter().find(|a| a.token == second).expect("top");
    assert!(top.back_pressed && !top.finished);
    let below = locked.activities.iter().find(|a| a.token == main).expect("below");
    assert!(!below.back_pressed, "the press is not broadcast to the task");
}
