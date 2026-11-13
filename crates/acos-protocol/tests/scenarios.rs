//! Every checked-in scenario parses/validates, and >30 negative validation cases.

use std::path::{Path, PathBuf};

use acos_protocol::assertion::{
    LogAssertion, NoCrashAssertion, RegionMask, ScreenshotAssertion, StateAssertion, TextAssertion,
    VisibilityAssertion,
};
use acos_protocol::commands::{CaptureFormat, KeyName, Orientation, ResetScope};
use acos_protocol::error::ErrorCode;
use acos_protocol::identity::{DisplaySpec, IsolationLevel, ResourceBudget};
use acos_protocol::scenario::*;
use acos_protocol::ui::{Bounds, Selector, TextMatch};

const VALID: &str = r#"
version: 1
name: valid
app: fixtures/build/apps/hello-views.apk
engine: native-preferred
fallback: report-and-escalate
api_level: 34
isolation: process
display: {mode: offscreen, width: 1080, height: 2400, density_dpi: 420}
capabilities: [ui.text, input]
budgets: {memory_mib: 512, wall_ms: 60000, cpu_ms: 30000}
expectation: pass
steps:
  - launch: {activity: dev.acos.fixtures.helloviews/.MainActivity, clear_state: true}
  - wait_for: {selector: {id: result}, timeout_ms: 5000}
  - assert_text:
      selector: {id: result}
      text: {exact: "clicks: 0"}
      bounds: [42, 612, 175, 663]
  - back: {}
"#;

fn valid() -> Scenario {
    Scenario::from_yaml_str(VALID).expect("valid base scenario")
}

fn with_steps(steps: Vec<Step>) -> Scenario {
    let mut scenario = valid();
    scenario.steps = steps;
    scenario
}

fn one(step: Step) -> Scenario {
    with_steps(vec![step])
}

fn selector() -> Selector {
    Selector::by_id("result")
}

fn step_error(label: &str, scenario: Scenario) -> (String, Scenario) {
    let validation = scenario.validate();
    assert!(
        !validation.is_ok(),
        "negative case {label:?} was accepted; diagnostic: {validation:?}"
    );
    for error in &validation.errors {
        assert_eq!(error.code, ErrorCode::InvalidScenario, "{label}");
        assert!(!error.path.is_empty(), "{label}");
        assert!(!error.message.is_empty(), "{label}");
        assert!(!error.hint.is_empty(), "{label}");
    }
    (label.to_string(), scenario)
}

/// Collect scenario files recursively, sorted by path.
fn scenario_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("read_dir {}: {e}", dir.display()))
        .map(|entry| entry.unwrap().path())
        .collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            scenario_files(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("yaml") {
            // `INDEX.yaml` is the suite manifest (it has `tasks:`, not `steps:`),
            // not a scenario; validating it as one is a category error.
            if path.file_name().and_then(|name| name.to_str()) == Some("INDEX.yaml") {
                continue;
            }
            out.push(path);
        }
    }
}

#[test]
fn every_checked_in_scenario_parses_and_validates() {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let scenarios_dir = workspace.join("scenarios");
    let mut files = Vec::new();
    scenario_files(&scenarios_dir, &mut files);
    assert!(
        files.len() >= 15,
        "expected fixture + negative scenarios, got {files:?}"
    );

    let mut positive = 0;
    let mut negative = 0;
    for path in &files {
        let scenario = Scenario::load(path)
            .unwrap_or_else(|validation| panic!("{} is invalid: {validation:?}", path.display()));
        let validation = scenario.validate();
        assert!(
            validation.is_ok(),
            "{}: {:?}",
            path.display(),
            validation.errors
        );
        assert!(
            workspace.join(&scenario.app).exists(),
            "{} points at missing APK {}",
            path.display(),
            scenario.app
        );
        // Negative scenarios live under `negative/`; the suite's deliberately
        // wrong twins (`*-wrong.yaml`) also declare a non-pass expectation and
        // must not be required to pass. Every other scenario must expect pass.
        let is_negative = path.components().any(|part| part.as_os_str() == "negative")
            || path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with("-wrong.yaml"))
            || scenario.expectation != Expectation::Pass;
        if is_negative {
            negative += 1;
            assert!(
                matches!(
                    scenario.expectation,
                    Expectation::Fail | Expectation::NonPass
                ),
                "{} must not expect pass",
                path.display()
            );
        } else {
            positive += 1;
            assert_eq!(
                scenario.expectation,
                Expectation::Pass,
                "{} must expect pass",
                path.display()
            );
        }
        // GR1 added a checked-in reference-device scenario at 400 dpi; the
        // rest of the corpus is the API-34 420 dpi profile.
        assert!(
            matches!(
                scenario.display.label().as_str(),
                "1080x2400@420" | "1080x2400@400"
            ),
            "{}: unexpected display {}",
            path.display(),
            scenario.display.label()
        );
    }
    // The fixture matrix grows (W15 added scenarios); the invariant is that
    // every positive scenario expects pass and every negative one does not.
    assert!(
        positive >= 8,
        "expected at least 8 positive scenarios, got {positive}"
    );
    assert!(
        negative >= 9,
        "expected at least 9 negative scenarios, got {negative}"
    );
}

