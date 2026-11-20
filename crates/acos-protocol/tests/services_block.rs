//! ADR-0029 item 4: the scenario `services:` block is a validated launch input.

use acos_protocol::scenario::Scenario;

const HEADER: &str = "version: 1\nname: svc\napp: app.apk\nisolation: process\ndisplay: {mode: offscreen, width: 1080, height: 2400, density_dpi: 420}\n";

#[test]
fn a_services_block_round_trips_into_the_scenario() {
    let yaml = format!(
        "{HEADER}services:\n  - id: feedback.vibrator\n    profile: stub\n    config: {{enable: [a]}}\n  - id: core\nsteps:\n  - launch: {{activity: .MainActivity}}\n"
    );
    let scenario = Scenario::from_yaml_str(&yaml).expect("scenario parses");
    assert_eq!(scenario.services.len(), 2);
    assert_eq!(scenario.services[0].id, "feedback.vibrator");
    assert_eq!(scenario.services[0].profile.as_deref(), Some("stub"));
    assert_eq!(
        scenario.services[0].config.as_ref().unwrap()["enable"][0],
        "a"
    );
    assert!(scenario.services[1].profile.is_none());
}

#[test]
fn an_unknown_profile_or_field_is_an_invalid_scenario() {
    let unknown_profile = format!(
        "{HEADER}services:\n  - id: feedback.vibrator\n    profile: rela\nsteps:\n  - launch: {{}}\n"
    );
    let error = Scenario::from_yaml_str(&unknown_profile).expect_err("typo must fail");
    assert!(error
        .errors
        .iter()
        .any(|e| e.path == "$.services[0].profile"));

    let unknown_field = format!(
        "{HEADER}services:\n  - id: feedback.vibrator\n    enable: true\nsteps:\n  - launch: {{}}\n"
    );
    assert!(Scenario::from_yaml_str(&unknown_field).is_err());
}

#[test]
fn a_step_can_require_a_service_module() {
    let yaml = format!(
        "{HEADER}steps:\n  - launch: {{activity: .MainActivity}}\n  - assert_text: {{selector: {{id: counter}}, text: {{exact: hi}}}}\n    requires_services: [feedback.vibrator]\n"
    );
    let scenario = Scenario::from_yaml_str(&yaml).expect("scenario parses");
    assert_eq!(scenario.steps[1].requires_services, ["feedback.vibrator"]);
}
