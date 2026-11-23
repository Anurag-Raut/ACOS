//! Host-side adb shell translator (§4.2 of docs/DEVICE_PANEL_SPEC.md).
//!
//! There is no shell in the guest. Only the commands needed by T3 Code's
//! device hub (`expo-device-hub`) are answered from real session state;
//! everything else exits non-zero with `acos: unsupported: <cmd>`.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use acos_apk::Apk;
use acos_protocol::ui::{UiNode, UiTree};

/// Live session snapshot passed to shell translators.
#[derive(Debug, Clone)]
pub struct AdbSessionSnapshot {
    pub package: String,
    pub activity: Option<String>,
    pub natural_size: (u32, u32),
    pub display_size: (u32, u32),
    pub rotation: u8,
    pub density: u32,
    pub animations: &'static str,
    pub guest_pid: Option<u32>,
    pub launcher_pid: Option<u32>,
    pub apk_path: PathBuf,
}

/// Operations an exposed adb endpoint / gRPC bridge can invoke on its backing session.
pub trait AdbSessionOps: Send + Sync + 'static {
    fn snapshot(&self) -> Result<AdbSessionSnapshot, String>;
    fn rotate(&self, rotation: u8) -> Result<(), String>;
    fn dump_tree(&self) -> Result<(UiTree, u8), String>;
    fn screenshot_png(&self) -> Result<Vec<u8>, String>;
    fn raw_screenshot(&self, out_path: &Path, format: &str) -> Result<(u32, u32, u8), String>;
    fn panel_touch(
        &self,
        action: i32,
        down_time: i64,
        event_time: i64,
        pointers: &[(i32, f32, f32)],
    ) -> Result<(), String>;
    fn panel_key(
        &self,
        code: i32,
        event_type: &str,
        meta_state: i32,
        device_id: i32,
    ) -> Result<(), String>;
    fn panel_text(&self, text: &str) -> Result<(), String>;
    fn stream_socket(&self) -> Option<PathBuf>;
    fn is_verify_run(&self) -> bool;
    fn on_panel_input_ignored(&self, _kind: &str, _x: Option<i64>, _y: Option<i64>, _detail: &str) {}
}

/// Per-session host-side virtual shell state.
#[derive(Debug, Default)]
pub struct AdbShellState {
    pub user_rotation_locked: Option<u8>,
    pub virtual_files: HashMap<String, Vec<u8>>,
}

/// Result of executing one adb shell or exec command.
#[derive(Debug, Clone)]
pub struct ShellOutput {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub exit_code: u8,
}

impl ShellOutput {
    pub fn ok_text(text: impl Into<String>) -> Self {
        Self {
            stdout: text.into().into_bytes(),
            stderr: Vec::new(),
            exit_code: 0,
        }
    }

    pub fn ok_bytes(stdout: Vec<u8>) -> Self {
        Self {
            stdout,
            stderr: Vec::new(),
            exit_code: 0,
        }
    }

    pub fn err_text(code: u8, msg: impl Into<String>) -> Self {
        Self {
            stdout: Vec::new(),
            stderr: msg.into().into_bytes(),
            exit_code: code,
        }
    }

    pub fn unsupported(cmd: &str) -> Self {
        Self::err_text(1, format!("acos: unsupported: {cmd}\n"))
    }
}

