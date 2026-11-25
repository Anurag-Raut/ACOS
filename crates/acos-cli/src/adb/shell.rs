//! Host-side adb shell translator (§4.2 of docs/DEVICE_PANEL_SPEC.md + Option B).
//!
//! There is no shell in the guest. Only the commands needed by T3 Code's
//! device hub (`expo-device-hub`) and `agent-device` are answered from real
//! session state (or forwarded through ACOS's real input path); everything else
//! exits non-zero with `acos: unsupported: <cmd>`.

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
    pub device_name: Option<String>,
    pub extra_apks: Vec<PathBuf>,
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
    fn adb_tap(&self, x: i32, y: i32) -> Result<(), String>;
    fn adb_swipe(&self, x1: i32, y1: i32, x2: i32, y2: i32, duration_ms: u64)
        -> Result<(), String>;
    fn adb_key(&self, code: i32, longpress: bool) -> Result<(), String>;
    fn adb_text(&self, text: &str) -> Result<(), String>;
    fn stream_socket(&self) -> Option<PathBuf>;
    fn is_verify_run(&self) -> bool;
    /// A verify run is watch-only: called when panel input was dropped, so the
    /// session's event feed can say so (DEVPANEL1 §3). No-op for live sessions.
    fn panel_input_dropped(&self) {}
    /// A verify run is watch-only: called when `adb shell input ...` was
    /// dropped, so the session's event feed records `source: adb`.
    fn adb_input_dropped(&self) {}
}

/// Per-session host-side virtual shell state.
#[derive(Debug, Default)]
pub struct AdbShellState {
    pub avd_name: String,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChainOp {
    Seq,
    And,
    Or,
}

/// Execute a shell or exec command (including compound `;`, `&&`, `||`) against the given session.
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

    let segments = split_compound_commands(trimmed);
    if segments.is_empty() {
        return ShellOutput::err_text(1, "acos: interactive shell is not supported\n");
    }

    let mut combined_stdout = Vec::new();
    let mut combined_stderr = Vec::new();
    let mut last_exit = 0u8;

    for (idx, (op, seg)) in segments.iter().enumerate() {
        if idx > 0 {
            match op {
                ChainOp::Seq => {}
                ChainOp::And if last_exit != 0 => continue,
                ChainOp::Or if last_exit == 0 => continue,
                _ => {}
            }
        }
        let seg_trimmed = seg.trim();
        if seg_trimmed.is_empty() {
            continue;
        }
        let out = run_single_command(seg_trimmed, target, state);
        combined_stdout.extend_from_slice(&out.stdout);
        combined_stderr.extend_from_slice(&out.stderr);
        last_exit = out.exit_code;
    }

    ShellOutput {
        stdout: combined_stdout,
        stderr: combined_stderr,
        exit_code: last_exit,
    }
}

