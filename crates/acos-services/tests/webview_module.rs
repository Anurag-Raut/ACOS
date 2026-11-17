//! WV8: the `webview` module (ADR-0029) — manifest, attach/detach, the provider
//! package it installs, and the single control path.
//!
//! Every assertion here is a behaviour that did not exist before this change:
//!
//! * before: a WebView needed the provider APK passed as a scenario
//!   `extra_apks:` line **and** `ACOS_WEBVIEW_GATE` to make the launcher bind
//!   its socket — two pieces of knowledge outside the module system, which no
//!   reply described and no detach removed.
//! * before: `acos_services`/`acos_capabilities` could not say whether this
//!   session has a WebView at all, and a verdict carried no `modules[webview]`
//!   row.

use acos_services::module::{
    system_apks, ModuleConfig, ModuleRegistry, ModuleSpec, Profile, SecurityClass,
};
use acos_services::modules::webview::{self, WebviewModule};
use acos_services::ServiceHost;

fn spec(profile: Profile) -> ModuleSpec {
    ModuleSpec::new(webview::MODULE_ID, profile)
}

/// The provider APK the launcher stages comes from the module, not from a
/// hand-written `extra_apks:` line: `system_apks` reads the manifest's
/// `system_apps` and resolves the host file.
#[test]
fn the_real_profile_contributes_the_provider_apk_to_the_launch() {
    let manifest = WebviewModule::manifest_ref();
    assert_eq!(manifest.system_apps, &[webview::PROVIDER_PACKAGE]);
    assert_eq!(manifest.attach.as_str(), "at-launch");
    assert_eq!(manifest.security, SecurityClass::HostTool);
    assert!(
        manifest.replaces.is_empty(),
        "`webviewupdate` is core's and answers from the package table: a module that \
         re-registered it would be a second source of truth"
    );

    let apks = system_apks(&[spec(Profile::Real)]);
    assert_eq!(apks.len(), 1, "one provider package, got {apks:?}");
    assert_eq!(
        apks[0].file_name().and_then(|n| n.to_str()),
        Some("webview-provider.apk"),
        "the provider APK is the one tools/build-apps.sh produces"
    );

    // `off` stages nothing: Android's own no-provider path.
    assert!(system_apks(&[spec(Profile::Off)]).is_empty());
    // A different module contributes nothing here.
    assert!(system_apks(&[ModuleSpec::new("network", Profile::Real)]).is_empty());
}

/// The gate/socket decision is a pure function of the specs the launcher was
/// given, so the staging step and the attach step cannot disagree.
#[test]
fn only_the_real_profile_asks_for_the_session_gate() {
    assert!(webview::broker_requested(&[spec(Profile::Real)]));
    assert!(!webview::broker_requested(&[spec(Profile::Off)]));
    assert!(!webview::broker_requested(&[spec(Profile::Stub)]));
    assert!(!webview::broker_requested(&[]));
    // Another module's `real` profile is not this module's gate.
    assert!(!webview::broker_requested(&[ModuleSpec::new(
        "network",
        Profile::Real
    )]));
}

#[test]
fn attach_resolves_the_provider_apk_and_refuses_a_profile_it_has_not_got() {
    let mut registry = ModuleRegistry::standard();
    let mut host = ServiceHost::new();

    // `stub`/`scripted` would be a scripted page — a fake success, not offered.
    // The registry refuses them from the manifest (before the module runs)…
    for profile in [Profile::Stub, Profile::Scripted] {
        let error = registry
            .attach(
                &mut host,
                webview::MODULE_ID,
                profile,
                &ModuleConfig::default(),
            )
            .expect_err("only off and real exist");
        assert_eq!(error.code(), "SERVICE_MODULE_REJECTED");
        assert!(error.to_string().contains("not implemented"), "got {error}");
    }
    // …and the module itself says what a `stub`/`scripted` WebView would be.
    let mut module = WebviewModule::new();
    let error = acos_services::module::Module::attach(
        &mut module,
        &mut host,
        Profile::Scripted,
        &ModuleConfig::default(),
    )
    .expect_err("no scripted profile");
    assert!(
        error.to_string().contains("scripted WebView"),
        "the module's own refusal says why, got {error}"
    );

    registry
        .attach(
            &mut host,
            webview::MODULE_ID,
            Profile::Real,
            &ModuleConfig::default(),
        )
        .expect("real attaches");
    let report = registry
        .reports()
        .into_iter()
        .find(|report| report.id == webview::MODULE_ID)
        .expect("report");
    assert!(report.attached);
    assert_eq!(report.profile, Some(Profile::Real));
    let reason = report.reason.expect("an attached module says what it does");
    assert!(reason.contains("com.android.webview"), "got {reason:?}");
    let details = report.details.expect("provider details");
    assert_eq!(details["provider_package"], "com.android.webview");
    assert_eq!(details["frame_resolution"], "engine CSS pixels");

    registry
        .detach(&mut host, webview::MODULE_ID)
        .expect("detach");
    let report = registry
        .reports()
        .into_iter()
        .find(|report| report.id == webview::MODULE_ID)
        .expect("report");
    assert!(!report.attached);
    assert_eq!(report.profile, None);
    assert!(report.reason.is_some(), "a detached module still says why");
}

