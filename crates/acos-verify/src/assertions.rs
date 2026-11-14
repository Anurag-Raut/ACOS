//! Assertion evaluation against one observation or log slice.
//!
//! Each evaluator returns an [`AssertionResult`] (for the verdict) plus an
//! optional [`BackendError`] when the assertion did not hold. The executor turns
//! the error into the step's status and the failure classification.

use acos_protocol::assertion::{
    AssertionDiff, AssertionResult, LogAssertion, StateAssertion, TextAssertion,
    VisibilityAssertion,
};
use acos_protocol::ui::UiNode;
use acos_protocol::ErrorCode;
use regex::Regex;
use serde_json::{json, Value};

use crate::backend::{BackendError, CrashRecord, LogRecord, SessionObservation};

/// The outcome of one assertion.
#[derive(Debug, Clone)]
pub struct Evaluated {
    /// The assertion result recorded in the verdict.
    pub result: AssertionResult,
    /// The failure, when the assertion did not hold.
    pub error: Option<BackendError>,
}

impl Evaluated {
    /// A passing assertion. `expected`/`actual` are always recorded: an agent
    /// reading the artifacts can tell *what* was asserted and what was seen,
    /// not only that the check held.
    pub(crate) fn pass(
        kind: &str,
        matched: Vec<String>,
        message: Option<String>,
        expected: Value,
        actual: Value,
    ) -> Self {
        Evaluated {
            result: AssertionResult {
                kind: kind.to_string(),
                passed: true,
                expected: Some(expected),
                actual: Some(actual),
                diff: None,
                matched_node_ids: matched,
                message,
            },
            error: None,
        }
    }

    pub(crate) fn fail(
        kind: &str,
        matched: Vec<String>,
        diff: AssertionDiff,
        error: BackendError,
    ) -> Self {
        Evaluated {
            result: AssertionResult {
                kind: kind.to_string(),
                passed: false,
                // The failure diff carries the same information plus a path and
                // a human detail; `expected`/`actual` are populated on both
                // paths so one shape reads every assertion result.
                expected: Some(diff.expected.clone()),
                actual: Some(diff.actual.clone()),
                diff: Some(diff),
                matched_node_ids: matched,
                message: None,
            },
            error: Some(error),
        }
    }
}

/// Find the matching node and its document index.
///
/// The selector's own visibility policy applies: only user-visible nodes match
/// unless `selector.include_invisible` is set (see [`Selector::accepts_node`]).
fn locate<'a>(observation: &'a SessionObservation, selector: &acos_protocol::Selector) -> Option<(usize, &'a UiNode)> {
    let index = observation
        .tree
        .all_nodes()
        .into_iter()
        .position(|node| {
            let mut probe = selector.clone();
            probe.index = None;
            probe.accepts_node(node)
        })?;
    observation.tree.all_nodes().get(index).map(|node| (index, *node))
}

/// Locate a node even when it is not visible to the user (`assert_not_visible`
/// and `assert_state {visible: false}` are checks *about* invisibility).
fn locate_including_invisible<'a>(
    observation: &'a SessionObservation,
    selector: &acos_protocol::Selector,
) -> Option<(usize, &'a UiNode)> {
    let mut probe = selector.clone();
    probe.include_invisible = true;
    locate(observation, &probe)
}