fn run_single_command(
    trimmed: &str,
    target: &dyn AdbSessionOps,
    state: &Mutex<AdbShellState>,
) -> ShellOutput {
    if trimmed == "dumpsys input | grep Classes:" || trimmed == "dumpsys input|grep Classes:" {
        return ShellOutput::ok_text("  Classes: ALPHAKEY\n");
    }

    let tokens = split_shell_words(trimmed);
    if tokens.is_empty() {
        return ShellOutput::err_text(1, "acos: interactive shell is not supported\n");
    }
    let args: Vec<&str> = tokens.iter().map(String::as_str).collect();

    match args.as_slice() {
        ["true"] => ShellOutput::ok_text(""),
        ["false"] => ShellOutput::err_text(1, ""),
        ["echo", rest @ ..] => run_echo(rest),
        ["getprop", rest @ ..] => run_getprop(rest, target, state, trimmed),
        ["input", rest @ ..] => run_input(rest, target, trimmed),
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
        ["dumpsys", "window"] | ["dumpsys", "window", "windows"] => {
            let Ok(snap) = target.snapshot() else {
                return ShellOutput::err_text(1, "acos: session unavailable\n");
            };
            ShellOutput::ok_text(format_dumpsys_window(&snap))
        }
        ["dumpsys", "display"] => {
            let Ok(snap) = target.snapshot() else {
                return ShellOutput::err_text(1, "acos: session unavailable\n");
            };
            let rot = snap.rotation % 4;
            ShellOutput::ok_text(format!(
                "DISPLAY MANAGER (dumpsys display)\n  \
                 DisplayDeviceInfo{{\"ACOS Display\": uniqueId=\"local:0\", {} x {}, density {}}}\n  \
                 mCurrentOrientation={rot}\n",
                snap.display_size.0, snap.display_size.1, snap.density
            ))
        }
        ["dumpsys", "input_method"] => {
            let Ok(snap) = target.snapshot() else {
                return ShellOutput::err_text(1, "acos: session unavailable\n");
            };
            ShellOutput::ok_text(format!(
                "Input Method Manager Service state:\n  \
                 mCurMethodId=acos/.NullIme\n  \
                 mCurFocusedWindowClient=ClientState{{packageName={}}}\n  \
                 mInputShown=false\n  \
                 mImeWindowVis=0x0\n",
                snap.package
            ))
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
        ["cmd", "package", "has-feature", feat] => {
            let supported = matches!(
                *feat,
                "android.hardware.touchscreen"
                    | "android.hardware.touchscreen.multitouch"
                    | "android.hardware.screen.portrait"
                    | "android.hardware.screen.landscape"
            );
            ShellOutput::ok_text(if supported { "true\n" } else { "false\n" })
        }
        ["pm", "list", "features"] => ShellOutput::ok_text(
            "feature:android.hardware.touchscreen\n\
             feature:android.hardware.touchscreen.multitouch\n\
             feature:android.hardware.screen.portrait\n\
             feature:android.hardware.screen.landscape\n",
        ),
        ["pm", "list", "packages", rest @ ..]
        | ["cmd", "package", "list", "packages", rest @ ..] => {
            run_pm_list_packages(rest, target)
        }
        ["pm", "path", pkg] => run_pm_path(pkg, target),
        ["cmd", "package", "query-activities", rest @ ..]
        | ["cmd", "package", "resolve-activity", rest @ ..] => {
            run_query_or_resolve_activity(rest, target)
        }
        ["am", "get-current-user"] => ShellOutput::ok_text("0\n"),
        ["am", "start", rest @ ..] => run_am_start(rest, target),
        ["am", sub, ..] if matches!(*sub, "force-stop" | "kill") => ShellOutput::err_text(
            1,
            format!(
                "acos: unsupported: am {sub} (an ACOS session hosts one live app; use acos_destroy or acos_reset)\n"
            ),
        ),
        ["settings", "get", "system", "accelerometer_rotation"] => {
            let guard = state.lock().unwrap_or_else(|e| e.into_inner());
            let locked = guard.user_rotation_locked.is_some();
            ShellOutput::ok_text(if locked { "0\n" } else { "1\n" })
        }
        ["settings", "put", "system", "accelerometer_rotation", "0"] => {
            let Ok(snap) = target.snapshot() else {
                return ShellOutput::err_text(1, "acos: session unavailable\n");
            };
            let mut guard = state.lock().unwrap_or_else(|e| e.into_inner());
            if guard.user_rotation_locked.is_none() {
                guard.user_rotation_locked = Some(snap.rotation % 4);
            }
            ShellOutput::ok_text("")
        }
        ["settings", "put", "system", "accelerometer_rotation", "1"] => {
            let mut guard = state.lock().unwrap_or_else(|e| e.into_inner());
            guard.user_rotation_locked = None;
            ShellOutput::ok_text("")
        }
        ["settings", "get", "system", "user_rotation"] => {
            let Ok(snap) = target.snapshot() else {
                return ShellOutput::err_text(1, "acos: session unavailable\n");
            };
            ShellOutput::ok_text(format!("{}\n", snap.rotation % 4))
        }
        ["settings", "put", "system", "user_rotation", rot_str] => {
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
        ["uiautomator", "dump", "--compressed"] => {
            run_uiautomator_dump("/sdcard/window_dump.xml", target, state)
        }
        ["uiautomator", "dump", path] => run_uiautomator_dump(path, target, state),
        ["uiautomator", "dump", "--compressed", path] => {
            run_uiautomator_dump(path, target, state)
        }
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
            "acos: unsupported: install into a live session is not supported; launch a new session with the APK\n",
        ),
        ["cmd", "package", sub, ..] if sub.starts_with("install") => ShellOutput::err_text(
            1,
            "acos: unsupported: install into a live session is not supported; launch a new session with the APK\n",
        ),
        _ => ShellOutput::unsupported(trimmed),
    }
}

fn run_echo(args: &[&str]) -> ShellOutput {
    if let Some((&"-n", rest)) = args.split_first() {
        ShellOutput::ok_text(rest.join(" "))
    } else {
        let mut out = args.join(" ");
        out.push('\n');
        ShellOutput::ok_text(out)
    }
}

fn effective_avd_name(snap: &AdbSessionSnapshot, state: &Mutex<AdbShellState>) -> String {
    let guard = state.lock().unwrap_or_else(|e| e.into_inner());
    if !guard.avd_name.trim().is_empty() {
        return guard.avd_name.clone();
    }
    if let Some(name) = snap.device_name.as_deref().filter(|s| !s.trim().is_empty()) {
        return name.to_string();
    }
    format!("ACOS {}", snap.package)
}

fn session_properties(snap: &AdbSessionSnapshot, avd_name: &str) -> Vec<(&'static str, String)> {
    let avd = avd_name.replace(['\r', '\n'], " ");
    vec![
        ("ro.kernel.qemu", "1".to_string()),
        ("ro.boot.qemu", "1".to_string()),
        ("ro.boot.qemu.avd_name", avd.clone()),
        ("ro.kernel.qemu.avd_name", avd.clone()),
        ("persist.sys.avd_name", avd),
        ("sys.boot_completed", "1".to_string()),
        ("dev.bootcomplete", "1".to_string()),
        ("init.svc.bootanim", "stopped".to_string()),
        ("ro.product.model", "ACOS".to_string()),
        ("ro.product.manufacturer", "ACOS".to_string()),
        ("ro.product.brand", "ACOS".to_string()),
        ("ro.product.name", "acos".to_string()),
        ("ro.product.device", "acos".to_string()),
        ("ro.product.board", "acos".to_string()),
        ("ro.build.version.release", "14".to_string()),
        ("ro.build.version.sdk", "34".to_string()),
        ("ro.build.display.id", "ACOS API 34".to_string()),
        ("ro.build.characteristics", "default".to_string()),
        ("ro.build.type", "userdebug".to_string()),
        ("ro.build.tags", "test-keys".to_string()),
        ("ro.product.cpu.abi", "x86_64".to_string()),
        ("ro.product.cpu.abilist", "x86_64,arm64-v8a".to_string()),
        ("ro.sf.lcd_density", snap.density.to_string()),
    ]
}

fn run_getprop(
    args: &[&str],
    target: &dyn AdbSessionOps,
    state: &Mutex<AdbShellState>,
    raw_trimmed: &str,
) -> ShellOutput {
    let Ok(snap) = target.snapshot() else {
        return ShellOutput::err_text(1, "acos: session unavailable\n");
    };
    let avd_name = effective_avd_name(&snap, state);
    let props = session_properties(&snap, &avd_name);
    match args {
        [] => {
            let mut out = String::new();
            for (k, v) in props {
                let _ = writeln!(out, "[{k}]: [{v}]");
            }
            ShellOutput::ok_text(out)
        }
        [key] => {
            let val = props
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.as_str())
                .unwrap_or("");
            ShellOutput::ok_text(format!("{val}\n"))
        }
        [key, default_val] => {
            let val = props
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.as_str())
                .unwrap_or(*default_val);
            ShellOutput::ok_text(format!("{val}\n"))
        }
        _ => ShellOutput::unsupported(raw_trimmed),
    }
}

