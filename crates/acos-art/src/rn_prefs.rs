//! METRO5: point a debug React Native app at its dev server — the ACOS side of
//! React Native's own "Debug server host & port for device" setting.
//!
//! React Native reads `debug_http_host` from the app's **default**
//! SharedPreferences (`PackagerConnectionSettings.debugServerHost`) before it
//! falls back to the emulator guess `10.0.2.2:8081`. ACOS is not the stock
//! emulator's network and never emulates `10.0.2.2`; instead, when
//! `ACOS_RN_DEBUG_HTTP_HOST=localhost:<port>` is set, the launcher writes that
//! preference into the session's private `/data/app-data/<package>` before the
//! guest boots, and `acos_net {action: hostport, port}` (the `adb reverse`
//! equivalent) makes `localhost:<port>` inside the session reach the host.
//!
//! Unset (every non-RN, release or ordinary run): nothing is read, written or
//! spawned.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// The launcher seam: `host:port` the debug app should use.
pub const DEBUG_HTTP_HOST_ENV: &str = "ACOS_RN_DEBUG_HTTP_HOST";

/// The preference key React Native reads.
pub const DEBUG_HTTP_HOST_KEY: &str = "debug_http_host";

/// `<data_dir>/shared_prefs/<package>_preferences.xml`.
#[must_use]
pub fn default_prefs_path(data_dir: &Path, package: &str) -> PathBuf {
    data_dir
        .join("shared_prefs")
        .join(format!("{package}_preferences.xml"))
}

/// A `host:port` the app can dial: a DNS name / IPv4 literal and a port.
#[must_use]
pub fn valid_host_port(value: &str) -> bool {
    let Some((host, port)) = value.rsplit_once(':') else {
        return false;
    };
    !host.is_empty()
        && host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
        && port.parse::<u16>().is_ok_and(|port| port > 0)
}

fn escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c => out.push(c),
        }
    }
    out
}

/// Set `<string name="key">value</string>` in a SharedPreferences XML document,
/// keeping every other entry verbatim. `existing = None` starts a new map.
///
/// A document that is not a `<map>` is refused (never clobbered: it is the
/// app's own state).
pub fn merge_string_pref(existing: Option<&str>, key: &str, value: &str) -> Result<String, String> {
    let element = format!(
        "    <string name=\"{}\">{}</string>\n",
        escape(key),
        escape(value)
    );
    let Some(text) = existing.filter(|text| !text.trim().is_empty()) else {
        return Ok(format!(
            "<?xml version='1.0' encoding='utf-8' standalone='yes' ?>\n<map>\n{element}</map>\n"
        ));
    };
    if text.contains("<map />") || text.contains("<map/>") {
        let empty = if text.contains("<map />") {
            "<map />"
        } else {
            "<map/>"
        };
        return Ok(text.replacen(empty, &format!("<map>\n{element}</map>"), 1));
    }
    let close = text
        .rfind("</map>")
        .ok_or_else(|| "not a SharedPreferences document (no </map>)".to_string())?;
    if !text.contains("<map>") {
        return Err("not a SharedPreferences document (no <map>)".to_string());
    }
    // Drop every existing entry for the key (any type), then append ours.
    let mut body = String::with_capacity(text.len() + element.len());
    let mut rest = &text[..close];
    let needle = format!("name=\"{}\"", escape(key));
    while let Some(at) = rest.find(&needle) {
        let Some(start) = rest[..at].rfind('<') else {
            break;
        };
        let end = start + element_end(&rest[start..]).ok_or_else(|| format!("unterminated entry for `{key}`"))?;
        // the element's own line (indentation + newline) goes with it
        let line_start = rest[..start].rfind('\n').map_or(start, |i| i + 1);
        let line_start = if rest[line_start..start].trim().is_empty() {
            line_start
        } else {
            start
        };
        let end = if rest[end..].starts_with('\n') {
            end + 1
        } else {
            end
        };
        body.push_str(&rest[..line_start]);
        rest = &rest[end..];
    }
    body.push_str(rest);
    if !body.ends_with('\n') {
        body.push('\n');
    }
    body.push_str(&element);
    body.push_str(&text[close..]);
    Ok(body)
}

