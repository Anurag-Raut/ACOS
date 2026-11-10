//! Round-trip serialization for every public protocol type.

use std::collections::BTreeMap;

use acos_protocol::assertion::{
    Assertion, AssertionDiff, AssertionResult, LogAssertion, NoCrashAssertion, RegionMask,
    ScreenshotAssertion, StateAssertion, TextAssertion, VisibilityAssertion,
};
use acos_protocol::commands::*;
use acos_protocol::envelope::{Backend, Envelope, Evidence, Fidelity, Metrics, Status};
use acos_protocol::error::{ErrorCode, ProtocolError, ALL_ERROR_CODES};
use acos_protocol::events::{Event, EventBody, EventKind, LifecycleState, LogLevel};
use acos_protocol::frame::FrameMeta;
use acos_protocol::identity::{DisplaySpec, IsolationLevel, ResourceBudget};
use acos_protocol::mcp::{mcp_tools, McpToolDescriptor, McpToolsList};
use acos_protocol::scenario::*;
use acos_protocol::ui::{Bounds, Selector, TextMatch, UiNode, UiTree};
use acos_protocol::verdict::*;
use acos_protocol::{
    frame_file_name, step_file_name, tree_file_name, ArtifactIssue, ArtifactValidation,
};
use serde::{de::DeserializeOwned, Serialize};

fn round_trip<T>(value: &T)
where
    T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug,
{
    let json = serde_json::to_value(value).expect("serialize");
    let back: T = serde_json::from_value(json.clone()).expect("deserialize");
    assert_eq!(&back, value, "JSON round trip failed for {json}");
}

fn frame() -> FrameMeta {
    FrameMeta::new(
        19,
        "2026-11-10T13:51:14Z",
        "req_19",
        1080,
        2400,
        420,
        "0123456789abcdef",
        true,
    )
}

fn bounds() -> Bounds {
    Bounds::new(42, 317, 231, 368)
}

fn selector() -> Selector {
    Selector {
        id: Some("dev.acos.fixtures.helloviews:id/result".to_string()),
        text: Some(TextMatch::contains("clicks")),
        content_desc: Some(TextMatch::regex(".*")),
        class: Some("android.widget.TextView".to_string()),
        bounds: Some(bounds()),
        index: Some(0),
    }
}

fn node() -> UiNode {
    UiNode::new("n1", "android.widget.TextView", bounds())
        .with_resource_id("dev.acos.fixtures.helloviews:id/result")
        .with_text("clicks: 0")
        .clickable(false)
        .with_children(vec![UiNode::new(
            "n2",
            "android.widget.Button",
            Bounds::new(42, 486, 273, 612),
        )])
}

fn tree() -> UiTree {
    let mut tree = UiTree::new(frame(), "dev.acos.fixtures.helloviews", vec![node()]);
    tree.truncated = true;
    tree.text_available = false;
    tree
}

fn base() -> RequestBase {
    RequestBase::new("req_1")
        .with_session_id("s_1")
        .with_isolation(IsolationLevel::Sandbox)
        .with_display(DisplaySpec::offscreen(1080, 2400, 420))
        .with_budget(ResourceBudget::wall_ms(5000).with_memory_mib(256))
}

fn assertion() -> Assertion {
    Assertion::Text(TextAssertion {
        selector: Selector::by_id("result"),
        text: TextMatch::exact("clicks: 0"),
        bounds: Some(bounds()),
        timeout_ms: Some(5000),
    })
}

