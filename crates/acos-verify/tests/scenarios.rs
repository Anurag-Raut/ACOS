//! End-to-end scenario tests.
//!
//! These run the real checked-in scenarios against the ART reference replay
//! backend. The point is the honesty contract:
//!
//! * the 5 negative scenarios must produce `status: fail` (never a pass);
//! * the positive scenarios must run as far as the backend supports and report
//!   a truthful `unsupported` partial (the reference replay cannot react to
//!   input);
//! * a step kind that is not implemented must be `unsupported` with the exact
//!   missing capability.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use acos_protocol::commands::VerifyRequest;
use acos_protocol::scenario::{Expectation, Scenario};
use acos_protocol::ui::{Bounds, UiNode};
use acos_protocol::{
    Backend as ProtocolBackend, Envelope, Fidelity, FrameMeta, LogLevel, Status, UiTree,
};
use acos_verify::backend::{
    Backend, BackendError, Capabilities, InputAction, InputDelivery, LaunchOutcome, LaunchRequest,
    LogRecord, ManifestInfo, SessionObservation,
};
use acos_verify::clock::FixedClock;
use acos_verify::executor::{run_scenario, RunOptions};
use acos_verify::reference::ReferenceBackend;
use acos_verify::verify::verify_with_clock;
use acos_verify::RunOutcome;

fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn scenario_path(name: &str) -> PathBuf {
    workspace().join("scenarios").join(name)
}

fn reference_dir(app: &str) -> PathBuf {
    workspace()
        .join("fixtures/reference")
        .join(app)
        .join("art-api34-x86_64")
}

fn scratch(name: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "acos-verify-scenarios-{}-{n}-{name}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn run_with_reference(scenario_name: &str, app: &str, artifacts_name: &str) -> RunOutcome {
    let scenario = Scenario::load(&scenario_path(scenario_name))
        .unwrap_or_else(|validation| panic!("{scenario_name}: {validation:?}"));
    let apk = workspace().join(&scenario.app);
    let reference = reference_dir(app);
    assert!(
        reference.join("ui_initial.json").exists(),
        "reference evidence missing at {}",
        reference.display()
    );
    let artifacts = scratch(artifacts_name);
    let mut backend = ReferenceBackend::new(&reference);
    let options = RunOptions {
        apk,
        scenario_path: scenario_path(scenario_name),
        artifacts_dir: artifacts,
        required_fidelity: None,
        request_id: format!("req_test_{scenario_name}"),
        flags: vec!["--reference".to_string()],
        acos_version: "0.1.0".to_string(),
        engine: "rust-vm".to_string(),
        warnings: Vec::new(),
    };
    let clock = FixedClock::new(1_794_318_674_000);
    let outcome =
        run_scenario(&scenario, &options, &mut backend, &clock).expect("run_scenario succeeds");
    // Clean up the scratch artifacts; the verdict is returned in-memory.
    let _ = std::fs::remove_dir_all(&options.artifacts_dir);
    outcome
}

#[test]
fn negative_scenarios_fail_and_never_pass() {
    let cases = [
        ("negative/wrong-text.yaml", "hello-views"),
        ("negative/wrong-bounds.yaml", "hello-views"),
        ("negative/missing-element.yaml", "hello-views"),
        ("negative/wrong-lifecycle-order.yaml", "hello-views"),
        ("negative/missing-jni-line.yaml", "hello-jni"),
    ];
    for (scenario_name, app) in cases {
        let outcome = run_with_reference(scenario_name, app, "negative");
        assert_eq!(
            outcome.verdict.verdict,
            Status::Fail,
            "{scenario_name} should fail, got {:?} (failure {:?})",
            outcome.verdict.verdict,
            outcome.verdict.failure
        );
        assert!(
            !outcome.verdict.verdict.is_pass(),
            "{scenario_name} must never pass"
        );
        assert!(
            outcome.verdict.failure.is_some(),
            "{scenario_name} must carry a failure"
        );
        let scenario = Scenario::load(&scenario_path(scenario_name)).unwrap();
        assert_eq!(scenario.expectation, Expectation::Fail);
    }
}

#[test]
fn positive_hello_views_is_a_truthful_partial() {
    let outcome = run_with_reference("hello-views.yaml", "hello-views", "positive-views");
    assert_eq!(outcome.verdict.verdict, Status::Unsupported);
    assert!(!outcome.verdict.verdict.is_pass());
    // Everything executed before the first input step passed.
    let (executed, unsupported) = (outcome.verdict.steps.len(), 1);
    assert!(executed >= 6, "expected a prefix of steps, got {executed}");
    assert_eq!(outcome.verdict.steps[executed - 1].step, "tap");
    assert_eq!(outcome.verdict.steps[executed - 1].status, Status::Unsupported);
    assert_eq!(unsupported, 1);
    for step in &outcome.verdict.steps[..executed - 1] {
        assert_eq!(step.status, Status::Pass, "step {} should pass", step.step);
    }
    let failure = outcome.verdict.failure.as_ref().unwrap();
    let details = failure.error.details.as_ref().unwrap();
    assert_eq!(details["capability"], "input");
}

#[test]
fn positive_hello_jni_is_a_truthful_partial() {
    let outcome = run_with_reference("hello-jni.yaml", "hello-jni", "positive-jni");
    assert_eq!(outcome.verdict.verdict, Status::Unsupported);
    let last = outcome.verdict.steps.last().unwrap();
    assert_eq!(last.step, "capture");
    assert_eq!(last.status, Status::Unsupported);
    assert_eq!(
        outcome.verdict.failure.as_ref().unwrap().error.details.as_ref().unwrap()["capability"],
        "screenshot"
    );
}

#[test]
fn unimplemented_step_kind_is_unsupported_with_the_missing_capability() {
    let outcome = run_with_reference("negative/wrong-text.yaml", "hello-views", "unknown-step");
    // Sanity: this scenario fails, not unsupported.
    assert_eq!(outcome.verdict.verdict, Status::Fail);

    // Now a scenario whose first step needs the environment capability.
    let yaml = r#"
version: 1
name: environment-only
app: fixtures/build/apps/hello-views.apk
isolation: process
display: {mode: offscreen, width: 1080, height: 2400, density_dpi: 420}
expectation: non_pass
steps:
  - set_locale: {locale: en-US}
"#;
    let scenario = Scenario::from_yaml_str(yaml).expect("valid scenario");
    let artifacts = scratch("environment-only");
    let mut backend = ReferenceBackend::new(reference_dir("hello-views"));
    let options = RunOptions {
        apk: workspace().join("fixtures/build/apps/hello-views.apk"),
        scenario_path: workspace().join("scenarios/hello-views.yaml"),
        artifacts_dir: artifacts.clone(),
        required_fidelity: None,
        request_id: "req_unknown_step".to_string(),
        flags: Vec::new(),
        acos_version: "0.1.0".to_string(),
        engine: "rust-vm".to_string(),
        warnings: Vec::new(),
    };
    let clock = FixedClock::new(1_794_318_674_000);
    let outcome = run_scenario(&scenario, &options, &mut backend, &clock).unwrap();
    assert_eq!(outcome.verdict.verdict, Status::Unsupported);
    let step = &outcome.verdict.steps[0];
    assert_eq!(step.step, "set_locale");
    assert_eq!(step.status, Status::Unsupported);
    assert_eq!(
        step.error.as_ref().unwrap().details.as_ref().unwrap()["capability"],
        "environment"
    );
    let _ = std::fs::remove_dir_all(&artifacts);
}

/// A minimal backend that only implements `launch`. It proves the trait is the
/// plug-in seam for M2/M3 capabilities without a scenario-schema change.
struct LaunchOnlyBackend {
    manifest: Option<ManifestInfo>,
}

impl Backend for LaunchOnlyBackend {
    fn id(&self) -> &'static str {
        "launch-only-mock"
    }

    fn protocol_backend(&self) -> ProtocolBackend {
        ProtocolBackend::AcosNative
    }

    fn fidelity(&self) -> Fidelity {
        Fidelity::NativeRuntime
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            static_manifest: true,
            launch: true,
            ..Capabilities::default()
        }
    }

    fn manifest(&mut self, apk: &Path) -> Result<ManifestInfo, BackendError> {
        let info = acos_verify::manifest::load_manifest(apk)?;
        self.manifest = Some(info.clone());
        Ok(info)
    }

    fn launch(&mut self, _request: &LaunchRequest) -> Result<LaunchOutcome, BackendError> {
        Ok(LaunchOutcome {
            session_id: "s_mock".to_string(),
            package: self
                .manifest
                .as_ref()
                .map(|m| m.package.clone())
                .unwrap_or_default(),
            frame: None,
            screenshot_png: None,
            crashed: None,
            detail: serde_json::json!({}),
        })
    }
}

#[test]
fn required_fidelity_mismatch_is_unsupported_not_a_pass() {
    let yaml = r#"
version: 1
name: launch-only
app: fixtures/build/apps/hello-views.apk
isolation: process
display: {mode: offscreen, width: 1080, height: 2400, density_dpi: 420}
expectation: pass
steps:
  - launch: {activity: dev.acos.fixtures.helloviews/.MainActivity}
"#;
    let scenario = Scenario::from_yaml_str(yaml).expect("valid scenario");
    let artifacts = scratch("fidelity-mismatch");
    let mut backend = LaunchOnlyBackend { manifest: None };
    let options = RunOptions {
        apk: workspace().join("fixtures/build/apps/hello-views.apk"),
        scenario_path: workspace().join("scenarios/hello-views.yaml"),
        artifacts_dir: artifacts.clone(),
        required_fidelity: Some(Fidelity::ArtReference),
        request_id: "req_fidelity".to_string(),
        flags: Vec::new(),
        acos_version: "0.1.0".to_string(),
        engine: "rust-vm".to_string(),
        warnings: Vec::new(),
    };
    let clock = FixedClock::new(1_794_318_674_000);
    let outcome = run_scenario(&scenario, &options, &mut backend, &clock).unwrap();
    assert_eq!(outcome.verdict.verdict, Status::Unsupported);
    let error = outcome.verdict.failure.as_ref().unwrap().error.clone();
    assert_eq!(error.code, acos_protocol::ErrorCode::UnsupportedBackend);
    let details = error.details.as_ref().unwrap();
    assert_eq!(details["required_fidelity"], "art-reference");
    assert_eq!(details["achieved_fidelity"], "native-runtime");
    let _ = std::fs::remove_dir_all(&artifacts);
}

