//! BINDERPID, host side: the caller identity a hosted service runs with is the
//! *process's* real uid and pid, not a placeholder.
//!
//! Three things have to agree for `Binder.getCallingUid()` inside a host service
//! (and for the framework's `Context.checkCallingPermission`) to answer like a
//! device:
//!
//! * the activity manager's `ProcessRecord` names the pid the kernel reported
//!   for that process's connection (`SO_PEERCRED`), not `4242`;
//! * the uid is the one the package manager allocated for that process's
//!   package, so two apps in one session have two uids;
//! * the permission answer is asked **by uid**
//!   (`IPermissionManager.checkUidPermission`, the
//!   `ActivityManager.checkComponentPermission` path), and a uid resolves to
//!   its own package's grants (`IPackageManager.getNameForUid`).

use std::path::PathBuf;

use acos_services::atms::AtmsService;
use acos_services::permissions::{PermissionStore, PERMISSION_DENIED, PERMISSION_GRANTED};
use acos_services::{ServiceHost, Value};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/build/apps")
        .join(name)
}

/// A host with `proc-a` as the launched package and `proc-b` installed as an
/// extra APK at its own uid (what a two-process session looks like).
fn two_package_host() -> ServiceHost {
    let mut host = ServiceHost::standard();
    host.set_host_side_services(true);
    host.install_package_from_apk_as(&fixture("proc-a.apk"), Some("/data/app/proc-a.apk"))
        .expect("install proc-a");
    host.install_extra_package_from_apk_as(
        &fixture("proc-b.apk"),
        Some("/data/app/proc-b.apk"),
        None,
    )
    .expect("install proc-b");
    host
}

/// One transaction to the service registered under `name`, as the framework's
/// proxy would send it.
fn service_q(host: &mut ServiceHost, name: &str, interface: &str, method: &str, args: &[Value]) -> Vec<Value> {
    let binder = host
        .check_service(name)
        .unwrap_or_else(|e| panic!("check_service({name}): {e}"))
        .unwrap_or_else(|| panic!("{name} is not served"));
    host.invoke(binder, interface, method, args)
        .unwrap_or_else(|e| panic!("{interface}::{method} failed: {e}"))
}

fn pms(host: &mut ServiceHost) -> &mut acos_services::pms::PmsService {
    host.ensure("android.content.pm.IPackageManager")
        .expect("package manager")
        .as_any_mut()
        .and_then(|any| any.downcast_mut::<acos_services::pms::PmsService>())
        .expect("PmsService")
}

/// `ProcessRecord.pid` is the pid the connection reported, and an attach does
/// not replace it (BINDERPID).
#[test]
fn a_process_record_carries_the_connections_real_pid() {
    let mut host = two_package_host();
    let object = host.ensure("android.app.IActivityTaskManager").unwrap();
    let atms = object
        .as_any_mut()
        .and_then(|any| any.downcast_mut::<AtmsService>())
        .expect("atms");
    // The peer's hello: tag 0 is proc-a, tag 1 is proc-b, each with the pid the
    // kernel reported on its own accepted socket.
    atms.note_process_started(0, "dev.acos.fixtures.proca", 10192, 4101);
    atms.note_process_started(1, "dev.acos.fixtures.procb", 10193, 4102);
    let records = {
        let state = atms.shared_state();
        let state = state.lock().unwrap();
        state
            .process_list()
            .into_iter()
            .map(|p| (p.0, p.1, p.2, p.3))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        records,
        vec![
            (0, "dev.acos.fixtures.proca".to_string(), 10192, 4101),
            (1, "dev.acos.fixtures.procb".to_string(), 10193, 4102),
        ],
        "each process record names its own uid and the connection's pid"
    );
    // An attach (the framework's `attachApplication`) keeps the identity.
    let state = atms.shared_state();
    let mut state = state.lock().unwrap();
    state.attach_application_for(1, 0x5151, 7, "dev.acos.fixtures.procb");
    let record = state.process(1).expect("proc-b's record");
    assert_eq!(
        (record.uid, record.pid),
        (10193, 4102),
        "an attach must not invent a pid or a uid for an existing process"
    );
}

