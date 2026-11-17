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

use acos_framework_natives::res::value::{TYPE_INT_COLOR_ARGB8, TYPE_NULL};
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
fn aapt_display(
    table: &acos_framework_natives::res::ResTableIndex,
    bytes: &[u8],
    v: ResValue,
) -> String {
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
        matches!(resolved.data_type, TYPE_INT_COLOR_ARGB8..=0x1f),
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
    let text_size =
        acos_framework_natives::res::TypedValueOut::read_style(&out_values[..7]).unwrap();
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
        assert!(
            (sp - 14.0).abs() < 0.001,
            "applyStyle expected 14sp, got {sp}{unit}"
        );
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
fn style_bag_string_keeps_its_asset_cookie() {
    // Regression for the DatePicker `TypedArray.getString` NPE: a
    // `TYPE_STRING` taken from a style/theme bag stores its `data` as an index
    // into that APK's *global* string pool. It must be written with
    // `assetCookie = source_index + 1` so `TypedArray.loadStringValueAt` reads
    // `mAssets.getPooledStringForCookie(...)` instead of the (null for
    // `obtainStyledAttributes(styleRes, ...)`) XML pool.
    if !acos_test_support::have_fixture(".cache/boot/android-34/framework-res.apk") {
        eprintln!("framework-res.apk missing; skipping");
        return;
    }
    let apk_path = workspace_root().join(".cache/boot/android-34/framework-res.apk");
    let mut mgr = build_manager(&apk_path);
    let theme = mgr.theme_create();
    // `android:style/TextAppearance.StatusBar.EventContent.Title` sets
    // `android:fontFamily = "sans-serif-light"` as a raw `string8`.
    let attrs = [0x0101_03aci32];
    let mut out_values = vec![0i32; attrs.len() * 7];
    let mut out_indices = vec![-1i32; attrs.len() + 1];
    mgr.apply_style(
        theme,
        0,
        0x0103_0068,
        0,
        &attrs,
        &mut out_values,
        &mut out_indices,
    );
    let tv = acos_framework_natives::res::TypedValueOut::read_style(&out_values[..7]).unwrap();
    assert_eq!(tv.data_type, 0x03, "fontFamily must be TYPE_STRING");
    assert_eq!(
        tv.asset_cookie, 1,
        "style-bag string must carry the ARSC asset cookie"
    );
}

/// Resolve `attr` through `theme`, then chase its `@resid` chain to a colour.
fn theme_color(mgr: &AssetManager, theme: u64, attr: u32) -> (u32, u32) {
    let value = mgr
        .theme(theme)
        .and_then(|t| t.get(attr))
        .unwrap_or_else(|| panic!("theme has no value for attr {attr:#010x}"));
    assert!(
        value.is_reference() && value.data != 0,
        "attr {attr:#010x} should be a @resid, got type {:#04x} data {:#x}",
        value.data_type,
        value.data
    );
    let (_src, entry, final_id) = mgr
        .resolve_references(value.data, true)
        .unwrap_or_else(|| panic!("cannot resolve {:#010x}", value.data));
    assert!(
        matches!(entry.value.data_type, TYPE_INT_COLOR_ARGB8..=0x1f),
        "attr {attr:#010x} should resolve to a colour, got type {:#04x}",
        entry.value.data_type
    );
    (entry.value.data, final_id)
}

/// The phone renders `Theme.DeviceDefault.Light.DarkActionBar` with a **dark**
/// action bar: `Widget.Material.Light.ActionBar.Solid`'s `android:background`
/// is `?attr/colorPrimary`, and the DeviceDefault theme overrides
/// `colorPrimary`/`colorPrimaryDark` with the neutral-900 device colours.
///
/// This pins the whole chain so it cannot silently fall back to the
/// `Theme.Material.Light` grandparent values (`primary_material_light`
/// `#fff5f5f5` / `primary_dark_material_light` `#ff757575`), which is the light
/// action bar the phone comparison shows ACOS is *not* supposed to draw.
#[test]
fn device_default_dark_action_bar_resolves_to_neutral_900() {
    if !acos_test_support::have_fixture(".cache/boot/android-34/framework-res.apk") {
        eprintln!("framework-res.apk missing; skipping");
        return;
    }
    let apk_path = workspace_root().join(".cache/boot/android-34/framework-res.apk");
    let mut mgr = build_manager(&apk_path);
    let theme = mgr.theme_create();
    mgr.theme_apply_style(theme, 0x0103_013f, false);

    // 1. `colorPrimary` / `colorPrimaryDark` are the DeviceDefault overrides,
    //    not the Material grandparent ones.
    let cp_ref = mgr.theme(theme).unwrap().get_raw(0x0101_0433).unwrap();
    assert_eq!(
        (cp_ref.data_type, cp_ref.data),
        (0x01, 0x0106_02a9),
        "colorPrimary must be @color/primary_device_default_dark"
    );
    let cpd_ref = mgr.theme(theme).unwrap().get_raw(0x0101_0434).unwrap();
    assert_eq!(
        (cpd_ref.data_type, cpd_ref.data),
        (0x01, 0x0106_02a0),
        "colorPrimaryDark must be @color/primary_dark_device_default_dark"
    );

    // 2. Both resolve to the same opaque neutral-900 device colour.
    for (name, attr) in [
        ("colorPrimary", 0x0101_0433u32),
        ("colorPrimaryDark", 0x0101_0434),
    ] {
        let (argb, final_id) = theme_color(&mgr, theme, attr);
        assert_eq!(argb, 0xff19_1c1e, "{name} should be system_neutral1_900");
        assert_eq!(
            final_id, 0x0106_0028,
            "{name} should end at system_neutral1_900"
        );
    }

    // 3. `actionBarStyle` comes from the Material parent (the DeviceDefault
    //    theme does not override it) ...
    let abs = mgr
        .theme(theme)
        .unwrap()
        .get(0x0101_02ce)
        .expect("actionBarStyle");
    assert_eq!(
        (abs.data_type, abs.data),
        (0x01, 0x0103_0290),
        "actionBarStyle must be Widget.Material.Light.ActionBar.Solid"
    );
    // ... whose `android:background` is `?attr/colorPrimary`.
    let abs_theme = mgr.theme_create();
    mgr.theme_apply_style(abs_theme, abs.data, false);
    let bg = mgr.theme(abs_theme).unwrap().get_raw(0x0101_00d4);
    assert_eq!(
        bg.map(|v| (v.data_type, v.data)),
        Some((0x02, 0x0101_0433)),
        "the solid action bar background must be ?attr/colorPrimary"
    );

    // 4. `nativeApplyStyle` for the action bar (defStyleAttr = actionBarStyle)
    //    writes the concrete dark colour into the TypedArray, which is what the
    //    framework turns into the action bar's `ColorDrawable`.
    let attrs = [0x0101_00d4i32];
    let mut out_values = vec![0i32; 7];
    let mut out_indices = vec![-1i32; 2];
    mgr.apply_style(
        theme,
        0x0101_02ce, // defStyleAttr = android:attr/actionBarStyle
        0,
        0,
        &attrs,
        &mut out_values,
        &mut out_indices,
    );
    let tv = acos_framework_natives::res::TypedValueOut::read_style(&out_values[..7]).unwrap();
    eprintln!(
        "action bar background -> type={:#04x} data={:#010x} (indices {:?})",
        tv.data_type, tv.data, out_indices
    );
    assert_eq!(tv.data_type, 0x1d, "action bar background must be a colour");
    assert_eq!(
        tv.data, 0xff19_1c1e,
        "action bar background must be #191c1e"
    );
}

/// `actionBarTheme` (`ThemeOverlay.Material.Dark.ActionBar`) is applied to the
/// action bar's `ContextThemeWrapper` with `force = true`. It must not touch
/// `colorPrimary` / `colorPrimaryDark` — the overlay only retargets
/// `colorControlNormal`/`windowBackground` — so the dark action bar colour
/// survives the overlay.
#[test]
fn dark_action_bar_overlay_keeps_device_default_primaries() {
    if !acos_test_support::have_fixture(".cache/boot/android-34/framework-res.apk") {
        eprintln!("framework-res.apk missing; skipping");
        return;
    }
    let apk_path = workspace_root().join(".cache/boot/android-34/framework-res.apk");
    let mut mgr = build_manager(&apk_path);
    let theme = mgr.theme_create();
    mgr.theme_apply_style(theme, 0x0103_013f, false);
    // 0x0103024c = ThemeOverlay.Material.Dark.ActionBar (the theme's actionBarTheme).
    mgr.theme_apply_style(theme, 0x0103_024c, true);
    for (name, attr, want) in [
        ("colorPrimary", 0x0101_0433u32, 0x0106_02a9u32),
        ("colorPrimaryDark", 0x0101_0434, 0x0106_02a0),
    ] {
        let v = mgr.theme(theme).unwrap().get_raw(attr).unwrap();
        assert_eq!(
            (v.data_type, v.data),
            (0x01, want),
            "the dark action-bar overlay must not retarget {name}"
        );
    }
    // The window background of the action-bar context does become the dark
    // Material background (`?attr/windowBackground`), which is why the window
    // background must be read from the *activity* theme, not this overlay.
    let wb = mgr.theme(theme).unwrap().get(0x0101_0036).unwrap();
    assert_eq!(
        (wb.data_type, wb.data),
        (0x01, 0x0106_0313),
        "ThemeOverlay.Material.Dark sets the dark windowBackground"
    );
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

/// Regression: `nativeRetrieveAttributes` must clear the whole `TypedArray`
/// output buffer. `TypedArray.obtain` recycles arrays and `resize` does not
/// clear a reused array, so an attribute absent from the current tag used to
/// leave the previous `TypedArray`'s value in place. Concretely,
/// `DrawableCorners` (5 slots) reused the `GradientDrawable` buffer and read the
/// `<shape android:shape="0">` int at slot 3, raising
/// `Can't convert value at index 3 to dimension: type=0x10, theme=null` while
/// inflating `android:drawable/btn_default_material` (the framework `Button`).
#[test]
fn null_reference_entry_resolves_to_type_null() {
    if !acos_test_support::have_fixture(".cache/boot/android-34/framework-res.apk") {
        return;
    }
    let apk_path = workspace_root().join(".cache/boot/android-34/framework-res.apk");
    let mgr = build_manager(&apk_path);
    // `android:drawable/config_scrollbarTrackVertical` is `@null` in the
    // default configuration. Resolving it must materialise `TYPE_NULL`, not
    // return the raw `TYPE_REFERENCE` with `data = 0`: the latter made
    // `ResourcesImpl.loadDrawableForCookie` throw `NotFoundException` while
    // inflating `ScrollView` (`config_scrollbarTrackVertical` is not a
    // Drawable).
    let (_src, entry, _final_id) = mgr
        .resolve_references(0x0108_027e, true)
        .expect("resolve config_scrollbarTrackVertical");
    assert_eq!(
        entry.value.data_type, TYPE_NULL,
        "@null must resolve to TYPE_NULL, got type {:#04x} data {:#x}",
        entry.value.data_type, entry.value.data
    );
}

#[test]
fn retrieve_attributes_clears_unset_slots() {
    if !acos_test_support::have_fixture(".cache/boot/android-34/framework-res.apk") {
        return;
    }
    let apk_path = workspace_root().join(".cache/boot/android-34/framework-res.apk");
    let mut mgr = build_manager(&apk_path);
    let cookie = 1i32;
    let handle = mgr
        .open_xml_asset(cookie, "res/drawable/btn_default_mtrl_shape.xml")
        .expect("open framework drawable");
    // Find the `<corners>` start tag.
    let corners_pos = {
        let block = mgr.xml_block(handle).unwrap();
        block
            .doc
            .events
            .iter()
            .enumerate()
            .find_map(|(i, ev)| match ev {
                acos_framework_natives::res::xmlblock::XmlEvent::StartTag(t) => {
                    (block.doc.string_block.get(t.name_index) == Some("corners")).then_some(i)
                }
                _ => None,
            })
            .expect("corners tag")
    };
    let psh = mgr.create_parse_state(handle, 0).unwrap();
    {
        let st = mgr.parse_state_mut(psh).unwrap();
        st.position = corners_pos;
        st.started = true;
    }
    // `DrawableCorners` styleable order: radius, topLeft, topRight, bottomLeft,
    // bottomRight. Only `radius` is present on `<corners>`.
    let attrs = [
        0x0101_01a8i32,
        0x0101_01a9,
        0x0101_01aa,
        0x0101_01ab,
        0x0101_01ac,
    ];
    // Poison every slot with the stale `TYPE_INT_DEC` that used to leak in.
    let mut out_values = vec![0x10i32; attrs.len() * 7];
    let mut out_indices = vec![-1i32; attrs.len() + 1];
    assert!(mgr.retrieve_attributes(psh, &attrs, &mut out_values, &mut out_indices));
    assert_eq!(out_indices[0], 1, "only radius is present");
    // radius is a `?attr/buttonCornerRadius`, kept as TYPE_ATTRIBUTE because
    // `retrieveAttributes` has no theme.
    assert_eq!(
        out_values[0], 0x02,
        "radius must stay TYPE_ATTRIBUTE, got type {:#04x}",
        out_values[0]
    );
    for i in 1..attrs.len() {
        let slot = &out_values[i * 7..(i + 1) * 7];
        assert!(
            slot.iter().all(|v| *v == 0),
            "slot {i} must be cleared (TYPE_NULL), got {slot:?}"
        );
    }
    mgr.destroy_parse_state(psh);
}

/// The Google system image ships `com.google.android.overlay.pixelconfigcommon`
/// as a **static** RRO over `android` (`isStatic=true`, `targetPackage="android"`,
/// `priority=1`). It sets `@bool/config_buttonTextAllCaps=false`, while
/// `framework-res.apk` itself declares `true`. A static overlay cannot be
/// disabled at runtime, so the reference emulator renders mixed-case Button
/// labels and a Button measures `text + 12dp/side` instead of the all-caps
/// width (the `counter` `inc` Button: 231 px, not 269).
///
/// This pins the overlay layer end to end: base value, overlay application,
/// and the `get_resource_{name,package,type,entry}_name` identity of the
/// overridden id.
#[test]
fn static_overlay_overrides_framework_bool() {
    if !acos_test_support::have_fixture(".cache/boot/android-34/framework-res.apk") {
        eprintln!("framework-res.apk missing; skipping");
        return;
    }
    if !acos_test_support::have_fixture(
        ".cache/boot/android-34/overlays/PixelConfigOverlayCommon.apk",
    ) {
        eprintln!("PixelConfigOverlayCommon.apk missing; skipping");
        return;
    }
    let root = workspace_root();
    let apk_path = root.join(".cache/boot/android-34/framework-res.apk");
    let overlay_path = root.join(".cache/boot/android-34/overlays/PixelConfigOverlayCommon.apk");

    // `@bool/config_buttonTextAllCaps` in the base table.
    const ID: u32 = 0x0111_004a;
    // A sibling bool the overlay does not declare.
    const UNTOUCHED: u32 = 0x0111_004b;

    let mut base = build_manager(&apk_path);
    let name = base.get_resource_name(ID).expect("resource name");
    assert_eq!(name, "android:bool/config_buttonTextAllCaps");
    let (_, before) = base.get_resource_value(ID, 0, true).expect("base value");
    assert_eq!(before.data_type, 0x12, "base is TYPE_INT_BOOLEAN");
    // AOSP's `TYPE_INT_BOOLEAN` true sentinel is `(uint32_t)-1`, not 1
    // (`base/libs/androidfw/ResourceTypes.cpp`: `outValue->data =
    // (uint32_t)-1`; `TypedArray.getBoolean` is `data != 0`). The old golden
    // value `1` was an ACOS-authored placeholder; this framework-res.apk
    // stores the AOSP sentinel, so 0xFFFF_FFFF is the device-accurate value.
    assert_eq!(
        before.data, 0xFFFF_FFFF,
        "framework-res declares config_buttonTextAllCaps=true"
    );
    let untouched_before = base
        .get_resource_value(UNTOUCHED, 0, true)
        .map(|(_, v)| v.data);

    let overridden = base.apply_overlay(ApkAssets::open(&overlay_path).unwrap());
    assert!(
        overridden > 0,
        "the overlay must map at least one framework resource"
    );

    let (_, after) = base
        .get_resource_value(ID, 0, true)
        .expect("overlaid value");
    assert_eq!(after.data_type, 0x12);
    assert_eq!(
        after.data, 0,
        "the static RRO must set config_buttonTextAllCaps=false"
    );
    // The overridden id keeps its target identity so the framework's
    // name/type lookups stay consistent.
    assert_eq!(
        base.get_resource_name(ID).as_deref(),
        Some("android:bool/config_buttonTextAllCaps")
    );
    assert_eq!(
        base.get_resource_entry_name(ID).as_deref(),
        Some("config_buttonTextAllCaps")
    );
    assert_eq!(base.get_resource_type_name(ID).as_deref(), Some("bool"));
    assert_eq!(
        base.get_resource_package_name(ID).as_deref(),
        Some("android")
    );

    // A resource the overlay does not declare keeps the base value.
    if let Some(before) = untouched_before {
        let after = base
            .get_resource_value(UNTOUCHED, 0, true)
            .map(|(_, v)| v.data);
        assert_eq!(after, Some(before), "non-overlaid bools must not change");
    }
}