#[test]
fn verify_envelope_satisfies_protocol_invariants() {
    let scenario = Scenario::load(&scenario_path("negative/wrong-text.yaml")).unwrap();
    let request = VerifyRequest {
        base: acos_protocol::RequestBase::new("req_envelope"),
        apk: Some(workspace().join(&scenario.app).display().to_string()),
        scenario: scenario_path("negative/wrong-text.yaml").display().to_string(),
        artifacts: scratch("envelope").display().to_string(),
        overwrite: false,
        reference: Some(reference_dir("hello-views").display().to_string()),
        required_fidelity: None,
        engine: acos_protocol::VerifyEngine::RustVm,
        flags: vec!["--reference".to_string()],
        animations: None,
        native: false,
    };
    let mut backend = ReferenceBackend::new(reference_dir("hello-views"));
    let clock = FixedClock::new(1_794_318_674_000);
    let envelope = verify_with_clock(&request, &mut backend, &clock);
    assert_eq!(envelope.status(), Status::Fail);
    assert!(!envelope.ok());
    // Round-trip through the validating deserializer.
    let json = serde_json::to_value(&envelope).unwrap();
    let back: Envelope = serde_json::from_value(json).expect("envelope validates");
    assert_eq!(back.status(), Status::Fail);
    let _ = std::fs::remove_dir_all(&request.artifacts);
}

// ---------------------------------------------------------------------------
// The advisor frame-binding rule, exercised end-to-end against a mock
// vertical-slice backend (launch -> UI tree -> tap -> changed state in both).
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum SliceMode {
    /// Tap changes the tree and the screenshot.
    Good,
    /// Tap changes only the tree; the screenshot is stale.
    TreeOnly,
    /// Tap changes only the screenshot; the tree is stale.
    ScreenshotOnly,
    /// `capture(fresh)` returns the frame that was already completed.
    StaleCapture,
    /// `observe` returns a tree whose frame is not the observation frame.
    UnboundTree,
    /// `observe` advances the frame without a wait_for/capture.
    DriftingObserve,
    /// The frame is not meaningful.
    Placeholder,
}

struct SliceBackend {
    mode: SliceMode,
    manifest: Option<ManifestInfo>,
    frame_seq: u64,
    tree_version: i32,
    shot_version: u8,
    screenshot_initial: Vec<u8>,
    screenshot_after: Vec<u8>,
    /// SE1: the strict settle's measured pixel fact. `None` = the backend
    /// reports no settle record at all (what a non-pixel backend does today).
    settle_pixels: Option<bool>,
}

impl SliceBackend {
    fn new(mode: SliceMode, initial: Vec<u8>, after: Vec<u8>) -> Self {
        SliceBackend {
            mode,
            manifest: None,
            frame_seq: 0,
            tree_version: 0,
            shot_version: 0,
            screenshot_initial: initial,
            screenshot_after: after,
            settle_pixels: None,
        }
    }

    /// SE1: report `settle.pixels_changed` on every input reply.
    fn with_settle_pixels(mut self, pixels_changed: bool) -> Self {
        self.settle_pixels = Some(pixels_changed);
        self
    }

    fn meaningful(&self) -> bool {
        self.mode != SliceMode::Placeholder
    }

    fn screenshot(&self) -> &[u8] {
        if self.shot_version == 0 {
            &self.screenshot_initial
        } else {
            &self.screenshot_after
        }
    }

    fn current(&self) -> SessionObservation {
        slice_observation(
            self.frame_seq,
            self.tree_version,
            self.screenshot(),
            self.meaningful(),
        )
    }
}

fn slice_observation(
    frame_seq: u64,
    tree_version: i32,
    screenshot: &[u8],
    meaningful: bool,
) -> SessionObservation {
    let frame = FrameMeta::new(
        frame_seq,
        "2026-11-10T13:51:14.000Z",
        format!("req:{frame_seq}"),
        1080,
        2400,
        420,
        format!("tree-clicks-{tree_version}"),
        meaningful,
    );
    let node = UiNode::new(
        "n1",
        "android.widget.TextView",
        Bounds::new(42, 612, 175, 663),
    )
    .with_resource_id("dev.acos.fixtures.helloviews:id/result")
    .with_text(format!("clicks: {tree_version}"));
    let tree = UiTree::new(
        frame.clone(),
        "dev.acos.fixtures.helloviews",
        vec![node],
    );
    SessionObservation {
        session_id: "s_slice".to_string(),
        package: "dev.acos.fixtures.helloviews".to_string(),
        frame,
        tree,
        screenshot_png: Some(screenshot.to_vec()),
        crashed: None,
        input: None,
        settle: None,
    }
}

impl Backend for SliceBackend {
    fn id(&self) -> &'static str {
        "vertical-slice-mock"
    }

    fn protocol_backend(&self) -> ProtocolBackend {
        ProtocolBackend::AcosNative
    }

    fn fidelity(&self) -> Fidelity {
        Fidelity::NativeRuntime
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            static_manifest: true,
            launch: true,
            ui_tree: true,
            screenshot: true,
            input: true,
            ..Capabilities::default()
        }
    }

    fn manifest(&mut self, apk: &Path) -> Result<ManifestInfo, BackendError> {
        let info = acos_verify::manifest::load_manifest(apk)?;
        self.manifest = Some(info.clone());
        Ok(info)
    }

    fn launch(&mut self, _request: &LaunchRequest) -> Result<LaunchOutcome, BackendError> {
        self.frame_seq = 1;
        self.tree_version = 0;
        self.shot_version = 0;
        let observation = self.current();
        Ok(LaunchOutcome {
            session_id: "s_slice".to_string(),
            package: "dev.acos.fixtures.helloviews".to_string(),
            frame: Some(observation.frame.clone()),
            screenshot_png: observation.screenshot_png.clone(),
            crashed: None,
            detail: serde_json::json!({"backend": "vertical-slice-mock"}),
        })
    }

    fn observe(&mut self) -> Result<SessionObservation, BackendError> {
        if self.mode == SliceMode::DriftingObserve {
            self.frame_seq += 1;
        }
        let mut observation = self.current();
        if self.mode == SliceMode::UnboundTree {
            observation.tree.frame = FrameMeta::new(
                self.frame_seq + 1,
                "2026-11-10T13:51:14.000Z",
                "req:unbound",
                1080,
                2400,
                420,
                observation.frame.content_hash.clone(),
                true,
            );
        }
        Ok(observation)
    }

    fn input(&mut self, action: &InputAction) -> Result<SessionObservation, BackendError> {
        if matches!(action, InputAction::Tap { .. }) {
            self.frame_seq += 1;
            match self.mode {
                SliceMode::TreeOnly => self.tree_version = 1,
                SliceMode::ScreenshotOnly => self.shot_version = 1,
                _ => {
                    self.tree_version = 1;
                    self.shot_version = 1;
                }
            }
        }
        let mut observation = self.current();
        if let Some(pixels_changed) = self.settle_pixels {
            observation.input = Some(InputDelivery {
                delivered: true,
                visual_change: pixels_changed,
                handled: None,
                published: 0,
                acks: Vec::new(),
                target: None,
                settle: Some(serde_json::json!({
                    "mode": "strict",
                    "settled": true,
                    "bound": "strict-event",
                    "strict_rule": "event",
                    "pixels_changed": pixels_changed,
                })),
            });
        }
        Ok(observation)
    }

    fn capture(&mut self) -> Result<SessionObservation, BackendError> {
        if self.mode != SliceMode::StaleCapture {
            self.frame_seq += 1;
        }
        Ok(self.current())
    }
}

fn run_slice(yaml: &str, mode: SliceMode, initial: Vec<u8>, after: Vec<u8>) -> RunOutcome {
    run_slice_with_settle(yaml, mode, initial, after, None)
}

fn run_slice_with_settle(
    yaml: &str,
    mode: SliceMode,
    initial: Vec<u8>,
    after: Vec<u8>,
    settle_pixels: Option<bool>,
) -> RunOutcome {
    let scenario = Scenario::from_yaml_str(yaml).expect("valid scenario");
    let artifacts = scratch("advisor-rule");
    let mut backend = SliceBackend::new(mode, initial, after);
    backend.settle_pixels = settle_pixels;
    let options = RunOptions {
        apk: workspace().join("fixtures/build/apps/hello-views.apk"),
        scenario_path: workspace().join("scenarios/hello-views.yaml"),
        artifacts_dir: artifacts.clone(),
        required_fidelity: None,
        request_id: "req_advisor_rule".to_string(),
        flags: Vec::new(),
        acos_version: "0.1.0".to_string(),
        engine: "rust-vm".to_string(),
        warnings: Vec::new(),
    };
    let clock = FixedClock::new(1_794_318_674_000);
    let outcome = run_scenario(&scenario, &options, &mut backend, &clock).expect("run");
    let _ = std::fs::remove_dir_all(&artifacts);
    outcome
}

/// Like [`run_slice`], but keeps the artifacts directory so a test can read the
/// files a verdict declares.
fn run_slice_keep(
    yaml: &str,
    mode: SliceMode,
    initial: Vec<u8>,
    after: Vec<u8>,
) -> (RunOutcome, PathBuf) {
    let scenario = Scenario::from_yaml_str(yaml).expect("valid scenario");
    let artifacts = scratch("slice-keep");
    let mut backend = SliceBackend::new(mode, initial, after);
    let options = RunOptions {
        apk: workspace().join("fixtures/build/apps/hello-views.apk"),
        scenario_path: workspace().join("scenarios/hello-views.yaml"),
        artifacts_dir: artifacts.clone(),
        required_fidelity: None,
        request_id: "req_slice_keep".to_string(),
        flags: Vec::new(),
        acos_version: "0.1.0".to_string(),
        engine: "rust-vm".to_string(),
        warnings: Vec::new(),
    };
    let clock = FixedClock::new(1_794_318_674_000);
    let outcome = run_scenario(&scenario, &options, &mut backend, &clock).expect("run");
    (outcome, artifacts)
}

