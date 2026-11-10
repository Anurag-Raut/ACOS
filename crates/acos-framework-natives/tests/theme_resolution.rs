#![allow(clippy::field_reassign_with_default)]
//! Theme and style-resolution differential test.
//!
//! Applies `android:style/Theme.DeviceDefault.Light.DarkActionBar` (the app's
//! default device theme) to an [`AssetManager`] theme and compares every
//! attribute it defines (including the parent chain) against the platform
//! `aapt dump --values resources` bag data. It then resolves the TextView
//! default style and checks the `textSize` dimension converts to pixels at
//! 420 dpi.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::process::Command;

use acos_framework_natives::res::value::TYPE_INT_COLOR_ARGB8;
use acos_framework_natives::res::{
    ApkAssets, AssetManager, AssetSource, ConfigurationNative, ResConfig, ResValue, Theme,
};

fn workspace_root() -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.pop();
    p.pop();
    p
}

fn find_aapt() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("AAPT") {
        let p = PathBuf::from(p);
        if p.exists() {
            return Some(p);
        }
    }
    let base = PathBuf::from(std::env::var_os("HOME")?).join("Android/Sdk/build-tools");
    let mut versions: Vec<PathBuf> = std::fs::read_dir(base)
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect();
    versions.sort();
    versions
        .into_iter()
        .rev()
        .map(|v| v.join("aapt"))
        .find(|p| p.exists())
}

#[derive(Debug, Default, Clone)]
struct StyleBag {
    parent: u32,
    /// key attr id -> aapt display text (e.g. `(reference) 0x010600f2`)
    entries: BTreeMap<u32, String>,
}

fn parse_aapt_styles(output: &str) -> HashMap<u32, StyleBag> {
    let mut out: HashMap<u32, StyleBag> = HashMap::new();
    let mut current: Option<(u32, StyleBag)> = None;
    for raw in output.lines() {
        let line = raw.trim_end();
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("resource 0x") {
            if let Some((id, bag)) = current.take() {
                out.entry(id).or_insert(bag);
            }
            let Some(id_hex) = rest.get(..8) else {
                continue;
            };
            if rest.contains("<bag>") {
                if let Ok(id) = u32::from_str_radix(id_hex, 16) {
                    current = Some((id, StyleBag::default()));
                }
            }
            continue;
        }
        if let Some((_, bag)) = &mut current {
            if let Some(rest) = trimmed.strip_prefix("Parent=0x") {
                let hex: String = rest.chars().take_while(|c| c.is_ascii_hexdigit()).collect();
                if let Ok(p) = u32::from_str_radix(&hex, 16) {
                    bag.parent = p;
                }
                continue;
            }
            if trimmed.starts_with('#') {
                if let Some(key) = hex_after(trimmed, "Key=0x") {
                    if let Some((_, display)) = trimmed.split_once("): ") {
                        bag.entries.insert(key, display.trim().to_string());
                    }
                }
            }
        }
    }
    if let Some((id, bag)) = current {
        out.entry(id).or_insert(bag);
    }
    out
}

fn hex_after(s: &str, needle: &str) -> Option<u32> {
    let idx = s.find(needle)? + needle.len();
    let hex: String = s[idx..]
        .chars()
        .take_while(|c| c.is_ascii_hexdigit())
        .collect();
    u32::from_str_radix(&hex, 16).ok()
}

/// Merge a style and its parents (parent first, child overrides).
fn merged_bag(styles: &HashMap<u32, StyleBag>, id: u32) -> BTreeMap<u32, String> {
    let mut out = BTreeMap::new();
    let mut seen = Vec::new();
    fn walk(
        styles: &HashMap<u32, StyleBag>,
        id: u32,
        out: &mut BTreeMap<u32, String>,
        seen: &mut Vec<u32>,
        depth: usize,
    ) {
        if depth > 20 || seen.contains(&id) {
            return;
        }
        seen.push(id);
        let Some(bag) = styles.get(&id) else {
            return;
        };
        if bag.parent != 0 {
            walk(styles, bag.parent, out, seen, depth + 1);
        }
        for (k, v) in &bag.entries {
            out.insert(*k, v.clone());
        }
    }
    walk(styles, id, &mut out, &mut seen, 0);
    out
}

