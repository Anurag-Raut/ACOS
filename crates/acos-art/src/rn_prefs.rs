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
        let tail = &rest[at..];
        let end = match (tail.find("/>"), tail.find("</")) {
            (Some(self_close), Some(close_tag)) if self_close < close_tag => at + self_close + 2,
            (_, Some(close_tag)) => {
                let after = at + close_tag;
                after + rest[after..].find('>').map_or(0, |i| i + 1)
            }
            (Some(self_close), None) => at + self_close + 2,
            (None, None) => return Err(format!("unterminated entry for `{key}`")),
        };
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

/// The launcher hook: apply [`DEBUG_HTTP_HOST_ENV`] when it is set. Returns a
/// diagnostic line for the launch log (`None` when unset: zero work).
pub fn apply_from_env(data_dir: &Path, package: &str) -> Option<String> {
    let value = std::env::var(DEBUG_HTTP_HOST_ENV).ok()?;
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    Some(match write_debug_http_host(data_dir, package, value) {
        Ok(path) => format!("rn: debug_http_host={value} -> {}", path.display()),
        Err(error) => format!("rn: debug_http_host not written: {error}"),
    })
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