#[test]
fn capture_name_is_persisted_in_the_step_file_name_and_verdict_map() {
    let yaml = r#"
version: 1
name: capture-named
app: fixtures/build/apps/hello-views.apk
isolation: process
display: {mode: offscreen, width: 1080, height: 2400, density_dpi: 420}
expectation: pass
steps:
  - launch: {activity: dev.acos.fixtures.helloviews/.MainActivity}
  - capture: {name: "meanings collapsed", format: png, fresh: true}
"#;
    let (outcome, artifacts) = run_slice_keep(
        yaml,
        SliceMode::Good,
        b"initial".to_vec(),
        b"after".to_vec(),
    );
    assert_eq!(outcome.verdict.verdict, Status::Pass);
    let step = outcome
        .verdict
        .steps
        .iter()
        .find(|step| step.step == "capture")
        .expect("a capture step");
    // 1. The step carries its own name.
    assert_eq!(step.name.as_deref(), Some("meanings collapsed"));
    let seq = step.frame_seq.expect("the capture bound a frame");
    // 2. The PNG exists under the sanitised name, with the frame sequence.
    let named = format!("frames/{seq}-meanings-collapsed.png");
    assert!(
        artifacts.join(&named).is_file(),
        "named capture frame missing: {}",
        artifacts.join(&named).display()
    );
    // The canonical frame the validator requires is still there, same bytes.
    let canonical = artifacts.join(format!("frames/{seq}.png"));
    assert!(canonical.is_file());
    assert_eq!(
        std::fs::read(&canonical).unwrap(),
        std::fs::read(artifacts.join(&named)).unwrap()
    );
    // 3. The verdict maps the scenario's own name to the file, and declares it
    //    as evidence.
    assert_eq!(outcome.verdict.captures.get("meanings collapsed"), Some(&named));
    assert!(
        step.evidence
            .iter()
            .any(|evidence| evidence.kind == "capture" && evidence.path == named),
        "the named frame is declared evidence"
    );
    // The same three facts survive on disk (the step JSON and verdict.json).
    let step_json: serde_json::Value = serde_json::from_slice(
        &std::fs::read(artifacts.join("steps/01-capture.json")).expect("step JSON on disk"),
    )
    .unwrap();
    assert_eq!(step_json["name"], "meanings collapsed");
    let verdict_json: serde_json::Value = serde_json::from_slice(
        &std::fs::read(artifacts.join("verdict.json")).expect("verdict.json on disk"),
    )
    .unwrap();
    assert_eq!(verdict_json["captures"]["meanings collapsed"], named);
    assert_eq!(verdict_json["status"], "pass", "status mirrors the envelope");
    assert_eq!(verdict_json["verdict"], "pass");
    // 0 dangling references: the run's own validator accepts the directory.
    let validation = acos_protocol::artifacts::validate_artifacts_dir(&artifacts);
    assert!(validation.ok, "{:?}", validation.issues);
    let _ = std::fs::remove_dir_all(&artifacts);
}

const TAP_SCENARIO: &str = r#"
version: 1
name: tap-both
app: fixtures/build/apps/hello-views.apk
isolation: process
display: {mode: offscreen, width: 1080, height: 2400, density_dpi: 420}
expectation: pass
steps:
  - launch: {activity: dev.acos.fixtures.helloviews/.MainActivity}
  - assert_text: {selector: {id: result}, text: {exact: "clicks: 0"}}
  - tap: {selector: {id: btn}}
"#;

fn tap_step(outcome: &RunOutcome) -> &acos_protocol::verdict::StepResult {
    outcome
        .verdict
        .steps
        .iter()
        .find(|step| step.step == "tap")
        .expect("tap step")
}

#[test]
fn tap_changes_state_in_both_tree_and_screenshot() {
    let outcome = run_slice(TAP_SCENARIO, SliceMode::Good, b"initial".to_vec(), b"after".to_vec());
    assert_eq!(outcome.verdict.verdict, Status::Pass, "{:?}", outcome.verdict.failure);
    assert!(
        outcome.artifact_validation.ok,
        "a claimed pass must pass artifact validation: {:?}",
        outcome.artifact_validation.issues
    );
    let tap = tap_step(&outcome);
    assert_eq!(tap.status, Status::Pass);
    assert!(
        tap.assertions.iter().any(|a| a.kind == "state_change" && a.passed),
        "tap must record a passing state_change assertion"
    );
}

#[test]
fn tap_that_changes_only_the_tree_fails() {
    let outcome =
        run_slice(TAP_SCENARIO, SliceMode::TreeOnly, b"initial".to_vec(), b"after".to_vec());
    assert_eq!(outcome.verdict.verdict, Status::Fail);
    let tap = tap_step(&outcome);
    assert_eq!(tap.status, Status::Fail);
    let failure = outcome.verdict.failure.as_ref().unwrap();
    assert_eq!(failure.error.code, acos_protocol::ErrorCode::AssertionFailed);
    assert!(failure
        .error
        .message
        .contains("both") || failure.error.message.contains("screenshot"));
}

#[test]
fn tap_that_changes_only_the_screenshot_fails() {
    let outcome = run_slice(
        TAP_SCENARIO,
        SliceMode::ScreenshotOnly,
        b"initial".to_vec(),
        b"after".to_vec(),
    );
    assert_eq!(outcome.verdict.verdict, Status::Fail);
    assert_eq!(tap_step(&outcome).status, Status::Fail);
}

#[test]
fn fresh_capture_requires_a_new_completed_frame() {
    let yaml = r#"
version: 1
name: capture-fresh
app: fixtures/build/apps/hello-views.apk
isolation: process
display: {mode: offscreen, width: 1080, height: 2400, density_dpi: 420}
expectation: non_pass
steps:
  - launch: {activity: dev.acos.fixtures.helloviews/.MainActivity}
  - capture: {name: shot, format: png, fresh: true}
"#;
    let outcome = run_slice(yaml, SliceMode::StaleCapture, b"initial".to_vec(), b"after".to_vec());
    assert!(!outcome.verdict.verdict.is_pass());
    let failure = outcome.verdict.failure.as_ref().unwrap();
    assert_eq!(failure.error.code, acos_protocol::ErrorCode::NoFreshFrame);
}

#[test]
fn unbound_tree_and_frame_is_a_backend_error_never_a_pass() {
    let yaml = r#"
version: 1
name: unbound
app: fixtures/build/apps/hello-views.apk
isolation: process
display: {mode: offscreen, width: 1080, height: 2400, density_dpi: 420}
expectation: non_pass
steps:
  - launch: {activity: dev.acos.fixtures.helloviews/.MainActivity}
  - assert_text: {selector: {id: result}, text: {exact: "clicks: 0"}}
"#;
    let outcome = run_slice(yaml, SliceMode::UnboundTree, b"initial".to_vec(), b"after".to_vec());
    assert!(!outcome.verdict.verdict.is_pass());
    let failure = outcome.verdict.failure.as_ref().unwrap();
    assert_eq!(failure.error.code, acos_protocol::ErrorCode::Internal);
}

#[test]
fn pure_assertion_drifting_frame_is_not_bound_to_the_completed_frame() {
    let yaml = r#"
version: 1
name: drifting
app: fixtures/build/apps/hello-views.apk
isolation: process
display: {mode: offscreen, width: 1080, height: 2400, density_dpi: 420}
expectation: non_pass
steps:
  - launch: {activity: dev.acos.fixtures.helloviews/.MainActivity}
  - wait_for: {selector: {id: result}, timeout_ms: 1000}
  - assert_text: {selector: {id: result}, text: {exact: "clicks: 0"}}
"#;
    let outcome =
        run_slice(yaml, SliceMode::DriftingObserve, b"initial".to_vec(), b"after".to_vec());
    assert!(!outcome.verdict.verdict.is_pass());
    let failure = outcome.verdict.failure.as_ref().unwrap();
    assert_eq!(failure.error.code, acos_protocol::ErrorCode::AssertionFailed);
    assert!(failure.error.message.contains("frame"));
}

#[test]
fn placeholder_frame_never_satisfies_an_assertion() {
    let yaml = r#"
version: 1
name: placeholder
app: fixtures/build/apps/hello-views.apk
isolation: process
display: {mode: offscreen, width: 1080, height: 2400, density_dpi: 420}
expectation: non_pass
steps:
  - launch: {activity: dev.acos.fixtures.helloviews/.MainActivity}
  - assert_text: {selector: {id: result}, text: {exact: "clicks: 0"}}
"#;
    let outcome = run_slice(yaml, SliceMode::Placeholder, b"initial".to_vec(), b"after".to_vec());
    assert!(!outcome.verdict.verdict.is_pass());
    let failure = outcome.verdict.failure.as_ref().unwrap();
    assert_eq!(failure.error.code, acos_protocol::ErrorCode::NoFreshFrame);
}

fn screenshot_scenario(baseline: &Path) -> String {
    format!(
        r#"
version: 1
name: screenshot-bound
app: fixtures/build/apps/hello-views.apk
isolation: process
display: {{mode: offscreen, width: 1080, height: 2400, density_dpi: 420}}
expectation: pass
steps:
  - launch: {{activity: dev.acos.fixtures.helloviews/.MainActivity}}
  - assert_screenshot:
      baseline: "{}"
      tolerance: 0.0
"#,
        baseline.display()
    )
}

fn read_reference_png(name: &str) -> Vec<u8> {
    std::fs::read(reference_dir("hello-views").join(name)).expect("reference png")
}

#[test]
fn screenshot_assertion_uses_the_same_completed_frame() {
    let baseline = reference_dir("hello-views").join("screenshot_initial.png");
    let bytes = read_reference_png("screenshot_initial.png");
    let outcome = run_slice(
        &screenshot_scenario(&baseline),
        SliceMode::Good,
        bytes.clone(),
        bytes,
    );
    assert_eq!(outcome.verdict.verdict, Status::Pass, "{:?}", outcome.verdict.failure);
    assert!(
        outcome.artifact_validation.ok,
        "a claimed pass must pass artifact validation: {:?}",
        outcome.artifact_validation.issues
    );
    assert_eq!(outcome.verdict.steps.last().unwrap().step, "assert_screenshot");
    assert_eq!(outcome.verdict.steps.last().unwrap().status, Status::Pass);
}