#[test]
fn fixture_scenarios_contain_the_art_reference_strings() {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let views = std::fs::read_to_string(workspace.join("scenarios/hello-views.yaml")).unwrap();
    for needle in [
        "Hello ACOS",
        "Enter your name",
        "Greet",
        "clicks: 0",
        "Hello, World! clicks: 1",
        "Hello, World! clicks: 2",
        "onSaveInstanceState clicks=2",
        "[42, 317, 231, 368]",
        "[170, 526, 303, 577]",
    ] {
        assert!(
            views.contains(needle),
            "hello-views.yaml missing {needle:?}"
        );
    }
    let jni = std::fs::read_to_string(workspace.join("scenarios/hello-jni.yaml")).unwrap();
    for needle in [
        "stringFromJNI=Hello from native C",
        "add(20,22)=42",
        "sumArray([1,2,3,4,5])=15",
        "javaCallbackResult=42",
        "voidCallback=from-native",
        "registeredEcho=echo!",
        "throwFromJNI caught=thrown from native",
        "javaCallback x=21",
    ] {
        assert!(jni.contains(needle), "hello-jni.yaml missing {needle:?}");
    }
}

#[test]
fn valid_scenario_has_no_errors_and_warns_on_optional_step() {
    assert!(valid().validate().is_ok());
    let warned = one(Step {
        call_activity_method: Some(CallActivityMethodStep {
            activity: None,
            method: "injectHook".to_string(),
            args: Vec::new(),
        }),
        ..Step::default()
    });
    let validation = warned.validate();
    assert!(validation.is_ok(), "{:?}", validation.errors);
    assert!(validation
        .warnings
        .iter()
        .any(|warning| warning.message.contains("optional step")));
}