fn is_input_source(s: &str) -> bool {
    matches!(
        s,
        "touchscreen"
            | "keyboard"
            | "trackball"
            | "stylus"
            | "mouse"
            | "dpad"
            | "touchpad"
            | "touchnavigation"
            | "joystick"
            | "gamepad"
            | "rotaryencoder"
            | "any"
    )
}

fn run_input(args: &[&str], target: &dyn AdbSessionOps, raw_trimmed: &str) -> ShellOutput {
    let args = match args.split_first() {
        Some((&first, rest)) if is_input_source(first) => rest,
        _ => args,
    };

    match args {
        ["tap", x_str, y_str] => {
            let Ok(x) = x_str.parse::<f32>() else {
                return ShellOutput::err_text(1, format!("Error: Invalid coordinates: {x_str} {y_str}\n"));
            };
            let Ok(y) = y_str.parse::<f32>() else {
                return ShellOutput::err_text(1, format!("Error: Invalid coordinates: {x_str} {y_str}\n"));
            };
            if target.is_verify_run() {
                target.adb_input_dropped();
                return ShellOutput::err_text(1, "acos: adb input ignored: verify run\n");
            }
            match target.adb_tap(x.round() as i32, y.round() as i32) {
                Ok(()) => ShellOutput::ok_text(""),
                Err(e) => ShellOutput::err_text(1, format!("acos: input tap failed: {e}\n")),
            }
        }
        ["swipe", x1_str, y1_str, x2_str, y2_str]
        | ["swipe", x1_str, y1_str, x2_str, y2_str, _] => {
            let dur_ms = if args.len() == 6 {
                match args[5].parse::<u64>() {
                    Ok(ms) if ms > 0 => ms,
                    _ => {
                        return ShellOutput::err_text(
                            1,
                            format!("Error: Invalid duration: {}\n", args[5]),
                        )
                    }
                }
            } else {
                300
            };
            let (Ok(x1), Ok(y1), Ok(x2), Ok(y2)) = (
                x1_str.parse::<f32>(),
                y1_str.parse::<f32>(),
                x2_str.parse::<f32>(),
                y2_str.parse::<f32>(),
            ) else {
                return ShellOutput::err_text(1, "Error: Invalid swipe coordinates\n");
            };
            if target.is_verify_run() {
                target.adb_input_dropped();
                return ShellOutput::err_text(1, "acos: adb input ignored: verify run\n");
            }
            match target.adb_swipe(
                x1.round() as i32,
                y1.round() as i32,
                x2.round() as i32,
                y2.round() as i32,
                dur_ms,
            ) {
                Ok(()) => ShellOutput::ok_text(""),
                Err(e) => ShellOutput::err_text(1, format!("acos: input swipe failed: {e}\n")),
            }
        }
        ["text", text_args @ ..] if !text_args.is_empty() => {
            if target.is_verify_run() {
                target.adb_input_dropped();
                return ShellOutput::err_text(1, "acos: adb input ignored: verify run\n");
            }
            let raw = text_args.join(" ");
            let decoded = decode_adb_input_text(&raw);
            match target.adb_text(&decoded) {
                Ok(()) => ShellOutput::ok_text(""),
                Err(e) => ShellOutput::err_text(1, format!("acos: input text failed: {e}\n")),
            }
        }
        ["keyevent", key_args @ ..] if !key_args.is_empty() => {
            let mut longpress = false;
            let mut codes = Vec::new();
            for &arg in key_args {
                if arg == "--longpress" {
                    longpress = true;
                    continue;
                }
                match resolve_android_keycode(arg) {
                    Some(code) => codes.push(code),
                    None => {
                        return ShellOutput::err_text(
                            1,
                            format!("Error: Unknown keycode: {arg}\n"),
                        );
                    }
                }
            }
            if codes.is_empty() {
                return ShellOutput::err_text(1, "Error: Expected keycode argument\n");
            }
            if target.is_verify_run() {
                target.adb_input_dropped();
                return ShellOutput::err_text(1, "acos: adb input ignored: verify run\n");
            }
            for (idx, code) in codes.into_iter().enumerate() {
                let lp = longpress && idx == 0;
                if let Err(e) = target.adb_key(code, lp) {
                    return ShellOutput::err_text(1, format!("acos: input keyevent failed: {e}\n"));
                }
            }
            ShellOutput::ok_text("")
        }
        _ => ShellOutput::unsupported(raw_trimmed),
    }
}