#[test]
fn screenshot_assertion_fails_beyond_tolerance() {
    let baseline = reference_dir("hello-views").join("screenshot_initial.png");
    let actual = read_reference_png("screenshot_after_click1.png");
    let outcome = run_slice(
        &screenshot_scenario(&baseline),
        SliceMode::Good,
        actual.clone(),
        actual,
    );
    assert_eq!(outcome.verdict.verdict, Status::Fail);
    assert_eq!(outcome.verdict.steps.last().unwrap().step, "assert_screenshot");
    assert_eq!(outcome.verdict.steps.last().unwrap().status, Status::Fail);
}


// ---------------------------------------------------------------------------
// wait_for must poll a fresh tree until the condition holds or the deadline
// passes. These two tests are the teeth: a never-true condition fails with a
// structured TIMEOUT (and really consumes its timeout), while a condition that
// becomes true on a later traversal returns early instead of waiting forever.
// ---------------------------------------------------------------------------

struct WaitBackend {
    manifest: Option<ManifestInfo>,
    polls: u64,
    match_after: u64,
    /// Polls `1..=unsettled_through` fail with P12's strict-settle TIMEOUT.
    unsettled_through: u64,
}

impl WaitBackend {
    fn new(match_after: u64) -> Self {
        WaitBackend {
            manifest: None,
            polls: 0,
            match_after,
            unsettled_through: 0,
        }
    }

    fn observation(&self) -> SessionObservation {
        let matched = self.match_after > 0 && self.polls >= self.match_after;
        let id = if matched { "result" } else { "placeholder" };
        let text = if matched { "clicks: 0" } else { "waiting" };
        let frame = FrameMeta::new(
            1 + self.polls,
            "2026-11-11T00:00:00.000Z",
            format!("wait:{}", self.polls),
            1080,
            2400,
            420,
            format!("wait-hash-{}-{matched}", self.polls),
            true,
        );
        let node = UiNode::new(
            "n1",
            "android.widget.TextView",
            Bounds::new(42, 612, 175, 663),
        )
        .with_resource_id(format!("dev.acos.fixtures.helloviews:id/{id}"))
        .with_text(text);
        let tree = UiTree::new(
            frame.clone(),
            "dev.acos.fixtures.helloviews",
            vec![node],
        );
        SessionObservation {
            session_id: "s_wait".to_string(),
            package: "dev.acos.fixtures.helloviews".to_string(),
            frame,
            tree,
            screenshot_png: Some(b"waiting".to_vec()),
            crashed: None,
            input: None,
            settle: None,
        }
    }
}

impl Backend for WaitBackend {
    fn id(&self) -> &'static str {
        "wait-mock"
    }

    fn protocol_backend(&self) -> ProtocolBackend {
        ProtocolBackend::AcosNative
    }

    fn fidelity(&self) -> Fidelity {
        Fidelity::NativeRuntime
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            static_manifest: true,
            launch: true,
            ui_tree: true,
            ..Capabilities::default()
        }
    }

    fn manifest(&mut self, apk: &Path) -> Result<ManifestInfo, BackendError> {
        let info = acos_verify::manifest::load_manifest(apk)?;
        self.manifest = Some(info.clone());
        Ok(info)
    }

    fn launch(&mut self, _request: &LaunchRequest) -> Result<LaunchOutcome, BackendError> {
        let observation = self.observation();
        Ok(LaunchOutcome {
            session_id: "s_wait".to_string(),
            package: "dev.acos.fixtures.helloviews".to_string(),
            frame: Some(observation.frame.clone()),
            screenshot_png: observation.screenshot_png.clone(),
            crashed: None,
            detail: serde_json::json!({"backend": "wait-mock"}),
        })
    }

    fn observe(&mut self) -> Result<SessionObservation, BackendError> {
        Ok(self.observation())
    }

    fn refresh(&mut self) -> Result<SessionObservation, BackendError> {
        self.polls += 1;
        if self.polls <= self.unsettled_through {
            // The shape of `settle_timeout_error` (acos-cli engine): TIMEOUT
            // with the guest's settle record in the details.
            return Err(BackendError::Protocol {
                code: acos_protocol::ErrorCode::Timeout,
                detail: "wait_for: strict settle timed out (newer-store)".to_string(),
                hint: None,
                details: Some(serde_json::json!({
                    "settle": {"settled": false, "bound": "timeout", "reason": "newer-store"},
                    "context": "wait_for",
                })),
            });
        }
        Ok(self.observation())
    }
}

fn run_wait_for(match_after: u64, timeout_ms: u64) -> (RunOutcome, u64) {
    run_wait_for_unsettled(0, match_after, timeout_ms)
}

fn run_wait_for_unsettled(
    unsettled_through: u64,
    match_after: u64,
    timeout_ms: u64,
) -> (RunOutcome, u64) {
    let yaml = format!(
        r#"
version: 1
name: wait-for-teeth
app: fixtures/build/apps/hello-views.apk
isolation: process
display: {{mode: offscreen, width: 1080, height: 2400, density_dpi: 420}}
expectation: pass
steps:
  - launch: {{activity: dev.acos.fixtures.helloviews/.MainActivity}}
  - wait_for: {{selector: {{id: result}}, timeout_ms: {timeout_ms}}}
"#
    );
    let scenario = Scenario::from_yaml_str(&yaml).expect("valid scenario");
    let artifacts = scratch("wait-for-teeth");
    let mut backend = WaitBackend::new(match_after);
    backend.unsettled_through = unsettled_through;
    let options = RunOptions {
        apk: workspace().join("fixtures/build/apps/hello-views.apk"),
        scenario_path: workspace().join("scenarios/hello-views.yaml"),
        artifacts_dir: artifacts.clone(),
        required_fidelity: None,
        request_id: "req_wait_for_teeth".to_string(),
        flags: Vec::new(),
        acos_version: "0.1.0".to_string(),
        engine: "rust-vm".to_string(),
        warnings: Vec::new(),
    };
    let clock = FixedClock::new(1_794_318_674_000);
    let outcome = run_scenario(&scenario, &options, &mut backend, &clock).expect("run");
    let polls = backend.polls;
    let _ = std::fs::remove_dir_all(&artifacts);
    (outcome, polls)
}

#[test]
fn wait_for_condition_that_never_appears_is_a_structured_timeout() {
    let started = std::time::Instant::now();
    let (outcome, polls) = run_wait_for(0, 120);
    let elapsed = started.elapsed();
    assert_eq!(outcome.verdict.verdict, Status::Timeout);
    let failure = outcome.verdict.failure.as_ref().expect("timeout failure");
    assert_eq!(failure.error.code, acos_protocol::ErrorCode::Timeout);
    let details = failure.error.details.as_ref().expect("timeout details");
    assert_eq!(details["timeout_ms"], 120);
    assert!(
        details["elapsed_ms"].as_u64().unwrap_or(0) >= 120,
        "the wait must consume its timeout: {details}"
    );
    assert!(polls >= 2, "the live tree must be polled repeatedly, got {polls}");
    assert!(
        elapsed >= std::time::Duration::from_millis(120),
        "wait_for returned in {:?}, before its timeout",
        elapsed
    );
    assert!(
        !outcome.verdict.verdict.is_pass(),
        "a timeout must never be reported as a pass"
    );
}

#[test]
fn wait_for_returns_as_soon_as_the_condition_appears() {
    let started = std::time::Instant::now();
    let (outcome, polls) = run_wait_for(2, 4000);
    let elapsed = started.elapsed();
    assert_eq!(
        outcome.verdict.verdict,
        Status::Pass,
        "condition appeared on poll {polls}: {:?}",
        outcome.verdict.failure
    );
    assert_eq!(polls, 2, "must stop at the first satisfying traversal");
    assert!(
        elapsed < std::time::Duration::from_secs(2),
        "wait_for did not return early: {:?}",
        elapsed
    );
}

/// P12 integration bug (found on Gaten, whose RN splash keeps the screen
/// changing): one poll's strict-settle timeout must not end a `wait_for` that
/// still has time left. The unsettled polls are "not yet"; the condition is
/// met only on a later, settled traversal.
#[test]
fn wait_for_keeps_polling_through_unsettled_polls() {
    let (outcome, polls) = run_wait_for_unsettled(2, 3, 4000);
    assert_eq!(
        outcome.verdict.verdict,
        Status::Pass,
        "two unsettled polls, match on poll 3: {:?}",
        outcome.verdict.failure
    );
    // REL3A §2: the match appears on poll 3, but a content-stable verdict needs
    // a second content-settled observation at least one window later, so the
    // wait confirms it on a later poll instead of passing on the first sample.
    assert!(
        polls >= 4,
        "the content verdict must be confirmed on a later settled poll, got {polls}"
    );
}

/// ...and a screen that never settles is still a TIMEOUT at the wait_for
/// deadline, carrying the settle evidence. Never a pass.
#[test]
fn wait_for_on_a_never_settling_screen_is_a_settle_timeout() {
    let started = std::time::Instant::now();
    let (outcome, polls) = run_wait_for_unsettled(u64::MAX, 1, 150);
    assert_eq!(outcome.verdict.verdict, Status::Timeout);
    assert!(!outcome.verdict.verdict.is_pass());
    assert!(polls >= 2, "must keep polling until the deadline, got {polls}");
    assert!(started.elapsed() >= std::time::Duration::from_millis(150));
    let failure = outcome.verdict.failure.as_ref().expect("timeout failure");
    assert_eq!(failure.error.code, acos_protocol::ErrorCode::Timeout);
    let details = failure.error.details.as_ref().expect("settle evidence");
    assert!(details.get("settle").is_some(), "{details}");
}

// ---------------------------------------------------------------------------
// W22: `assert_log.timeout_ms` is a real wait, and a refused log channel is a
// structured failure (never "no line matched", never a pass).
// ---------------------------------------------------------------------------

struct LogBackend {
    manifest: Option<ManifestInfo>,
    /// The `logs_since` call number at which the line appears.
    emit_at: u64,
    polls: u64,
    /// When set, every `logs_since` is refused with this code (the shape of
    /// `TrustGuard`'s untrusted-run refusal).
    refuse: Option<acos_protocol::ErrorCode>,
}