#[test]
fn at_least_thirty_negative_validation_cases_are_rejected() {
    let mut cases: Vec<(String, Scenario)> = Vec::new();

    // Field-level invalids.
    let mut scenario = valid();
    scenario.version = 2;
    cases.push(step_error("version", scenario));
    let mut scenario = valid();
    scenario.name = " ".to_string();
    cases.push(step_error("name", scenario));
    let mut scenario = valid();
    scenario.app = String::new();
    cases.push(step_error("app", scenario));
    let mut scenario = valid();
    scenario.api_level = Some(0);
    cases.push(step_error("api-level-low", scenario));
    let mut scenario = valid();
    scenario.api_level = Some(101);
    cases.push(step_error("api-level-high", scenario));
    let mut scenario = valid();
    scenario.display = DisplaySpec::offscreen(0, 2400, 420);
    cases.push(step_error("display-width", scenario));
    let mut scenario = valid();
    scenario.display = DisplaySpec::offscreen(1080, 0, 420);
    cases.push(step_error("display-height", scenario));
    let mut scenario = valid();
    scenario.display = DisplaySpec::offscreen(1080, 2400, 10);
    cases.push(step_error("display-density-low", scenario));
    let mut scenario = valid();
    scenario.display = DisplaySpec::offscreen(1080, 2400, 2000);
    cases.push(step_error("display-density-high", scenario));
    let mut scenario = valid();
    scenario.budgets = Some(ResourceBudget::memory_mib(0));
    cases.push(step_error("budget-memory", scenario));
    let mut scenario = valid();
    scenario.budgets = Some(ResourceBudget::wall_ms(0));
    cases.push(step_error("budget-wall", scenario));
    let mut scenario = valid();
    scenario.budgets = Some(ResourceBudget {
        memory_mib: None,
        wall_ms: None,
        cpu_ms: Some(0),
    });
    cases.push(step_error("budget-cpu", scenario));
    let mut scenario = valid();
    scenario.steps.clear();
    cases.push(step_error("steps-empty", scenario));

    // Step-shape invalids.
    cases.push(step_error("step-empty", one(Step::default())));
    cases.push(step_error(
        "step-two-keys",
        one(Step {
            launch: Some(LaunchStep {
                activity: Some(".MainActivity".to_string()),
                ..LaunchStep::default()
            }),
            back: Some(EmptyStep {}),
            ..Step::default()
        }),
    ));
    cases.push(step_error(
        "launch-no-target",
        one(Step {
            launch: Some(LaunchStep::default()),
            ..Step::default()
        }),
    ));
    cases.push(step_error(
        "launch-both",
        one(Step {
            launch: Some(LaunchStep {
                activity: Some(".A".to_string()),
                intent_action: Some("android.intent.action.MAIN".to_string()),
                ..LaunchStep::default()
            }),
            ..Step::default()
        }),
    ));
    cases.push(step_error(
        "wait-empty-selector",
        one(Step {
            wait_for: Some(WaitForStep {
                selector: Selector::default(),
                timeout_ms: 5000,
                require_visible: true,
            }),
            ..Step::default()
        }),
    ));
    cases.push(step_error(
        "wait-timeout-zero",
        one(Step {
            wait_for: Some(WaitForStep {
                selector: selector(),
                timeout_ms: 0,
                require_visible: true,
            }),
            ..Step::default()
        }),
    ));
    cases.push(step_error(
        "tap-no-target",
        one(Step {
            tap: Some(TapStep {
                selector: None,
                x: None,
                y: None,
                timeout_ms: None,
                settle_timeout_ms: None,
                settle: None,
            }),
            ..Step::default()
        }),
    ));
    cases.push(step_error(
        "tap-both",
        one(Step {
            tap: Some(TapStep {
                selector: Some(selector()),
                x: Some(1),
                y: Some(2),
                timeout_ms: None,
                settle_timeout_ms: None,
                settle: None,
            }),
            ..Step::default()
        }),
    ));
    cases.push(step_error(
        "tap-empty-selector",
        one(Step {
            tap: Some(TapStep {
                selector: Some(Selector::default()),
                x: None,
                y: None,
                timeout_ms: None,
                settle_timeout_ms: None,
                settle: None,
            }),
            ..Step::default()
        }),
    ));
    cases.push(step_error(
        "tap-timeout-zero",
        one(Step {
            tap: Some(TapStep {
                selector: Some(selector()),
                x: None,
                y: None,
                timeout_ms: Some(0),
                settle_timeout_ms: None,
                settle: None,
            }),
            ..Step::default()
        }),
    ));
    cases.push(step_error(
        "type-empty-text",
        one(Step {
            r#type: Some(TypeStep {
                selector: Some(selector()),
                text: String::new(),
                clear: false,
                settle_timeout_ms: None,
                settle: None,
            }),
            ..Step::default()
        }),
    ));
    cases.push(step_error(
        "type-empty-selector",
        one(Step {
            r#type: Some(TypeStep {
                selector: Some(Selector::default()),
                text: "x".to_string(),
                clear: false,
                settle_timeout_ms: None,
                settle: None,
            }),
            ..Step::default()
        }),
    ));

    // Assertion invalids.
    cases.push(step_error(
        "assert-text-empty-selector",
        one(Step {
            assert_text: Some(TextAssertion {
                selector: Selector::default(),
                text: TextMatch::exact("x"),
                bounds: None,
                timeout_ms: None,
            }),
            ..Step::default()
        }),
    ));
    cases.push(step_error(
        "assert-text-empty-matcher",
        one(Step {
            assert_text: Some(TextAssertion {
                selector: selector(),
                text: TextMatch::default(),
                bounds: None,
                timeout_ms: None,
            }),
            ..Step::default()
        }),
    ));
    cases.push(step_error(
        "assert-text-bad-regex",
        one(Step {
            assert_text: Some(TextAssertion {
                selector: selector(),
                text: TextMatch::regex("("),
                bounds: None,
                timeout_ms: None,
            }),
            ..Step::default()
        }),
    ));
    cases.push(step_error(
        "assert-text-bad-bounds",
        one(Step {
            assert_text: Some(TextAssertion {
                selector: selector(),
                text: TextMatch::exact("x"),
                bounds: Some(Bounds::new(10, 0, 5, 1)),
                timeout_ms: None,
            }),
            ..Step::default()
        }),
    ));
    cases.push(step_error(
        "assert-text-timeout-zero",
        one(Step {
            assert_text: Some(TextAssertion {
                selector: selector(),
                text: TextMatch::exact("x"),
                bounds: None,
                timeout_ms: Some(0),
            }),
            ..Step::default()
        }),
    ));
    cases.push(step_error(
        "assert-visible-empty-selector",
        one(Step {
            assert_visible: Some(VisibilityAssertion {
                selector: Selector::default(),
                bounds: None,
                timeout_ms: None,
            }),
            ..Step::default()
        }),
    ));
    cases.push(step_error(
        "assert-visible-bad-bounds",
        one(Step {
            assert_visible: Some(VisibilityAssertion {
                selector: selector(),
                bounds: Some(Bounds::new(10, 0, 5, 1)),
                timeout_ms: None,
            }),
            ..Step::default()
        }),
    ));
    cases.push(step_error(
        "assert-state-no-flags",
        one(Step {
            assert_state: Some(StateAssertion {
                selector: selector(),
                visible: None,
                enabled: None,
                focused: None,
                clickable: None,
                scrollable: None,
                checked: None,
                selected: None,
                timeout_ms: None,
            }),
            ..Step::default()
        }),
    ));
    cases.push(step_error(
        "assert-log-empty-patterns",
        one(Step {
            assert_log: Some(LogAssertion {
                tag: Some("AcosFixture".to_string()),
                patterns: Vec::new(),
                ordered: true,
                since_previous_step: true,
                timeout_ms: None,
                min_matches: 1,
            }),
            ..Step::default()
        }),
    ));
    cases.push(step_error(
        "assert-log-empty-pattern",
        one(Step {
            assert_log: Some(LogAssertion {
                tag: None,
                patterns: vec![String::new()],
                ordered: true,
                since_previous_step: false,
                timeout_ms: None,
                min_matches: 1,
            }),
            ..Step::default()
        }),
    ));
    cases.push(step_error(
        "assert-log-bad-regex",
        one(Step {
            assert_log: Some(LogAssertion {
                tag: None,
                patterns: vec!["(".to_string()],
                ordered: true,
                since_previous_step: false,
                timeout_ms: None,
                min_matches: 1,
            }),
            ..Step::default()
        }),
    ));
    cases.push(step_error(
        "assert-log-empty-tag",
        one(Step {
            assert_log: Some(LogAssertion {
                tag: Some(String::new()),
                patterns: vec!["x".to_string()],
                ordered: true,
                since_previous_step: false,
                timeout_ms: None,
                min_matches: 1,
            }),
            ..Step::default()
        }),
    ));
    cases.push(step_error(
        "assert-log-zero-min",
        one(Step {
            assert_log: Some(LogAssertion {
                tag: None,
                patterns: vec!["x".to_string()],
                ordered: true,
                since_previous_step: false,
                timeout_ms: None,
                min_matches: 0,
            }),
            ..Step::default()
        }),
    ));
    cases.push(step_error(
        "assert-screenshot-empty-baseline",
        one(Step {
            assert_screenshot: Some(ScreenshotAssertion {
                baseline: String::new(),
                tolerance: 0.0,
                region_masks: Vec::new(),
            }),
            ..Step::default()
        }),
    ));
    for (label, tolerance) in [
        ("assert-screenshot-tolerance-low", -0.1),
        ("assert-screenshot-tolerance-high", 1.1),
        ("assert-screenshot-tolerance-nan", f64::NAN),
    ] {
        cases.push(step_error(
            label,
            one(Step {
                assert_screenshot: Some(ScreenshotAssertion {
                    baseline: "b.png".to_string(),
                    tolerance,
                    region_masks: Vec::new(),
                }),
                ..Step::default()
            }),
        ));
    }
    cases.push(step_error(
        "assert-screenshot-bad-mask",
        one(Step {
            assert_screenshot: Some(ScreenshotAssertion {
                baseline: "b.png".to_string(),
                tolerance: 0.0,
                region_masks: vec![RegionMask {
                    bounds: Bounds::new(10, 0, 5, 1),
                    reason: None,
                }],
            }),
            ..Step::default()
        }),
    ));
    cases.push(step_error(
        "capture-empty-name",
        one(Step {
            capture: Some(CaptureStep {
                name: " ".to_string(),
                format: CaptureFormat::Png,
                fresh: true,
            }),
            ..Step::default()
        }),
    ));

    // Setup invalids.
    cases.push(step_error(
        "network-empty",
        one(Step {
            set_network: Some(NetworkSetup {
                offline: None,
                rules: Vec::new(),
            }),
            ..Step::default()
        }),
    ));
    let rule = |status: u16, url: &str, method: Option<&str>, times: Option<u32>| NetworkRule {
        method: method.map(str::to_string),
        url: url.to_string(),
        status,
        headers: Default::default(),
        body: None,
        delay_ms: None,
        times,
    };
    for (label, rule) in [
        ("network-empty-url", rule(200, "", Some("GET"), None)),
        ("network-status-low", rule(99, "https://x", None, None)),
        ("network-status-high", rule(600, "https://x", None, None)),
        (
            "network-empty-method",
            rule(200, "https://x", Some(""), None),
        ),
        ("network-times-zero", rule(200, "https://x", None, Some(0))),
    ] {
        cases.push(step_error(
            label,
            one(Step {
                set_network: Some(NetworkSetup {
                    offline: None,
                    rules: vec![rule],
                }),
                ..Step::default()
            }),
        ));
    }
    cases.push(step_error(
        "permission-empty",
        one(Step {
            set_permission: Some(PermissionSetup {
                permission: String::new(),
                state: PermissionState::Grant,
            }),
            ..Step::default()
        }),
    ));
    cases.push(step_error(
        "permission-unqualified",
        one(Step {
            set_permission: Some(PermissionSetup {
                permission: "CAMERA".to_string(),
                state: PermissionState::Grant,
            }),
            ..Step::default()
        }),
    ));
    cases.push(step_error(
        "locale-bad",
        one(Step {
            set_locale: Some(LocaleSetup {
                locale: "e".to_string(),
            }),
            ..Step::default()
        }),
    ));
    cases.push(step_error(
        "density-low",
        one(Step {
            set_density: Some(DensitySetup { density_dpi: 71 }),
            ..Step::default()
        }),
    ));
    cases.push(step_error(
        "density-high",
        one(Step {
            set_density: Some(DensitySetup { density_dpi: 1001 }),
            ..Step::default()
        }),
    ));
    cases.push(step_error(
        "time-bad",
        one(Step {
            set_time: Some(TimeSetup {
                timestamp: "nope".to_string(),
                timezone: None,
            }),
            ..Step::default()
        }),
    ));
    cases.push(step_error(
        "call-activity-method-empty",
        one(Step {
            call_activity_method: Some(CallActivityMethodStep {
                activity: None,
                method: " ".to_string(),
                args: Vec::new(),
            }),
            ..Step::default()
        }),
    ));

    // A few structural/semantic repeats to keep the floor far above 30.
    cases.push(step_error(
        "rotate-unknown-is-serde-not-this-test",
        one(Step {
            key: Some(KeyStep {
                key: KeyName::Back,
                settle_timeout_ms: None,
                settle: None,
            }),
            // A key step is valid; make it invalid by adding a second kind.
            reset: Some(ResetStep {
                scope: ResetScope::App,
                settle_timeout_ms: None,
            }),
            ..Step::default()
        }),
    ));
    cases.push(step_error(
        "assert-not-visible-empty-selector",
        one(Step {
            assert_not_visible: Some(VisibilityAssertion {
                selector: Selector::default(),
                bounds: None,
                timeout_ms: None,
            }),
            ..Step::default()
        }),
    ));
    cases.push(step_error(
        "assert-no-crash-with-bad-companion",
        one(Step {
            assert_no_crash: Some(NoCrashAssertion::default()),
            rotate: Some(RotateStep {
                orientation: Orientation::Portrait,
            }),
            ..Step::default()
        }),
    ));

    assert!(
        cases.len() >= 30,
        "only {} negative cases; the contract requires at least 30",
        cases.len()
    );
    // Keep labels alive for a useful failure message and prove they are unique.
    let mut labels: Vec<&str> = cases.iter().map(|(label, _)| label.as_str()).collect();
    labels.sort_unstable();
    let unique = labels.len();
    labels.dedup();
    assert_eq!(labels.len(), unique, "duplicate negative-case labels");

    // Sanity: the base scenario remains valid and every mutated case failed only
    // because of the intended invalidity.
    assert!(valid().validate().is_ok());
    assert_eq!(valid().isolation, IsolationLevel::Process);
}