/// The byte length of the element that starts at `text[0] == '<'`: a
/// self-closing `<tag ... />`, or `<tag ...>` up to its **matching** `</tag>`
/// (same-name nesting counted), so a `<set name="k"><string>v</string></set>`
/// is removed whole instead of at its first inner `</string>`.
fn element_end(text: &str) -> Option<usize> {
    let open_end = text.find('>')?;
    if text[..open_end].ends_with('/') {
        return Some(open_end + 1);
    }
    let name: String = text[1..]
        .chars()
        .take_while(|c| !c.is_whitespace() && *c != '>' && *c != '/')
        .collect();
    if name.is_empty() {
        return None;
    }
    let (open, close) = (format!("<{name}"), format!("</{name}>"));
    let mut depth = 1usize;
    let mut cursor = open_end + 1;
    while depth > 0 {
        let rest = &text[cursor..];
        let next_close = rest.find(&close)?;
        // a nested opening tag of the same name before that close
        let nested = rest[..next_close].match_indices(&open).find(|(i, _)| {
            rest[i + open.len()..]
                .chars()
                .next()
                .is_some_and(|c| c.is_whitespace() || c == '>' || c == '/')
        });
        match nested {
            Some((i, _)) => {
                let tag_end = rest[i..].find('>')? + i;
                if !rest[..tag_end].ends_with('/') {
                    depth += 1;
                }
                cursor += tag_end + 1;
            }
            None => {
                depth -= 1;
                cursor += next_close + close.len();
            }
        }
    }
    Some(cursor)
}

/// Write `debug_http_host = host_port` into `<data_dir>`'s default prefs.
pub fn write_debug_http_host(
    data_dir: &Path,
    package: &str,
    host_port: &str,
) -> io::Result<PathBuf> {
    if !valid_host_port(host_port) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{DEBUG_HTTP_HOST_ENV}={host_port:?} is not host:port"),
        ));
    }
    let path = default_prefs_path(data_dir, package);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let existing = match fs::read_to_string(&path) {
        Ok(text) => Some(text),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    let text = merge_string_pref(existing.as_deref(), DEBUG_HTTP_HOST_KEY, host_port)
        .map_err(|message| io::Error::new(io::ErrorKind::InvalidData, message))?;
    let staging = path.with_extension("xml.acos-tmp");
    fs::write(&staging, text)?;
    fs::rename(&staging, &path)?;
    Ok(path)
}

/// The launcher hook: apply [`DEBUG_HTTP_HOST_ENV`] when it is set.
///
/// Unset (or empty) is `Ok(None)` and does no work. Set, it is an explicit
/// request: an invalid value or a failed write is an `Err` the launch must
/// report, never a log line next to a session that silently dials the wrong host.
pub fn apply_from_env(data_dir: &Path, package: &str) -> io::Result<Option<String>> {
    let Ok(value) = std::env::var(DEBUG_HTTP_HOST_ENV) else {
        return Ok(None);
    };
    apply_value(data_dir, package, &value)
}