impl LogBackend {
    fn new(emit_at: u64) -> Self {
        LogBackend {
            manifest: None,
            emit_at,
            polls: 0,
            refuse: None,
        }
    }
}

impl Backend for LogBackend {
    fn id(&self) -> &'static str {
        "log-poll-mock"
    }

    fn protocol_backend(&self) -> ProtocolBackend {
        ProtocolBackend::AcosNative
    }

    fn fidelity(&self) -> Fidelity {
        Fidelity::NativeRuntime
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            static_manifest: true,
            launch: true,
            logs: true,
            ..Capabilities::default()
        }
    }

    fn manifest(&mut self, apk: &Path) -> Result<ManifestInfo, BackendError> {
        let info = acos_verify::manifest::load_manifest(apk)?;
        self.manifest = Some(info.clone());
        Ok(info)
    }

    fn launch(&mut self, _request: &LaunchRequest) -> Result<LaunchOutcome, BackendError> {
        Ok(LaunchOutcome {
            session_id: "s_log".to_string(),
            package: self
                .manifest
                .as_ref()
                .map(|m| m.package.clone())
                .unwrap_or_default(),
            frame: None,
            screenshot_png: None,
            crashed: None,
            detail: serde_json::json!({}),
        })
    }

    fn logs_since(&mut self, since: u64) -> Result<Vec<LogRecord>, BackendError> {
        self.polls += 1;
        if let Some(code) = self.refuse {
            return Err(BackendError::Failed {
                code,
                detail: "UNTRUSTED_LOG_ASSERTION: logs are guest-asserted".to_string(),
            });
        }
        if self.polls < self.emit_at || since >= 1 {
            return Ok(Vec::new());
        }
        Ok(vec![LogRecord {
            seq: 1,
            timestamp_ms: 1_794_318_674_000,
            level: LogLevel::Info,
            tag: "AcosFixture".to_string(),
            message: "onResume".to_string(),
        }])
    }
}

fn log_scenario(timeout_ms: u64) -> (Scenario, PathBuf) {
    let yaml = format!(
        r#"
version: 1
name: log-poll
app: fixtures/build/apps/hello-views.apk
isolation: process
display: {{mode: offscreen, width: 1080, height: 2400, density_dpi: 420}}
expectation: pass
steps:
  - launch: {{activity: dev.acos.fixtures.helloviews/.MainActivity}}
  - assert_log:
      tag: AcosFixture
      patterns: ['^onResume$']
      timeout_ms: {timeout_ms}
"#
    );
    let scenario = Scenario::from_yaml_str(&yaml).expect("valid scenario");
    (scenario, scratch("log-poll"))
}

fn run_log_scenario(
    scenario: &Scenario,
    artifacts: &Path,
    backend: &mut dyn Backend,
) -> RunOutcome {
    let options = RunOptions {
        apk: workspace().join("fixtures/build/apps/hello-views.apk"),
        scenario_path: workspace().join("scenarios/v2-art-counter.yaml"),
        artifacts_dir: artifacts.to_path_buf(),
        required_fidelity: None,
        request_id: "req_log_poll".to_string(),
        flags: Vec::new(),
        acos_version: "0.1.0".to_string(),
        engine: "rust-vm".to_string(),
        warnings: Vec::new(),
    };
    let clock = FixedClock::new(1_794_318_674_000);
    run_scenario(scenario, &options, backend, &clock).expect("run_scenario succeeds")
}

#[test]
fn assert_log_polls_until_the_line_appears_or_the_deadline_passes() {
    // The line appears on the third fetch: the first is the normal per-step
    // flush, the second the first wait poll. A single-shot implementation would
    // fail this.
    let (scenario, artifacts) = log_scenario(2000);
    let mut backend = LogBackend::new(3);
    let outcome = run_log_scenario(&scenario, &artifacts, &mut backend);
    assert_eq!(outcome.verdict.verdict, Status::Pass, "{:?}", outcome.verdict.failure);
    assert_eq!(outcome.verdict.steps[1].step, "assert_log");
    assert_eq!(outcome.verdict.steps[1].status, Status::Pass);
    assert!(
        backend.polls >= 3,
        "assert_log must poll the log stream, got {} fetches",
        backend.polls
    );
    let _ = std::fs::remove_dir_all(&artifacts);

    // Negative: a line that never appears is an assertion failure at the
    // deadline, never a pass and never a hang.
    let (scenario, artifacts) = log_scenario(50);
    let mut backend = LogBackend::new(u64::MAX);
    let outcome = run_log_scenario(&scenario, &artifacts, &mut backend);
    assert_eq!(outcome.verdict.verdict, Status::Fail);
    assert!(!outcome.verdict.verdict.is_pass());
    assert_eq!(
        outcome
            .verdict
            .failure
            .as_ref()
            .map(|failure| failure.code),
        Some(acos_protocol::ErrorCode::AssertionFailed)
    );
    assert!(backend.polls >= 2, "the wait must poll at least twice");
    let _ = std::fs::remove_dir_all(&artifacts);
}

#[test]
fn a_refused_log_channel_fails_closed_with_the_backend_code() {
    // This is the untrusted-run shape: the backend refuses guest-asserted logs.
    // The step must carry INSUFFICIENT_HOST_EVIDENCE, not a generic "no line".
    let (scenario, artifacts) = log_scenario(1);
    let mut backend = LogBackend::new(1);
    backend.refuse = Some(acos_protocol::ErrorCode::InsufficientHostEvidence);
    let outcome = run_log_scenario(&scenario, &artifacts, &mut backend);
    assert_eq!(outcome.verdict.verdict, Status::Fail);
    let failure = outcome.verdict.failure.as_ref().expect("a structured failure");
    assert_eq!(
        failure.code,
        acos_protocol::ErrorCode::InsufficientHostEvidence
    );
    assert!(failure.error.message.contains("UNTRUSTED_LOG_ASSERTION"));
    let _ = std::fs::remove_dir_all(&artifacts);
}

const FAST_TAP_ASSERT: &str = r#"
version: 1
name: fast-tap-assert
app: fixtures/build/apps/hello-views.apk
isolation: process
display: {mode: offscreen, width: 1080, height: 2400, density_dpi: 420}
expectation: pass
steps:
  - launch: {activity: dev.acos.fixtures.helloviews/.MainActivity}
  - tap: {selector: {id: btn}, settle: fast}
  - assert_text: {selector: {id: result}, text: {exact: "clicks: 1"}}
"#;

#[test]
fn assertion_on_a_fast_input_frame_fails_closed() {
    // GR3/G3a: `settle: fast` is navigation only. An assertion bound to that
    // frame must fail, even when the frame would satisfy it.
    let outcome =
        run_slice(FAST_TAP_ASSERT, SliceMode::Good, b"initial".to_vec(), b"after".to_vec());
    assert_eq!(outcome.verdict.verdict, Status::Fail, "{:?}", outcome.verdict.failure);
    let failure = outcome.verdict.failure.as_ref().unwrap();
    assert_eq!(failure.error.code, acos_protocol::ErrorCode::AssertionFailed);
    assert!(failure.error.message.contains("settle: fast"), "{}", failure.error.message);
}

#[test]
fn a_strict_wait_for_after_a_fast_input_rebinds_the_frame() {
    let yaml = FAST_TAP_ASSERT.replace(
        "  - assert_text:",
        "  - wait_for: {selector: {id: result}, timeout_ms: 1000}\n  - assert_text:",
    );
    let outcome = run_slice(&yaml, SliceMode::Good, b"initial".to_vec(), b"after".to_vec());
    assert_eq!(outcome.verdict.verdict, Status::Pass, "{:?}", outcome.verdict.failure);
}

// ---------------------------------------------------------------------------
// SE1: the opt-in per-step `expect_change` rule (ADR-0026 decision 2, GR4 G4c).
//
// P12 keeps the strict rule: an action that drew but changed no pixel settles
// and passes, and its reply reports `pixels_changed:false`. `expect_change: true`
// is the per-step opt-in that turns that measured fact into a failure.
// ---------------------------------------------------------------------------

const EXPECT_CHANGE_TAP: &str = r#"
version: 1
name: expect-change-tap
app: fixtures/build/apps/hello-views.apk
isolation: process
display: {mode: offscreen, width: 1080, height: 2400, density_dpi: 420}
expectation: pass
steps:
  - launch: {activity: dev.acos.fixtures.helloviews/.MainActivity}
  - assert_text: {selector: {id: result}, text: {exact: "clicks: 0"}}
  - tap: {selector: {id: btn}, expect_change: true}
  - assert_text: {selector: {id: result}, text: {exact: "clicks: 1"}}
"#;

/// A changed settled frame satisfies `expect_change`, and the step still carries
/// the P12 pixel fact as evidence.
#[test]
fn expect_change_passes_when_the_settled_frame_changed() {
    let outcome = run_slice_with_settle(
        EXPECT_CHANGE_TAP,
        SliceMode::Good,
        b"initial".to_vec(),
        b"after".to_vec(),
        Some(true),
    );
    assert_eq!(
        outcome.verdict.verdict,
        Status::Pass,
        "{:?}",
        outcome.verdict.failure
    );
    let tap = tap_step(&outcome);
    assert_eq!(tap.status, Status::Pass);
    let settle = tap.settle.as_ref().expect("settle evidence");
    assert_eq!(settle["pixels_changed"], serde_json::json!(true));
}

