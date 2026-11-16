//! `ModuleManifest::process_initializers` (ADR-0029 item 1).
//!
//! AOSP installs the mainline `ServiceManager` holders in **every** app process
//! from one place: `ActivityThread.main` -> `ActivityThread.initializeMainlineModules()`
//! (`base/core/java/android/app/ActivityThread.java:8601-8610`, image authority
//! `framework/classes.dex` `ActivityThread.main` insn 001c). The guest launcher
//! runs that same framework call once per process (GATENBOOT), so telephony,
//! Bluetooth, media, stats, NFC and DeviceConfig are never a module's job: their
//! setters throw on a second call, so a manifest that declared one would
//! double-register.
//!
//! `process_initializers` therefore stays only for a manager AOSP's call does
//! not install. No module needs one today, so every attached session forwards an
//! empty list; the mechanism, its wire form and its validation rules are kept and
//! tested here because the alternative (a per-module branch in the launcher) is
//! what ADR-0029 item 1 forbids.
//!
//! Before GATENBOOT, telephony and Bluetooth each declared an initializer and the
//! guest ran it — a second registration of a manager the framework's own
//! bootstrap is supposed to install (and, in ACOS, did not, which is why
//! `MediaServiceManager` was null in Gaten's accessibility service).

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

/// No standard module declares a process initializer, because AOSP's
/// per-process bootstrap installs every manager the standard set reads.
#[test]
fn no_standard_module_declares_a_process_initializer() {
    let (_host, registry) = session();
    let declaring: Vec<&str> = registry
        .manifests()
        .into_iter()
        .filter(|manifest| !manifest.process_initializers.is_empty())
        .map(|manifest| manifest.id)
        .collect();
    assert_eq!(
        declaring,
        Vec::<&str>::new(),
        "a module may only declare an initializer AOSP's \
         ActivityThread.initializeMainlineModules() does not install"
    );
}

/// The telephony and Bluetooth managers are the framework's own: the modules
/// that own the services must not re-install them.
#[test]
fn telephony_and_bluetooth_do_not_reinstall_the_frameworks_managers() {
    let (mut host, mut registry) = session();
    for (id, profile) in [
        ("net.telephony", Profile::Scripted),
        ("hw.bluetooth", Profile::Scripted),
    ] {
        let manifest = registry
            .manifest(id)
            .unwrap_or_else(|| panic!("{id} is a standard module"));
        assert!(
            manifest.process_initializers.is_empty(),
            "{id} must not declare an initializer AOSP already runs"
        );
        registry
            .attach(&mut host, id, profile, &ModuleConfig::default())
            .unwrap_or_else(|error| panic!("{id} attaches: {error}"));
    }
    assert_eq!(
        registry.process_initializers(),
        Vec::new(),
        "an attached session forwards no initializer when AOSP's bootstrap covers them"
    );
}

/// Detach withdraws nothing because nothing was declared: the guest's list is
/// empty for the whole attach/detach cycle.
#[test]
fn attach_and_detach_forward_an_empty_list() {
    let (mut host, mut registry) = session();
    registry
        .attach(
            &mut host,
            "hw.bluetooth",
            Profile::Scripted,
            &ModuleConfig::default(),
        )
        .expect("bluetooth attaches");
    assert_eq!(registry.process_initializers(), Vec::new());
    registry.detach(&mut host, "hw.bluetooth").expect("detach");
    assert_eq!(registry.process_initializers(), Vec::new());
}

/// A session that attached nothing that needs a process initializer forwards
/// none: the framework keeps its documented missing path instead of getting a
/// manager whose module is not there.
#[test]
fn a_session_without_such_a_module_forwards_no_initializer() {
    let (_host, registry) = session();
    assert_eq!(registry.process_initializers(), Vec::new());
}

/// The manifest JSON still carries the (empty) list, so an agent reading the
/// capability tables sees the promise and its evidence shape.
#[test]
fn the_manifest_json_carries_the_initializer_list() {
    let (_host, registry) = session();
    let json = registry
        .manifest("hw.bluetooth")
        .expect("bluetooth manifest")
        .to_json();
    let rows = json["process_initializers"]
        .as_array()
        .expect("process_initializers array");
    assert_eq!(rows.len(), 0);
}

/// The wire form the launcher forwards and the guest splits on (`#`) is unchanged
/// and is still exercised by the mechanism's own fixture.
#[test]
fn the_wire_form_is_class_hash_method_hash_manager() {
    use acos_services::module::ProcessInitializer;
    let initializer = ProcessInitializer {
        class: "android.test.FrameworkInitializer",
        method: "setManager",
        manager: "android.os.TestManager",
        evidence: "frameworks/base/test/TestFrameworkInitializer.java",
    };
    assert_eq!(
        initializer.wire(),
        "android.test.FrameworkInitializer#setManager#android.os.TestManager"
    );
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
    base(good)
        .validate()
        .expect("a complete initializer is valid");

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