fn inspect_result() -> InspectResult {
    InspectResult {
        apk_path: "fixtures/build/apps/hello-views.apk".to_string(),
        apk_size_bytes: 12756,
        zip_entries: 12,
        package: "dev.acos.fixtures.helloviews".to_string(),
        version_code: Some(1),
        version_code_major: Some(0),
        version_name: Some("1.0".to_string()),
        min_sdk: Some(26),
        target_sdk: Some(34),
        max_sdk: None,
        compile_sdk: Some(34),
        label: Some("Hello Views".to_string()),
        application: ApplicationSummary {
            name: Some("android.app.Application".to_string()),
            debuggable: Some(true),
            allow_backup: Some(false),
            extract_native_libs: Some(true),
            theme: Some(0x7f0f0000),
            process: Some("dev.acos.fixtures.helloviews".to_string()),
        },
        launcher_activity: Some("dev.acos.fixtures.helloviews.MainActivity".to_string()),
        permissions: vec![PermissionSummary {
            name: "android.permission.INTERNET".to_string(),
            max_sdk: None,
        }],
        uses_features: vec![FeatureSummary {
            name: "android.hardware.touchscreen".to_string(),
            required: Some(false),
            version: Some(1),
        }],
        components: ComponentsSummary {
            activities: vec![ComponentSummary {
                kind: ComponentKindSummary::Activity,
                name: "dev.acos.fixtures.helloviews.MainActivity".to_string(),
                exported: Some(true),
                launch_mode: Some("standard".to_string()),
                process: Some(".app".to_string()),
                permission: Some("android.permission.INTERNET".to_string()),
                label: Some("Hello Views".to_string()),
                intent_filters: vec![IntentFilterSummary {
                    actions: vec!["android.intent.action.MAIN".to_string()],
                    categories: vec!["android.intent.category.LAUNCHER".to_string()],
                    data: vec![IntentDataSummary {
                        scheme: Some("https".to_string()),
                        host: Some("example.test".to_string()),
                        port: Some("443".to_string()),
                        path: Some("/x".to_string()),
                        path_prefix: None,
                        path_pattern: Some(".*".to_string()),
                        mime_type: Some("text/plain".to_string()),
                    }],
                }],
            }],
            services: vec![],
            receivers: vec![],
            providers: vec![],
        },
        dex_files: vec![DexFileSummary {
            name: "classes.dex".to_string(),
            size: 8000,
        }],
        native_abis: vec!["x86_64".to_string()],
        native_libs: vec![NativeLibSummary {
            abi: "x86_64".to_string(),
            name: "lib/x86_64/libhellojni.so".to_string(),
            size: 10000,
        }],
        assets: vec![AssetSummary {
            name: "assets/data.txt".to_string(),
            size: 3,
        }],
        signing: SigningSummary {
            v1: false,
            v2: true,
            v3: false,
            v3_1: false,
            source_stamp: false,
            note: "signature schemes detected only; not cryptographically verified".to_string(),
        },
        resources: ResourceSummary {
            present: true,
            packages: 1,
            entries: 9,
            type_specs: 4,
            global_strings: 12,
            types: vec![ResourceTypeSummary {
                name: "string".to_string(),
                entries: 4,
            }],
        },
    }
}

fn resource_report() -> ResourceReport {
    ResourceReport {
        wall_ms: 120,
        cpu_ms: Some(100),
        peak_rss_bytes: Some(64 * 1024 * 1024),
        memory_mib: Some(64),
        frames_captured: 3,
        events: 12,
        sessions: 1,
        steps: 4,
    }
}

fn verdict() -> Verdict {
    let repro = Reproducer::new(
        "0.1.0",
        "fixtures/build/apps/hello-views.apk",
        "apk-sha",
        "scenarios/hello-views.yaml",
        "scenario-sha",
        IsolationLevel::Process,
        DisplaySpec::offscreen(1080, 2400, 420),
        Fidelity::NativeRuntime,
        Backend::AcosNative,
    )
    .with_flags(["zeta", "alpha"]);
    let mut verdict = Verdict::new(
        Status::Pass,
        Fidelity::NativeRuntime,
        Backend::AcosNative,
        IsolationLevel::Process,
        "req_verify",
        "2026-11-10T13:00:00Z",
        "2026-11-10T13:00:01Z",
        AppIdentity {
            path: "fixtures/build/apps/hello-views.apk".to_string(),
            package: Some("dev.acos.fixtures.helloviews".to_string()),
            sha256: "apk-sha".to_string(),
        },
        ScenarioIdentity {
            path: "scenarios/hello-views.yaml".to_string(),
            name: "hello-views".to_string(),
            sha256: "scenario-sha".to_string(),
        },
        repro,
    );
    verdict.required_fidelity = Some(Fidelity::NativeRuntime);
    verdict.session_id = Some("s_1".to_string());
    let mut step = StepResult::new(0, "launch", Status::Pass, 12);
    step.frame_seq = Some(7);
    step.assertions.push(AssertionResult {
        kind: "text".to_string(),
        passed: true,
        diff: Some(AssertionDiff {
            expected: serde_json::json!("clicks: 0"),
            actual: serde_json::json!("clicks: 0"),
            path: Some("nodes[1].text".to_string()),
            detail: Some("exact match".to_string()),
        }),
        matched_node_ids: vec!["n1".to_string()],
        message: Some("ok".to_string()),
    });
    step.evidence = vec![
        ArtifactRef::new("frame", frame_file_name(7)),
        ArtifactRef::new("tree", tree_file_name(7)),
    ];
    verdict.steps.push(step.clone());
    verdict.failure = Some(FailureClassification {
        code: ErrorCode::AssertionFailed,
        step_index: Some(1),
        summary: "text mismatch".to_string(),
        error: ProtocolError::new(ErrorCode::AssertionFailed, "expected clicks: 0"),
    });
    verdict.resources = resource_report();
    verdict.notes = vec!["ART reference values".to_string()];
    verdict
}