/// `getNameForUid` maps a uid to its package, and an unknown uid to null
/// (BINDERPID: the framework's check path turns a calling uid into a package).
#[test]
fn the_package_manager_names_the_package_of_a_uid() {
    let mut host = two_package_host();
    let uid_b = pms(&mut host)
        .shared_state()
        .lock()
        .unwrap()
        .lookup(Some("dev.acos.fixtures.procb"))
        .expect("proc-b is installed")
        .app
        .uid;
    assert_ne!(uid_b, 10192, "the second package has its own uid");
    let answer = service_q(
        &mut host,
        "package",
        "android.content.pm.IPackageManager",
        "getNameForUid",
        &[Value::Int(uid_b)],
    );
    assert_eq!(
        answer,
        vec![Value::Str(Some("dev.acos.fixtures.procb".into()))],
        "the uid resolves to its own package"
    );
    let unknown = service_q(
        &mut host,
        "package",
        "android.content.pm.IPackageManager",
        "getNameForUid",
        &[Value::Int(99_999)],
    );
    assert_eq!(
        unknown,
        vec![Value::Str(None)],
        "a uid no package owns is null, never a guessed package"
    );
}

/// `checkUidPermission` answers per uid: an app with the permission is granted,
/// the app without it is denied, and `system_server` (uid 1000) is granted
/// everything (BINDERPID — this is the `checkCallingPermission` path).
#[test]
fn the_permission_answer_differs_per_calling_uid() {
    let mut host = two_package_host();
    // `checkCallingPermission` reaches IPermissionManager, so the answer must
    // come from the table the package manager published.
    let pms_state = pms(&mut host).shared_state();
    let (uid_a, uid_b) = {
        let state = pms_state.lock().unwrap();
        let a = state
            .lookup(Some("dev.acos.fixtures.proca"))
            .expect("proc-a")
            .app
            .uid;
        let b = state
            .lookup(Some("dev.acos.fixtures.procb"))
            .expect("proc-b")
            .app
            .uid;
        (a, b)
    };
    let perm = "android.permission.INTERNET";
    // Grant it to the session's primary package only (a scenario's
    // `system.permissions` `grant`): the runtime table describes that package,
    // so its uid is granted and the other app's uid is not.
    host.permission_store()
        .expect("the session published one permission table")
        .lock()
        .unwrap()
        .grant(perm);
    // The table describes proc-a, so exactly its uid is granted.
    assert_eq!(
        pms_state
            .lock()
            .unwrap()
            .uid_grants()
            .iter()
            .filter(|(uid, _, _)| *uid == uid_a)
            .count(),
        1,
        "the package manager published proc-a's uid"
    );
    for (uid, expected) in [(uid_a, true), (uid_b, false)] {
        let answer = service_q(
            &mut host,
            "package",
            "android.content.pm.IPackageManager",
            "checkUidPermission",
            &[Value::Str(Some(perm.into())), Value::Int(uid)],
        );
        assert_eq!(
            answer,
            vec![Value::Int(if expected {
                PERMISSION_GRANTED
            } else {
                PERMISSION_DENIED
            })],
            "uid {uid}: the answer is that package's own grant ({expected})"
        );
    }
    assert_ne!(
        service_q(
            &mut host,
            "package",
            "android.content.pm.IPackageManager",
            "checkUidPermission",
            &[Value::Str(Some(perm.into())), Value::Int(uid_a)],
        ),
        service_q(
            &mut host,
            "package",
            "android.content.pm.IPackageManager",
            "checkUidPermission",
            &[Value::Str(Some(perm.into())), Value::Int(uid_b)],
        ),
        "two apps with different uids must not get the same answer for a grant only one holds"
    );
    // `system_server` (and root) are granted everything, which is what makes a
    // host-originated transaction (`sender_euid` 1000) pass a calling-permission
    // check on a device.
    for uid in [1000u32, 0] {
        let answer = service_q(
            &mut host,
            "package",
            "android.content.pm.IPackageManager",
            "checkUidPermission",
            &[
                Value::Str(Some("android.permission.REBOOT".into())),
                Value::Int(uid as i32),
            ],
        );
        assert_eq!(
            answer,
            vec![Value::Int(PERMISSION_GRANTED)],
            "uid {uid} is granted every permission"
        );
    }
    // An unknown uid is denied, and an isolated app is denied even for a
    // permission its package declared.
    let unknown = service_q(
        &mut host,
        "package",
        "android.content.pm.IPackageManager",
        "checkUidPermission",
        &[Value::Str(Some(perm.into())), Value::Int(55_555)],
    );
    assert_eq!(unknown, vec![Value::Int(PERMISSION_DENIED)]);
    let isolated = service_q(
        &mut host,
        "package",
        "android.content.pm.IPackageManager",
        "checkUidPermission",
        &[Value::Str(Some(perm.into())), Value::Int(99_001)],
    );
    assert_eq!(isolated, vec![Value::Int(PERMISSION_DENIED)]);
}

