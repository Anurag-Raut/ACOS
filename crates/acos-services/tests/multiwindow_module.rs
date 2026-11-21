//! Unit and integration tests for the `multiwindow` module and the
//! `ServiceHost` guest-command outbox (ADR-0029, 2026-11-18 amendment).

use std::sync::{Arc, Mutex};

use acos_services::modules::multiwindow::{MultiwindowModule, MODULE_ID};
use acos_services::wms::WmsState;
use acos_services::{
    GuestCommand, ModuleConfig, ModuleRegistry, Profile, ServiceError, ServiceHost,
};
use acos_wm::{
    RectI, DEFAULT_TASK_ID, WINDOWING_MODE_FULLSCREEN, WINDOWING_MODE_MULTI_WINDOW,
};
use serde_json::json;

fn host_with_live_wms() -> (ServiceHost, Arc<Mutex<WmsState>>, ModuleRegistry) {
    let mut host = ServiceHost::new();
    let wms = Arc::new(Mutex::new(WmsState::new()));
    host.set_wms(Arc::clone(&wms));
    let registry = ModuleRegistry::standard();
    (host, wms, registry)
}

#[test]
fn off_profile_refuses_controls_and_unsupported_profiles_are_rejected() {
    let (mut host, _wms, mut registry) = host_with_live_wms();

    // Default (detached / off): control fails closed with ModuleRejected naming attach call.
    let err = registry
        .control(&mut host, MODULE_ID, "enter_split", &json!({}))
        .expect_err("off multiwindow must refuse enter_split");
    match err {
        ServiceError::ModuleRejected { module, detail } => {
            assert_eq!(module, MODULE_ID);
            assert!(detail.contains("module is off"), "{detail}");
            assert!(detail.contains("multiwindow"), "{detail}");
        }
        other => panic!("expected ModuleRejected for off module, got {other:?}"),
    }

    // Stub and scripted profiles are deliberately absent (no fake split).
    for profile in [Profile::Stub, Profile::Scripted] {
        let err = registry
            .attach(&mut host, MODULE_ID, profile, &ModuleConfig::default())
            .expect_err("multiwindow must reject stub/scripted profiles");
        assert!(matches!(err, ServiceError::ModuleRejected { .. }), "{err:?}");
    }
}

#[test]
fn real_profile_without_live_wms_refuses_mutations_with_unsupported_api() {
    let mut host = ServiceHost::new();
    let mut registry = ModuleRegistry::standard();
    registry
        .attach(&mut host, MODULE_ID, Profile::Real, &ModuleConfig::default())
        .expect("real attach on bare host succeeds");

    let status = registry
        .control(&mut host, MODULE_ID, "status", &json!({}))
        .expect("status succeeds without live WMS");
    assert_eq!(status["live_wms"], false);
    assert_eq!(status["active"], false);

    for op in ["enter_split", "exit"] {
        let err = registry
            .control(&mut host, MODULE_ID, op, &json!({}))
            .expect_err("mutation without live WMS must fail closed");
        match err {
            ServiceError::UnsupportedAndroidApi { method, .. } => {
                assert!(
                    method.contains("ART engine"),
                    "error must name ART engine, got: {method}"
                );
            }
            other => panic!("expected UnsupportedAndroidApi, got {other:?}"),
        }
    }
    assert!(host.drain_guest_commands().is_empty());
}