/// The failure for a selector that matched only nodes the user cannot see.
///
/// This is the class of false pass GR2 W4 measured: the pager's warm-mounted
/// next page is in the tree at `x=+displayWidth`, so a text/`wait_for` selector
/// used to match content that is not on screen. It is never a pass; the error
/// names the off-screen node and its bounds so the caller can scroll first.
pub fn offscreen_error(
    kind: &str,
    selector: &acos_protocol::Selector,
    observation: &SessionObservation,
) -> Evaluated {
    let node = observation.tree.find_invisible(selector);
    let described = node
        .map(|node| {
            format!(
                "text {:?} content_desc {:?} class {} bounds {} visible_bounds {} in window {:?}",
                node.text,
                node.content_desc,
                node.class,
                node.bounds,
                node.visible_bounds
                    .map_or_else(|| "<none>".to_string(), |b| b.to_string()),
                node.window
            )
        })
        .unwrap_or_else(|| "an invisible node".to_string());
    Evaluated::fail(
        kind,
        node.map(|node| vec![node.id.clone()]).unwrap_or_default(),
        AssertionDiff {
            expected: json!({"selector": selector, "visible": true}),
            actual: node.map_or(Value::Null, |node| {
                json!({
                    "id": node.id,
                    "text": node.text,
                    "content_desc": node.content_desc,
                    "bounds": node.bounds,
                    "visible_bounds": node.visible_bounds,
                    "visible": node.visible,
                    "window": node.window,
                })
            }),
            path: node.map(|_| node_path(0, "visible")),
            detail: Some(format!(
                "selector matched only a node that is not visible to the user ({described}); scroll it into view first"
            )),
        },
        BackendError::Failed {
            code: ErrorCode::ElementOffscreen,
            detail: format!(
                "selector {selector} matched no visible node, but it matches a node that is not visible to the user ({described}); this is the GR2 W4 false-pass class — scroll it into view or target a visible node, never treat it as a pass"
            ),
        },
    )
}

fn node_path(index: usize, field: &str) -> String {
    format!("nodes[{index}].{field}")
}

/// The error for a selector that matched nothing.
///
/// A tree the probe marked `truncated` (any window) does **not** prove the node
/// is absent: the selector may have fallen beyond the sampled node budget. That
/// case is a distinct, non-retryable `TREE_TRUNCATED`, never `ELEMENT_NOT_FOUND`.
pub fn missing_error(
    observation: Option<&SessionObservation>,
    selector: &acos_protocol::Selector,
) -> BackendError {
    let truncated = observation.is_some_and(|observation| observation.tree.truncated);
    if truncated {
        BackendError::Failed {
            code: ErrorCode::TreeTruncated,
            detail: format!(
                "selector {selector} matched no node, but the tree was truncated at the node budget; the node may exist beyond the sampled prefix"
            ),
        }
    } else {
        BackendError::Failed {
            code: ErrorCode::ElementNotFound,
            detail: format!("selector {selector} matched no node"),
        }
    }
}

/// Build the failure for a selector that matched nothing.
pub fn element_missing(
    kind: &str,
    selector: &acos_protocol::Selector,
    observation: Option<&SessionObservation>,
) -> Evaluated {
    let truncated = observation.is_some_and(|observation| observation.tree.truncated);
    Evaluated::fail(
        kind,
        Vec::new(),
        AssertionDiff {
            expected: json!(selector),
            actual: Value::Null,
            path: None,
            detail: Some(if truncated {
                "selector matched no node in a truncated tree".to_string()
            } else {
                "selector matched no node".to_string()
            }),
        },
        missing_error(observation, selector),
    )
}

/// Evaluate a `text` assertion (optional exact bounds).
pub fn eval_text(assertion: &TextAssertion, observation: &SessionObservation) -> Evaluated {
    let Some((index, node)) = locate(observation, &assertion.selector) else {
        if observation.tree.find_invisible(&assertion.selector).is_some() {
            return offscreen_error("text", &assertion.selector, observation);
        }
        return Evaluated::fail(
            "text",
            Vec::new(),
            AssertionDiff {
                expected: json!({"selector": assertion.selector, "text": assertion.text}),
                actual: Value::Null,
                path: None,
                detail: Some("selector matched no node".to_string()),
            },
            missing_error(Some(observation), &assertion.selector),
        );
    };
    let matched = vec![node.id.clone()];
    // An EditText exposes its placeholder through `getHint()`; a scenario that
    // asks for the hint text ("Name", "Enter your name") must match it even
    // when the widget also has literal text. The guest-asserted provenance of
    // the tree is unchanged (S8).
    if !assertion.text.matches(&node.text) && !assertion.text.matches(&node.hint) {
        return Evaluated::fail(
            "text",
            matched,
            AssertionDiff {
                expected: json!(assertion.text),
                actual: json!({"text": node.text, "hint": node.hint}),
                path: Some(node_path(index, "text")),
                detail: Some(format!(
                    "text/hint did not match ({})",
                    assertion.text.describe()
                )),
            },
            BackendError::Failed {
                code: ErrorCode::AssertionFailed,
                detail: format!(
                    "expected text {}, got text {:?} / hint {:?}",
                    assertion.text.describe(),
                    node.text,
                    node.hint
                ),
            },
        );
    }
    if let Some(expected_bounds) = assertion.bounds {
        if node.bounds != expected_bounds {
            return Evaluated::fail(
                "text",
                matched,
                AssertionDiff {
                    expected: json!(expected_bounds),
                    actual: json!(node.bounds),
                    path: Some(node_path(index, "bounds")),
                    detail: Some("bounds did not match".to_string()),
                },
                BackendError::Failed {
                    code: ErrorCode::AssertionFailed,
                    detail: format!(
                        "expected bounds {expected_bounds}, got {}",
                        node.bounds
                    ),
                },
            );
        }
    }
    Evaluated::pass(
        "text",
        matched,
        None,
        json!({
            "selector": assertion.selector,
            "text": assertion.text,
            "bounds": assertion.bounds,
        }),
        json!({
            "node": node.id,
            "text": node.text,
            "hint": node.hint,
            "bounds": node.bounds,
            "visible": node.visible,
        }),
    )
}

