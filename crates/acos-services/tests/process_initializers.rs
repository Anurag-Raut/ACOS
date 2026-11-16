//! `ModuleManifest::process_initializers` (ADR-0029 item 1).
//!
//! A mainline module on a device has its own process that installs the
//! framework's service manager before any app runs: the telephony module's
//! process calls `TelephonyFrameworkInitializer.setTelephonyServiceManager`, the
//! Bluetooth module's calls `BluetoothFrameworkInitializer.setBluetoothServiceManager`.
//! ACOS has no such process, so the manifest declares the call and the guest
//! runs it before `Application.onCreate` — but only for the modules the session
//! actually attached.
//!
//! Every test here fails on the tree before this change: the field, the
//! registry accessor and the wire form did not exist (the launcher instead keyed
//! two hard-coded environment variables off two module ids, which could not
//! report what a module needs and could not be extended by a new module).

use acos_services::module::{ModuleConfig, ModuleManifest, ModuleRegistry, Profile};
use acos_services::ServiceHost;

fn session() -> (ServiceHost, ModuleRegistry) {
    let mut host = ServiceHost::new();
    let mut registry = ModuleRegistry::standard();
    registry
        .attach(&mut host, "core", Profile::Real, &ModuleConfig::default())
        .expect("core attaches");
    (host, registry)
}

/// A session that attached nothing that needs a process initializer forwards
/// none: the framework keeps its documented missing path instead of getting a
/// manager whose module is not there.
#[test]
fn a_session_without_such_a_module_forwards_no_initializer() {
    let (_host, registry) = session();
    assert_eq!(registry.process_initializers(), Vec::new());
}

/// `net.telephony` declares the telephony initializer, and only an attached
/// session forwards it.
#[test]
fn the_telephony_module_forwards_its_framework_initializer_when_attached() {
    let (mut host, mut registry) = session();
    let manifest = registry
        .manifest("net.telephony")
        .expect("net.telephony is a standard module");
    assert_eq!(manifest.process_initializers.len(), 1);
    assert_eq!(
        manifest.process_initializers[0].class,
        "android.telephony.TelephonyFrameworkInitializer"
    );
    assert_eq!(
        manifest.process_initializers[0].method,
        "setTelephonyServiceManager"
    );
    assert_eq!(
        manifest.process_initializers[0].manager,
        "android.os.TelephonyServiceManager"
    );
    assert!(
        !manifest.process_initializers[0].evidence.is_empty(),
        "an initializer without AOSP evidence is a claim, not a decision"
    );

    registry
        .attach(
            &mut host,
            "net.telephony",
            Profile::Scripted,
            &ModuleConfig::default(),
        )
        .expect("telephony attaches");
    let forwarded = registry.process_initializers();
    assert_eq!(forwarded.len(), 1);
    assert_eq!(
        forwarded[0].wire(),
        "android.telephony.TelephonyFrameworkInitializer#setTelephonyServiceManager#android.os.TelephonyServiceManager"
    );

    // Detaching withdraws it: the guest then leaves the manager null.
    registry.detach(&mut host, "net.telephony").expect("detach");
    assert_eq!(registry.process_initializers(), Vec::new());
}

/// `hw.bluetooth` declares the Bluetooth initializer (hot), and the guest gets
/// the adapter path's manager only while it is attached.
#[test]
fn the_bluetooth_module_forwards_its_framework_initializer_when_attached() {
    let (mut host, mut registry) = session();
    registry
        .attach(
            &mut host,
            "hw.bluetooth",
            Profile::Scripted,
            &ModuleConfig::default(),
        )
        .expect("bluetooth attaches");
    let forwarded = registry.process_initializers();
    assert_eq!(forwarded.len(), 1);
    assert_eq!(
        forwarded[0].wire(),
        "android.bluetooth.BluetoothFrameworkInitializer#setBluetoothServiceManager#android.os.BluetoothServiceManager"
    );
    registry.detach(&mut host, "hw.bluetooth").expect("detach");
    assert_eq!(registry.process_initializers(), Vec::new());
}