#[test]
fn error_taxonomy_round_trips() {
    for &code in ALL_ERROR_CODES {
        round_trip(&code);
        round_trip(&ProtocolError::new(code, "message").with_details(serde_json::json!({"x": 1})));
    }
    assert!(!ALL_ERROR_CODES.is_empty());
}

#[test]
fn envelope_and_identity_round_trip() {
    for status in [
        Status::Pass,
        Status::Fail,
        Status::Error,
        Status::Unsupported,
        Status::Crash,
        Status::Timeout,
        Status::Blocked,
    ] {
        round_trip(&status);
    }
    for backend in [
        Backend::AcosNative,
        Backend::AcvmReference,
        Backend::HostTooling,
        Backend::Jvm,
        Backend::PhysicalDevice,
    ] {
        round_trip(&backend);
    }
    for fidelity in [
        Fidelity::StaticAnalysis,
        Fidelity::JvmReference,
        Fidelity::NativeRuntime,
        Fidelity::ArtReference,
        Fidelity::PhysicalDevice,
        Fidelity::Simulated,
    ] {
        round_trip(&fidelity);
    }
    round_trip(&Metrics::wall_ms(3).with_cpu_ms(2).with_peak_rss_bytes(1));
    round_trip(&Evidence {
        screenshot: Some("frames/7.png".to_string()),
        tree: Some("tree/7.json".to_string()),
        events: Some("events.jsonl".to_string()),
        log: Some("logs/logcat.txt".to_string()),
        verdict: Some("verdict.json".to_string()),
    });
    round_trip(&frame());
    round_trip(&DisplaySpec::None);
    round_trip(&DisplaySpec::offscreen(1080, 2400, 420));
    for isolation in [
        IsolationLevel::SharedTask,
        IsolationLevel::Process,
        IsolationLevel::Sandbox,
        IsolationLevel::Core,
        IsolationLevel::Vm,
    ] {
        round_trip(&isolation);
    }
    round_trip(&ResourceBudget::wall_ms(1).with_cpu_ms(2).with_memory_mib(3));
    round_trip(&RequestBase::new("req_1"));

    let pass = Envelope::pass(
        "req_1",
        Backend::HostTooling,
        Fidelity::StaticAnalysis,
        Metrics::wall_ms(3),
    )
    .with_job_id("job_1")
    .with_session_id("s_1")
    .with_isolation(IsolationLevel::Sandbox)
    .with_frame(frame())
    .with_capabilities(["apk.inspect", "ui.text"])
    .with_evidence(Evidence {
        screenshot: Some("frames/7.png".to_string()),
        ..Evidence::default()
    })
    .with_validated_against(Fidelity::ArtReference)
    .with_result(serde_json::json!({"package": "x"}))
    .unwrap();
    round_trip(&pass);

    let fail = Envelope::failure(
        "req_2",
        Backend::AcosNative,
        Fidelity::NativeRuntime,
        ProtocolError::new(ErrorCode::UnsupportedNativeSymbol, "missing symbol"),
        Metrics::wall_ms(4),
    );
    round_trip(&fail);
}

#[test]
fn ui_round_trips() {
    round_trip(&bounds());
    for text in [
        TextMatch::exact("x"),
        TextMatch::contains("x"),
        TextMatch::regex("x.*"),
        TextMatch::default(),
    ] {
        round_trip(&text);
    }
    round_trip(&selector());
    round_trip(&node());
    round_trip(&tree());
}

