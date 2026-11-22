//! The module conformance kit, run against the shipped modules (ADR-0029 item 7).
//!
//! Every test here failed before the module framework existed: there was no
//! registry, no profile, no tombstone and no way to ask "is this name attached".
//!
//! * `core` is today's service set: attach → detach twice must return the
//!   process (threads, fds) **and** the host (binders, names) to baseline.
//! * `feedback.vibrator` is the reference module: the same kit, plus the
//!   tombstone/attach behaviour an agent sees.
//! * A gap name decided `tombstone` answers a structured
//!   `UNSUPPORTED_ANDROID_API` naming the module and the attach call; a name
//!   decided `null` keeps Android's missing path and is reported `off` with the
//!   AOSP reason.

use acos_parcel::Parcel;
use acos_services::module::{
    self, conformance, AospReaction, ModuleConfig, ModuleRegistry, Profile, TOMBSTONES,
};
use acos_services::{ServiceError, ServiceHost};

fn transact(host: &mut ServiceHost, name: &str, code: u32) -> Result<Vec<i32>, ServiceError> {
    let binder = host
        .check_service(name)
        .expect("lookup is infallible")
        .unwrap_or_else(|| panic!("{name} did not resolve to a binder"));
    let descriptor = host
        .descriptor_of(binder)
        .unwrap_or("<unknown>")
        .to_string();
    let mut data = Parcel::new();
    data.write_interface_token(&descriptor).expect("token");
    data.set_data_position(0);
    let mut reply = Parcel::new();
    host.transact(binder, code, &mut data, &mut reply, 0)?;
    reply.set_data_position(0);
    let mut out = Vec::new();
    while let Ok(value) = reply.read_int32() {
        out.push(value);
    }
    Ok(out)
}