/// Execute a shell or exec command against the given session.
pub fn run_command(
    raw_cmd: &str,
    target: &dyn AdbSessionOps,
    state: &Mutex<AdbShellState>,
) -> ShellOutput {
    let trimmed = raw_cmd.trim();
    if trimmed.is_empty() {
        return ShellOutput::err_text(1, "acos: interactive shell is not supported\n");
    }

    // Handle known pipeline forms first (`dumpsys input | grep Classes:`).
    if trimmed == "dumpsys input | grep Classes:" || trimmed == "dumpsys input|grep Classes:" {
        return ShellOutput::ok_text("  Classes: ALPHAKEY\n");
    }

    let tokens = split_shell_words(trimmed);
    if tokens.is_empty() {
        return ShellOutput::err_text(1, "acos: interactive shell is not supported\n");
    }
    let args: Vec<&str> = tokens.iter().map(String::as_str).collect();

    match args.as_slice() {
        ["wm", "size"] => {
            let Ok(snap) = target.snapshot() else {
                return ShellOutput::err_text(1, "acos: session unavailable\n");
            };
            ShellOutput::ok_text(format!(
                "Physical size: {}x{}\n",
                snap.natural_size.0, snap.natural_size.1
            ))
        }
        ["wm", "density"] => {
            let Ok(snap) = target.snapshot() else {
                return ShellOutput::err_text(1, "acos: session unavailable\n");
            };
            ShellOutput::ok_text(format!("Physical density: {}\n", snap.density))
        }
        ["dumpsys", "window", "displays"] => {
            let Ok(snap) = target.snapshot() else {
                return ShellOutput::err_text(1, "acos: session unavailable\n");
            };
            let deg = u32::from(snap.rotation % 4) * 90;
            ShellOutput::ok_text(format!(
                "DISPLAY MANAGER (dumpsys window displays)\n\
                 Display: mDisplayId=0\n  \
                 mDisplayRotation=ROTATION_{deg} mCurrentRotation=ROTATION_{deg} mRotation=ROTATION_{deg}}}\n"
            ))
        }
        ["dumpsys", "window"] => {
            let Ok(snap) = target.snapshot() else {
                return ShellOutput::err_text(1, "acos: session unavailable\n");
            };
            ShellOutput::ok_text(format_dumpsys_window(&snap))
        }
        ["dumpsys", "activity", "activities"] | ["dumpsys", "activity"] => {
            let Ok(snap) = target.snapshot() else {
                return ShellOutput::err_text(1, "acos: session unavailable\n");
            };
            ShellOutput::ok_text(format_dumpsys_activity(&snap))
        }
        ["dumpsys", "power"] => {
            ShellOutput::ok_text("Power Manager State:\n  mWakefulness=Awake\n")
        }
        ["dumpsys", "input"] => {
            ShellOutput::ok_text("InputReader State:\n  Device 1: acos-keyboard\n    Classes: ALPHAKEY\n")
        }
        ["dumpsys", "package", pkg] => {
            let Ok(snap) = target.snapshot() else {
                return ShellOutput::err_text(1, "acos: session unavailable\n");
            };
            if *pkg != snap.package {
                return ShellOutput::err_text(1, format!("Unable to find package: {pkg}\n"));
            }
            ShellOutput::ok_text(format_dumpsys_package(&snap))
        }
        ["cmd", "window", "user-rotation"] => {
            let guard = state.lock().unwrap_or_else(|e| e.into_inner());
            match guard.user_rotation_locked {
                Some(r) => ShellOutput::ok_text(format!("lock {r}\n")),
                None => ShellOutput::ok_text("free\n"),
            }
        }
        ["cmd", "window", "user-rotation", "free"] => {
            let mut guard = state.lock().unwrap_or_else(|e| e.into_inner());
            guard.user_rotation_locked = None;
            ShellOutput::ok_text("")
        }
        ["cmd", "window", "user-rotation", "lock", rot_str] => {
            let Ok(rot) = rot_str.parse::<u8>() else {
                return ShellOutput::unsupported(trimmed);
            };
            if rot > 3 {
                return ShellOutput::err_text(1, format!("acos: invalid rotation: {rot}\n"));
            }
            if let Err(err) = target.rotate(rot) {
                return ShellOutput::err_text(1, format!("acos: rotate failed: {err}\n"));
            }
            let mut guard = state.lock().unwrap_or_else(|e| e.into_inner());
            guard.user_rotation_locked = Some(rot);
            ShellOutput::ok_text("")
        }
        ["cmd", "uimode", "night"] => ShellOutput::ok_text("Night mode: no\n"),
        ["settings", "get", "system", "user_rotation"] => {
            let Ok(snap) = target.snapshot() else {
                return ShellOutput::err_text(1, "acos: session unavailable\n");
            };
            ShellOutput::ok_text(format!("{}\n", snap.rotation % 4))
        }
        ["settings", "get", "system", "font_scale"] => ShellOutput::ok_text("1.0\n"),
        ["settings", "get", "global", "wifi_on"] => ShellOutput::ok_text("1\n"),
        ["settings", "get", "global", "mobile_data"] => ShellOutput::ok_text("0\n"),
        ["settings", "get", "global", key]
            if matches!(
                *key,
                "transition_animation_scale"
                    | "window_animation_scale"
                    | "animator_duration_scale"
            ) =>
        {
            let Ok(snap) = target.snapshot() else {
                return ShellOutput::err_text(1, "acos: session unavailable\n");
            };
            let val = if snap.animations == "off" {
                "0.0\n"
            } else {
                "1.0\n"
            };
            ShellOutput::ok_text(val)
        }
        ["settings", "get", "secure", "high_text_contrast_enabled"] => ShellOutput::ok_text("0\n"),
        ["settings", "get", "secure", "font_weight_adjustment"] => ShellOutput::ok_text("0\n"),
        ["settings", "get", "secure", "show_ime_with_hard_keyboard"] => {
            ShellOutput::ok_text("1\n")
        }
        ["pidof", pkg] => {
            let Ok(snap) = target.snapshot() else {
                return ShellOutput::err_text(1, "acos: session unavailable\n");
            };
            if *pkg == snap.package {
                if let Some(pid) = snap.guest_pid.or(snap.launcher_pid) {
                    return ShellOutput::ok_text(format!("{pid}\n"));
                }
            }
            ShellOutput::err_text(1, "")
        }
        ["uiautomator", "dump"] => run_uiautomator_dump("/sdcard/window_dump.xml", target, state),
        ["uiautomator", "dump", path] => run_uiautomator_dump(path, target, state),
        ["cat", path] => {
            let guard = state.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(bytes) = guard.virtual_files.get(*path) {
                ShellOutput::ok_bytes(bytes.clone())
            } else {
                ShellOutput::err_text(1, format!("cat: {path}: No such file or directory\n"))
            }
        }
        ["rm", path] => run_rm(path, false, state),
        ["rm", "-f", path] | ["rm", "-rf", path] => run_rm(path, true, state),
        ["screencap", "-p"] | ["screencap", "-d", _, "-p"] => match target.screenshot_png() {
            Ok(bytes) => ShellOutput::ok_bytes(bytes),
            Err(err) => ShellOutput::err_text(1, format!("acos: screencap failed: {err}\n")),
        },
        ["pm", sub, ..] if sub.starts_with("install") => ShellOutput::err_text(
            1,
            "acos: install into a live session is not supported; launch a new session with the APK\n",
        ),
        ["cmd", "package", sub, ..] if sub.starts_with("install") => ShellOutput::err_text(
            1,
            "acos: install into a live session is not supported; launch a new session with the APK\n",
        ),
        _ => ShellOutput::unsupported(trimmed),
    }
}