/// The module is `at-launch`: a live attach answers the launch call instead of
/// pretending it could install a provider package into a running session.
#[test]
fn a_live_attach_is_refused_with_the_launch_call() {
    let mut registry = ModuleRegistry::standard();
    let mut host = ServiceHost::new();
    let error = registry
        .attach_live(
            &mut host,
            webview::MODULE_ID,
            Profile::Real,
            &ModuleConfig::default(),
        )
        .expect_err("the provider package must exist before the guest boots");
    let text = error.to_string();
    assert!(
        text.contains("at-launch") && text.contains("acos_launch"),
        "the refusal must say the module is at-launch and how to attach it, got {text:?}"
    );
}

/// One control path: `acos_service_ctl {module: "webview", op: ...}` (MCP and
/// the CLI both reach this arm) answers the module's own state, and every op the
/// manifest advertises is dispatchable.
#[test]
fn every_advertised_control_op_is_dispatched_and_unknown_ops_are_refused() {
    let mut registry = ModuleRegistry::standard();
    let mut host = ServiceHost::new();
    let manifest = WebviewModule::manifest_ref();
    assert!(
        !manifest.controls.is_empty(),
        "an attached module with state to report advertises its ops"
    );
    registry
        .attach(
            &mut host,
            webview::MODULE_ID,
            Profile::Real,
            &ModuleConfig::default(),
        )
        .expect("attach");

    for op in manifest.controls {
        let reply = registry
            .control(
                &mut host,
                webview::MODULE_ID,
                op.name,
                &serde_json::Value::Null,
            )
            .unwrap_or_else(|error| panic!("control {} refused: {error}", op.name));
        assert_eq!(reply["module"], webview::MODULE_ID);
        assert_eq!(reply["profile"], "real");
        assert_eq!(reply["provider"]["package"], "com.android.webview");
        assert!(
            reply["provider"]["apk"].is_string(),
            "the status names the APK this session installs: {reply}"
        );
        assert_eq!(
            reply["frames"]["resolution"],
            "engine CSS pixels (device pixels / density)"
        );
        assert_eq!(reply["budget"]["fixed_host_ram_mib"], 140);
    }

    let error = registry
        .control(
            &mut host,
            webview::MODULE_ID,
            "open",
            &serde_json::Value::Null,
        )
        .expect_err("unknown op");
    assert_eq!(error.code(), "SERVICE_MODULE_REJECTED");
    assert!(
        error.to_string().contains("status"),
        "the refusal lists the ops"
    );

    // Detached: every op refuses instead of answering session state.
    registry
        .detach(&mut host, webview::MODULE_ID)
        .expect("detach");
    let error = registry
        .control(
            &mut host,
            webview::MODULE_ID,
            "status",
            &serde_json::Value::Null,
        )
        .expect_err("a detached module has no state");
    assert_eq!(error.code(), "SERVICE_MODULE_REJECTED");
}

/// `status` answers the endpoints the launcher actually bound (published on the
/// host), never a path it recomputed: the two cannot name different sockets.
#[test]
fn the_status_op_reports_the_host_published_gate_and_broker() {
    let mut registry = ModuleRegistry::standard();
    let mut host = ServiceHost::new();
    host.set_webview_gate(acos_services::host::WebviewGate {
        socket: Some("/run/user/1000/acos-wv/sess/webview.sock".into()),
        broker_socket: Some("/run/user/1000/acos-wv/broker.sock".into()),
    });
    registry
        .attach(
            &mut host,
            webview::MODULE_ID,
            Profile::Real,
            &ModuleConfig::default(),
        )
        .expect("attach");
    let reply = registry
        .control(
            &mut host,
            webview::MODULE_ID,
            "status",
            &serde_json::Value::Null,
        )
        .expect("status");
    assert_eq!(
        reply["gate"]["socket"],
        "/run/user/1000/acos-wv/sess/webview.sock"
    );
    assert_eq!(
        reply["gate"]["broker_socket"],
        "/run/user/1000/acos-wv/broker.sock"
    );
}