/// The G4c case: the action was delivered, the frame settled, and nothing
/// changed. `expect_change` fails the step with the measured fact and both frame
/// refs, and the verdict names the step.
#[test]
fn expect_change_fails_when_the_settled_frame_is_pixel_identical() {
    let outcome = run_slice_with_settle(
        EXPECT_CHANGE_TAP,
        SliceMode::Good,
        b"initial".to_vec(),
        b"after".to_vec(),
        Some(false),
    );
    assert_eq!(outcome.verdict.verdict, Status::Fail);
    let tap = tap_step(&outcome);
    assert_eq!(tap.status, Status::Fail);
    assert!(
        tap.assertions
            .iter()
            .any(|a| a.kind == "expect_change" && !a.passed),
        "the failed step must name the rule: {:?}",
        tap.assertions
    );
    let failure = outcome.verdict.failure.as_ref().expect("failure");
    assert_eq!(failure.step_index, Some(2));
    assert_eq!(
        failure.error.code,
        acos_protocol::ErrorCode::AssertionFailed
    );
    let details = failure.error.details.as_ref().expect("details");
    assert_eq!(details["reason"], serde_json::json!("expect_change"));
    assert_eq!(details["pixels_changed"], serde_json::json!(false));
    assert_eq!(details["step_kind"], serde_json::json!("tap"));
    // Before/after frame refs: launch frame 1, the action's frame 2.
    assert_eq!(details["before"]["frame_seq"], serde_json::json!(1));
    assert_eq!(details["after"]["frame_seq"], serde_json::json!(2));
    assert_eq!(
        details["after"]["frame"],
        serde_json::json!("frames/2.png")
    );
    // The failing step is still evidence: its frame is a claimed artifact.
    assert_eq!(tap.frame_seq, Some(2));
}

/// Without the flag, the P12 rule is untouched: a delivered action with no
/// pixel change settles and passes, and the step states `pixels_changed:false`.
#[test]
fn a_no_change_tap_without_the_flag_keeps_the_p12_rule() {
    let yaml = EXPECT_CHANGE_TAP.replace(", expect_change: true", "");
    let outcome = run_slice_with_settle(
        &yaml,
        SliceMode::Good,
        b"initial".to_vec(),
        b"after".to_vec(),
        Some(false),
    );
    assert_eq!(
        outcome.verdict.verdict,
        Status::Pass,
        "{:?}",
        outcome.verdict.failure
    );
    let tap = tap_step(&outcome);
    assert_eq!(tap.status, Status::Pass);
    assert_eq!(
        tap.settle.as_ref().expect("settle evidence")["pixels_changed"],
        serde_json::json!(false)
    );
}

/// A backend that cannot measure the pixels must not be able to satisfy
/// `expect_change`: the step is `unsupported` (never a pass, never a fake fail).
#[test]
fn expect_change_without_a_settle_record_is_unsupported() {
    let outcome = run_slice_with_settle(
        EXPECT_CHANGE_TAP,
        SliceMode::Good,
        b"initial".to_vec(),
        b"after".to_vec(),
        None,
    );
    assert!(!outcome.verdict.verdict.is_pass());
    assert_eq!(outcome.verdict.verdict, Status::Unsupported);
    let failure = outcome.verdict.failure.as_ref().expect("failure");
    assert_eq!(
        failure.error.code,
        acos_protocol::ErrorCode::UnsupportedAssertion
    );
    let details = failure.error.details.as_ref().expect("details");
    assert_eq!(details["reason"], serde_json::json!("expect_change"));
    assert_eq!(details["pixels_changed"], serde_json::Value::Null);
}

// ---------------------------------------------------------------------------
// PROVENANCE: a verdict names the scenario and the APK it judged.
// ---------------------------------------------------------------------------

/// The feedback entry this covers: a mistyped scenario path ran an older
/// scenario against a stale APK and returned `pass`; nothing in the verdict or
/// the reply named either input (only `repro.json` did).
#[test]
fn the_verdict_names_both_inputs_absolutely_with_their_digests() {
    let dir = scratch("provenance");
    std::fs::create_dir_all(&dir).unwrap();
    // A real APK: the verifier parses the archive for the manifest identity.
    let declared = workspace().join("fixtures/build/apps/hello-views.apk");
    let scenario_file = dir.join("s.yaml");
    std::fs::write(
        &scenario_file,
        format!(
            r#"version: 1
name: provenance
app: {}
engine: art
isolation: process
display: {{mode: offscreen, width: 1080, height: 2400, density_dpi: 420}}
steps:
  - launch: {{activity: dev.acos.fixtures.helloviews/.MainActivity}}
"#,
            declared.display()
        ),
    )
    .unwrap();

    // The scenario's own `app:` resolves from the scenario file, so the run has
    // one APK and no override warning.
    let request = VerifyRequest {
        base: acos_protocol::RequestBase::new("req_provenance"),
        apk: None,
        scenario: scenario_file.display().to_string(),
        artifacts: dir.join("artifacts").display().to_string(),
        overwrite: false,
        reference: Some(reference_dir("hello-views").display().to_string()),
        required_fidelity: None,
        engine: acos_protocol::VerifyEngine::Art,
        flags: Vec::new(),
        animations: None,
        native: false,
    };
    let mut backend = ReferenceBackend::new(reference_dir("hello-views"));
    let clock = FixedClock::new(1_794_318_674_000);
    let envelope = verify_with_clock(&request, &mut backend, &clock);
    let result = envelope
        .result()
        .unwrap_or_else(|| panic!("a result: {:?}", envelope.error()));
    let provenance = &result["verdict"]["provenance"];
    let expected_scenario = std::fs::canonicalize(&scenario_file).unwrap_or(scenario_file.clone());
    let expected_apk = std::fs::canonicalize(&declared).unwrap_or(declared.clone());
    assert_eq!(
        provenance["scenario"]["path"],
        serde_json::json!(expected_scenario.display().to_string()),
        "{provenance}"
    );
    assert_eq!(
        provenance["apk"]["path"],
        serde_json::json!(expected_apk.display().to_string()),
        "{provenance}"
    );
    for file in ["scenario", "apk"] {
        let sha = provenance[file]["sha256"].as_str().unwrap_or_default();
        assert_eq!(sha.len(), 64, "{file}: {provenance}");
        assert_eq!(
            provenance[file]["sha256_12"],
            serde_json::json!(acos_protocol::provenance::short_sha(sha)),
            "{file}"
        );
        assert_eq!(provenance[file]["sha256_12"].as_str().unwrap().len(), 12);
    }
    assert_eq!(provenance["engine"], "art");
    assert_eq!(provenance["scenario_engine"], "art");
    assert_eq!(provenance["fidelity"], "art-reference");
    assert_eq!(provenance["warnings"], serde_json::json!([]));
    // The reproducer stores absolute paths too: a replay must name the same
    // bytes, not a path relative to whatever cwd it runs in.
    let reproducer = &result["verdict"]["reproducer"];
    assert_eq!(
        reproducer["scenario_path"],
        serde_json::json!(expected_scenario.display().to_string()),
        "{reproducer}"
    );
    assert!(std::path::Path::new(reproducer["apk_path"].as_str().unwrap()).is_absolute());
    let _ = std::fs::remove_dir_all(&dir);
}