fn run_uiautomator_dump(
    path: &str,
    target: &dyn AdbSessionOps,
    state: &Mutex<AdbShellState>,
) -> ShellOutput {
    let Ok(snap) = target.snapshot() else {
        return ShellOutput::err_text(1, "acos: session unavailable\n");
    };
    let (tree, rot) = match target.dump_tree() {
        Ok(res) => res,
        Err(err) => return ShellOutput::err_text(1, format!("acos: uiautomator dump failed: {err}\n")),
    };
    let xml = format_uiautomator_xml(&tree, rot % 4, &snap.package);
    let mut guard = state.lock().unwrap_or_else(|e| e.into_inner());
    // Cap virtual file map size in case callers dump repeatedly without rm.
    if guard.virtual_files.len() >= 16 && !guard.virtual_files.contains_key(path) {
        if let Some(oldest) = guard.virtual_files.keys().next().cloned() {
            guard.virtual_files.remove(&oldest);
        }
    }
    guard
        .virtual_files
        .insert(path.to_string(), xml.into_bytes());
    ShellOutput::ok_text(format!("UI hierchary dumped to: {path}\n"))
}

fn run_rm(path: &str, force: bool, state: &Mutex<AdbShellState>) -> ShellOutput {
    let mut guard = state.lock().unwrap_or_else(|e| e.into_inner());
    if guard.virtual_files.remove(path).is_some() || force {
        ShellOutput::ok_text("")
    } else {
        ShellOutput::err_text(1, format!("rm: {path}: No such file or directory\n"))
    }
}

fn format_component(snap: &AdbSessionSnapshot) -> Option<String> {
    let pkg = snap.package.trim();
    if pkg.is_empty() {
        return None;
    }
    let act = snap
        .activity
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(".MainActivity");
    let act_norm = if act.starts_with('.') || act.contains('.') {
        act.to_string()
    } else {
        format!(".{act}")
    };
    Some(format!("{pkg}/{act_norm}"))
}

fn format_dumpsys_window(snap: &AdbSessionSnapshot) -> String {
    match format_component(snap) {
        Some(comp) => format!(
            "WINDOW MANAGER WINDOWS (dumpsys window)\n  \
             mCurrentFocus=Window{{acos u0 {comp}}}\n  \
             mFocusedApp=ActivityRecord{{acos u0 {comp} t1}}\n"
        ),
        None => "WINDOW MANAGER WINDOWS (dumpsys window)\n  mCurrentFocus=null\n".to_string(),
    }
}