#[test]
fn enter_split_resize_exit_mutate_wms_and_queue_guest_commands() {
    let (mut host, wms, mut registry) = host_with_live_wms();
    registry
        .attach(&mut host, MODULE_ID, Profile::Real, &ModuleConfig::default())
        .expect("attach real");

    // 1. enter_split top 0.5
    let enter = registry
        .control(
            &mut host,
            MODULE_ID,
            "enter_split",
            &json!({"position": "top", "ratio": 0.5}),
        )
        .expect("enter_split");
    assert_eq!(enter["task_id"], DEFAULT_TASK_ID);
    assert_eq!(enter["position"], "top");
    assert_eq!(enter["windowing_mode"], WINDOWING_MODE_MULTI_WINDOW);
    assert_eq!(enter["in_multi_window_mode"], true);
    assert_eq!(enter["bounds"], json!([0, 0, 1080, 1200]));
    assert_eq!(enter["app_bounds"], json!([0, 128, 1080, 1200]));
    assert_eq!(enter["width_dp"], 411);
    assert_eq!(enter["height_dp"], 408);

    // WMS holds the task bounds.
    let task_record = wms
        .lock()
        .unwrap()
        .task_bounds(DEFAULT_TASK_ID)
        .expect("WMS task bounds set");
    assert_eq!(task_record.windowing_mode, WINDOWING_MODE_MULTI_WINDOW);
    assert_eq!(task_record.bounds, RectI::new(0, 0, 1080, 1200));
    assert_eq!(task_record.app_bounds, RectI::new(0, 128, 1080, 1200));

    // Outbox has the typed GuestCommand::Window, drained exactly once.
    let cmds = host.drain_guest_commands();
    assert_eq!(
        cmds,
        vec![GuestCommand::Window {
            task: DEFAULT_TASK_ID,
            process: 0,
            rect: RectI::new(0, 0, 1080, 1200),
            app_bounds: RectI::new(0, 128, 1080, 1200),
            windowing_mode: WINDOWING_MODE_MULTI_WINDOW,
        }]
    );
    assert!(host.drain_guest_commands().is_empty());

    // 2. resize 0.35
    let resized = registry
        .control(&mut host, MODULE_ID, "resize", &json!({"ratio": 0.35}))
        .expect("resize");
    assert_eq!(resized["bounds"], json!([0, 0, 1080, 840]));
    assert_eq!(resized["app_bounds"], json!([0, 128, 1080, 840]));
    assert_eq!(resized["width_dp"], 411);
    assert_eq!(resized["height_dp"], 271);

    let cmds = host.drain_guest_commands();
    assert_eq!(
        cmds,
        vec![GuestCommand::Window {
            task: DEFAULT_TASK_ID,
            process: 0,
            rect: RectI::new(0, 0, 1080, 840),
            app_bounds: RectI::new(0, 128, 1080, 840),
            windowing_mode: WINDOWING_MODE_MULTI_WINDOW,
        }]
    );

    // 3. exit restores fullscreen (matching reference_merged_configuration under InsetsMode::Report)
    let exited = registry
        .control(&mut host, MODULE_ID, "exit", &json!({}))
        .expect("exit");
    assert_eq!(exited["windowing_mode"], WINDOWING_MODE_FULLSCREEN);
    assert_eq!(exited["in_multi_window_mode"], false);
    assert_eq!(exited["bounds"], json!([0, 0, 1080, 2400]));
    assert_eq!(exited["app_bounds"], json!([0, 0, 1080, 2400]));
    assert_eq!(exited["width_dp"], 411);
    assert_eq!(exited["height_dp"], 914);
    assert!(wms.lock().unwrap().task_bounds(DEFAULT_TASK_ID).is_none());

    let cmds = host.drain_guest_commands();
    assert_eq!(
        cmds,
        vec![GuestCommand::Window {
            task: DEFAULT_TASK_ID,
            process: 0,
            rect: RectI::new(0, 0, 1080, 2400),
            app_bounds: RectI::new(0, 0, 1080, 2400),
            windowing_mode: WINDOWING_MODE_FULLSCREEN,
        }]
    );
}

#[test]
fn validation_slice2_refusal_and_detach_cleanup() {
    let (mut host, wms, mut registry) = host_with_live_wms();
    registry
        .attach(&mut host, MODULE_ID, Profile::Real, &ModuleConfig::default())
        .expect("attach real");

    // Two-activity `bottom` arg is explicitly refused in slice 1.
    let err = registry
        .control(
            &mut host,
            MODULE_ID,
            "enter_split",
            &json!({"position": "top", "bottom": "dev.acos.second/.Main"}),
        )
        .expect_err("bottom arg must be refused in slice 1");
    match err {
        ServiceError::UnsupportedAndroidApi { method, .. } => {
            assert!(method.contains("slice 2"), "got: {method}");
        }
        other => panic!("expected UnsupportedAndroidApi, got {other:?}"),
    }

    // Resize before enter_split fails with IllegalState.
    let err = registry
        .control(&mut host, MODULE_ID, "resize", &json!({"ratio": 0.4}))
        .expect_err("resize before enter_split must fail");
    assert!(matches!(err, ServiceError::IllegalState { .. }), "{err:?}");

    // Invalid ratios and positions fail with BadRequest.
    for bad in [
        json!({"ratio": 0.1}),
        json!({"ratio": 0.9}),
        json!({"position": "center"}),
    ] {
        let err = registry
            .control(&mut host, MODULE_ID, "enter_split", &bad)
            .expect_err("invalid args must fail");
        assert!(matches!(err, ServiceError::BadRequest { .. }), "{err:?}");
    }

    // Detach while active clears WMS task bounds and queues fullscreen restore.
    registry
        .control(&mut host, MODULE_ID, "enter_split", &json!({"position": "left", "ratio": 0.5}))
        .expect("enter_split left");
    let _ = host.drain_guest_commands();
    assert!(wms.lock().unwrap().task_bounds(DEFAULT_TASK_ID).is_some());

    registry.detach(&mut host, MODULE_ID).expect("detach");
    assert!(wms.lock().unwrap().task_bounds(DEFAULT_TASK_ID).is_none());
    let restore = host.drain_guest_commands();
    assert_eq!(restore.len(), 1);
    match restore[0] {
        GuestCommand::Window { windowing_mode, .. } => {
            assert_eq!(windowing_mode, WINDOWING_MODE_FULLSCREEN);
        }
    }

    // Manifest sanity: real-only, hot, in-process.
    let m = MultiwindowModule::manifest_ref();
    assert_eq!(m.id, MODULE_ID);
    assert_eq!(m.profiles, &[Profile::Real]);
}
