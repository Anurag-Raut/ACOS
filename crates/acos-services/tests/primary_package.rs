//! SYSUI3A: the primary process's package is the launcher's fact.
//!
//! `attachApplication` carries no package (`IActivityManager.attachApplication
//! (IApplicationThread, long)`), and ACOS's primary process never sends the
//! peer's handshake (it is the launcher's own guest), so the primary's
//! `ProcessRecord` used to be created **unnamed**. Everything that asks "which
//! package does tag 0 run?" reads that record — in particular
//! `Peer::package_of(0)` -> `AtmsState::pending_launch_for`, which is how a
//! `startActivity` into the already-running primary (a notification's content
//! `PendingIntent`) is turned into a host-built `ClientTransaction`. An empty
//! name made that launch undeliverable (the notification shade's row tap on ART).
//!
//! On a device the name exists before the fork (`startProcessLocked` creates the
//! `ProcessRecord` with `info.processName`); here the launcher, which is what
//! starts the primary process, seeds it.

use acos_services::atms::AtmsState;
use acos_services::pms::FIXTURE_UID;

const PACKAGE: &str = "dev.acos.fixtures.notifpermprobe";

#[test]
fn an_attach_without_a_package_keeps_the_seeded_primary_name() {
    let mut atms = AtmsState::new();
    assert!(
        atms.primary_package().is_none(),
        "nothing is invented before the launcher says so"
    );
    assert!(atms.set_primary_package(PACKAGE));
    assert_eq!(atms.primary_package(), Some(PACKAGE));

    // The real `attachApplication` path: no package argument at all.
    atms.attach_application_for(0, 0x1000_0001, 7, "");
    let record = atms.process(0).expect("attach created the primary record");
    assert_eq!(
        record.package_name, PACKAGE,
        "the primary record names the package the launcher started"
    );
    // PROCUID: `ProcessRecord.uid` starts **unknown** (`0`): `attachApplication`
    // carries no uid either, and the attach must never claim the launched
    // package's uid for a process it cannot identify (a secondary app would run
    // as another app's uid). The peer's handshake names the credential, and
    // `process_started` fills an unknown uid in place without touching the
    // name.
    assert_eq!(
        record.uid, 0,
        "the attach leaves the uid unknown rather than guessing it"
    );
    atms.process_started(0, "", FIXTURE_UID, 4242);
    let record = atms.process(0).expect("record");
    assert_eq!(record.uid, FIXTURE_UID, "the handshake names the real uid");
    assert_eq!(record.pid, 4242, "and the pid the kernel reported");
    assert_eq!(
        record.package_name, PACKAGE,
        "a uid with no package leaves the seeded name alone"
    );

    // A process the launcher did not name stays unnamed rather than invented.
    atms.attach_application_for(3, 0x3000_0001, 9, "");
    assert_eq!(atms.process(3).expect("record").package_name, "");
}

#[test]
fn the_seed_names_an_existing_record_and_never_overwrites_a_different_one() {
    let mut atms = AtmsState::new();
    // The guest attached first (record created with the fixture uid, no name).
    atms.attach_application_for(0, 0x1000_0001, 7, "");
    assert_eq!(atms.process(0).expect("record").package_name, "");

    assert!(atms.set_primary_package(PACKAGE));
    let record = atms.process(0).expect("record");
    assert_eq!(record.package_name, PACKAGE);
    assert_eq!(
        record.app_thread,
        Some(0x1000_0001),
        "naming the process must not disturb the attach"
    );

    // A different name is refused, not applied: the record is already named.
    assert!(!atms.set_primary_package("dev.acos.fixtures.somethingelse"));
    assert_eq!(atms.process(0).expect("record").package_name, PACKAGE);
    // An empty seed is not a name.
    assert!(!atms.set_primary_package(""));
    assert_eq!(atms.primary_package(), Some(PACKAGE));
}

#[test]
fn the_service_host_seam_names_the_primary_record_the_peer_asks_about() {
    // The service face is what `Peer::package_of` reads: the launcher seeds it
    // through `ServiceHost`, then the record the peer asks about carries the
    // name.
    let mut host = acos_services::ServiceHost::standard();
    assert!(
        host.seed_primary_package(PACKAGE),
        "the session's activity service takes the launcher's primary package"
    );
    // The real attach path (`attachApplication` carries no package) goes
    // through `AtmsService`; `seed_attached_process` is the same record path the
    // launcher uses after a snapshot restore.
    host.seed_attached_process(0x1000_0001, 7, "")
        .expect("attach record");
    let (app_thread, seq, package) = host.attached_process().expect("primary record");
    assert_eq!(app_thread, 0x1000_0001);
    assert_eq!(seq, 7);
    assert_eq!(
        package, PACKAGE,
        "an attach without a package keeps the name the launcher seeded"
    );
}
