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
    Backend as ProtocolBackend, Envelope, Fidelity, FrameMeta, Status, UiTree,
};
use acos_verify::backend::{
    Backend, BackendError, Capabilities, InputAction, LaunchOutcome, LaunchRequest, ManifestInfo,
    SessionObservation,
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
        reference: Some(reference_dir("hello-views").display().to_string()),
        required_fidelity: None,
        flags: vec!["--reference".to_string()],
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
        }
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
        Ok(self.current())
    }

    fn capture(&mut self) -> Result<SessionObservation, BackendError> {
        if self.mode != SliceMode::StaleCapture {
            self.frame_seq += 1;
        }
        Ok(self.current())
    }
}

fn run_slice(yaml: &str, mode: SliceMode, initial: Vec<u8>, after: Vec<u8>) -> RunOutcome {
    let scenario = Scenario::from_yaml_str(yaml).expect("valid scenario");
    let artifacts = scratch("advisor-rule");
    let mut backend = SliceBackend::new(mode, initial, after);
    let options = RunOptions {
        apk: workspace().join("fixtures/build/apps/hello-views.apk"),
        scenario_path: workspace().join("scenarios/hello-views.yaml"),
        artifacts_dir: artifacts.clone(),
        required_fidelity: None,
        request_id: "req_advisor_rule".to_string(),
        flags: Vec::new(),
        acos_version: "0.1.0".to_string(),
    };
    let clock = FixedClock::new(1_794_318_674_000);
    let outcome = run_scenario(&scenario, &options, &mut backend, &clock).expect("run");
    let _ = std::fs::remove_dir_all(&artifacts);
    outcome
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