fn format_dumpsys_activity(snap: &AdbSessionSnapshot) -> String {
    match format_component(snap) {
        Some(comp) => format!(
            "ACTIVITY MANAGER ACTIVITIES (dumpsys activity activities)\n  \
             topResumedActivity=ActivityRecord{{acos u0 {comp} t1}}\n  \
             mResumedActivity: ActivityRecord{{acos u0 {comp} t1}}\n"
        ),
        None => "ACTIVITY MANAGER ACTIVITIES (dumpsys activity activities)\n".to_string(),
    }
}

fn format_dumpsys_package(snap: &AdbSessionSnapshot) -> String {
    let mut out = format!("Packages:\n  Package [{}] (acos):\n", snap.package);
    if let Ok(apk) = Apk::open(&snap.apk_path) {
        if let Ok(manifest) = apk.manifest() {
            let vc = manifest.version_code.unwrap_or(1);
            let min_sdk = manifest.min_sdk.unwrap_or(21);
            let target_sdk = manifest.target_sdk.unwrap_or(34);
            let _ = writeln!(
                out,
                "    versionCode={vc} minSdk={min_sdk} targetSdk={target_sdk}"
            );
            if let Some(vn) = &manifest.version_name {
                let _ = writeln!(out, "    versionName={vn}");
            }
            let res = apk.resources().ok().flatten();
            if let Some(label) = manifest.app_label(res.as_ref()) {
                let safe = label.replace('\'', " ");
                let _ = writeln!(out, "    application-label:'{safe}'");
            }
            if manifest.application.debuggable == Some(true) {
                out.push_str("    pkgFlags=[ DEBUGGABLE ]\n");
            } else {
                out.push_str("    pkgFlags=[ ]\n");
            }
            return out;
        }
    }
    out.push_str("    pkgFlags=[ ]\n");
    out
}

/// Render an ACOS [`UiTree`] in standard Android `uiautomator dump` XML.
pub fn format_uiautomator_xml(tree: &UiTree, rotation: u8, fallback_pkg: &str) -> String {
    let mut out = String::from("<?xml version='1.0' encoding='UTF-8' standalone='yes' ?>\n");
    let _ = writeln!(out, "<hierarchy rotation=\"{}\">", rotation % 4);
    for (idx, node) in tree.nodes.iter().enumerate() {
        write_xml_node(&mut out, node, idx, fallback_pkg, 1);
    }
    out.push_str("</hierarchy>\n");
    out
}

fn write_xml_node(out: &mut String, node: &UiNode, index: usize, fallback_pkg: &str, depth: usize) {
    for _ in 0..depth {
        out.push_str("  ");
    }
    let pkg = if node.package.is_empty() {
        fallback_pkg
    } else {
        &node.package
    };
    let text_val = if !node.text.is_empty() {
        &node.text
    } else {
        &node.hint
    };
    let _ = write!(
        out,
        "<node index=\"{index}\" text=\"{}\" resource-id=\"{}\" class=\"{}\" package=\"{}\" \
         content-desc=\"{}\" checkable=\"false\" checked=\"{}\" clickable=\"{}\" \
         enabled=\"{}\" focusable=\"true\" focused=\"{}\" scrollable=\"{}\" \
         long-clickable=\"false\" password=\"{}\" selected=\"{}\" bounds=\"[{},{}][{},{}]\"",
        escape_xml_attr(text_val),
        escape_xml_attr(&node.resource_id),
        escape_xml_attr(&node.class),
        escape_xml_attr(pkg),
        escape_xml_attr(&node.content_desc),
        node.checked,
        node.clickable,
        node.enabled,
        node.focused,
        node.scrollable,
        node.password,
        node.selected,
        node.bounds.left(),
        node.bounds.top(),
        node.bounds.right(),
        node.bounds.bottom(),
    );
    if node.children.is_empty() {
        out.push_str(" />\n");
    } else {
        out.push_str(">\n");
        for (idx, child) in node.children.iter().enumerate() {
            write_xml_node(out, child, idx, fallback_pkg, depth + 1);
        }
        for _ in 0..depth {
            out.push_str("  ");
        }
        out.push_str("</node>\n");
    }
}

fn escape_xml_attr(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '\n' => out.push_str("&#10;"),
            '\r' => out.push_str("&#13;"),
            '\t' => out.push_str("&#9;"),
            c if (c as u32) < 0x20 => {}
            c => out.push(c),
        }
    }
    out
}