/// Decode `adb shell input text` escaping (%s -> ASCII space, matching AOSP InputShellCommand).
pub fn decode_adb_input_text(raw: &str) -> String {
    raw.replace("%s", " ")
}

/// Parse an Android keycode argument (decimal integer or `KEYCODE_*` name).
pub fn resolve_android_keycode(arg: &str) -> Option<i32> {
    if let Ok(code) = arg.parse::<i32>() {
        return (code >= 0).then_some(code);
    }
    let upper = arg.trim().to_ascii_uppercase();
    let label = upper.strip_prefix("KEYCODE_").unwrap_or(&upper);
    if let Some(code) = acos_services::keychars::keycode(label) {
        return Some(code);
    }
    match label {
        "UNKNOWN" => Some(0),
        "SOFT_LEFT" => Some(1),
        "SOFT_RIGHT" => Some(2),
        "CALL" => Some(5),
        "ENDCALL" => Some(6),
        "VOLUME_UP" => Some(24),
        "VOLUME_DOWN" => Some(25),
        "POWER" => Some(26),
        "CAMERA" => Some(27),
        "CLEAR" => Some(28),
        "ALT_LEFT" => Some(57),
        "ALT_RIGHT" => Some(58),
        "SHIFT_LEFT" => Some(59),
        "SHIFT_RIGHT" => Some(60),
        "NOTIFICATION" => Some(83),
        "MEDIA_PLAY_PAUSE" => Some(85),
        "MEDIA_STOP" => Some(86),
        "MEDIA_NEXT" => Some(87),
        "MEDIA_PREVIOUS" => Some(88),
        "MEDIA_REWIND" => Some(89),
        "MEDIA_FAST_FORWARD" => Some(90),
        "MUTE" => Some(91),
        "CTRL_LEFT" => Some(113),
        "CTRL_RIGHT" => Some(114),
        "CAPS_LOCK" => Some(115),
        "SYSRQ" => Some(120),
        "BREAK" => Some(121),
        "MEDIA_PLAY" => Some(126),
        "MEDIA_PAUSE" => Some(127),
        "VOLUME_MUTE" => Some(164),
        "APP_SWITCH" => Some(187),
        "SLEEP" => Some(223),
        "WAKEUP" => Some(224),
        _ => None,
    }
}

fn installed_packages(snap: &AdbSessionSnapshot) -> Vec<(String, PathBuf, u64)> {
    let mut pkgs = Vec::new();
    let main_vc = read_apk_version_code(&snap.apk_path).unwrap_or(1);
    pkgs.push((snap.package.clone(), snap.apk_path.clone(), main_vc));
    for extra in &snap.extra_apks {
        if let Ok(apk) = Apk::open(extra) {
            if let Ok(m) = apk.manifest() {
                if !pkgs.iter().any(|(p, _, _)| *p == m.package) {
                    pkgs.push((m.package, extra.clone(), m.version_code.unwrap_or(1) as u64));
                }
            }
        }
    }
    pkgs
}

fn read_apk_version_code(apk_path: &Path) -> Option<u64> {
    let apk = Apk::open(apk_path).ok()?;
    let m = apk.manifest().ok()?;
    m.version_code.map(|v| v as u64)
}

fn run_pm_list_packages(args: &[&str], target: &dyn AdbSessionOps) -> ShellOutput {
    let Ok(snap) = target.snapshot() else {
        return ShellOutput::err_text(1, "acos: session unavailable\n");
    };
    let mut show_path = false;
    let mut show_vc = false;
    let mut system_only = false;
    let mut filter_sub: Option<&str> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i] {
            "-f" => show_path = true,
            "--show-versioncode" => show_vc = true,
            "-s" => system_only = true,
            "-3" | "-e" | "-d" | "-u" => {}
            "--user" => {
                i += 1;
            }
            flag if flag.starts_with('-') => {}
            other => filter_sub = Some(other),
        }
        i += 1;
    }
    if system_only {
        return ShellOutput::ok_text("");
    }
    let mut out = String::new();
    for (pkg, apk_path, vc) in installed_packages(&snap) {
        if let Some(sub) = filter_sub {
            if !pkg.contains(sub) {
                continue;
            }
        }
        out.push_str("package:");
        if show_path {
            let _ = write!(out, "{}=", apk_path.display());
        }
        out.push_str(&pkg);
        if show_vc {
            let _ = write!(out, " versionCode:{vc}");
        }
        out.push('\n');
    }
    ShellOutput::ok_text(out)
}

