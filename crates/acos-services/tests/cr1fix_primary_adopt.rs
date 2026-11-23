//! CR1FIX #22: the synthetic `FIXTURE_ACTIVITY_TOKEN` record must not count as
//! a real primary-process activity when the guest's first real token is adopted.
//! Otherwise `register_adopted_activity` is skipped and the running app is
//! absent from the AMS task list (Launcher then starts a second instance).

use acos_services::atms::{AtmsState, FIXTURE_ACTIVITY_TOKEN};

const PACKAGE: &str = "dev.acos.fixtures.multiactivity";
const MAIN: &str = "dev.acos.fixtures.multiactivity.MainActivity";
const GUEST_TAG: u64 = 1 << 32;

#[test]
fn first_guest_token_is_registered_in_ams_despite_fixture_record() {
    let mut atms = AtmsState::new();
    atms.set_primary_activity(PACKAGE, MAIN);
    atms.attach_application(0x1000, 7, PACKAGE);
    atms.finish_attach_application(7).expect("finish attach");
    assert!(atms
        .activities
        .iter()
        .any(|a| a.token == FIXTURE_ACTIVITY_TOKEN));

    let token = GUEST_TAG | 0x77;
    atms.activity_resumed(token, true).expect("adopt");
    assert!(
        atms.ams
            .activities
            .iter()
            .any(|a| a.token == token && a.class == MAIN),
        "the adopted primary activity is an AMS record: {:?}",
        atms.ams.activities
    );
    assert!(atms.ams.tasks.iter().any(|t| t.activities.contains(&token)));
    assert!(!atms
        .activities
        .iter()
        .any(|a| a.token == FIXTURE_ACTIVITY_TOKEN));
}