/// Both at once: two entries, in the registry's (standard-set) order so the
/// guest's log line is the same for every run.
#[test]
fn two_attached_modules_forward_two_initializers_in_attach_order() {
    let (mut host, mut registry) = session();
    registry
        .attach(&mut host, "hw.bluetooth", Profile::Stub, &ModuleConfig::default())
        .expect("bluetooth attaches");
    registry
        .attach(
            &mut host,
            "net.telephony",
            Profile::Stub,
            &ModuleConfig::default(),
        )
        .expect("telephony attaches");
    let classes: Vec<&str> = registry
        .process_initializers()
        .iter()
        .map(|initializer| initializer.class)
        .collect();
    assert_eq!(
        classes,
        vec![
            "android.telephony.TelephonyFrameworkInitializer",
            "android.bluetooth.BluetoothFrameworkInitializer",
        ]
    );
}

/// A module that needs no initializer declares none — the field is not a
/// catch-all.
#[test]
fn modules_that_need_no_initializer_declare_none() {
    let (_host, registry) = session();
    for id in [
        "core",
        "feedback.vibrator",
        "hw.biometric",
        "sensors",
        "hw.usb",
    ] {
        let manifest = registry
            .manifest(id)
            .unwrap_or_else(|| panic!("{id} is a standard module"));
        assert!(
            manifest.process_initializers.is_empty(),
            "{id} must not claim a process initializer it does not need"
        );
    }
}

/// The capability JSON carries the list, so an agent can see why a module needs
/// the guest to install something before the app boots.
#[test]
fn the_manifest_json_lists_the_initializers_with_their_evidence() {
    let (_host, registry) = session();
    let json = registry
        .manifest("hw.bluetooth")
        .expect("bluetooth manifest")
        .to_json();
    let rows = json["process_initializers"]
        .as_array()
        .expect("process_initializers array");
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0]["manager"],
        serde_json::json!("android.os.BluetoothServiceManager")
    );
    assert!(rows[0]["evidence"]
        .as_str()
        .is_some_and(|text| text.contains("BluetoothFrameworkInitializer")));
}

/// The validation rules the conformance kit relies on: an initializer needs a
/// class, a method, a fully qualified manager and AOSP evidence.
#[test]
fn an_unusable_initializer_fails_manifest_validation() {
    use acos_services::module::{AttachMode, Budget, ProcessInitializer, SecurityClass};

    fn base(items: &'static [ProcessInitializer]) -> ModuleManifest {
        ModuleManifest {
            id: "test.initializers",
            version: "1.0.0",
            summary: "manifest validation fixture",
            services: &[acos_services::module::ServiceProvide {
                name: "test",
                descriptor: "android.os.ITest",
            }],
            replaces: &[],
            process_initializers: items,
            features: &[],
            providers: &[],
            broadcasts: &[],
            windows: &[],
            system_apps: &[],
            requires: &[],
            requires_host_tools: &[],
            profiles: &[Profile::Stub],
            budget: Budget {
                ram_mib: 1,
                startup_ms: 1,
            },
            attach: AttachMode::Hot,
            controls: &[],
            security: SecurityClass::InProcess,
        }
    }

    let good: &'static [ProcessInitializer] = &[ProcessInitializer {
        class: "android.test.FrameworkInitializer",
        method: "setManager",
        manager: "android.os.TestManager",
        evidence: "frameworks/base/test/TestFrameworkInitializer.java",
    }];
    base(good).validate().expect("a complete initializer is valid");

    let no_evidence: &'static [ProcessInitializer] = &[ProcessInitializer {
        class: "android.test.FrameworkInitializer",
        method: "setManager",
        manager: "android.os.TestManager",
        evidence: "",
    }];
    let error = base(no_evidence).validate().expect_err("no evidence");
    assert!(error.contains("no AOSP evidence"), "{error}");

    let unqualified: &'static [ProcessInitializer] = &[ProcessInitializer {
        class: "FrameworkInitializer",
        method: "setManager",
        manager: "TestManager",
        evidence: "frameworks/base/test/TestFrameworkInitializer.java",
    }];
    let error = base(unqualified).validate().expect_err("unqualified");
    assert!(error.contains("fully qualified"), "{error}");
}