#[test]
fn assertion_round_trips() {
    round_trip(&RegionMask {
        bounds: bounds(),
        reason: Some("clock".to_string()),
    });
    round_trip(&ScreenshotAssertion {
        baseline: "baseline.png".to_string(),
        tolerance: 0.02,
        region_masks: vec![RegionMask {
            bounds: bounds(),
            reason: None,
        }],
    });
    round_trip(&TextAssertion {
        selector: Selector::by_id("result"),
        text: TextMatch::exact("clicks: 0"),
        bounds: Some(bounds()),
        timeout_ms: Some(5000),
    });
    round_trip(&VisibilityAssertion {
        selector: Selector::by_id("result"),
        bounds: Some(bounds()),
        timeout_ms: Some(5000),
    });
    round_trip(&StateAssertion {
        selector: Selector::by_id("input"),
        visible: Some(true),
        enabled: Some(true),
        focused: Some(true),
        clickable: Some(true),
        scrollable: Some(false),
        checked: Some(false),
        timeout_ms: Some(5000),
    });
    round_trip(&LogAssertion {
        tag: Some("AcosFixture".to_string()),
        patterns: vec!["^onCreate$".to_string()],
        ordered: true,
        since_previous_step: true,
        timeout_ms: Some(5000),
        min_matches: 1,
    });
    round_trip(&NoCrashAssertion { since_seq: Some(3) });
    for assertion in [
        Assertion::Text(TextAssertion {
            selector: Selector::by_id("result"),
            text: TextMatch::exact("clicks: 0"),
            bounds: None,
            timeout_ms: None,
        }),
        Assertion::Visible(VisibilityAssertion {
            selector: Selector::by_id("result"),
            bounds: None,
            timeout_ms: None,
        }),
        Assertion::NotVisible(VisibilityAssertion {
            selector: Selector::by_id("result"),
            bounds: None,
            timeout_ms: None,
        }),
        Assertion::State(StateAssertion {
            selector: Selector::by_id("input"),
            visible: Some(true),
            enabled: None,
            focused: None,
            clickable: None,
            scrollable: None,
            checked: None,
            timeout_ms: None,
        }),
        Assertion::Log(LogAssertion {
            tag: None,
            patterns: vec!["x".to_string()],
            ordered: false,
            since_previous_step: false,
            timeout_ms: None,
            min_matches: 1,
        }),
        Assertion::NoCrash(NoCrashAssertion::default()),
        Assertion::Screenshot(ScreenshotAssertion {
            baseline: "b.png".to_string(),
            tolerance: 0.0,
            region_masks: Vec::new(),
        }),
    ] {
        round_trip(&assertion);
    }
    round_trip(&AssertionDiff {
        expected: serde_json::json!("a"),
        actual: serde_json::json!("b"),
        path: Some("nodes[0].text".to_string()),
        detail: Some("mismatch".to_string()),
    });
    round_trip(&AssertionResult {
        kind: "text".to_string(),
        passed: false,
        diff: Some(AssertionDiff {
            expected: serde_json::json!("a"),
            actual: serde_json::json!("b"),
            path: None,
            detail: None,
        }),
        matched_node_ids: vec!["n1".to_string()],
        message: None,
    });
}

#[test]
fn events_round_trip() {
    for kind in [
        EventKind::Lifecycle,
        EventKind::Log,
        EventKind::Crash,
        EventKind::Anr,
        EventKind::Frame,
        EventKind::InputAck,
    ] {
        round_trip(&kind);
    }
    for state in [
        LifecycleState::AppCreated,
        LifecycleState::Created,
        LifecycleState::Started,
        LifecycleState::Resumed,
        LifecycleState::Paused,
        LifecycleState::Stopped,
        LifecycleState::SaveInstanceState,
        LifecycleState::Destroyed,
        LifecycleState::Recreated,
        LifecycleState::ProcessKilled,
        LifecycleState::ProcessStarted,
    ] {
        round_trip(&state);
    }
    for level in [
        LogLevel::Verbose,
        LogLevel::Debug,
        LogLevel::Info,
        LogLevel::Warn,
        LogLevel::Error,
    ] {
        round_trip(&level);
    }
    let bodies = vec![
        EventBody::Lifecycle {
            state: LifecycleState::Created,
            activity: Some(".MainActivity".to_string()),
        },
        EventBody::Log {
            tag: "AcosFixture".to_string(),
            level: LogLevel::Info,
            message: "onCreate".to_string(),
        },
        EventBody::Crash {
            exception: "java.lang.RuntimeException".to_string(),
            message: "boom".to_string(),
            stack: vec!["at X.y(X.java:1)".to_string()],
            fatal: true,
        },
        EventBody::Anr {
            reason: "main thread blocked".to_string(),
            activity: Some(".MainActivity".to_string()),
            stack: vec!["at X.y(X.java:1)".to_string()],
        },
        EventBody::Frame {
            frame_seq: 7,
            cause_id: "req_7".to_string(),
            meaningful: true,
            content_hash: "abc".to_string(),
        },
        EventBody::InputAck {
            cause_id: "req_8".to_string(),
            action: "tap".to_string(),
            accepted: true,
        },
    ];
    for (index, body) in bodies.into_iter().enumerate() {
        let event = Event::new(index as u64, 1000 + index as u64, body).with_session_id("s_1");
        round_trip(&event);
    }
}