/// Evaluate a `visible` assertion (optional exact bounds).
pub fn eval_visible(assertion: &VisibilityAssertion, observation: &SessionObservation) -> Evaluated {
    let Some((index, node)) = locate(observation, &assertion.selector) else {
        if observation.tree.find_invisible(&assertion.selector).is_some() {
            return offscreen_error("visible", &assertion.selector, observation);
        }
        return Evaluated::fail(
            "visible",
            Vec::new(),
            AssertionDiff {
                expected: json!({"selector": assertion.selector, "visible": true}),
                actual: Value::Null,
                path: None,
                detail: Some("selector matched no node".to_string()),
            },
            missing_error(Some(observation), &assertion.selector),
        );
    };
    let matched = vec![node.id.clone()];
    if !node.visible {
        return Evaluated::fail(
            "visible",
            matched,
            AssertionDiff {
                expected: json!(true),
                actual: json!(false),
                path: Some(node_path(index, "visible")),
                detail: Some("node is not visible".to_string()),
            },
            BackendError::Failed {
                code: ErrorCode::AssertionFailed,
                detail: "node is present but not visible".to_string(),
            },
        );
    }
    if let Some(expected_bounds) = assertion.bounds {
        if node.bounds != expected_bounds {
            return Evaluated::fail(
                "visible",
                matched,
                AssertionDiff {
                    expected: json!(expected_bounds),
                    actual: json!(node.bounds),
                    path: Some(node_path(index, "bounds")),
                    detail: Some("bounds did not match".to_string()),
                },
                BackendError::Failed {
                    code: ErrorCode::AssertionFailed,
                    detail: format!("expected bounds {expected_bounds}, got {}", node.bounds),
                },
            );
        }
    }
    Evaluated::pass(
        "visible",
        matched,
        None,
        json!({
            "selector": assertion.selector,
            "visible": true,
            "bounds": assertion.bounds,
        }),
        json!({
            "node": node.id,
            "visible": node.visible,
            "bounds": node.bounds,
            "visible_bounds": node.visible_bounds,
        }),
    )
}

