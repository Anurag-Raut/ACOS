//! Unit and contract tests for the `security.keystore` ADR-0029 service module.

use acos_services::module::{
    AttachMode, Module, ModuleConfig, ModuleRegistry, ModuleSpec, Profile, SecurityClass,
};
use acos_services::modules::keystore::{
    explicit_off, keystore_requested, requested_profile, KeystoreModule, KEYSTORE_DIR_SUFFIX,
    MODULE_ID, PROVIDER_NAME,
};
use acos_services::ServiceHost;
use serde_json::json;

#[test]
fn manifest_declares_at_launch_software_provider_honestly() {
    let m = KeystoreModule::manifest_ref();
    assert_eq!(m.id, MODULE_ID);
    assert_eq!(m.id, "security.keystore");
    assert_eq!(m.attach, AttachMode::AtLaunch);
    assert_eq!(m.profiles, &[Profile::Real]);
    assert_eq!(m.providers, &[PROVIDER_NAME]);
    assert!(m.services.is_empty());
    assert!(m.process_initializers.is_empty());
    assert_eq!(m.security, SecurityClass::InProcess);
    let summary_lower = m.summary.to_ascii_lowercase();
    assert!(
        summary_lower.contains("software")
            && summary_lower.contains("security_level_software")
            && !summary_lower.contains("hardware-backed keystore provided"),
        "manifest summary must state software-only backing honestly: {}",
        m.summary
    );
}

#[test]
fn module_spec_helpers_resolve_real_off_and_last_wins() {
    assert_eq!(requested_profile(&[]), None);
    assert!(!keystore_requested(&[]));
    assert!(!explicit_off(&[]));

    let real_only = vec![ModuleSpec {
        id: MODULE_ID.to_string(),
        profile: Profile::Real,
        config: ModuleConfig::default(),
    }];
    assert_eq!(requested_profile(&real_only), Some(Profile::Real));
    assert!(keystore_requested(&real_only));
    assert!(!explicit_off(&real_only));

    let off_only = vec![ModuleSpec {
        id: MODULE_ID.to_string(),
        profile: Profile::Off,
        config: ModuleConfig::default(),
    }];
    assert_eq!(requested_profile(&off_only), Some(Profile::Off));
    assert!(!keystore_requested(&off_only));
    assert!(explicit_off(&off_only));

    let overridden_off = vec![
        ModuleSpec {
            id: MODULE_ID.to_string(),
            profile: Profile::Real,
            config: ModuleConfig::default(),
        },
        ModuleSpec {
            id: MODULE_ID.to_string(),
            profile: Profile::Off,
            config: ModuleConfig::default(),
        },
    ];
    assert_eq!(requested_profile(&overridden_off), Some(Profile::Off));
    assert!(!keystore_requested(&overridden_off));
    assert!(explicit_off(&overridden_off));
}

#[test]
fn attach_accepts_real_refuses_unimplemented_profiles_and_detaches() {
    let mut host = ServiceHost::new();
    let mut module = KeystoreModule::new();
    assert_eq!(module.report().profile, None);
    assert!(!module.report().attached);

    for bad in [Profile::Stub, Profile::Scripted] {
        let err = module
            .attach(&mut host, bad, &ModuleConfig::default())
            .expect_err("unimplemented profile must be refused");
        assert!(
            err.to_string().contains("not implemented"),
            "unexpected refusal for {bad}: {err}"
        );
    }

    module
        .attach(&mut host, Profile::Real, &ModuleConfig::default())
        .expect("real attaches");
    let attached_report = module.report();
    assert!(attached_report.attached);
    assert_eq!(attached_report.profile, Some(Profile::Real));
    let reason = attached_report.reason.as_deref().unwrap_or_default();
    assert!(
        reason.contains("SECURITY_LEVEL_SOFTWARE")
            && reason.contains("non-hardware-backed")
            && reason.contains("no TEE/StrongBox"),
        "attached report reason must state software limits honestly: {reason:?}"
    );
    let details = attached_report.details.expect("attached details");
    assert_eq!(details["provider"], PROVIDER_NAME);
    assert_eq!(details["backing"], "software");
    assert_eq!(details["security_level"], "SECURITY_LEVEL_SOFTWARE");
    assert_eq!(details["inside_secure_hardware"], false);
    assert_eq!(details["strongbox"], false);
    assert_eq!(details["attestation"], false);
    assert_eq!(details["user_auth"], false);
    assert!(
        details["storage"]
            .as_str()
            .unwrap_or_default()
            .contains(KEYSTORE_DIR_SUFFIX),
        "details must name session storage suffix: {details}"
    );

    module.detach(&mut host).expect("detach succeeds");
    assert!(!module.report().attached);
    assert_eq!(module.report().profile, None);
}

#[test]
fn control_status_works_when_attached_and_refuses_when_off() {
    let mut host = ServiceHost::new();
    let mut module = KeystoreModule::new();

    let off_err = module
        .control(&mut host, "status", &json!({}))
        .expect_err("control while off must error");
    assert!(off_err.to_string().contains("off"));

    module
        .attach(&mut host, Profile::Real, &ModuleConfig::default())
        .expect("attach real");
    let status = module
        .control(&mut host, "status", &json!({}))
        .expect("status control op");
    assert_eq!(status["module"], MODULE_ID);
    assert_eq!(status["profile"], "real");
    assert_eq!(status["attached"], true);
    assert_eq!(status["backing"], "software");
    assert_eq!(status["security_level"], "SECURITY_LEVEL_SOFTWARE");
    assert_eq!(status["inside_secure_hardware"], false);
    assert_eq!(status["strongbox"], false);
    assert_eq!(status["attestation"], false);
    assert_eq!(status["user_auth"], false);

    let bad_op = module
        .control(&mut host, "wipe", &json!({}))
        .expect_err("unknown control op must error");
    assert!(bad_op.to_string().contains("unknown control op"));
}

#[test]
fn registry_refuses_live_attach_and_reports_at_launch() {
    let mut host = ServiceHost::new();
    let mut registry = ModuleRegistry::standard();
    registry
        .attach(&mut host, "core", Profile::Real, &ModuleConfig::default())
        .expect("core attaches");
    let err = registry
        .attach_live(&mut host, MODULE_ID, Profile::Real, &ModuleConfig::default())
        .expect_err("security.keystore is at-launch only");
    let msg = err.to_string();
    assert!(
        msg.contains("at-launch") && msg.contains("acos_launch"),
        "live attach refusal must direct caller to acos_launch: {msg:?}"
    );
}