fn run_pm_path(pkg: &str, target: &dyn AdbSessionOps) -> ShellOutput {
    let Ok(snap) = target.snapshot() else {
        return ShellOutput::err_text(1, "acos: session unavailable\n");
    };
    for (p, apk_path, _) in installed_packages(&snap) {
        if p == pkg {
            return ShellOutput::ok_text(format!("package:{}\n", apk_path.display()));
        }
    }
    ShellOutput::err_text(1, "")
}

fn run_query_or_resolve_activity(args: &[&str], target: &dyn AdbSessionOps) -> ShellOutput {
    let Ok(snap) = target.snapshot() else {
        return ShellOutput::err_text(1, "acos: session unavailable\n");
    };
    let mut category: Option<&str> = None;
    let mut pkg_arg: Option<&str> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i] {
            "-c" => {
                if let Some(val) = args.get(i + 1) {
                    category = Some(*val);
                    i += 1;
                }
            }
            "-a" | "-d" | "-t" | "-p" | "--user" => {
                if args.get(i + 1).is_some() {
                    if args[i] == "-p" {
                        pkg_arg = Some(args[i + 1]);
                    }
                    i += 1;
                }
            }
            flag if flag.starts_with('-') => {}
            other => pkg_arg = Some(other),
        }
        i += 1;
    }
    if category == Some("android.intent.category.LEANBACK_LAUNCHER") {
        return ShellOutput::ok_text("");
    }
    if let Some(p) = pkg_arg {
        if p != snap.package {
            return ShellOutput::err_text(1, "No activity found\n");
        }
    }
    match format_component(&snap) {
        Some(comp) => ShellOutput::ok_text(format!("{comp}\n")),
        None => ShellOutput::err_text(1, "No activity found\n"),
    }
}

fn run_am_start(args: &[&str], target: &dyn AdbSessionOps) -> ShellOutput {
    let Ok(snap) = target.snapshot() else {
        return ShellOutput::err_text(1, "acos: session unavailable\n");
    };
    let mut target_pkg: Option<&str> = None;
    let mut component: Option<&str> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i] {
            "-W" | "-S" | "-D" | "-N" | "-R" | "-P" | "--activity-clear-top" => {}
            "-n" => {
                if let Some(val) = args.get(i + 1) {
                    component = Some(*val);
                    target_pkg = val.split_once('/').map(|(p, _)| p).or(Some(*val));
                    i += 1;
                }
            }
            "-p" => {
                if let Some(val) = args.get(i + 1) {
                    target_pkg = Some(*val);
                    i += 1;
                }
            }
            "-a" | "-c" | "-d" | "-t" | "--user" | "-f" => {
                if args.get(i + 1).is_some() {
                    i += 1;
                }
            }
            "-e" | "--es" | "--ez" | "--ei" | "--el" | "--ef" => {
                if args.get(i + 2).is_some() {
                    i += 2;
                }
            }
            flag if flag.starts_with('-') => {}
            other => {
                if let Some((p, _)) = other.split_once('/') {
                    component = Some(other);
                    target_pkg = Some(p);
                } else {
                    target_pkg = Some(other);
                }
            }
        }
        i += 1;
    }

    let matches_session = target_pkg.is_some_and(|p| p == snap.package);
    if !matches_session {
        return ShellOutput::err_text(
            1,
            format!(
                "Error: Activity not started, unable to resolve Intent (live ACOS session only runs {})\n",
                snap.package
            ),
        );
    }

    let resolved = component
        .map(str::to_string)
        .or_else(|| format_component(&snap))
        .unwrap_or_else(|| format!("{}/.MainActivity", snap.package));
    ShellOutput::ok_text(format!(
        "Starting: Intent {{ cmp={resolved} }}\n\
         Status: ok\n\
         Activity: {resolved}\n\
         ThisTime: 0\n\
         TotalTime: 0\n\
         WaitTime: 0\n\
         Complete\n"
    ))
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
        Err(err) => {
            return ShellOutput::err_text(1, format!("acos: uiautomator dump failed: {err}\n"))
        }
    };
    let xml = format_uiautomator_xml(&tree, rot % 4, &snap.package);
    if matches!(path, "/dev/tty" | "/dev/stdout" | "-") {
        return ShellOutput::ok_text(xml);
    }
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

fn is_checkable_class(class: &str) -> bool {
    let short = class.rsplit('.').next().unwrap_or(class);
    matches!(
        short,
        "CheckBox"
            | "Switch"
            | "SwitchCompat"
            | "RadioButton"
            | "ToggleButton"
            | "CheckedTextView"
            | "MaterialCheckBox"
            | "MaterialSwitch"
    )
}