/// Evaluate a `not_visible` assertion.
pub fn eval_not_visible(
    assertion: &VisibilityAssertion,
    observation: &SessionObservation,
) -> Evaluated {
    // `not_visible` is a check *about* invisibility, so it must be able to see
    // the invisible node it is asserting about (otherwise every off-screen node
    // would trivially "pass" as "absent", which is the same lie in reverse).
    let Some((index, node)) = locate_including_invisible(observation, &assertion.selector) else {
        // Absent is the expected state.
        return Evaluated::pass(
            "not_visible",
            Vec::new(),
            Some("node absent".to_string()),
            json!({"selector": assertion.selector, "visible": false}),
            json!({"present": false, "visible": Value::Null}),
        );
    };
    let matched = vec![node.id.clone()];
    if node.visible {
        return Evaluated::fail(
            "not_visible",
            matched,
            AssertionDiff {
                expected: json!(false),
                actual: json!(true),
                path: Some(node_path(index, "visible")),
                detail: Some("node is visible".to_string()),
            },
            BackendError::Failed {
                code: ErrorCode::AssertionFailed,
                detail: "node is visible but was expected not to be".to_string(),
            },
        );
    }
    Evaluated::pass(
        "not_visible",
        matched,
        Some(format!(
            "node is present in the tree but not visible to the user (bounds {}, visible_bounds {})",
            node.bounds,
            node.visible_bounds
                .map_or_else(|| "<none>".to_string(), |b| b.to_string())
        )),
        json!({"selector": assertion.selector, "visible": false, "bounds": assertion.bounds}),
        json!({
            "node": node.id,
            "present": true,
            "visible": node.visible,
            "bounds": node.bounds,
            "visible_bounds": node.visible_bounds,
        }),
    )
}

/// Evaluate a `state` assertion (all specified flags must match).
pub fn eval_state(assertion: &StateAssertion, observation: &SessionObservation) -> Evaluated {
    // A state check about an explicitly invisible node (`visible: false`) must
    // be able to reach it; every other state check matches visible nodes only.
    let located = if assertion.visible == Some(false) {
        locate_including_invisible(observation, &assertion.selector)
    } else {
        locate(observation, &assertion.selector)
    };
    let Some((index, node)) = located else {
        return Evaluated::fail(
            "state",
            Vec::new(),
            AssertionDiff {
                expected: json!({"selector": assertion.selector}),
                actual: Value::Null,
                path: None,
                detail: Some("selector matched no node".to_string()),
            },
            missing_error(Some(observation), &assertion.selector),
        );
    };
    let matched = vec![node.id.clone()];
    let checks: [(&str, Option<bool>, bool); 7] = [
        ("visible", assertion.visible, node.visible),
        ("enabled", assertion.enabled, node.enabled),
        ("focused", assertion.focused, node.focused),
        ("clickable", assertion.clickable, node.clickable),
        ("scrollable", assertion.scrollable, node.scrollable),
        ("checked", assertion.checked, node.checked),
        ("selected", assertion.selected, node.selected),
    ];
    for (field, expected, actual) in checks {
        if let Some(expected) = expected {
            if expected != actual {
                return Evaluated::fail(
                    "state",
                    matched,
                    AssertionDiff {
                        expected: json!({field: expected}),
                        actual: json!({field: actual}),
                        path: Some(node_path(index, field)),
                        detail: Some(format!("{field} mismatch")),
                    },
                    BackendError::Failed {
                        code: ErrorCode::AssertionFailed,
                        detail: format!("expected {field}={expected}, got {actual}"),
                    },
                );
            }
        }
    }
    Evaluated::pass(
        "state",
        matched,
        None,
        json!({
            "selector": assertion.selector,
            "visible": assertion.visible,
            "enabled": assertion.enabled,
            "focused": assertion.focused,
            "clickable": assertion.clickable,
            "scrollable": assertion.scrollable,
            "checked": assertion.checked,
            "selected": assertion.selected,
        }),
        json!({
            "node": node.id,
            "visible": node.visible,
            "enabled": node.enabled,
            "focused": node.focused,
            "clickable": node.clickable,
            "scrollable": node.scrollable,
            "checked": node.checked,
            "selected": node.selected,
        }),
    )
}