#[test]
fn command_requests_and_responses_round_trip() {
    let requests = vec![
        CommandRequest::Inspect(InspectRequest {
            base: base(),
            apk: "a.apk".to_string(),
            native_libs: true,
            abi_map: true,
        }),
        CommandRequest::RunDex(RunDexRequest {
            base: base(),
            path: "classes.dex".to_string(),
            main: "Main".to_string(),
            args: vec!["a".to_string()],
            heap_max_bytes: Some(1024),
            gc_stress: true,
        }),
        CommandRequest::Run(RunRequest {
            base: base(),
            apk: "a.apk".to_string(),
            activity: Some(".MainActivity".to_string()),
            intent_action: Some("android.intent.action.MAIN".to_string()),
            data: Some("https://example.test".to_string()),
            extras: BTreeMap::from([("k".to_string(), "v".to_string())]),
            data_policy: Some(DataPolicy::Ephemeral),
            name: Some("login".to_string()),
        }),
        CommandRequest::Observe(ObserveRequest {
            base: base(),
            tree: true,
            text: true,
            include_frame: true,
            max_nodes: Some(60),
        }),
        CommandRequest::Capture(CaptureRequest {
            base: base(),
            format: CaptureFormat::Raw,
            fresh: true,
            after_seq: Some(7),
            after_cause: Some("req_7".to_string()),
            output: Some("frames/8.png".to_string()),
        }),
        CommandRequest::Action(ActionRequest {
            base: base(),
            action: Action::Swipe {
                from: Point { x: 1, y: 2 },
                to: Point { x: 3, y: 4 },
                duration_ms: 300,
            },
        }),
        CommandRequest::Assert(AssertRequest {
            base: base(),
            assertion: assertion(),
            fresh_frame: true,
        }),
        CommandRequest::Events(EventsRequest {
            base: base(),
            since_seq: Some(3),
            kinds: vec![EventKind::Crash, EventKind::Log],
            limit: Some(10),
            timeout_ms: Some(1000),
        }),
        CommandRequest::Reset(ResetRequest {
            base: base(),
            scope: ResetScope::App,
        }),
        CommandRequest::Profile(ProfileRequest {
            base: base(),
            memory: true,
            cpu: true,
            frames: true,
            duration_ms: Some(100),
        }),
        CommandRequest::Destroy(DestroyRequest { base: base() }),
        CommandRequest::Verify(VerifyRequest {
            base: base(),
            apk: Some("a.apk".to_string()),
            scenario: "s.yaml".to_string(),
            artifacts: "out/".to_string(),
            reference: Some("ref/".to_string()),
            required_fidelity: Some(Fidelity::NativeRuntime),
            flags: vec!["--x".to_string()],
        }),
        CommandRequest::CapabilityReport(CapabilityReportRequest {
            base: base(),
            apk: "a.apk".to_string(),
            scenario: Some("s.yaml".to_string()),
            native_libs: true,
        }),
    ];
    for request in &requests {
        round_trip(request);
        round_trip(&request.name());
    }

    for action in [
        Action::Tap {
            selector: Some(Selector::by_id("btn")),
            x: None,
            y: None,
            timeout_ms: Some(1000),
        },
        Action::LongPress {
            selector: None,
            x: Some(1),
            y: Some(2),
            duration_ms: 600,
        },
        Action::Swipe {
            from: Point { x: 1, y: 2 },
            to: Point { x: 3, y: 4 },
            duration_ms: 300,
        },
        Action::Type {
            selector: Some(Selector::by_id("input")),
            text: "World".to_string(),
            clear: true,
        },
        Action::Key {
            key: KeyName::Enter,
        },
        Action::Back,
        Action::Rotate {
            orientation: Orientation::Landscape,
        },
        Action::Wait {
            selector: Selector::by_id("result"),
            timeout_ms: 5000,
        },
    ] {
        round_trip(&action);
        round_trip(&action.kind().to_string());
    }
    for key in [
        KeyName::Back,
        KeyName::Home,
        KeyName::Enter,
        KeyName::Tab,
        KeyName::Delete,
        KeyName::Escape,
        KeyName::VolumeUp,
        KeyName::VolumeDown,
        KeyName::Power,
        KeyName::Menu,
        KeyName::DpadUp,
        KeyName::DpadDown,
        KeyName::DpadLeft,
        KeyName::DpadRight,
    ] {
        round_trip(&key);
    }
    round_trip(&Orientation::Portrait);
    round_trip(&Point { x: 1, y: 2 });

    let responses = vec![
        CommandResponse::Inspect(inspect_result()),
        CommandResponse::RunDex(RunDexResult {
            exit_code: 0,
            stdout: "hello\n".to_string(),
            stderr: String::new(),
            uncaught_exception: Some(UncaughtException {
                class: "java.lang.RuntimeException".to_string(),
                message: Some("boom".to_string()),
                stack: vec!["at X.y(X.java:1)".to_string()],
            }),
        }),
        CommandResponse::Run(RunResponse {
            session_id: "s_1".to_string(),
            package: "dev.acos.fixtures.helloviews".to_string(),
            activity: Some(".MainActivity".to_string()),
            isolation: IsolationLevel::Sandbox,
            display: DisplaySpec::offscreen(1080, 2400, 420),
            fidelity: Fidelity::NativeRuntime,
            frame: Some(frame()),
        }),
        CommandResponse::Observe(ObserveResponse {
            session_id: "s_1".to_string(),
            package: "dev.acos.fixtures.helloviews".to_string(),
            frame: Some(frame()),
            tree: Some(tree()),
            text: Some("android.widget.TextView | id | clicks: 0 | [0,0][1,1]".to_string()),
        }),
        CommandResponse::Capture(CaptureResponse {
            frame: frame(),
            format: CaptureFormat::Png,
            path: Some("frames/19.png".to_string()),
            bytes: Some(1234),
            width: 1080,
            height: 2400,
        }),
        CommandResponse::Action(ActionResponse {
            session_id: "s_1".to_string(),
            action: "tap".to_string(),
            accepted: true,
            state_changed: true,
            input_ack_seq: Some(8),
            frame: Some(frame()),
        }),
        CommandResponse::Assert(AssertResponse {
            passed: true,
            result: AssertionResult {
                kind: "text".to_string(),
                passed: true,
                diff: None,
                matched_node_ids: vec!["n1".to_string()],
                message: None,
            },
            frame: Some(frame()),
        }),
        CommandResponse::Events(EventsResponse {
            session_id: "s_1".to_string(),
            events: vec![Event::lifecycle(1, 1000, LifecycleState::Created, None)],
            next_seq: 1,
            truncated: false,
        }),
        CommandResponse::Reset(ResetResponse {
            session_id: "s_1".to_string(),
            scope: ResetScope::App,
            state_reset: true,
            data_cleared: true,
            isolation: IsolationLevel::Sandbox,
        }),
        CommandResponse::Profile(ProfileResponse {
            session_id: "s_1".to_string(),
            report: resource_report(),
        }),
        CommandResponse::Destroy(DestroyResponse {
            session_id: "s_1".to_string(),
            destroyed: true,
            already_absent: false,
        }),
        CommandResponse::Verify(VerifyResponse {
            verdict: verdict(),
            verdict_path: "out/verdict.json".to_string(),
            artifacts_dir: "out/".to_string(),
            artifact_validation: ArtifactValidation::ok(),
        }),
        CommandResponse::CapabilityReport(CapabilityReport {
            apk: "a.apk".to_string(),
            package: Some("dev.acos.fixtures.helloviews".to_string()),
            abis: vec!["x86_64".to_string()],
            min_sdk: Some(26),
            target_sdk: Some(34),
            checks: vec![CapabilityCheck {
                capability: "ui.text".to_string(),
                status: CapabilityStatus::Supported,
                code: None,
                detail: Some("accessibility tree".to_string()),
                hint: None,
            }],
            all_required_supported: true,
            fidelity: Fidelity::NativeRuntime,
            status: Status::Pass,
        }),
    ];
    for response in &responses {
        round_trip(response);
    }

    for command in [
        CommandName::Inspect,
        CommandName::RunDex,
        CommandName::Run,
        CommandName::Observe,
        CommandName::Capture,
        CommandName::Action,
        CommandName::Assert,
        CommandName::Events,
        CommandName::Reset,
        CommandName::Profile,
        CommandName::Destroy,
        CommandName::Verify,
        CommandName::CapabilityReport,
    ] {
        round_trip(&command);
    }
    for capture in [CaptureFormat::Png, CaptureFormat::Raw] {
        round_trip(&capture);
    }
    for policy in [
        DataPolicy::Ephemeral,
        DataPolicy::Persistent,
        DataPolicy::Fresh,
    ] {
        round_trip(&policy);
    }
    for scope in [
        ResetScope::Task,
        ResetScope::App,
        ResetScope::Data,
        ResetScope::All,
    ] {
        round_trip(&scope);
    }
    for status in [
        CapabilityStatus::Supported,
        CapabilityStatus::Unsupported,
        CapabilityStatus::Unknown,
        CapabilityStatus::RequiresDevice,
        CapabilityStatus::RequiresGms,
        CapabilityStatus::RequiresAbi,
    ] {
        round_trip(&status);
    }
}

