//! Unknown-argument rejection for every MCP tool.
//!
//! The bug this module exists for (feedback 2026-11-15): the session tools
//! advertised `"additionalProperties": true`, so `acos_observe_tree
//! {"screenshot": true}` on a build that had no `screenshot` argument looked
//! like a success — the caller believed it had a PNG, and the reply was an
//! ordinary tree. An unknown argument is a caller error and must be answered
//! as one, naming the argument and the names that do exist, so a typo costs one
//! error instead of a silently wrong run.
//!
//! Two layers, one rule:
//!
//! 1. every published schema says `"additionalProperties": false`
//!    ([`crate::descriptors`]), so a client that validates locally refuses the
//!    same call;
//! 2. [`check`] rejects it in the dispatcher ([`crate::server`]) regardless of
//!    what the client validated, because a schema is advice and the server is
//!    the authority.
//!
//! Only top-level argument names are checked: `base` is an object the protocol
//! owns and its own fields are typed there.

use serde_json::{json, Value};

use acos_protocol::{
    Backend as ProtocolBackend, Envelope, ErrorCode, Fidelity, Metrics, ProtocolError,
};

/// A `did you mean` suggestion for a misspelled argument, if one is close.
///
/// Case-insensitive edit distance 1 (one insertion/deletion/substitution) or a
/// prefix match, which covers the typos that actually happen
/// (`screenshot`/`screenshots`, `timeout` for `timeout_ms` is reported but not
/// guessed as a rename).
pub fn suggestion<'a>(unknown: &str, valid: &'a [String]) -> Option<&'a str> {
    let mut best: Option<(usize, &str)> = None;
    for name in valid {
        let distance = edit_distance(&unknown.to_ascii_lowercase(), &name.to_ascii_lowercase());
        let is_prefix = name.len() > unknown.len() && name.starts_with(unknown);
        let score = if distance <= 1 || is_prefix {
            Some(distance)
        } else {
            None
        };
        if let Some(score) = score {
            if best.is_none_or(|(current, _)| score < current) {
                best = Some((score, name.as_str()));
            }
        }
    }
    best.map(|(_, name)| name)
}

/// Levenshtein distance, capped at two (only "is it a typo" matters).
fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.len().abs_diff(b.len()) > 2 {
        return 3;
    }
    let mut previous: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut current = vec![i + 1];
        for (j, cb) in b.iter().enumerate() {
            let substitute = previous[j] + usize::from(ca != cb);
            let insert = current[j] + 1;
            let delete = previous[j + 1] + 1;
            current.push(substitute.min(insert).min(delete));
        }
        previous = current;
    }
    previous[b.len()]
}

/// Reject the call when `args` carries a top-level name `schema` does not
/// declare. Returns `None` for a valid call (or a schema without properties).
///
/// The error is `INVALID_ARGUMENT` — the caller's mistake, retryable after
/// fixing the argument — with `details.unknown` and `details.valid`, and a hint
/// that lists the valid names.
pub fn check(tool: &str, args: &Value, schema: &Value) -> Option<Envelope> {
    let object = args.as_object()?;
    let properties = schema.get("properties").and_then(Value::as_object)?;
    let mut unknown: Vec<&str> = object
        .keys()
        .filter(|name| !properties.contains_key(name.as_str()))
        .map(String::as_str)
        .collect();
    if unknown.is_empty() {
        return None;
    }
    unknown.sort_unstable();
    let mut valid: Vec<String> = properties.keys().cloned().collect();
    valid.sort();

    let mut message = format!(
        "unknown argument{} for `{tool}`: {}",
        if unknown.len() == 1 { "" } else { "s" },
        unknown
            .iter()
            .map(|name| format!("`{name}`"))
            .collect::<Vec<_>>()
            .join(", ")
    );
    if let Some(guess) = unknown
        .iter()
        .find_map(|name| suggestion(name, &valid).map(|guess| (*name, guess)))
    {
        message.push_str(&format!(" (did you mean `{}`?)", guess.1));
    }
    let error = ProtocolError::new(ErrorCode::InvalidArgument, message)
        .with_hint(format!(
            "Valid arguments for `{tool}`: {}. An unknown argument is rejected instead of ignored: \
             a call that silently drops an argument looks like a success for work that never \
             happened.",
            valid
                .iter()
                .map(|name| format!("`{name}`"))
                .collect::<Vec<_>>()
                .join(", ")
        ))
        .with_details(json!({
            "tool": tool,
            "unknown": unknown,
            "valid": valid,
            "field": "arguments",
        }));
    Some(Envelope::failure(
        "req_invalid_argument",
        ProtocolBackend::AcosNative,
        Fidelity::StaticAnalysis,
        error,
        Metrics::wall_ms(0),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schema() -> Value {
        json!({
            "type": "object",
            "properties": {
                "apk": {"type": "string"},
                "settle": {"type": "string"},
                "screenshot": {"type": "boolean"},
                "base": {"type": "object"}
            }
        })
    }

    #[test]
    fn known_arguments_pass() {
        assert!(check("acos_observe_tree", &json!({"screenshot": true}), &schema()).is_none());
        assert!(check("acos_observe_tree", &json!({}), &schema()).is_none());
    }

    #[test]
    fn an_unknown_argument_names_itself_and_the_valid_ones() {
        let envelope = check(
            "acos_observe",
            &json!({"apk": "a.apk", "screenshots": true}),
            &schema(),
        )
        .expect("rejected");
        assert_eq!(envelope.status().as_str(), "error");
        let error = envelope.error().expect("error");
        assert_eq!(error.code, ErrorCode::InvalidArgument);
        assert!(error.message.contains("`screenshots`"), "{}", error.message);
        assert!(error.hint.as_str().contains("`settle`"), "{:?}", error.hint);
        let details = error.details.clone().expect("details");
        assert_eq!(details["unknown"], json!(["screenshots"]));
        assert_eq!(
            details["valid"],
            json!(["apk", "base", "screenshot", "settle"])
        );
        // The near miss is named, so one retry is enough.
        assert!(
            error.message.contains("did you mean `screenshot`"),
            "{}",
            error.message
        );
    }

    #[test]
    fn a_far_off_name_is_still_rejected_without_a_guess() {
        let envelope = check("acos_tap", &json!({"x": 1, "y": 2, "nope": 3}), &schema()).unwrap();
        let error = envelope.error().unwrap();
        assert!(error.message.contains("`nope`"), "{}", error.message);
        assert!(!error.message.contains("did you mean"), "{}", error.message);
    }

    #[test]
    fn edit_distance_is_capped() {
        assert_eq!(edit_distance("apk", "apk"), 0);
        assert_eq!(edit_distance("apk", "apks"), 1);
        assert_eq!(edit_distance("settle", "settl"), 1);
        assert_eq!(edit_distance("completely-different", "apk"), 3);
    }
}