/// Evaluate a `log` assertion against a log slice.
pub fn eval_log(assertion: &LogAssertion, logs: &[LogRecord]) -> Evaluated {
    let filtered: Vec<&LogRecord> = logs
        .iter()
        .filter(|record| {
            assertion
                .tag
                .as_deref()
                .map(|tag| record.tag == tag)
                .unwrap_or(true)
        })
        .collect();
    let regexes: Result<Vec<Regex>, _> = assertion
        .patterns
        .iter()
        .map(|pattern| Regex::new(pattern))
        .collect();
    let Ok(regexes) = regexes else {
        return Evaluated::fail(
            "log",
            Vec::new(),
            AssertionDiff {
                expected: json!(assertion.patterns),
                actual: json!(filtered
                    .iter()
                    .map(|record| format!("{}: {}", record.tag, record.message))
                    .collect::<Vec<String>>()),
                path: None,
                detail: Some("invalid log pattern".to_string()),
            },
            BackendError::Failed {
                code: ErrorCode::AssertionFailed,
                detail: "invalid log pattern".to_string(),
            },
        );
    };
    let min_matches = assertion.min_matches.max(1);

    let passed = if assertion.ordered {
        ordered_log_match(&regexes, &filtered, min_matches)
    } else {
        regexes.iter().all(|regex| {
            filtered
                .iter()
                .filter(|record| regex.is_match(&record.message))
                .count()
                >= min_matches
        })
    };

    if passed {
        let samples: Vec<String> = filtered
            .iter()
            .filter(|record| regexes.iter().any(|regex| regex.is_match(&record.message)))
            .take(5)
            .map(|record| format!("{}: {}", record.tag, record.message))
            .collect();
        return Evaluated::pass(
            "log",
            Vec::new(),
            None,
            json!({
                "tag": assertion.tag,
                "patterns": assertion.patterns,
                "ordered": assertion.ordered,
                "min_matches": min_matches,
                "since_previous_step": assertion.since_previous_step,
            }),
            json!({
                "matched_lines": filtered
                    .iter()
                    .filter(|record| regexes.iter().any(|regex| regex.is_match(&record.message)))
                    .count(),
                "sample": samples,
            }),
        );
    }
    Evaluated::fail(
        "log",
        Vec::new(),
        AssertionDiff {
            expected: json!({"patterns": assertion.patterns, "ordered": assertion.ordered, "min_matches": min_matches}),
            actual: json!(filtered
                .iter()
                .map(|record| format!("{}: {}", record.tag, record.message))
                .collect::<Vec<String>>()),
            path: None,
            detail: Some(if assertion.ordered {
                "log patterns did not appear in the required order".to_string()
            } else {
                "one or more log patterns did not match".to_string()
            }),
        },
        BackendError::Failed {
            code: ErrorCode::AssertionFailed,
            detail: "log assertion did not hold".to_string(),
        },
    )
}

/// Sequential ordered match: each pattern must match `min_matches` times before
/// the next pattern may match. Extra lines are allowed.
fn ordered_log_match(regexes: &[Regex], logs: &[&LogRecord], min_matches: usize) -> bool {
    let mut pattern = 0usize;
    let mut matched_for_pattern = 0usize;
    for record in logs {
        if pattern >= regexes.len() {
            break;
        }
        if regexes[pattern].is_match(&record.message) {
            matched_for_pattern += 1;
            if matched_for_pattern >= min_matches {
                pattern += 1;
                matched_for_pattern = 0;
            }
        }
    }
    pattern >= regexes.len()
}