#[test]
#[allow(clippy::vec_init_then_push)]
fn scenario_all_steps_round_trip_json_and_yaml() {
    let mut steps = Vec::new();
    steps.push(Step::launch(LaunchStep {
        activity: Some(".MainActivity".to_string()),
        intent_action: None,
        data: None,
        extras: BTreeMap::new(),
        clear_state: true,
    }));
    steps.push(Step {
        wait_for: Some(WaitForStep {
            selector: Selector::by_id("result"),
            timeout_ms: 5000,
            require_visible: true,
        }),
        ..Step::default()
    });
    steps.push(Step {
        tap: Some(TapStep {
            selector: Some(Selector::by_id("btn")),
            x: None,
            y: None,
            timeout_ms: Some(1000),
        }),
        ..Step::default()
    });
    steps.push(Step {
        r#type: Some(TypeStep {
            selector: Some(Selector::by_id("input")),
            text: "World".to_string(),
            clear: true,
        }),
        ..Step::default()
    });
    steps.push(Step {
        key: Some(KeyStep {
            key: KeyName::Enter,
        }),
        ..Step::default()
    });
    steps.push(Step::back());
    steps.push(Step {
        rotate: Some(RotateStep {
            orientation: Orientation::Landscape,
        }),
        ..Step::default()
    });
    steps.push(Step {
        assert_text: Some(TextAssertion {
            selector: Selector::by_id("result"),
            text: TextMatch::exact("clicks: 0"),
            bounds: Some(bounds()),
            timeout_ms: None,
        }),
        ..Step::default()
    });
    steps.push(Step {
        assert_visible: Some(VisibilityAssertion {
            selector: Selector::by_id("result"),
            bounds: None,
            timeout_ms: None,
        }),
        ..Step::default()
    });
    steps.push(Step {
        assert_not_visible: Some(VisibilityAssertion {
            selector: Selector::by_id("result"),
            bounds: None,
            timeout_ms: None,
        }),
        ..Step::default()
    });
    steps.push(Step {
        assert_state: Some(StateAssertion {
            selector: Selector::by_id("input"),
            visible: Some(true),
            enabled: Some(true),
            focused: Some(true),
            clickable: Some(true),
            scrollable: Some(false),
            checked: Some(false),
            timeout_ms: None,
        }),
        ..Step::default()
    });
    steps.push(Step {
        assert_log: Some(LogAssertion {
            tag: Some("AcosFixture".to_string()),
            patterns: vec!["^onCreate$".to_string()],
            ordered: true,
            since_previous_step: true,
            timeout_ms: None,
            min_matches: 1,
        }),
        ..Step::default()
    });
    steps.push(Step {
        assert_no_crash: Some(NoCrashAssertion::default()),
        ..Step::default()
    });
    steps.push(Step {
        assert_screenshot: Some(ScreenshotAssertion {
            baseline: "baseline.png".to_string(),
            tolerance: 0.01,
            region_masks: vec![RegionMask {
                bounds: bounds(),
                reason: Some("clock".to_string()),
            }],
        }),
        ..Step::default()
    });
    steps.push(Step {
        capture: Some(CaptureStep {
            name: "shot".to_string(),
            format: CaptureFormat::Png,
            fresh: true,
        }),
        ..Step::default()
    });
    steps.push(Step {
        reset: Some(ResetStep {
            scope: ResetScope::App,
        }),
        ..Step::default()
    });
    steps.push(Step {
        set_network: Some(NetworkSetup {
            offline: Some(false),
            rules: vec![NetworkRule {
                method: Some("GET".to_string()),
                url: "https://example.test/user".to_string(),
                status: 429,
                headers: BTreeMap::from([("Retry-After".to_string(), "1".to_string())]),
                body: Some("{}".to_string()),
                delay_ms: Some(10),
                times: Some(1),
            }],
        }),
        ..Step::default()
    });
    steps.push(Step {
        set_permission: Some(PermissionSetup {
            permission: "android.permission.CAMERA".to_string(),
            state: PermissionState::Deny,
        }),
        ..Step::default()
    });
    steps.push(Step {
        set_locale: Some(LocaleSetup {
            locale: "en-US".to_string(),
        }),
        ..Step::default()
    });
    steps.push(Step {
        set_density: Some(DensitySetup { density_dpi: 420 }),
        ..Step::default()
    });
    steps.push(Step {
        set_theme: Some(ThemeSetup { theme: Theme::Dark }),
        ..Step::default()
    });
    steps.push(Step {
        set_time: Some(TimeSetup {
            timestamp: "2026-11-10T10:00:00Z".to_string(),
            timezone: Some("UTC".to_string()),
        }),
        ..Step::default()
    });
    steps.push(Step {
        call_activity_method: Some(CallActivityMethodStep {
            activity: Some(".MainActivity".to_string()),
            method: "injectTestHook".to_string(),
            args: vec![serde_json::json!(1), serde_json::json!("x")],
        }),
        ..Step::default()
    });

    let scenario = Scenario {
        version: SCENARIO_VERSION,
        name: "round-trip".to_string(),
        description: Some("all steps".to_string()),
        app: "a.apk".to_string(),
        engine: Engine::NativePreferred,
        fallback: FallbackPolicy::ReportAndEscalate,
        api_level: Some(34),
        isolation: IsolationLevel::Sandbox,
        display: DisplaySpec::offscreen(1080, 2400, 420),
        capabilities: vec!["ui.text".to_string()],
        budgets: Some(ResourceBudget::wall_ms(1000)),
        expectation: Expectation::Pass,
        steps,
        output: OutputOptions::default(),
    };
    round_trip(&scenario);
    for step in &scenario.steps {
        round_trip(step);
        assert_eq!(step.variant_count(), 1);
        assert!(step.as_step().is_some());
        round_trip(&step.kind().to_string());
    }
    // YAML is the authoritative scenario encoding.
    let yaml = scenario.to_yaml_string().unwrap();
    let back: Scenario = serde_yaml_ng::from_str(&yaml).unwrap();
    assert_eq!(back, scenario);
    assert!(scenario.validate().is_ok(), "{:?}", scenario.validate());
}