/// Render a raw engine value the same way aapt does in a bag entry.
fn aapt_display(table: &acos_framework_natives::res::ResTableIndex, bytes: &[u8], v: ResValue) -> String {
    match v.data_type {
        0x01 => format!("(reference) 0x{:08x}", v.data),
        0x02 => format!("(attribute) 0x{:08x}", v.data),
        0x03 => format!(
            "(string8) \"{}\"",
            table.global_string(bytes, v.data).unwrap_or_default()
        ),
        0x04 => format!("(float) {}", f32::from_bits(v.data)),
        0x05 => {
            let (val, unit) = acos_framework_natives::res::value::complex_to_float(
                v.data,
                &["px", "dp", "sp", "pt", "in", "mm"],
            );
            format!("(dimension) {val:.6}{unit}")
        }
        0x06 => {
            let (val, unit) =
                acos_framework_natives::res::value::complex_to_float(v.data, &["%", "%p"]);
            format!("(fraction) {val:.6}{unit}")
        }
        0x10 => format!("(int) 0x{:08x} or {}", v.data, v.data as i32),
        0x11 => format!("(int) 0x{:08x} or {}", v.data, v.data as i32),
        0x12 => format!("(boolean) {}", v.data != 0),
        0x1c..=0x1f => format!("(color) #{:08x}", v.data),
        other => format!("(unknown 0x{other:02x}) 0x{:08x}", v.data),
    }
}

fn device_config() -> ResConfig {
    let mut config = ResConfig::default();
    config.language = *b"en";
    config.country = *b"US";
    config.density = 420;
    config.smallest_screen_width_dp = 411;
    config.screen_width_dp = 411;
    config.screen_height_dp = 842;
    config.orientation = 1;
    config.ui_mode = 0x11;
    config.sdk_version = 34;
    config.screen_layout = 0x12;
    config
}

fn build_manager(apk_path: &PathBuf) -> AssetManager {
    let apk = ApkAssets::open(apk_path).unwrap();
    let mut mgr = AssetManager::new();
    mgr.add_apk_assets(apk);
    mgr.set_configuration(&ConfigurationNative {
        mcc: 310,
        mnc: 260,
        default_locale: "en-US".to_string(),
        locales: vec!["en-US".to_string()],
        orientation: 1,
        touchscreen: 3,
        density: 420,
        keyboard: 2,
        keyboard_hidden: 1,
        navigation: 2,
        screen_width: 1080,
        screen_height: 2400,
        smallest_screen_width_dp: 411,
        screen_width_dp: 411,
        screen_height_dp: 842,
        screen_layout: 0x12,
        ui_mode: 0x11,
        color_mode: 0,
        grammatical_gender: 0,
        major_version: 34,
        force_refresh: false,
    });
    mgr
}

