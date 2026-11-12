//! W38: the guest's real activity token, adopted by the host.
//!
//! Before this wave every launch delivered a `ClientTransaction` whose
//! `mActivityToken` was null (`Parcel.marshall()` cannot carry a binder, and the
//! transaction travelled over the peer's byte pipe), so the framework reported
//! `token 0x0` and every `IActivityClientController` callback failed with
//! `illegal state in atms: no activity for token 0x0`. The guest now mints a
//! real `Binder` and delivers in-process; the host adopts the token on the first
//! callback.
//!
//! Rows:
//! * a guest-tagged token is adopted, the synthetic fixture record drops out, and
//!   `finishActivity` resolves (the Gaten `finish()` wall);
//! * a token of a launch the host recorded (`startActivity`) adopts *that* launch:
//!   the class is the target's, the placeholder token is rewritten in the task
//!   stack, and the launch leaves `pending_launches` (materialised by the guest);
//! * an arbitrary non-zero token (not guest-tagged, no recorded launch) and token
//!   `0x0` still return the structured `no activity for token` error.

use acos_services::ams::{AmsState, IntentFacts};
use acos_services::atms::AtmsState;
use acos_services::pms::FIXTURE_UID;

const PACKAGE: &str = "dev.acos.fixtures.multiactivity";
const SECOND: &str = "dev.acos.fixtures.multiactivity.SecondActivity";
/// The peer's `JAVA_TAG`: a binder owned by the guest JVM.
const GUEST_TAG: u64 = 1 << 32;

fn attached() -> AtmsState {
    let mut atms = AtmsState::new();
    atms.attach_application(0x1000, 7, PACKAGE);
    atms.finish_attach_application(7).expect("finish attach");
    atms
}

fn apk() -> std::path::PathBuf {
    acos_services::workspace_path("fixtures/build/apps/multi-activity.apk")
        .expect("multi-activity fixture APK")
}

#[test]
fn a_guest_token_is_adopted_and_finish_resolves() {
    let mut atms = attached();
    // The synthetic fixture record is the placeholder the real token replaces.
    assert!(atms.activities.iter().any(|a| a.token == 0xabc));

    let token = GUEST_TAG | 0x77;
    atms.activity_resumed(token, true).expect("adopted on resume");
    assert!(
        atms.activities.iter().any(|a| a.token == token && a.resumed),
        "the real token owns a record"
    );
    assert!(
        !atms.activities.iter().any(|a| a.token == 0xabc),
        "the fixture placeholder is gone"
    );
    atms.activity_idle(token).expect("idle resolves too");

    // The Gaten wall: `finish()` on a real activity token.
    atms.finish_activity(token).expect("finish resolves");
    assert!(atms.activities.iter().any(|a| a.token == token && a.finished));

    // A callback before the process attached cannot adopt anything.
    let mut detached = AtmsState::new();
    assert!(detached.activity_resumed(token, true).is_err());
    assert!(detached.finish_activity(token).is_err());
}

#[test]
fn a_recorded_launch_is_rebound_to_the_real_token() {
    let mut atms = attached();
    atms.ams
        .manifests
        .add_apk(apk(), FIXTURE_UID)
        .expect("seed the APK component table");

    let code = atms
        .ams
        .start_activity(&IntentFacts::explicit(PACKAGE, SECOND), 0, 0, 0)
        .expect("startActivity resolves the declared target");
    assert_eq!(code, acos_services::ams::START_SUCCESS);
    let pending = atms.ams.pending_launches.clone();
    assert_eq!(pending.len(), 1, "the request is recorded");
    let placeholder = pending[0].token;

    let real = GUEST_TAG | 0x88;
    atms.activity_resumed(real, true).expect("adopt");

    let record = atms
        .activities
        .iter()
        .find(|a| a.token == real)
        .expect("record for the real token");
    assert_eq!(record.activity, SECOND, "the class comes from the launch");
    assert!(
        !atms.activities.iter().any(|a| a.token == placeholder),
        "the host placeholder is replaced"
    );
    // `AmsState`'s own task stack agrees with the real token.
    assert!(
        atms.ams
            .tasks
            .iter()
            .any(|t| t.activities.iter().any(|tok| *tok == real)),
        "task stack holds the real token"
    );
    assert!(
        atms.ams.pending_launches.is_empty(),
        "the guest materialised the launch; nothing is left pending"
    );

    atms.finish_activity(real).expect("finish of the second activity");
    assert!(atms.ams.tasks.is_empty(), "the task emptied on finish");
}

#[test]
fn an_unknown_or_zero_token_still_errors() {
    let mut atms = attached();
    // Not guest-tagged and no recorded launch: never adopted.
    let err = atms
        .activity_resumed(0x77, true)
        .expect_err("an arbitrary number is not an activity");
    assert!(err.to_string().contains("no activity for token 0x77"), "{err}");
    assert!(atms.finish_activity(0x77).is_err());
    // Token 0 is what the old launch delivered; it stays an error so a null
    // token can never silently resolve.
    assert!(atms.finish_activity(0).is_err());
    assert!(atms.activity_idle(0).is_err());
}