/// The table's own answer: a runtime grant for the primary package applies to
/// its uid and never leaks to the other app's uid.
#[test]
fn a_runtime_grant_applies_to_its_uid_only() {
    let mut store = PermissionStore::for_package("dev.acos.fixtures.proca", &[]);
    store.set_uid_grants(vec![
        (10192, "dev.acos.fixtures.proca".into(), Vec::new()),
        (10193, "dev.acos.fixtures.procb".into(), Vec::new()),
    ]);
    let perm = "android.permission.CAMERA";
    assert_eq!(
        store.check_uid_permission(10192, perm),
        PERMISSION_DENIED,
        "not granted yet"
    );
    store.grant(perm);
    assert_eq!(
        store.check_uid_permission(10192, perm),
        PERMISSION_GRANTED,
        "the runtime grant belongs to the package the table describes"
    );
    assert_eq!(
        store.check_uid_permission(10193, perm),
        PERMISSION_DENIED,
        "another app's uid must not inherit it"
    );
}

/// The launcher's order: `system.permissions` attaches **before** the launched
/// APK is installed into the package manager. The runtime grant must survive
/// that install, or `checkCallingPermission` answers from the previous APK's
/// manifest (this is how the ART probe first failed: `internet=-1` for an app
/// the scenario had granted the permission to).
#[test]
fn a_session_grant_survives_the_launch_install() {
    let mut host = ServiceHost::standard();
    let mut registry = acos_services::module::ModuleRegistry::standard();
    registry
        .attach(
            &mut host,
            "system.permissions",
            acos_services::module::Profile::Real,
            &acos_services::module::ModuleConfig::from_json(
                "system.permissions",
                &serde_json::json!({"grant": ["android.permission.INTERNET"]}),
            )
            .expect("config"),
        )
        .expect("system.permissions attaches");
    // Now the launch: the package manager is re-pointed at the real APK.
    host.install_package_from_apk_as(&fixture("proc-a.apk"), Some("/data/app/proc-a.apk"))
        .expect("install proc-a");
    let uid = pms(&mut host)
        .shared_state()
        .lock()
        .unwrap()
        .lookup(Some("dev.acos.fixtures.proca"))
        .expect("proc-a")
        .app
        .uid;
    let answer = service_q(
        &mut host,
        "package",
        "android.content.pm.IPackageManager",
        "checkUidPermission",
        &[
            Value::Str(Some("android.permission.INTERNET".into())),
            Value::Int(uid),
        ],
    );
    assert_eq!(
        answer,
        vec![Value::Int(PERMISSION_GRANTED)],
        "the scenario's grant must apply after the launch install"
    );
}

/// `IActivityManager.checkPermission(permission, pid, uid)` answers for the
/// **uid** the framework passes (BINDERPID): this is the call the image's
/// `Context.checkCallingPermission`/`checkSelfPermission` path actually reaches
/// (measured on ART: `IActivityManager code 9`), so it must not answer from one
/// package regardless of the caller.
#[test]
fn the_activity_manager_answers_check_permission_per_uid() {
    let mut host = two_package_host();
    let perm = "android.permission.INTERNET";
    host.permission_store()
        .expect("one permission table")
        .lock()
        .unwrap()
        .grant(perm);
    let granted = service_q(
        &mut host,
        "activity",
        "android.app.IActivityManager",
        "checkPermission",
        &[
            Value::Str(Some(perm.into())),
            Value::Int(2),
            Value::Int(10192),
        ],
    );
    let denied = service_q(
        &mut host,
        "activity",
        "android.app.IActivityManager",
        "checkPermission",
        &[
            Value::Str(Some(perm.into())),
            Value::Int(3),
            Value::Int(55_555),
        ],
    );
    assert_eq!(granted, vec![Value::Int(PERMISSION_GRANTED)]);
    assert_eq!(
        denied,
        vec![Value::Int(PERMISSION_DENIED)],
        "a uid the table does not own must not inherit the primary package's grant"
    );
}