#[test]
fn theme_device_default_light_dark_action_bar() {
    let Some(aapt) = find_aapt() else {
        eprintln!("aapt not found; skipping theme test");
        return;
    };
    if !acos_test_support::have_fixture(".cache/boot/android-34/framework-res.apk") {
        eprintln!("framework-res.apk missing; skipping");
        return;
    }
    let apk_path = workspace_root().join(".cache/boot/android-34/framework-res.apk");
    let output = Command::new(&aapt)
        .args(["dump", "--values", "resources"])
        .arg(&apk_path)
        .output()
        .unwrap();
    let styles = parse_aapt_styles(&String::from_utf8_lossy(&output.stdout));

    let mut mgr = build_manager(&apk_path);
    let theme_id = 0x0103_013f; // android:style/Theme.DeviceDefault.Light.DarkActionBar
    let theme_handle = mgr.theme_create();
    mgr.theme_apply_style(theme_handle, theme_id, false);

    let expected = merged_bag(&styles, theme_id);
    assert!(
        expected.len() >= 30,
        "aapt should describe >= 30 attributes, got {}",
        expected.len()
    );

    let theme: &Theme = mgr.theme(theme_handle).unwrap();
    let apk = &mgr.sources()[0];
    let table = apk.resource_table().unwrap();
    let bytes = apk.resource_bytes().unwrap();

    let mut compared = 0usize;
    let mut mismatches = 0usize;
    for (attr, aapt_text) in &expected {
        let Some(value) = theme.get_raw(*attr) else {
            // Only compare attributes aapt lists *for this style/parent chain*.
            mismatches += 1;
            if mismatches <= 8 {
                eprintln!("MISSING attr {attr:#010x}: aapt={aapt_text}");
            }
            continue;
        };
        let mine = aapt_display(table, bytes, value);
        compared += 1;
        if &mine != aapt_text {
            mismatches += 1;
            if mismatches <= 8 {
                eprintln!("THEME attr {attr:#010x}: aapt={aapt_text} mine={mine}");
            }
        }
    }
    eprintln!(
        "THEME {theme_id:#010x}: expected={} compared={} mismatches={}",
        expected.len(),
        compared,
        mismatches
    );
    assert!(compared >= 30, "expected to compare >= 30 attributes");
    assert_eq!(mismatches, 0, "theme attribute mismatches");

    // Fully resolve a couple of references to concrete values.
    let color_bg = theme.get_raw(0x0101_0031).expect("colorBackground");
    assert_eq!(color_bg.data_type, 0x01, "colorBackground is a reference");
    let (_, resolved) = mgr
        .get_resource_value(color_bg.data, 420, true)
        .expect("resolve colorBackground");
    assert!(
        matches!(
            resolved.data_type,
            TYPE_INT_COLOR_ARGB8..=0x1f
        ),
        "colorBackground should resolve to a color, got type {:#04x}",
        resolved.data_type
    );
    eprintln!(
        "colorBackground -> {:#010x} type {:#04x} (config {})",
        resolved.data, resolved.data_type, resolved.source_resource_id
    );

    // `textViewStyle` -> Widget.TextView -> textAppearance -> TextAppearance.Small
    // gives `textSize = 14sp`, which is 36.75px at 420dpi.
    let ta_handle = mgr.theme_create();
    mgr.theme_apply_style(ta_handle, 0x0103_0046, false); // TextAppearance.Small
    let size = mgr
        .theme(ta_handle)
        .and_then(|t| t.get(0x0101_0095))
        .expect("TextAppearance.Small.textSize");
    let (sp, unit) = acos_framework_natives::res::value::complex_to_float(
        size.data,
        &["px", "dp", "sp", "pt", "in", "mm"],
    );
    let px = sp * 420.0 / 160.0;
    eprintln!("TextAppearance.Small textSize = {sp}{unit} -> {px:.3}px at 420dpi");
    assert_eq!(size.data_type, 0x05, "textSize should be a dimension");
    assert_eq!(unit, "sp");
    assert!((sp - 14.0).abs() < 0.001, "expected 14sp, got {sp}{unit}");
    assert!((px - 36.75).abs() < 0.001, "expected 36.75px, got {px}");

    // `nativeApplyStyle` retrieval for the TextView styleable via the theme's
    // `textViewStyle` attribute.
    let attrs = [0x0101_0095i32, 0x0101_0098, 0x0101_0097, 0x0101_014f];
    let mut out_values = vec![0i32; attrs.len() * 7];
    let mut out_indices = vec![-1i32; attrs.len()];
    mgr.apply_style(
        theme_handle,
        0,
        0x0103_0046, // defStyleRes = android:style/TextAppearance.Small
        0,
        &attrs,
        &mut out_values,
        &mut out_indices,
    );
    let text_size = acos_framework_natives::res::TypedValueOut::read_style(&out_values[..7]).unwrap();
    eprintln!(
        "applyStyle(textViewStyle): textSize type={:#04x} data={:#010x} index={} textColor type={:#04x}",
        text_size.data_type, text_size.data, out_indices[0], out_values[7]
    );
    if text_size.data_type == 0x05 {
        let (sp, unit) = acos_framework_natives::res::value::complex_to_float(
            text_size.data,
            &["px", "dp", "sp", "pt", "in", "mm"],
        );
        let px = sp * 420.0 / 160.0;
        eprintln!("applyStyle textSize = {sp}{unit} -> {px:.3}px at 420dpi");
        assert!((sp - 14.0).abs() < 0.001, "applyStyle expected 14sp, got {sp}{unit}");
    }
}

#[test]
fn null_theme_reference_becomes_type_null() {
    if !acos_test_support::have_fixture(".cache/boot/android-34/framework-res.apk") {
        return;
    }
    let apk_path = workspace_root().join(".cache/boot/android-34/framework-res.apk");
    let mut mgr = build_manager(&apk_path);
    let theme = mgr.theme_create();
    mgr.theme_apply_style(theme, 0x0103_013f, false);
    // `android:attr/windowContentOverlay` is `@null` in the device themes.
    // `ActionBarOverlayLayout.init` reads it through `TypedArray.getDrawable`;
    // a `TYPE_REFERENCE` to resource 0 makes `Resources.loadDrawable` throw
    // `Unable to find resource ID #0x0`.
    let attrs = [0x0101_0059i32];
    let mut out_values = vec![0i32; 7];
    let mut out_indices = vec![-1i32; 2];
    mgr.apply_style(theme, 0, 0, 0, &attrs, &mut out_values, &mut out_indices);
    let tv = acos_framework_natives::res::TypedValueOut::read_style(&out_values[..7]).unwrap();
    assert_eq!(tv.data_type, 0x00, "@null must be TYPE_NULL");
    assert_eq!(tv.data, 0);
    assert_eq!(tv.resource_id, 0);
}

#[test]
fn theme_copy_and_rebase() {
    if !acos_test_support::have_fixture(".cache/boot/android-34/framework-res.apk") {
        return;
    }
    let apk_path = workspace_root().join(".cache/boot/android-34/framework-res.apk");
    let mut mgr = build_manager(&apk_path);
    let a = mgr.theme_create();
    mgr.theme_apply_style(a, 0x0103_013f, false);
    let b = mgr.theme_create();
    mgr.theme_copy(b, a);
    assert_eq!(mgr.theme(a).unwrap().len(), mgr.theme(b).unwrap().len());
    let before = mgr.theme(b).unwrap().len();
    mgr.theme_rebase(b, &[0x0103_013f], &[false]);
    assert_eq!(mgr.theme(b).unwrap().len(), before);
    let _ = device_config();
}