/// Minimal shell word splitter handling single and double quotes.
fn split_shell_words(input: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut in_word = false;
    let mut quote: Option<char> = None;
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                } else if c == '\\' && q == '"' {
                    if let Some(next) = chars.next() {
                        current.push(next);
                    }
                } else {
                    current.push(c);
                }
            }
            None => match c {
                '\'' | '"' => {
                    quote = Some(c);
                    in_word = true;
                }
                '\\' => {
                    if let Some(next) = chars.next() {
                        current.push(next);
                        in_word = true;
                    }
                }
                c if c.is_whitespace() => {
                    if in_word {
                        words.push(std::mem::take(&mut current));
                        in_word = false;
                    }
                }
                _ => {
                    current.push(c);
                    in_word = true;
                }
            },
        }
    }
    if in_word {
        words.push(current);
    }
    words
}

#[cfg(test)]
mod tests {
    use super::*;
    use acos_protocol::ui::Bounds;

    struct MockTarget;
    impl AdbSessionOps for MockTarget {
        fn snapshot(&self) -> Result<AdbSessionSnapshot, String> {
            Ok(AdbSessionSnapshot {
                package: "dev.acos.counter".into(),
                activity: Some(".MainActivity".into()),
                natural_size: (1080, 2400),
                display_size: (1080, 2400),
                rotation: 0,
                density: 420,
                animations: "off",
                guest_pid: Some(12345),
                launcher_pid: Some(12300),
                apk_path: PathBuf::from("/nonexistent.apk"),
            })
        }
        fn rotate(&self, _rot: u8) -> Result<(), String> {
            Ok(())
        }
        fn dump_tree(&self) -> Result<(UiTree, u8), String> {
            let meta = acos_protocol::FrameMeta::new(1, "2026-11-23T00:00:00Z", "test", 1080, 2400, 420, "", true);
            let node = UiNode::new("n0", "android.widget.Button", Bounds::new(10, 20, 110, 80))
                .with_text("Tap me")
                .clickable(true);
            Ok((UiTree::new(meta, "dev.acos.counter", vec![node]), 0))
        }
        fn screenshot_png(&self) -> Result<Vec<u8>, String> {
            Ok(vec![0x89, 0x50, 0x4e, 0x47])
        }
        fn raw_screenshot(&self, _out_path: &Path, _format: &str) -> Result<(u32, u32, u8), String> {
            Ok((1080, 2400, 0))
        }
        fn panel_touch(
            &self,
            _action: i32,
            _down_time: i64,
            _event_time: i64,
            _pointers: &[(i32, f32, f32)],
        ) -> Result<(), String> {
            Ok(())
        }
        fn panel_key(
            &self,
            _code: i32,
            _event_type: &str,
            _meta_state: i32,
            _device_id: i32,
        ) -> Result<(), String> {
            Ok(())
        }
        fn panel_text(&self, _text: &str) -> Result<(), String> {
            Ok(())
        }
        fn stream_socket(&self) -> Option<PathBuf> {
            None
        }
        fn is_verify_run(&self) -> bool {
            false
        }
    }

    #[test]
    fn shell_hub_commands_and_unsupported() {
        let state = Mutex::new(AdbShellState::default());
        let t = MockTarget;
        assert_eq!(
            String::from_utf8_lossy(&run_command("wm size", &t, &state).stdout),
            "Physical size: 1080x2400\n"
        );
        assert_eq!(
            String::from_utf8_lossy(&run_command("wm density", &t, &state).stdout),
            "Physical density: 420\n"
        );
        let disp = String::from_utf8_lossy(&run_command("dumpsys window displays", &t, &state).stdout).into_owned();
        assert!(disp.contains("Display: mDisplayId=0"));
        assert!(disp.contains("mDisplayRotation=ROTATION_0"));

        let dump = run_command("uiautomator dump /sdcard/w.xml", &t, &state);
        assert_eq!(dump.exit_code, 0);
        let cat = run_command("cat /sdcard/w.xml", &t, &state);
        assert_eq!(cat.exit_code, 0);
        let xml = String::from_utf8_lossy(&cat.stdout);
        assert!(xml.contains("text=\"Tap me\""));
        assert!(xml.contains("bounds=\"[10,20][110,80]\""));
        assert_eq!(run_command("rm /sdcard/w.xml", &t, &state).exit_code, 0);
        assert_eq!(run_command("cat /sdcard/w.xml", &t, &state).exit_code, 1);

        let logcat = run_command("logcat", &t, &state);
        assert_eq!(logcat.exit_code, 1);
        assert!(String::from_utf8_lossy(&logcat.stderr).contains("acos: unsupported: logcat"));

        let install = run_command("pm install foo.apk", &t, &state);
        assert_eq!(install.exit_code, 1);
        assert!(String::from_utf8_lossy(&install.stderr).contains("launch a new session"));
    }
}