fn apply_value(data_dir: &Path, package: &str, value: &str) -> io::Result<Option<String>> {
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }
    let path = write_debug_http_host(data_dir, package, value).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("{DEBUG_HTTP_HOST_ENV} was set but debug_http_host was not written: {error}"),
        )
    })?;
    Ok(Some(format!("rn: debug_http_host={value} -> {}", path.display())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_prefs_file() {
        let text = merge_string_pref(None, DEBUG_HTTP_HOST_KEY, "localhost:8081").unwrap();
        assert_eq!(
            text,
            "<?xml version='1.0' encoding='utf-8' standalone='yes' ?>\n<map>\n    <string name=\"debug_http_host\">localhost:8081</string>\n</map>\n"
        );
    }

    #[test]
    fn merge_keeps_other_prefs_and_replaces_the_key() {
        let existing = "<?xml version='1.0' encoding='utf-8' standalone='yes' ?>\n<map>\n    <boolean name=\"js_dev_mode_debug\" value=\"true\" />\n    <string name=\"debug_http_host\">10.0.2.2:8081</string>\n    <string name=\"theme\">dark &amp; bold</string>\n</map>\n";
        let text =
            merge_string_pref(Some(existing), DEBUG_HTTP_HOST_KEY, "localhost:19000").unwrap();
        assert!(text.contains("<boolean name=\"js_dev_mode_debug\" value=\"true\" />"));
        assert!(text.contains("<string name=\"theme\">dark &amp; bold</string>"));
        assert!(!text.contains("10.0.2.2"), "{text}");
        assert_eq!(text.matches("debug_http_host").count(), 1, "{text}");
        assert!(text.contains("<string name=\"debug_http_host\">localhost:19000</string>\n</map>"));
        // idempotent
        assert_eq!(
            merge_string_pref(Some(&text), DEBUG_HTTP_HOST_KEY, "localhost:19000").unwrap(),
            text
        );
        // an empty map
        let text = merge_string_pref(Some("<?xml version='1.0' ?>\n<map />\n"), "k", "v").unwrap();
        assert!(
            text.contains("<map>\n    <string name=\"k\">v</string>\n</map>"),
            "{text}"
        );
    }

    #[test]
    fn a_nested_set_entry_is_removed_whole() {
        let existing = "<?xml version='1.0' ?>\n<map>\n    <set name=\"debug_http_host\">\n        <string>old</string>\n        <string>older</string>\n    </set>\n    <string name=\"keep\">yes</string>\n</map>\n";
        let text = merge_string_pref(Some(existing), DEBUG_HTTP_HOST_KEY, "localhost:8081").unwrap();
        assert!(!text.contains("old"), "{text}");
        assert!(!text.contains("</set>"), "{text}");
        assert!(text.contains("<string name=\"keep\">yes</string>"), "{text}");
        assert_eq!(
            text,
            "<?xml version='1.0' ?>\n<map>\n    <string name=\"keep\">yes</string>\n    <string name=\"debug_http_host\">localhost:8081</string>\n</map>\n"
        );
        // a self-closing entry
        let text = merge_string_pref(
            Some("<map>\n    <int name=\"debug_http_host\" value=\"3\" />\n</map>\n"),
            DEBUG_HTTP_HOST_KEY,
            "localhost:1",
        )
        .unwrap();
        assert_eq!(text, "<map>\n    <string name=\"debug_http_host\">localhost:1</string>\n</map>\n");
        // unterminated is refused, never clobbered
        assert!(merge_string_pref(Some("<map><set name=\"debug_http_host\"><string>x</string></map>"), DEBUG_HTTP_HOST_KEY, "a:1").is_err());
    }

    #[test]
    fn an_explicit_invalid_request_is_an_error_and_unset_is_a_no_op() {
        let dir = std::env::temp_dir().join(format!("acos-rn-prefs-env-{}", std::process::id()));
        assert!(apply_value(&dir, "p", "").unwrap().is_none());
        assert!(apply_value(&dir, "p", "   ").unwrap().is_none());
        let error = apply_value(&dir, "p", "10.0.2.2").unwrap_err();
        assert!(error.to_string().contains("was set but debug_http_host was not written"), "{error}");
        assert!(!dir.exists(), "nothing is written for a refused value");
        assert!(apply_value(&dir, "p", "localhost:8081").unwrap().is_some());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn values_are_escaped_and_garbage_is_refused() {
        let text = merge_string_pref(None, "k", "a<b>&\"c'").unwrap();
        assert!(text.contains(">a&lt;b&gt;&amp;&quot;c&apos;<"), "{text}");
        assert!(merge_string_pref(Some("not xml at all"), "k", "v").is_err());
    }

    #[test]
    fn writes_into_the_data_dir_and_validates_host_port() {
        let dir = std::env::temp_dir().join(format!("acos-rn-prefs-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let path =
            write_debug_http_host(&dir, "dev.acos.fixtures.rnhello", "localhost:8081").unwrap();
        assert_eq!(
            path,
            dir.join("shared_prefs/dev.acos.fixtures.rnhello_preferences.xml")
        );
        assert!(fs::read_to_string(&path)
            .unwrap()
            .contains(">localhost:8081<"));
        for bad in ["localhost", "localhost:0", "local host:80", ":80", "a:b"] {
            assert!(write_debug_http_host(&dir, "p", bad).is_err(), "{bad}");
        }
        let _ = fs::remove_dir_all(&dir);
    }
}