fn is_focusable_class(class: &str) -> bool {
    let short = class.rsplit('.').next().unwrap_or(class);
    short.contains("EditText")
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
    let checkable = node.checked || is_checkable_class(&node.class);
    let focusable = node.focused || node.clickable || is_focusable_class(&node.class);
    let _ = write!(
        out,
        "<node index=\"{index}\" text=\"{}\" resource-id=\"{}\" class=\"{}\" package=\"{}\" \
         content-desc=\"{}\" checkable=\"{}\" checked=\"{}\" clickable=\"{}\" \
         enabled=\"{}\" focusable=\"{}\" focused=\"{}\" scrollable=\"{}\" \
         long-clickable=\"false\" password=\"{}\" selected=\"{}\" bounds=\"[{},{}][{},{}]\"",
        escape_xml_attr(text_val),
        escape_xml_attr(&node.resource_id),
        escape_xml_attr(&node.class),
        escape_xml_attr(pkg),
        escape_xml_attr(&node.content_desc),
        checkable,
        node.checked,
        node.clickable,
        node.enabled,
        focusable,
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

/// Split top-level unquoted `;`, `&&`, and `||` command chains.
fn split_compound_commands(input: &str) -> Vec<(ChainOp, String)> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut next_op = ChainOp::Seq;
    let mut quote: Option<char> = None;
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        match quote {
            Some(q) => {
                current.push(c);
                if c == q {
                    quote = None;
                } else if c == '\\' && q == '"' {
                    if let Some(next) = chars.next() {
                        current.push(next);
                    }
                }
            }
            None => match c {
                '\'' | '"' => {
                    quote = Some(c);
                    current.push(c);
                }
                '\\' => {
                    current.push(c);
                    if let Some(next) = chars.next() {
                        current.push(next);
                    }
                }
                ';' => {
                    out.push((next_op, std::mem::take(&mut current)));
                    next_op = ChainOp::Seq;
                }
                '&' if chars.peek() == Some(&'&') => {
                    chars.next();
                    out.push((next_op, std::mem::take(&mut current)));
                    next_op = ChainOp::And;
                }
                '|' if chars.peek() == Some(&'|') => {
                    chars.next();
                    out.push((next_op, std::mem::take(&mut current)));
                    next_op = ChainOp::Or;
                }
                _ => current.push(c),
            },
        }
    }
    if !current.trim().is_empty() || out.is_empty() {
        out.push((next_op, current));
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

    #[derive(Default)]
    struct MockTarget {
        verify: bool,
        calls: Mutex<Vec<String>>,
        rotation: std::sync::atomic::AtomicU8,
    }

    impl AdbSessionOps for MockTarget {
        fn snapshot(&self) -> Result<AdbSessionSnapshot, String> {
            Ok(AdbSessionSnapshot {
                package: "dev.acos.counter".into(),
                activity: Some(".MainActivity".into()),
                natural_size: (1080, 2400),
                display_size: (1080, 2400),
                rotation: self.rotation.load(std::sync::atomic::Ordering::Relaxed),
                density: 420,
                animations: "off",
                guest_pid: Some(12345),
                launcher_pid: Some(12300),
                apk_path: PathBuf::from("/nonexistent.apk"),
                device_name: Some("Counter · s1".into()),
                extra_apks: Vec::new(),
            })
        }
        fn rotate(&self, rot: u8) -> Result<(), String> {
            self.rotation.store(rot, std::sync::atomic::Ordering::Relaxed);
            Ok(())
        }
        fn dump_tree(&self) -> Result<(UiTree, u8), String> {
            let meta = acos_protocol::FrameMeta::new(
                1,
                "2026-11-23T00:00:00Z",
                "test",
                1080,
                2400,
                420,
                "",
                true,
            );
            let tap = UiNode::new("n0", "android.widget.Button", Bounds::new(10, 20, 110, 80))
                .with_text("Tap me")
                .with_resource_id("dev.acos.counter:id/inc")
                .clickable(true);
            let mut cb = UiNode::new("n1", "android.widget.CheckBox", Bounds::new(10, 100, 110, 160))
                .with_text("Opt in");
            cb.checked = true;
            let mut edit = UiNode::new("n2", "android.widget.EditText", Bounds::new(10, 180, 400, 240));
            edit.hint = "Name & <tag>".to_string();
            Ok((
                UiTree::new(meta, "dev.acos.counter", vec![tap, cb, edit]),
                0,
            ))
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
        fn adb_tap(&self, x: i32, y: i32) -> Result<(), String> {
            self.calls.lock().unwrap().push(format!("tap:{x},{y}"));
            Ok(())
        }
        fn adb_swipe(
            &self,
            x1: i32,
            y1: i32,
            x2: i32,
            y2: i32,
            duration_ms: u64,
        ) -> Result<(), String> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("swipe:{x1},{y1},{x2},{y2},{duration_ms}"));
            Ok(())
        }
        fn adb_key(&self, code: i32, longpress: bool) -> Result<(), String> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("key:{code}:{longpress}"));
            Ok(())
        }
        fn adb_text(&self, text: &str) -> Result<(), String> {
            self.calls.lock().unwrap().push(format!("text:{text}"));
            Ok(())
        }
        fn stream_socket(&self) -> Option<PathBuf> {
            None
        }
        fn is_verify_run(&self) -> bool {
            self.verify
        }
        fn adb_input_dropped(&self) {
            self.calls.lock().unwrap().push("adb_dropped".to_string());
        }
    }

    #[test]
    fn shell_hub_commands_and_unsupported() {
        let state = Mutex::new(AdbShellState {
            avd_name: "Counter · s1".into(),
            ..Default::default()
        });
        let t = MockTarget::default();
        assert_eq!(
            String::from_utf8_lossy(&run_command("wm size", &t, &state).stdout),
            "Physical size: 1080x2400\n"
        );
        assert_eq!(
            String::from_utf8_lossy(&run_command("wm density", &t, &state).stdout),
            "Physical density: 420\n"
        );
        let disp = String::from_utf8_lossy(&run_command("dumpsys window displays", &t, &state).stdout)
            .into_owned();
        assert!(disp.contains("Display: mDisplayId=0"));
        assert!(disp.contains("mDisplayRotation=ROTATION_0"));

        let gp_all = String::from_utf8_lossy(&run_command("getprop", &t, &state).stdout).into_owned();
        assert!(gp_all.contains("[ro.kernel.qemu]: [1]\n"));
        assert!(gp_all.contains("[ro.boot.qemu]: [1]\n"));
        assert!(gp_all.contains("[ro.boot.qemu.avd_name]: [Counter · s1]\n"));
        assert_eq!(
            String::from_utf8_lossy(&run_command("getprop ro.product.model", &t, &state).stdout),
            "ACOS\n"
        );
        assert_eq!(
            String::from_utf8_lossy(
                &run_command(
                    "getprop ro.product.model; getprop ro.build.version.release",
                    &t,
                    &state
                )
                .stdout
            ),
            "ACOS\n14\n"
        );

        let logcat = run_command("logcat", &t, &state);
        assert_eq!(logcat.exit_code, 1);
        assert!(String::from_utf8_lossy(&logcat.stderr).contains("acos: unsupported: logcat"));

        let install = run_command("pm install foo.apk", &t, &state);
        assert_eq!(install.exit_code, 1);
        assert!(String::from_utf8_lossy(&install.stderr).contains("launch a new session"));
    }

    #[test]
    fn uiautomator_xml_golden_and_dev_tty() {
        let state = Mutex::new(AdbShellState::default());
        let t = MockTarget::default();

        let dump = run_command("uiautomator dump /sdcard/w.xml", &t, &state);
        assert_eq!(dump.exit_code, 0);
        let cat = run_command("cat /sdcard/w.xml", &t, &state);
        assert_eq!(cat.exit_code, 0);
        let xml = String::from_utf8_lossy(&cat.stdout).into_owned();
        let expected = "\
<?xml version='1.0' encoding='UTF-8' standalone='yes' ?>
<hierarchy rotation=\"0\">
  <node index=\"0\" text=\"Tap me\" resource-id=\"dev.acos.counter:id/inc\" class=\"android.widget.Button\" package=\"dev.acos.counter\" content-desc=\"\" checkable=\"false\" checked=\"false\" clickable=\"true\" enabled=\"true\" focusable=\"true\" focused=\"false\" scrollable=\"false\" long-clickable=\"false\" password=\"false\" selected=\"false\" bounds=\"[10,20][110,80]\" />
  <node index=\"1\" text=\"Opt in\" resource-id=\"\" class=\"android.widget.CheckBox\" package=\"dev.acos.counter\" content-desc=\"\" checkable=\"true\" checked=\"true\" clickable=\"false\" enabled=\"true\" focusable=\"false\" focused=\"false\" scrollable=\"false\" long-clickable=\"false\" password=\"false\" selected=\"false\" bounds=\"[10,100][110,160]\" />
  <node index=\"2\" text=\"Name &amp; &lt;tag&gt;\" resource-id=\"\" class=\"android.widget.EditText\" package=\"dev.acos.counter\" content-desc=\"\" checkable=\"false\" checked=\"false\" clickable=\"false\" enabled=\"true\" focusable=\"true\" focused=\"false\" scrollable=\"false\" long-clickable=\"false\" password=\"false\" selected=\"false\" bounds=\"[10,180][400,240]\" />
</hierarchy>
";
        assert_eq!(xml, expected);

        let tty = run_command("uiautomator dump /dev/tty", &t, &state);
        assert_eq!(tty.exit_code, 0);
        assert_eq!(String::from_utf8_lossy(&tty.stdout), expected);

        assert_eq!(run_command("rm /sdcard/w.xml", &t, &state).exit_code, 0);
        assert_eq!(run_command("cat /sdcard/w.xml", &t, &state).exit_code, 1);
    }

    #[test]
    fn input_parsing_escaping_keycodes_and_verify_watch_only() {
        let state = Mutex::new(AdbShellState::default());
        let t = MockTarget::default();

        assert_eq!(run_command("input tap 120 340", &t, &state).exit_code, 0);
        assert_eq!(
            run_command("input touchscreen swipe 10 20 10 200 450", &t, &state).exit_code,
            0
        );
        assert_eq!(
            run_command("input text hello%sworld", &t, &state).exit_code,
            0
        );
        assert_eq!(
            run_command("input keyevent KEYCODE_ENTER", &t, &state).exit_code,
            0
        );
        assert_eq!(
            run_command("input keyevent --longpress 4", &t, &state).exit_code,
            0
        );
        assert_eq!(run_command("input keyevent APP_SWITCH", &t, &state).exit_code, 0);

        let recorded = t.calls.lock().unwrap().clone();
        assert_eq!(
            recorded,
            vec![
                "tap:120,340",
                "swipe:10,20,10,200,450",
                "text:hello world",
                "key:66:false",
                "key:4:true",
                "key:187:false",
            ]
        );

        let bad_key = run_command("input keyevent NO_SUCH_KEY", &t, &state);
        assert_eq!(bad_key.exit_code, 1);

        // Verify run rejects adb input and notifies adb_input_dropped.
        let v = MockTarget {
            verify: true,
            ..Default::default()
        };
        let res = run_command("input tap 50 60", &v, &state);
        assert_eq!(res.exit_code, 1);
        assert!(String::from_utf8_lossy(&res.stderr).contains("verify run"));
        assert_eq!(v.calls.lock().unwrap().as_slice(), &["adb_dropped"]);
    }

    #[test]
    fn agent_device_inventory_and_lifecycle_commands() {
        let state = Mutex::new(AdbShellState::default());
        let t = MockTarget::default();

        assert_eq!(
            String::from_utf8_lossy(&run_command("pm list packages -3", &t, &state).stdout),
            "package:dev.acos.counter\n"
        );
        assert_eq!(
            String::from_utf8_lossy(
                &run_command("cmd package list packages --show-versioncode", &t, &state).stdout
            ),
            "package:dev.acos.counter versionCode:1\n"
        );
        assert_eq!(
            String::from_utf8_lossy(&run_command("pm path dev.acos.counter", &t, &state).stdout),
            "package:/nonexistent.apk\n"
        );
        assert_eq!(
            run_command("pm path com.missing.helper", &t, &state).exit_code,
            1
        );
        assert_eq!(
            String::from_utf8_lossy(
                &run_command(
                    "pidof com.missing.helper || echo AGENT_DEVICE_NO_HELPER",
                    &t,
                    &state
                )
                .stdout
            ),
            "AGENT_DEVICE_NO_HELPER\n"
        );
        assert_eq!(
            String::from_utf8_lossy(
                &run_command("cmd package has-feature android.hardware.type.television", &t, &state)
                    .stdout
            ),
            "false\n"
        );
        assert_eq!(
            String::from_utf8_lossy(
                &run_command("cmd package has-feature android.hardware.touchscreen", &t, &state)
                    .stdout
            ),
            "true\n"
        );
        assert_eq!(
            String::from_utf8_lossy(
                &run_command(
                    "cmd package resolve-activity --brief -a android.intent.action.MAIN -c android.intent.category.LAUNCHER dev.acos.counter",
                    &t,
                    &state
                )
                .stdout
            ),
            "dev.acos.counter/.MainActivity\n"
        );
        assert_eq!(
            run_command("am start -W -p dev.acos.counter", &t, &state).exit_code,
            0
        );
        assert_eq!(
            run_command("am start -W -p com.other.app", &t, &state).exit_code,
            1
        );
        assert_eq!(
            run_command("am force-stop dev.acos.counter", &t, &state).exit_code,
            1
        );
        let disp = String::from_utf8_lossy(&run_command("dumpsys display", &t, &state).stdout)
            .into_owned();
        assert!(disp.contains("mCurrentOrientation=0"));
        let ime = String::from_utf8_lossy(&run_command("dumpsys input_method", &t, &state).stdout)
            .into_owned();
        assert!(ime.contains("mInputShown=false"));

        // `agent-device` orientation (`settings put system accelerometer_rotation` / `user_rotation`).
        assert_eq!(
            String::from_utf8_lossy(
                &run_command("settings get system accelerometer_rotation", &t, &state).stdout
            ),
            "1\n"
        );
        assert_eq!(
            run_command("settings put system accelerometer_rotation 0", &t, &state).exit_code,
            0
        );
        assert_eq!(
            run_command("settings put system user_rotation 1", &t, &state).exit_code,
            0
        );
        assert_eq!(
            String::from_utf8_lossy(
                &run_command("settings get system accelerometer_rotation", &t, &state).stdout
            ),
            "0\n"
        );
        assert_eq!(
            String::from_utf8_lossy(
                &run_command("settings get system user_rotation", &t, &state).stdout
            ),
            "1\n"
        );
        assert_eq!(
            run_command("settings put system accelerometer_rotation 1", &t, &state).exit_code,
            0
        );
        assert_eq!(
            String::from_utf8_lossy(
                &run_command("settings get system accelerometer_rotation", &t, &state).stdout
            ),
            "1\n"
        );
    }
}
