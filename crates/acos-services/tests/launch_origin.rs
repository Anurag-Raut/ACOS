//! MERGEFIX2: a launch is delivered by **one** side of the ADR-0029 seam.
//!
//! `gaten-backstack`'s first tap stopped settling after the SYSUI3A merge: the
//! launcher named the primary process (`seed_primary_package`) so
//! `Peer::package_of(0)` answered, and `Peer::take_host_transactions` then
//! delivered the **guest's own** `startActivity` as a host-built
//! `ClientTransaction` — while the guest's start-activity guard
//! (`LaunchMain.launchLocalActivity`) had already materialised the same launch
//! and adopted its own token. Two `MainActivity` instances ran, each with its
//! own `React` root ("Running \"Gaten\"" twice), and the app re-rendered on every
//! frame, so no settle could ever bind (`settle.bound: "timeout"`,
//! `frame_ms: null`, pending `queue`/`newer-store`).
//!
//! The seam is per **origin** and per **process**:
//!
//! * a [`LaunchOrigin::Guest`] launch of the primary (tag 0) is the guest's
//!   (its token is the guest's `Binder`, reported back on `activityResumed`);
//! * a [`LaunchOrigin::Host`] launch of the primary is the host's (the shade's
//!   `IIntentSender`, the permission dialog): no guest `startActivity` exists;
//! * a spawned secondary (any other tag) is `ACOSLAUNCH_HOST_SCHEDULES` and
//!   waits for the host's transaction for **every** origin (MPROC7).

use acos_services::ams::{AmsState, IntentFacts, LaunchOrigin};
use acos_services::atms::{AtmsState, PRIMARY_PROCESS_TAG};

const PACKAGE: &str = "com.gaten";
const SECONDARY: &str = "dev.acos.fixtures.secondary";
const ACTIVITY: &str = "com.gaten.app.MainActivity";

fn intent(package: &str, class: &str) -> IntentFacts {
    IntentFacts::explicit(package, class)
}

/// A state with a manifest that resolves both fixtures' activities.
fn state_with_manifests() -> AmsState {
    let mut state = AmsState::new();
    let mut db = acos_services::ams::ManifestDb::new();
    for package in [PACKAGE, SECONDARY] {
        db.packages.push(acos_services::ams::PackageRecord {
            package: package.into(),
            uid: acos_services::pms::FIXTURE_UID,
            activities: vec![acos_services::ams::DeclaredComponent {
                package: package.into(),
                class: format!("{package}.app.MainActivity"),
                exported: true,
                launch_mode: 0,
                filters: vec![],
            }],
            services: vec![],
            receivers: vec![],
            providers: vec![],
        });
    }
    state.manifests = db;
    state
}

#[test]
fn a_guest_launch_of_the_primary_is_not_the_hosts_to_deliver() {
    let mut state = state_with_manifests();
    state
        .start_activity(&intent(PACKAGE, ACTIVITY), 0, 0, 0)
        .expect("the guest's own startActivity is accepted");

    // The guest's half: the peer must find nothing to encode for tag 0.
    assert!(
        state
            .pending_launch_for_tag(PRIMARY_PROCESS_TAG, PACKAGE)
            .is_none(),
        "the primary's guest materialises its own launch (LaunchMain.launchLocalActivity)"
    );

    // …and the guest's adopt path binds the token it minted to that launch.
    let (placeholder, class) = state.adopt_launch_token(0x1000_00aa).expect("adopted");
    assert_eq!(class, ACTIVITY);
    assert_ne!(placeholder, 0x1000_00aa);
    assert!(
        state.pending_launch_for(PACKAGE).is_none(),
        "the adopted launch is no longer owed to anyone"
    );
}

#[test]
fn a_host_launch_of_the_primary_is_the_hosts_to_deliver() {
    let mut state = state_with_manifests();
    // `system_server` as the client: the notification shade's row tap fires an
    // `IIntentSender` whose `TYPE_ACTIVITY` goes through this entry point.
    state
        .start_activity_from_host(&intent(PACKAGE, ACTIVITY), 0, 0, -1)
        .expect("the host's launch is accepted");

    let launch = state
        .pending_launch_for_tag(PRIMARY_PROCESS_TAG, PACKAGE)
        .expect("the peer can encode the host's launch for tag 0");
    assert_eq!(launch.origin, LaunchOrigin::Host);
    assert_eq!(launch.class, ACTIVITY);

    // The guest never saw a `startActivity`, so it never adopts this one.
    assert!(
        state.adopt_launch_token(0x1000_00ab).is_none(),
        "a host-initiated launch is not the guest's to adopt"
    );
    assert!(state.pending_launch_for(PACKAGE).is_some());
}

#[test]
fn a_guest_launch_of_a_spawned_secondary_is_the_hosts_to_deliver() {
    let mut state = state_with_manifests();
    state
        .start_activity(&intent(SECONDARY, &format!("{SECONDARY}.app.MainActivity")), 0, 0, 0)
        .expect("accepted");

    // A secondary waits for the host's `scheduleTransaction` whatever the origin
    // (MPROC7: `ACOSLAUNCH_HOST_SCHEDULES`, `waiting-for-host-transaction`).
    assert!(
        state.pending_launch_for_tag(1, SECONDARY).is_some(),
        "a spawned secondary is host-scheduled"
    );
    // The host-only rule is scoped to tag 0 *and* to that tag's own package:
    // `Peer::package_of(0)` answers the primary's package, so another package's
    // launch is never looked up under tag 0. The primary's own launches stay the
    // guest's (previous test), the secondary's are the host's.
    assert!(
        state
            .pending_launch_for_tag(2, SECONDARY)
            .is_some(),
        "every spawned tag is host-scheduled"
    );
}

#[test]
fn the_primary_seed_does_not_make_the_host_deliver_the_guests_launch() {
    // The exact SYSUI3A interaction: the launcher names tag 0, so
    // `Peer::package_of(0)` answers and `Peer::take_host_transactions` runs for
    // the primary. It must still not encode the guest's own launch.
    let mut atms = AtmsState::new();
    assert!(atms.set_primary_package(PACKAGE));
    atms.attach_application_for(0, 0x1000_0001, 7, "");
    assert_eq!(
        atms.process(0).map(|record| record.package_name.clone()),
        Some(PACKAGE.to_string())
    );
    atms.ams.manifests = state_with_manifests().manifests;
    atms.ams
        .start_activity(&intent(PACKAGE, ACTIVITY), 0, 0, 0)
        .expect("accepted");
    assert!(
        atms.ams
            .pending_launch_for_tag(PRIMARY_PROCESS_TAG, PACKAGE)
            .is_none(),
        "naming the primary must not hand the guest's launch to the host"
    );
    // The host's own start (the shade) is still deliverable for the same tag.
    atms.ams
        .start_activity_from_host(&intent(PACKAGE, ACTIVITY), 0, 0, -1)
        .expect("accepted");
    assert!(
        atms.ams
            .pending_launch_for_tag(PRIMARY_PROCESS_TAG, PACKAGE)
            .is_some(),
        "a host-initiated launch into the running primary is the shade's path"
    );
}