/// Evaluate an `assert_no_crash` step.
pub fn eval_no_crash(crashed: Option<&CrashRecord>) -> Evaluated {
    match crashed {
        None => Evaluated::pass(
            "no_crash",
            Vec::new(),
            Some("no crash recorded".to_string()),
            json!("no crash"),
            json!({"crashes": 0}),
        ),
        Some(record) => Evaluated::fail(
            "no_crash",
            Vec::new(),
            AssertionDiff {
                expected: json!("no crash"),
                actual: json!({"exception": record.exception, "message": record.message}),
                path: None,
                detail: Some("a crash was recorded".to_string()),
            },
            BackendError::Crash {
                exception: record.exception.clone(),
                message: record.message.clone(),
                stack: record.stack.clone(),
                fatal: record.fatal,
            },
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use acos_protocol::assertion::TextAssertion;
    use acos_protocol::events::LogLevel;
    use acos_protocol::ui::{Bounds, Selector, TextMatch, UiNode, UiTree};
    use acos_protocol::FrameMeta;

    fn observation() -> SessionObservation {
        let result = UiNode::new("n1", "android.widget.TextView", Bounds::new(42, 612, 175, 663))
            .with_resource_id("dev.acos.fixtures.helloviews:id/result")
            .with_text("clicks: 0");
        let frame = FrameMeta::new(1, "2026-11-10T13:51:14.000Z", "req:launch:0", 1080, 2400, 420, "abc", true);
        let tree = UiTree::new(frame.clone(), "dev.acos.fixtures.helloviews", vec![result]);
        SessionObservation {
            session_id: "s_test".to_string(),
            package: "dev.acos.fixtures.helloviews".to_string(),
            frame,
            tree,
            screenshot_png: None,
            crashed: None,
            input: None,
        }
    }

    #[test]
    fn text_assertion_passes_on_match() {
        let evaluated = eval_text(
            &TextAssertion {
                selector: Selector::by_id("result"),
                text: TextMatch::exact("clicks: 0"),
                bounds: Some(Bounds::new(42, 612, 175, 663)),
                timeout_ms: None,
            },
            &observation(),
        );
        assert!(evaluated.result.passed);
        assert!(evaluated.error.is_none());
    }

    #[test]
    fn text_assertion_fails_on_wrong_text() {
        let evaluated = eval_text(
            &TextAssertion {
                selector: Selector::by_id("result"),
                text: TextMatch::exact("clicks: 9"),
                bounds: None,
                timeout_ms: None,
            },
            &observation(),
        );
        assert!(!evaluated.result.passed);
        assert_eq!(
            evaluated.error.as_ref().unwrap().code(),
            ErrorCode::AssertionFailed
        );
    }

    #[test]
    fn a_passing_text_assertion_echoes_expected_and_actual_geometry() {
        let evaluated = eval_text(
            &TextAssertion {
                selector: Selector::by_id("result"),
                text: TextMatch::exact("clicks: 0"),
                bounds: Some(Bounds::new(42, 612, 175, 663)),
                timeout_ms: None,
            },
            &observation(),
        );
        assert!(evaluated.result.passed);
        let expected = evaluated.result.expected.as_ref().expect("expected is echoed");
        let actual = evaluated.result.actual.as_ref().expect("actual is echoed");
        // What was asserted, from the artifacts alone.
        assert_eq!(expected["text"]["exact"], "clicks: 0");
        assert_eq!(expected["bounds"], serde_json::json!([42, 612, 175, 663]));
        assert_eq!(expected["selector"]["id"], "result");
        // What the run actually saw.
        assert_eq!(actual["text"], "clicks: 0");
        assert_eq!(actual["bounds"], serde_json::json!([42, 612, 175, 663]));
        assert_eq!(actual["node"], "n1");
        // A pass carries no failure diff.
        assert!(evaluated.result.diff.is_none());
    }

    #[test]
    fn a_passing_state_assertion_echoes_every_requested_flag() {
        let evaluated = eval_state(
            &StateAssertion {
                selector: Selector::by_id("result"),
                visible: Some(true),
                enabled: Some(true),
                focused: None,
                clickable: None,
                scrollable: None,
                checked: None,
                selected: None,
                timeout_ms: None,
            },
            &observation(),
        );
        assert!(evaluated.result.passed);
        let expected = evaluated.result.expected.as_ref().unwrap();
        let actual = evaluated.result.actual.as_ref().unwrap();
        assert_eq!(expected["visible"], true);
        assert_eq!(expected["enabled"], true);
        assert!(expected["focused"].is_null(), "only requested flags");
        assert_eq!(actual["visible"], true);
        assert_eq!(actual["enabled"], true);
        assert_eq!(actual["node"], "n1");
    }

    #[test]
    fn a_passing_not_visible_assertion_echoes_the_expectation() {
        let evaluated = eval_not_visible(
            &VisibilityAssertion {
                selector: Selector::by_text("Hidden"),
                bounds: None,
                timeout_ms: None,
            },
            &offscreen_observation(),
        );
        assert!(evaluated.result.passed);
        assert_eq!(
            evaluated.result.expected.as_ref().unwrap()["visible"],
            false
        );
        let actual = evaluated.result.actual.as_ref().unwrap();
        assert_eq!(actual["present"], true);
        assert_eq!(actual["visible"], false);
        assert_eq!(actual["node"], "n2");
    }

    #[test]
    fn selector_miss_messages_use_the_human_selector_form() {
        let selector = Selector::by_text("Nonexistent");
        let error = missing_error(Some(&observation()), &selector);
        let message = error.to_protocol_error().message;
        assert!(message.contains("text==\"Nonexistent\""), "{message}");
        assert!(!message.contains("Selector {"), "{message}");
        assert!(!message.contains("TextMatch {"), "{message}");

        // The same in the offscreen (ELEMENT_OFFSCREEN) message every agent
        // reads when triaging a stale selector.
        let mut selector = Selector::by_text("Hidden");
        selector.content_desc = Some(TextMatch::exact("X"));
        let error = offscreen_error("text", &selector, &offscreen_observation());
        let message = error.error.unwrap().to_protocol_error().message;
        assert!(message.contains("text==\"Hidden\""), "{message}");
        assert!(message.contains("content_desc==\"X\""), "{message}");
        assert!(!message.contains("Selector {"), "{message}");
    }

    #[test]
    fn text_assertion_fails_on_wrong_bounds() {
        let evaluated = eval_text(
            &TextAssertion {
                selector: Selector::by_id("result"),
                text: TextMatch::exact("clicks: 0"),
                bounds: Some(Bounds::new(0, 0, 1, 1)),
                timeout_ms: None,
            },
            &observation(),
        );
        assert!(!evaluated.result.passed);
        assert_eq!(
            evaluated.result.diff.as_ref().unwrap().path.as_deref(),
            Some("nodes[0].bounds")
        );
    }

    /// GR2 W4: a warm-mounted page parked off screen is in the tree but is not a
    /// match, and `assert_visible` on it is `ELEMENT_OFFSCREEN`, never a pass.
    fn offscreen_observation() -> SessionObservation {
        let visible = UiNode::new("n1", "android.widget.TextView", Bounds::new(42, 317, 196, 368))
            .with_text("Page one")
            .with_visible_bounds(Bounds::new(42, 317, 196, 368));
        let mut hidden =
            UiNode::new("n2", "android.widget.TextView", Bounds::new(1122, 317, 1239, 368))
                .with_text("Hidden");
        hidden.visible = false;
        let frame = FrameMeta::new(1, "2026-11-13T10:00:00.000Z", "req:launch:0", 1080, 2400, 420, "abc", true);
        let tree = UiTree::new(
            frame.clone(),
            "dev.acos.fixtures.offscreenpager",
            vec![visible, hidden],
        );
        SessionObservation {
            session_id: "s_test".to_string(),
            package: "dev.acos.fixtures.offscreenpager".to_string(),
            frame,
            tree,
            screenshot_png: None,
            crashed: None,
            input: None,
        }
    }

    #[test]
    fn assert_visible_on_an_offscreen_node_is_element_offscreen_not_a_pass() {
        let observation = offscreen_observation();
        let evaluated = eval_visible(
            &VisibilityAssertion {
                selector: Selector::by_text("Hidden"),
                bounds: None,
                timeout_ms: None,
            },
            &observation,
        );
        assert!(!evaluated.result.passed);
        assert_eq!(
            evaluated.error.as_ref().unwrap().code(),
            ErrorCode::ElementOffscreen
        );
        let detail = evaluated.error.as_ref().unwrap().to_protocol_error().message;
        assert!(detail.contains("1122"), "names the off-screen bounds: {detail}");
        // The old false pass: the node used to be `visible: true`, so this hit.
        assert!(observation.tree.find_invisible(&Selector::by_text("Hidden")).is_some());
        assert!(observation.tree.find_one(&Selector::by_text("Hidden")).is_none());
    }

    #[test]
    fn assert_not_visible_sees_the_present_but_offscreen_node() {
        let evaluated = eval_not_visible(
            &VisibilityAssertion {
                selector: Selector::by_text("Hidden"),
                bounds: None,
                timeout_ms: None,
            },
            &offscreen_observation(),
        );
        assert!(evaluated.result.passed);
        let message = evaluated.result.message.as_deref().unwrap_or_default();
        assert!(
            message.contains("not visible to the user"),
            "honest message, not 'absent': {message}"
        );
    }

    #[test]
    fn assert_text_on_an_offscreen_node_is_element_offscreen() {
        let evaluated = eval_text(
            &TextAssertion {
                selector: Selector::by_text("Hidden"),
                text: TextMatch::exact("Hidden"),
                bounds: None,
                timeout_ms: None,
            },
            &offscreen_observation(),
        );
        assert!(!evaluated.result.passed);
        assert_eq!(
            evaluated.error.as_ref().unwrap().code(),
            ErrorCode::ElementOffscreen
        );
    }

    #[test]
    fn selectors_match_the_visible_node_first() {
        let observation = offscreen_observation();
        let matched = observation
            .tree
            .find_one(&Selector::by_text("Page one"))
            .expect("the on-screen node matches");
        assert!(matched.visible);
        // A selector that only matches the off-screen node matches nothing.
        let mut both = Selector::by_text("Hidden");
        both.include_invisible = true;
        assert_eq!(observation.tree.find_all(&both).len(), 1);
    }

    #[test]
    fn truncated_tree_reports_tree_truncated_not_element_not_found() {
        let selector = Selector::by_id("does_not_exist");
        let assertion = VisibilityAssertion {
            selector: selector.clone(),
            bounds: None,
            timeout_ms: None,
        };
        // Same missing selector, same tree shape: only `truncated` differs.
        let plain = eval_visible(&assertion, &observation());
        assert_eq!(
            plain.error.as_ref().unwrap().code(),
            ErrorCode::ElementNotFound
        );
        let mut truncated_observation = observation();
        truncated_observation.tree.truncated = true;
        let truncated = eval_visible(&assertion, &truncated_observation);
        assert_eq!(
            truncated.error.as_ref().unwrap().code(),
            ErrorCode::TreeTruncated
        );
        assert!(truncated
            .error
            .as_ref()
            .unwrap()
            .to_protocol_error()
            .message
            .contains("truncated"));
        assert!(!plain
            .error
            .as_ref()
            .unwrap()
            .to_protocol_error()
            .message
            .contains("truncated"));
        // The distinct code is honest about what it means: never retryable as a
        // stale ref, and the hint names the node budget.
        assert!(!ErrorCode::TreeTruncated.retryable());
        assert!(ErrorCode::TreeTruncated.hint().contains("2000"));
        assert!(ErrorCode::TreeTruncated.hint().contains("budget"));
    }

    #[test]
    fn missing_node_is_element_not_found() {
        let evaluated = eval_visible(
            &VisibilityAssertion {
                selector: Selector::by_id("does_not_exist"),
                bounds: None,
                timeout_ms: None,
            },
            &observation(),
        );
        assert!(!evaluated.result.passed);
        assert_eq!(
            evaluated.error.as_ref().unwrap().code(),
            ErrorCode::ElementNotFound
        );
    }

    #[test]
    fn ordered_logs_require_order() {
        let logs = vec![
            LogRecord {
                seq: 1,
                timestamp_ms: 1,
                level: LogLevel::Info,
                tag: "AcosFixture".to_string(),
                message: "onCreate".to_string(),
            },
            LogRecord {
                seq: 2,
                timestamp_ms: 2,
                level: LogLevel::Info,
                tag: "AcosFixture".to_string(),
                message: "onStart".to_string(),
            },
            LogRecord {
                seq: 3,
                timestamp_ms: 3,
                level: LogLevel::Info,
                tag: "AcosFixture".to_string(),
                message: "onResume".to_string(),
            },
        ];
        let good = assert_log(&logs, &["^onCreate$", "^onStart$", "^onResume$"]);
        assert!(good.result.passed);
        let bad = assert_log(&logs, &["^onResume$", "^onStart$", "^onCreate$"]);
        assert!(!bad.result.passed);
    }

    fn assert_log(logs: &[LogRecord], patterns: &[&str]) -> Evaluated {
        eval_log(
            &LogAssertion {
                tag: Some("AcosFixture".to_string()),
                patterns: patterns.iter().map(|p| p.to_string()).collect(),
                ordered: true,
                since_previous_step: false,
                timeout_ms: None,
                min_matches: 1,
            },
            logs,
        )
    }
}