#[test]
fn verdict_and_mcp_round_trip() {
    round_trip(&ArtifactRef::new("frame", "frames/7.png"));
    round_trip(&StepResult::new(0, "launch", Status::Pass, 1));
    round_trip(&resource_report());
    round_trip(&Reproducer::new(
        "0.1.0",
        "a.apk",
        "sha",
        "s.yaml",
        "sha2",
        IsolationLevel::Sandbox,
        DisplaySpec::offscreen(1080, 2400, 420),
        Fidelity::NativeRuntime,
        Backend::AcosNative,
    ));
    round_trip(&AppIdentity {
        path: "a.apk".to_string(),
        package: Some("p".to_string()),
        sha256: "sha".to_string(),
    });
    round_trip(&ScenarioIdentity {
        path: "s.yaml".to_string(),
        name: "s".to_string(),
        sha256: "sha".to_string(),
    });
    round_trip(&verdict());
    round_trip(&ArtifactIssue {
        code: ErrorCode::ArtifactsInvalid,
        path: "frames/7.png".to_string(),
        message: "missing".to_string(),
    });
    round_trip(&ArtifactValidation {
        ok: false,
        issues: vec![ArtifactIssue {
            code: ErrorCode::ArtifactsInvalid,
            path: "frames/7.png".to_string(),
            message: "missing".to_string(),
        }],
    });
    round_trip(&mcp_tools());
    round_trip(&McpToolsList {
        tools: mcp_tools().tools,
    });
    let descriptor = McpToolDescriptor::new::<InspectRequest>(CommandName::Inspect, "description");
    round_trip(&descriptor);
    assert_eq!(
        step_file_name(3, "assert_text"),
        "steps/03-assert_text.json"
    );
    assert_eq!(frame_file_name(19), "frames/19.png");
    assert_eq!(tree_file_name(19), "tree/19.json");
}