/// Run the kit for `module` in a **quiet child process**.
///
/// The detach check compares `/proc/self/task` and `/proc/self/fd`, which are
/// process-wide: in the test binary's own process, sibling tests open files and
/// spawn threads while the kit measures, so a difference would be noise rather
/// than a module leak. The child runs exactly one test, so the counts are the
/// module's own.
fn run_in_quiet_process(module: &str) {
    let exe = std::env::current_exe().expect("test executable");
    let output = std::process::Command::new(exe)
        .args([
            "--exact",
            "quiet_process_child",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("ACOS_KIT_CHILD", module)
        // NET5: `network`'s `real` profile names its relay session after the
        // session, and a session without an id is refused at attach (a network
        // no client could address). The kit attaches at the module's default
        // profile, so the id a launcher always publishes is part of the
        // environment the kit runs in.
        .env("ACOS_ART_SESSION_ID", "kit")
        .output()
        .expect("spawn the kit child");
    assert!(
        output.status.success(),
        "the conformance kit for {module} failed in a quiet process:\n--- stdout ---\n{}\n--- stderr ---\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

/// The child half of [`run_in_quiet_process`]. Does nothing unless the parent
/// asked for it (it runs in the parent's own process too, as a no-op).
#[test]
fn quiet_process_child() {
    let Ok(module) = std::env::var("ACOS_KIT_CHILD") else {
        return;
    };
    let report = conformance::run(&module);
    for check in &report.checks {
        println!(
            "{}: {} {}",
            check.name,
            if check.pass { "PASS" } else { "FAIL" },
            check.detail
        );
    }
    assert!(
        report.pass(),
        "{} failed the kit: {}",
        report.module,
        report.failures()
    );
}

#[test]
fn core_passes_the_conformance_kit() {
    run_in_quiet_process("core");
}

#[test]
fn the_accessibility_module_passes_the_conformance_kit() {
    run_in_quiet_process("accessibility");
}

#[test]
fn the_vibrator_module_passes_the_conformance_kit() {
    run_in_quiet_process("feedback.vibrator");
}

/// MODNOTIF2: the permissions module takes a `core` name over, so its kit run
/// has one extra property the vibrator's has not: after detach the name must
/// answer **core's** answer again, not be absent (a bare host has no core, so
/// the kit sees it absent there — `modnotif2_permissions.rs` covers the session
/// case with core attached).
/// NET5: the network module. Its `off` state is the absence of the module, so
/// the kit's name checks are empty for it — the properties it owes are the
/// attach/detach cycle (the process-wide gate is set and cleared) and the honest
/// refusal of every control while detached (`tests/network_module.rs` proves the
/// framework view of both).
#[test]
fn the_network_module_passes_the_conformance_kit() {
    run_in_quiet_process("network");
}

/// WV8: the `webview` module's kit row. It provides no `ServiceManager` name
/// (`webviewupdate` and `hasSystemFeature` are `core`'s, answered from the
/// package table the provider install writes), so the names half of the kit is
/// empty; what it owes is the attach/detach cycle (nothing bound, nothing left),
/// the honest report, the refusals and the budget.
#[test]
fn the_webview_module_passes_the_conformance_kit() {
    run_in_quiet_process("webview");
}

#[test]
fn the_permissions_module_passes_the_conformance_kit() {
    run_in_quiet_process("system.permissions");
}

/// MODNOTIF4: the notifications module takes the `notification` name over from
/// `core` and its records are real state, so it owes the same kit as the
/// permissions module.
#[test]
fn the_notifications_module_passes_the_conformance_kit() {
    run_in_quiet_process("system.notifications");
}

#[test]
fn the_telephony_module_passes_the_conformance_kit() {
    run_in_quiet_process("net.telephony");
}

#[test]
fn the_bluetooth_and_biometric_modules_pass_the_conformance_kit() {
    run_in_quiet_process("hw.bluetooth");
    run_in_quiet_process("hw.biometric");
}

#[test]
fn the_location_module_passes_the_conformance_kit() {
    run_in_quiet_process("location");
}

#[test]
fn the_camera_module_passes_the_conformance_kit() {
    run_in_quiet_process("hw.camera");
}

#[test]
fn the_multiwindow_module_passes_the_conformance_kit() {
    run_in_quiet_process("multiwindow");
}

#[test]
fn a_gap_name_answers_a_structured_tombstone_naming_the_module_and_the_attach_call() {
    let mut host = ServiceHost::new();
    let mut registry = ModuleRegistry::standard();
    registry
        .attach(&mut host, "core", Profile::Real, &ModuleConfig::default())
        .expect("core attaches");
    let installed = registry.install_tombstones(&mut host);
    assert!(installed.contains(&"vibrator_manager"));

    let error = transact(&mut host, "vibrator_manager", 1).expect_err("tombstone must error");
    match &error {
        ServiceError::ModuleUnavailable {
            service,
            module,
            attach,
            ..
        } => {
            assert_eq!(service, "vibrator_manager");
            assert_eq!(*module, "feedback.vibrator");
            assert!(
                attach.contains("acos_launch") && attach.contains("feedback.vibrator"),
                "the tombstone must name the attach call, got {attach}"
            );
        }
        other => panic!("expected ModuleUnavailable, got {other:?}"),
    }
    assert_eq!(error.code(), "UNSUPPORTED_ANDROID_API");
}

#[test]
fn a_null_decided_name_keeps_android_s_missing_path_and_is_reported_off() {
    // `wallpaper`: AOSP turns a null binder into a working DisabledWallpaperManager
    // (SystemServiceRegistry.java:774-793), so the name must stay null.
    let mut host = ServiceHost::new();
    let mut registry = ModuleRegistry::standard();
    registry
        .attach(&mut host, "core", Profile::Real, &ModuleConfig::default())
        .expect("core attaches");
    registry.install_tombstones(&mut host);
    assert!(host
        .check_service("wallpaper")
        .expect("lookup is infallible")
        .is_none());

    let status = registry.status_json();
    let gaps = status["gaps"].as_array().expect("gaps array");
    let wallpaper = gaps
        .iter()
        .find(|gap| gap["service"] == "wallpaper")
        .expect("wallpaper is in the gap list");
    assert_eq!(wallpaper["state"], "off");
    assert_eq!(wallpaper["aosp"], "null");
    assert!(
        wallpaper["evidence"]
            .as_str()
            .unwrap_or_default()
            .contains("SystemServiceRegistry.java"),
        "the off entry must carry its AOSP evidence"
    );
}

#[test]
fn every_gap_name_is_reported_with_its_decision() {
    let registry = ModuleRegistry::standard();
    let status = registry.status_json();
    let gaps = status["gaps"].as_array().expect("gaps array");
    assert_eq!(gaps.len(), TOMBSTONES.len());
    for tombstone in TOMBSTONES {
        let gap = gaps
            .iter()
            .find(|gap| gap["service"] == tombstone.name)
            .unwrap_or_else(|| panic!("{} missing from capabilities", tombstone.name));
        assert_eq!(
            gap["state"],
            match tombstone.reaction {
                AospReaction::Tombstone => "tombstone",
                AospReaction::Null => "off",
            },
            "{} has the wrong reported state",
            tombstone.name
        );
        assert!(!gap["reason"].as_str().unwrap_or_default().is_empty());
    }
}

#[test]
fn attaching_the_vibrator_serves_the_name_and_the_report_says_which_profile() {
    let mut host = ServiceHost::new();
    let mut registry = ModuleRegistry::standard();
    registry
        .attach(&mut host, "core", Profile::Real, &ModuleConfig::default())
        .expect("core attaches");
    registry.install_tombstones(&mut host);

    // Attaching over a tombstone replaces it (the name is the module's own).
    registry
        .attach(
            &mut host,
            "feedback.vibrator",
            Profile::Scripted,
            &ModuleConfig::default(),
        )
        .expect("vibrator attaches");

    let reply = transact(&mut host, "vibrator_manager", 1).expect("getVibratorIds answers");
    // The first word is the reply's exception header (`status_t`, 0 = none).
    assert_eq!(&reply[1..], [1, 0], "one vibrator with id 0");

    // A void method is recorded, not faked into a hardware effect. The request
    // has the image's `vibrate(int uid, int deviceId, String opPkg,
    // CombinedVibration, VibrationAttributes, String reason, IBinder token)`
    // shape, because the scripted profile records the pattern from it.
    let binder = host.check_service("vibrator_manager").unwrap().unwrap();
    let mut data = Parcel::new();
    data.write_interface_token("android.os.IVibratorManagerService")
        .expect("token");
    data.write_int32(10123); // uid
    data.write_int32(0); // deviceId
    data.write_string16("dev.acos.fixtures.tailprobe").unwrap();
    data.write_int32(1); // the CombinedVibration typed object is present
    data.write_int32(1); // PARCEL_TOKEN_MONO
    data.write_int64(250); // a OneShot-shaped payload
    data.write_int32(128);
    data.set_data_position(0);
    let mut reply = Parcel::new();
    host.transact(binder, 7, &mut data, &mut reply, 0)
        .expect("vibrate answers");
    let calls = registry
        .control(
            &mut host,
            "feedback.vibrator",
            "calls",
            &serde_json::Value::Null,
        )
        .expect("calls control op");
    assert_eq!(calls["vibrate_calls"], 1);

    let report = registry
        .reports()
        .into_iter()
        .find(|report| report.id == "feedback.vibrator")
        .expect("report");
    assert!(report.attached);
    assert_eq!(report.profile, Some(Profile::Scripted));
    let reason = report.reason.unwrap_or_default();
    assert!(
        reason.contains("scripted") && reason.contains("no vibration hardware"),
        "an attached stub/scripted module must say so in its report, got {reason:?}"
    );
}

#[test]
fn a_live_attach_of_an_at_launch_module_is_refused_with_the_launch_call() {
    let mut host = ServiceHost::new();
    let mut registry = ModuleRegistry::standard();
    registry
        .attach(&mut host, "core", Profile::Real, &ModuleConfig::default())
        .expect("core attaches");
    let error = registry
        .attach_live(&mut host, "core", Profile::Real, &ModuleConfig::default())
        .expect_err("core is at-launch only");
    let text = error.to_string();
    assert!(
        text.contains("at-launch") && text.contains("acos_launch"),
        "the refusal must say the module is at-launch and how to attach it, got {text:?}"
    );
    // A hot module attaches live.
    registry
        .attach_live(
            &mut host,
            "feedback.vibrator",
            Profile::Stub,
            &ModuleConfig::default(),
        )
        .expect("vibrator is hot");
    assert_eq!(registry.profile("feedback.vibrator"), Some(Profile::Stub));
}

#[test]
fn detaching_core_removes_its_registrations() {
    let mut host = ServiceHost::standard();
    assert!(host
        .check_service("package")
        .expect("lookup is infallible")
        .is_some());
    // `ServiceHost::standard()` = install; a module registry owns the same set
    // when it attaches `core`, so build that state and detach it.
    let mut host2 = ServiceHost::new();
    let mut registry = ModuleRegistry::standard();
    registry
        .attach(&mut host2, "core", Profile::Real, &ModuleConfig::default())
        .expect("core attaches");
    assert!(host2
        .check_service("package")
        .expect("lookup is infallible")
        .is_some());
    registry.detach(&mut host2, "core").expect("detach");
    assert!(host2
        .check_service("package")
        .expect("lookup is infallible")
        .is_none());
    // Every core registration is gone, `location` included. The `location` node
    // is **core's** now (`core_module::FILLED_BY_MODULE`, ADR-0029 note MODSENS:
    // the module fills it while attached and a detach empties it in place), so it
    // is rolled back with the rest instead of surviving as a second,
    // module-owned node. Nothing survives a `core` detach.
    assert!(
        host2.service_names().is_empty(),
        "a core detach must leave no registration behind, got {:?}",
        host2.service_names()
    );
    assert!(
        host2
            .check_service("location")
            .expect("lookup is infallible")
            .is_none(),
        "the location node is core's, so detaching core removes it"
    );
}

#[test]
fn the_standard_install_attaches_core_and_tombstones_the_gaps() {
    let mut host = ServiceHost::new();
    let registry = module::install_standard(&mut host);
    assert_eq!(registry.profile("core"), Some(Profile::Real));
    // Core registrations are exactly today's: the NAMES table resolves.
    for (name, _) in acos_services::registry::NAMES
        .iter()
        .filter(|(name, _)| !acos_services::registry::AUX_BACKED_NAMES.contains(name))
    {
        assert!(
            host.check_service(name)
                .expect("lookup is infallible")
                .is_some(),
            "core must register {name}"
        );
    }
    assert!(host
        .check_service("wifi")
        .expect("lookup is infallible")
        .is_none());
    assert!(host
        .check_service("vibrator_manager")
        .expect("lookup is infallible")
        .is_some());
}

/// The kit runs over **every registered module**, not a hand-kept list.
///
/// ADR-0029 item 7 says every module passes the kit; before this test a module
/// added to `ModuleRegistry::standard()` could ship with no kit row at all
/// (the tail modules had only their own crate-level tests). The list is the
/// registry's, so a new module cannot be missed.
#[test]
fn every_registered_module_passes_the_conformance_kit() {
    let registry = ModuleRegistry::standard();
    let ids = registry.ids();
    assert!(
        ids.len() >= 12,
        "the standard registry lost modules: {ids:?}"
    );
    for id in ids {
        run_in_quiet_process(id);
    }
}

/// The kit's two sides that are a *registry* property rather than a single
/// module's: every module is a verdict row with the profile it is attached
/// with, and every `hot` module really attaches and detaches on a live host.
#[test]
fn every_registered_module_is_a_verdict_row_and_honours_its_attach_mode() {
    use acos_services::module::AttachMode;

    let mut registry = ModuleRegistry::standard();
    let rows = registry.verdict_modules();
    for manifest in registry.manifests() {
        let row = rows
            .iter()
            .find(|row| row["id"] == manifest.id)
            .unwrap_or_else(|| panic!("{} is missing from the verdict rows", manifest.id));
        assert_eq!(row["state"], "off", "{} starts off", manifest.id);
        assert_eq!(row["attach"], manifest.attach.as_str(), "{}", manifest.id);
        assert_eq!(
            row["fidelity"], "off",
            "{} starts at the off profile",
            manifest.id
        );
    }

    // A live attach is the `acos_services` path: `hot` modules succeed, an
    // `at-launch` module refuses with the launch call. Modules whose attach
    // needs a host tool (`webview`'s gate socket, `network`'s relay session)
    // prove their live path in their own ART scenarios and the kit's quiet
    // process; this test owns the in-process rule.
    let mut host = ServiceHost::new();
    registry
        .attach(&mut host, "core", Profile::Real, &ModuleConfig::default())
        .expect("core attaches");
    registry.install_tombstones(&mut host);
    for manifest in registry.manifests() {
        if manifest.id == "core" {
            continue;
        }
        let Some(profile) = manifest.profiles.iter().copied().min() else {
            // `off`-only: nothing to attach, and `attach` refuses (the kit's
            // `attach` check proves it per module).
            continue;
        };
        if !manifest.requires_host_tools.is_empty() {
            continue;
        }
        match manifest.attach {
            AttachMode::Hot => {
                registry
                    .attach_live(&mut host, manifest.id, profile, &ModuleConfig::default())
                    .unwrap_or_else(|error| {
                        panic!("{} is hot but refused a live attach: {error}", manifest.id)
                    });
                let row = registry
                    .verdict_modules()
                    .into_iter()
                    .find(|row| row["id"] == manifest.id)
                    .expect("row");
                assert_eq!(row["state"], "attached", "{}", manifest.id);
                assert_eq!(row["fidelity"], profile.as_str(), "{}", manifest.id);
                registry
                    .detach_live(&mut host, manifest.id)
                    .unwrap_or_else(|error| panic!("{} detach: {error}", manifest.id));
            }
            AttachMode::AtLaunch => {
                assert!(
                    registry
                        .attach_live(&mut host, manifest.id, profile, &ModuleConfig::default())
                        .is_err(),
                    "at-launch module {} must refuse a live attach",
                    manifest.id
                );
            }
        }
    }
}