/// `--apk` overriding a different scenario `app:` is allowed and **warned** —
/// never silent, and never a failure: the run really is about the `--apk` file.
#[test]
fn an_apk_that_overrides_the_scenario_app_is_a_structured_warning() {
    let dir = scratch("provenance-override");
    std::fs::create_dir_all(&dir).unwrap();
    let declared = workspace().join("fixtures/build/apps/counter.apk");
    let used = workspace().join("fixtures/build/apps/hello-views.apk");
    let scenario_file = dir.join("s.yaml");
    std::fs::write(
        &scenario_file,
        format!(
            r#"version: 1
name: provenance-override
app: {}
isolation: process
display: {{mode: offscreen, width: 1080, height: 2400, density_dpi: 420}}
steps:
  - launch: {{activity: dev.acos.fixtures.helloviews/.MainActivity}}
"#,
            declared.display()
        ),
    )
    .unwrap();

    let request = VerifyRequest {
        base: acos_protocol::RequestBase::new("req_provenance_override"),
        apk: Some(used.display().to_string()),
        scenario: scenario_file.display().to_string(),
        artifacts: dir.join("artifacts").display().to_string(),
        overwrite: false,
        reference: Some(reference_dir("hello-views").display().to_string()),
        required_fidelity: None,
        engine: acos_protocol::VerifyEngine::RustVm,
        flags: Vec::new(),
        animations: None,
        native: false,
    };
    let mut backend = ReferenceBackend::new(reference_dir("hello-views"));
    let clock = FixedClock::new(1_794_318_674_000);
    let envelope = verify_with_clock(&request, &mut backend, &clock);
    let result = envelope
        .result()
        .unwrap_or_else(|| panic!("a result: {:?}", envelope.error()));
    let provenance = &result["verdict"]["provenance"];
    let expected_declared = std::fs::canonicalize(&declared).unwrap_or(declared.clone());
    let expected_used = std::fs::canonicalize(&used).unwrap_or(used.clone());
    assert_eq!(
        provenance["apk"]["path"],
        serde_json::json!(expected_used.display().to_string()),
        "{provenance}"
    );
    let warnings = provenance["warnings"].as_array().expect("warnings");
    assert_eq!(warnings.len(), 1, "{provenance}");
    assert_eq!(
        warnings[0]["code"],
        acos_protocol::provenance::WARN_SCENARIO_APP_OVERRIDDEN
    );
    assert_eq!(
        warnings[0]["scenario_app"],
        serde_json::json!(expected_declared.display().to_string())
    );
    assert_eq!(
        warnings[0]["apk"],
        serde_json::json!(expected_used.display().to_string())
    );
    // A warning, not a fail: the verdict is what the steps decided.
    assert!(
        envelope.status() != Status::Pass || envelope.ok(),
        "warnings never turn a pass into a failure"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A scenario whose `engine:` names one engine while the request runs another is
/// **warned**, never silently accepted and never a status change.
///
/// The scenario field records the engine the scenario was written for; the
/// request's `engine` (`--engine` / MCP `engine`) selects the engine that runs.
/// A reader of the verdict must be able to see the disagreement.
#[test]
fn a_scenario_engine_that_differs_from_the_run_is_a_structured_warning() {
    let dir = scratch("provenance-engine-mismatch");
    std::fs::create_dir_all(&dir).unwrap();
    let declared = workspace().join("fixtures/build/apps/hello-views.apk");
    let scenario_file = dir.join("s.yaml");
    std::fs::write(
        &scenario_file,
        format!(
            r#"version: 1
name: engine-mismatch
# the scenario was written for the frozen VM:
engine: rust-vm
app: {}
isolation: process
display: {{mode: offscreen, width: 1080, height: 2400, density_dpi: 420}}
steps:
  - launch: {{activity: dev.acos.fixtures.helloviews/.MainActivity}}
"#,
            declared.display()
        ),
    )
    .unwrap();

    let request = VerifyRequest {
        base: acos_protocol::RequestBase::new("req_engine_mismatch"),
        apk: None,
        scenario: scenario_file.display().to_string(),
        artifacts: dir.join("artifacts").display().to_string(),
        overwrite: false,
        reference: Some(reference_dir("hello-views").display().to_string()),
        required_fidelity: None,
        // The engine that runs; the scenario does not select it.
        engine: acos_protocol::VerifyEngine::Art,
        flags: Vec::new(),
        animations: None,
        native: false,
    };
    let mut backend = ReferenceBackend::new(reference_dir("hello-views"));
    let clock = FixedClock::new(1_794_318_674_000);
    let envelope = verify_with_clock(&request, &mut backend, &clock);
    let result = envelope
        .result()
        .unwrap_or_else(|| panic!("a result: {:?}", envelope.error()));
    let provenance = &result["verdict"]["provenance"];
    let warnings = provenance["warnings"].as_array().expect("warnings");
    let mismatch = warnings
        .iter()
        .find(|warning| warning["code"] == acos_protocol::provenance::WARN_SCENARIO_ENGINE_MISMATCH)
        .unwrap_or_else(|| panic!("SCENARIO_ENGINE_MISMATCH missing: {provenance}"));
    assert_eq!(mismatch["scenario_engine"], "rust-vm", "{mismatch}");
    assert_eq!(mismatch["engine"], "art", "{mismatch}");
    assert!(
        mismatch["message"]
            .as_str()
            .unwrap_or("")
            .contains("this run used `art`"),
        "{mismatch}"
    );
    // The engine that ran is the one the request asked for.
    assert_eq!(provenance["engine"], "art");
    assert_eq!(provenance["scenario_engine"], "rust-vm");
    // …and the summary line a text-only reader sees names the warning.
    let line = acos_protocol::provenance::summary_line(provenance);
    assert!(
        line.contains(acos_protocol::provenance::WARN_SCENARIO_ENGINE_MISMATCH),
        "{line}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A scenario that does **not** declare `engine:` gets no mismatch warning, even
/// when the request runs an engine other than the scenario default: nothing was
/// declared, so nothing disagrees.
#[test]
fn an_omitted_engine_is_not_a_mismatch() {
    let dir = scratch("provenance-engine-omitted");
    std::fs::create_dir_all(&dir).unwrap();
    let declared = workspace().join("fixtures/build/apps/hello-views.apk");
    let scenario_file = dir.join("s.yaml");
    std::fs::write(
        &scenario_file,
        format!(
            r#"version: 1
name: engine-omitted
app: {}
isolation: process
display: {{mode: offscreen, width: 1080, height: 2400, density_dpi: 420}}
steps:
  - launch: {{activity: dev.acos.fixtures.helloviews/.MainActivity}}
"#,
            declared.display()
        ),
    )
    .unwrap();
    let request = VerifyRequest {
        base: acos_protocol::RequestBase::new("req_engine_omitted"),
        apk: None,
        scenario: scenario_file.display().to_string(),
        artifacts: dir.join("artifacts").display().to_string(),
        overwrite: false,
        reference: Some(reference_dir("hello-views").display().to_string()),
        required_fidelity: None,
        engine: acos_protocol::VerifyEngine::Art,
        flags: Vec::new(),
        animations: None,
        native: false,
    };
    let mut backend = ReferenceBackend::new(reference_dir("hello-views"));
    let clock = FixedClock::new(1_794_318_674_000);
    let envelope = verify_with_clock(&request, &mut backend, &clock);
    let result = envelope.result().expect("a result");
    let warnings = result["verdict"]["provenance"]["warnings"]
        .as_array()
        .expect("warnings");
    assert!(
        warnings.iter().all(|warning| {
            warning["code"] != acos_protocol::provenance::WARN_SCENARIO_ENGINE_MISMATCH
        }),
        "{warnings:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A backend that rewrites one input file during the run, then answers exactly
/// like the reference backend.
///
/// It is how the `INPUT_CHANGED_DURING_RUN` wiring is tested for real: the file
/// the run hashed must be the file it read, and if something else rewrote it
/// mid-run the verdict has to say so.
struct RewritingApk {
    inner: ReferenceBackend,
    apk: PathBuf,
    rewrite: Option<Vec<u8>>,
}

impl Backend for RewritingApk {
    fn id(&self) -> &'static str {
        self.inner.id()
    }
    fn protocol_backend(&self) -> ProtocolBackend {
        self.inner.protocol_backend()
    }
    fn fidelity(&self) -> Fidelity {
        self.inner.fidelity()
    }
    fn capabilities(&self) -> Capabilities {
        self.inner.capabilities()
    }
    fn manifest(&mut self, apk: &Path) -> Result<ManifestInfo, BackendError> {
        let manifest = self.inner.manifest(apk)?;
        if let Some(bytes) = self.rewrite.take() {
            // Same size, different bytes, newer mtime: the run is now judging a
            // file that no longer matches its own provenance digest.
            std::thread::sleep(std::time::Duration::from_millis(10));
            std::fs::write(&self.apk, bytes).expect("rewrite apk");
        }
        Ok(manifest)
    }
    fn launch(&mut self, request: &LaunchRequest) -> Result<LaunchOutcome, BackendError> {
        self.inner.launch(request)
    }
    fn observe(&mut self) -> Result<SessionObservation, BackendError> {
        self.inner.observe()
    }
    fn refresh(&mut self) -> Result<SessionObservation, BackendError> {
        self.inner.refresh()
    }
    fn logs_since(&mut self, since: u64) -> Result<Vec<LogRecord>, BackendError> {
        self.inner.logs_since(since)
    }
    fn input(&mut self, action: &InputAction) -> Result<SessionObservation, BackendError> {
        self.inner.input(action)
    }
}

#[test]
fn an_input_rewritten_during_the_run_is_a_warning_in_the_verdict() {
    let dir = scratch("input-changed");
    std::fs::create_dir_all(&dir).unwrap();
    let source = workspace().join("fixtures/build/apps/hello-views.apk");
    let apk = dir.join("app.apk");
    std::fs::copy(&source, &apk).unwrap();
    let bytes = std::fs::read(&apk).unwrap();
    let scenario_file = dir.join("s.yaml");
    std::fs::write(
        &scenario_file,
        format!(
            r#"version: 1
name: input-changed
app: app.apk
isolation: process
display: {{mode: offscreen, width: 1080, height: 2400, density_dpi: 420}}
steps:
  - launch: {{activity: dev.acos.fixtures.helloviews/.MainActivity}}
"#,
        ),
    )
    .unwrap();

    let request = VerifyRequest {
        base: acos_protocol::RequestBase::new("req_input_changed"),
        apk: None,
        scenario: scenario_file.display().to_string(),
        artifacts: dir.join("artifacts").display().to_string(),
        overwrite: false,
        reference: Some(reference_dir("hello-views").display().to_string()),
        required_fidelity: None,
        engine: acos_protocol::VerifyEngine::Art,
        flags: Vec::new(),
        animations: None,
        native: false,
    };
    let backend = RewritingApk {
        inner: ReferenceBackend::new(reference_dir("hello-views")),
        apk: std::fs::canonicalize(&apk).unwrap_or(apk.clone()),
        rewrite: Some(bytes),
    };
    let mut backend = backend;
    let clock = FixedClock::new(1_794_318_674_000);
    let envelope = verify_with_clock(&request, &mut backend, &clock);
    let result = envelope.result().expect("a result");
    let provenance = &result["verdict"]["provenance"];
    let warnings = provenance["warnings"].as_array().expect("warnings");
    let changed = warnings
        .iter()
        .find(|warning| warning["code"] == acos_protocol::provenance::WARN_INPUT_CHANGED)
        .unwrap_or_else(|| panic!("INPUT_CHANGED_DURING_RUN missing: {provenance}"));
    assert_eq!(changed["input"], "apk", "{changed}");
    assert_eq!(
        changed["sha256_12"], provenance["apk"]["sha256_12"],
        "the warning names the bytes the run hashed: {changed}"
    );
    // The status stays exactly as measured: a changed input is a fact about the
    // evidence, not a rewritten verdict. The envelope's status still matches the
    // verdict's own status.
    assert_eq!(
        envelope.status().as_str(),
        result["verdict"]["verdict"].as_str().unwrap_or("?"),
        "a warning never rewrites the measured status: {provenance}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// A relative `app:` resolves against the scenario path **as written**, while
/// provenance keeps the canonical path.
///
/// The regression: a scenario file symlinked into a workspace
/// (`out/s.yaml -> ../design/actual.yaml`) with `app: app.apk` sitting next to
/// the *link* used to break, because the app was resolved from the canonicalized
/// scenario directory instead of the caller's path. Resolution uses the path the
/// caller named (as it always did); only the value written into provenance is
/// canonical.
#[cfg(unix)]
#[test]
fn a_relative_app_resolves_against_the_scenario_path_as_written() {
    let dir = scratch("relative-app");
    let real = dir.join("real");
    let work = dir.join("work");
    std::fs::create_dir_all(&real).unwrap();
    std::fs::create_dir_all(&work).unwrap();
    let apk = work.join("app.apk");
    std::fs::copy(
        workspace().join("fixtures/build/apps/hello-views.apk"),
        &apk,
    )
    .unwrap();
    let actual = real.join("actual.yaml");
    std::fs::write(
        &actual,
        r#"version: 1
name: relative-app
app: app.apk
isolation: process
display: {mode: offscreen, width: 1080, height: 2400, density_dpi: 420}
steps:
  - launch: {activity: dev.acos.fixtures.helloviews/.MainActivity}
"#,
    )
    .unwrap();
    // The scenario the caller names is a symlink in a *different* directory from
    // the file it points at; `app.apk` exists only next to the link.
    let linked = work.join("s.yaml");
    std::os::unix::fs::symlink(&actual, &linked).unwrap();

    let request = VerifyRequest {
        base: acos_protocol::RequestBase::new("req_relative_app"),
        apk: None,
        scenario: linked.display().to_string(),
        artifacts: dir.join("artifacts").display().to_string(),
        overwrite: false,
        reference: Some(reference_dir("hello-views").display().to_string()),
        required_fidelity: None,
        engine: acos_protocol::VerifyEngine::Art,
        flags: Vec::new(),
        animations: None,
        native: false,
    };
    let mut backend = ReferenceBackend::new(reference_dir("hello-views"));
    let clock = FixedClock::new(1_794_318_674_000);
    let envelope = verify_with_clock(&request, &mut backend, &clock);
    let result = envelope.result().unwrap_or_else(|| {
        panic!(
            "a relative app next to the named scenario must resolve: {:?}",
            envelope.error()
        )
    });
    let provenance = &result["verdict"]["provenance"];
    // Provenance is canonical: the real scenario file and the APK it resolved.
    assert_eq!(
        provenance["scenario"]["path"],
        serde_json::json!(std::fs::canonicalize(&actual)
            .unwrap()
            .display()
            .to_string()),
        "{provenance}"
    );
    assert_eq!(
        provenance["apk"]["path"],
        serde_json::json!(std::fs::canonicalize(&apk).unwrap().display().to_string()),
        "{provenance}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------------
// MERGEINTEG §5: one step, one entry.
//
// A step that needed several real attempts records them in `attempts`; the
// step's own `status` is the **final** outcome. `verdict.json` and the reply's
// `result.verdict` are built from the same struct, so a passing run can never
// show a retried non-pass step in one view and a pass in the other.
// ---------------------------------------------------------------------------

struct ScrollBackend {
    manifest: Option<ManifestInfo>,
    attempts: u64,
    /// The attempt number whose settled traversal first shows the target.
    /// `None` means it never appears (the search must miss and say so).
    match_on: Option<u64>,
}

impl ScrollBackend {
    fn new(match_on: Option<u64>) -> Self {
        ScrollBackend {
            manifest: None,
            attempts: 0,
            match_on,
        }
    }

    fn observation(&self) -> SessionObservation {
        let matched = self.match_on.is_some_and(|on| self.attempts >= on);
        let frame = FrameMeta::new(
            1 + self.attempts,
            "2026-11-11T00:00:00.000Z",
            format!("scroll:{}", self.attempts),
            1080,
            2400,
            420,
            format!("scroll-hash-{}-{matched}", self.attempts),
            true,
        );
        let node = UiNode::new(
            "n1",
            "android.widget.TextView",
            Bounds::new(42, 612, 175, 663),
        )
        .with_text(if matched { "Row 9" } else { "Row 1" });
        let tree = UiTree::new(frame.clone(), "dev.acos.fixtures.helloviews", vec![node]);
        SessionObservation {
            session_id: "s_scroll".to_string(),
            package: "dev.acos.fixtures.helloviews".to_string(),
            frame,
            tree,
            screenshot_png: Some(b"scroll".to_vec()),
            crashed: None,
            input: None,
            settle: None,
        }
    }
}

impl Backend for ScrollBackend {
    fn id(&self) -> &'static str {
        "scroll-mock"
    }

    fn protocol_backend(&self) -> ProtocolBackend {
        ProtocolBackend::AcosNative
    }

    fn fidelity(&self) -> Fidelity {
        Fidelity::NativeRuntime
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            static_manifest: true,
            launch: true,
            ui_tree: true,
            input: true,
            ..Capabilities::default()
        }
    }

    fn manifest(&mut self, apk: &Path) -> Result<ManifestInfo, BackendError> {
        let info = acos_verify::manifest::load_manifest(apk)?;
        self.manifest = Some(info.clone());
        Ok(info)
    }

    fn launch(&mut self, _request: &LaunchRequest) -> Result<LaunchOutcome, BackendError> {
        let observation = self.observation();
        Ok(LaunchOutcome {
            session_id: "s_scroll".to_string(),
            package: "dev.acos.fixtures.helloviews".to_string(),
            frame: Some(observation.frame.clone()),
            screenshot_png: observation.screenshot_png.clone(),
            crashed: None,
            detail: serde_json::json!({"backend": "scroll-mock"}),
        })
    }

    fn observe(&mut self) -> Result<SessionObservation, BackendError> {
        Ok(self.observation())
    }

    fn refresh(&mut self) -> Result<SessionObservation, BackendError> {
        Ok(self.observation())
    }

    /// One real gesture per attempt, exactly the default `scroll_until` contract.
    fn input(&mut self, _action: &InputAction) -> Result<SessionObservation, BackendError> {
        self.attempts += 1;
        Ok(self.observation())
    }
}

fn attempt_counter() -> &'static AtomicU64 {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    &COUNTER
}

fn run_scroll(match_on: Option<u64>, max_attempts: usize) -> (RunOutcome, Vec<u8>) {
    let yaml = format!(
        r#"
version: 1
name: scroll-attempts
app: fixtures/build/apps/hello-views.apk
isolation: process
display: {{mode: offscreen, width: 1080, height: 2400, density_dpi: 420}}
expectation: pass
steps:
  - launch: {{activity: dev.acos.fixtures.helloviews/.MainActivity}}
  - scroll_until:
      selector: {{text: {{exact: "Row 9"}}}}
      from: {{x: 540, y: 1800}}
      to: {{x: 540, y: 600}}
      max_attempts: {max_attempts}
"#
    );
    let scenario = Scenario::from_yaml_str(&yaml).expect("valid scenario");
    let n = attempt_counter().fetch_add(1, Ordering::Relaxed);
    let artifacts = workspace()
        .join(".cache/mergeinteg-verify-tests")
        .join(format!("scroll-{}-{n}-{max_attempts}", std::process::id()));
    let _ = std::fs::remove_dir_all(&artifacts);
    let options = RunOptions {
        apk: workspace().join("fixtures/build/apps/hello-views.apk"),
        scenario_path: workspace().join("scenarios/hello-views.yaml"),
        artifacts_dir: artifacts.clone(),
        required_fidelity: None,
        request_id: "req_scroll_attempts".to_string(),
        flags: Vec::new(),
        acos_version: "0.1.0".to_string(),
        engine: "rust-vm".to_string(),
        warnings: Vec::new(),
    };
    let clock = FixedClock::new(1_794_318_674_000);
    let mut backend = ScrollBackend::new(match_on);
    let outcome = run_scenario(&scenario, &options, &mut backend, &clock).expect("run");
    // The disk copy is the second view of the same run: read the bytes back.
    let disk = std::fs::read(artifacts.join("verdict.json")).expect("verdict.json");
    let _ = std::fs::remove_dir_all(&artifacts);
    (outcome, disk)
}

#[test]
fn a_retried_step_is_one_entry_with_its_attempts() {
    let (outcome, disk) = run_scroll(Some(2), 3);
    assert_eq!(
        outcome.verdict.verdict,
        Status::Pass,
        "found on attempt 2: {:?}",
        outcome.verdict.failure
    );
    let step = &outcome.verdict.steps[1];
    assert_eq!(step.step, "scroll_until");
    // One entry, and its status is the final outcome - not the failed attempt.
    assert_eq!(step.status, Status::Pass);
    assert!(step.error.is_none(), "a passing step carries no error");
    // The history is explicit, not hidden: attempt 1 missed, attempt 2 passed.
    assert_eq!(step.attempts.len(), 2, "{:?}", step.attempts);
    assert_eq!(step.attempts[0].attempt, 1);
    assert_eq!(step.attempts[0].status, Status::Fail);
    assert!(
        step.attempts[0]
            .reason
            .as_deref()
            .is_some_and(|reason| reason.contains("not visible")),
        "{:?}",
        step.attempts[0]
    );
    assert_eq!(step.attempts[1].attempt, 2);
    assert_eq!(step.attempts[1].status, Status::Pass);
    assert_eq!(step.attempts[1].frame_seq, step.frame_seq);

    // Both views of the run are the same document.
    let disk: serde_json::Value = serde_json::from_slice(&disk).expect("disk verdict parses");
    let inline = serde_json::to_value(&outcome.verdict).expect("verdict serializes");
    assert_eq!(disk["steps"], inline["steps"], "verdict.json vs inline steps");
    assert_eq!(disk["verdict"], inline["verdict"]);
    // And the disk copy carries the same attempt history.
    assert_eq!(disk["steps"][1]["attempts"].as_array().map(Vec::len), Some(2));
    assert_eq!(disk["steps"][1]["status"], "pass");
}

#[test]
fn a_step_that_never_passes_fails_with_every_attempt() {
    let (outcome, disk) = run_scroll(None, 3);
    assert_eq!(outcome.verdict.verdict, Status::Fail);
    let step = &outcome.verdict.steps[1];
    assert_eq!(step.step, "scroll_until");
    assert_eq!(step.status, Status::Fail);
    assert_eq!(
        step.error.as_ref().map(|error| error.code),
        Some(acos_protocol::ErrorCode::ElementNotFound)
    );
    // All three attempts are kept, none is a pass.
    assert_eq!(step.attempts.len(), 3, "{:?}", step.attempts);
    assert!(step.attempts.iter().all(|attempt| attempt.status == Status::Fail));
    assert_eq!(
        step.attempts.last().and_then(|attempt| attempt.reason.as_deref()),
        Some(
            "selector not visible after this attempt's settled traversal (search exhausted)"
        )
    );

    let disk: serde_json::Value = serde_json::from_slice(&disk).expect("disk verdict parses");
    let inline = serde_json::to_value(&outcome.verdict).expect("verdict serializes");
    assert_eq!(disk["steps"], inline["steps"], "verdict.json vs inline steps");
    assert_eq!(disk["steps"][1]["attempts"].as_array().map(Vec::len), Some(3));
}

#[test]
fn a_first_attempt_pass_records_no_attempts_array() {
    let (outcome, disk) = run_scroll(Some(1), 3);
    assert_eq!(outcome.verdict.verdict, Status::Pass);
    let step = &outcome.verdict.steps[1];
    assert_eq!(step.status, Status::Pass);
    assert!(
        step.attempts.is_empty(),
        "no retry, no attempts array: {:?}",
        step.attempts
    );
    let disk: serde_json::Value = serde_json::from_slice(&disk).expect("disk verdict parses");
    assert!(
        disk["steps"][1].get("attempts").is_none(),
        "a single attempt is the step's own outcome, not a history"
    );
}
