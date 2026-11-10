#![allow(clippy::field_reassign_with_default)]
//! Resource engine benchmark (harness = false).
//!
//! Run with `cargo bench -p acos-framework-natives`. Prints p50/p95 latencies
//! for the operations the Android launch actually performs, and writes
//! `results/res/bench.json`.

use std::path::PathBuf;
use std::time::Instant;

use acos_framework_natives::res::{
    ApkAssets, AssetManager, AssetSource, ConfigurationNative, ResConfig, ResValue, TypedValueOut,
    XmlBlock, STYLE_NUM_ENTRIES,
};

fn workspace_root() -> PathBuf {
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.pop();
    p.pop();
    p
}

fn percentile(sorted: &[u128], p: f64) -> u128 {
    if sorted.is_empty() {
        return 0;
    }
    let idx = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

fn report(name: &str, mut samples: Vec<u128>) -> (u128, u128) {
    samples.sort_unstable();
    let p50 = percentile(&samples, 0.50);
    let p95 = percentile(&samples, 0.95);
    println!("{name:<44} p50={p50:>10} ns  p95={p95:>10} ns  n={}", samples.len());
    (p50, p95)
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

fn main() {
    let apk_path = workspace_root().join(".cache/boot/android-34/framework-res.apk");
    if !apk_path.exists() {
        eprintln!("framework-res.apk missing; skipping benchmark");
        return;
    }

    // --- load time -------------------------------------------------------
    let mut load_samples = Vec::new();
    let mut last = None;
    for _ in 0..10 {
        let t = Instant::now();
        let apk = ApkAssets::open(&apk_path).unwrap();
        let _ = apk.resource_table();
        load_samples.push(t.elapsed().as_nanos());
        last = Some(apk);
    }
    let apk = last.unwrap();
    let (load_p50, load_p95) = report("framework_res_load", load_samples);

    let config = device_config();

    // --- getResourceValue (uncached: distinct entry ids) ------------------
    let table = apk.resource_table().unwrap();
    let bytes = apk.resource_bytes().unwrap();
    let mut uncached = Vec::new();
    for i in 0..2000u32 {
        let id = 0x0104_0000 + i;
        let t = Instant::now();
        let _ = table.resolve(bytes, id, &config);
        uncached.push(t.elapsed().as_nanos());
    }
    let (rv_uncached_p50, rv_uncached_p95) = report("getResourceValue_uncached", uncached);

    // --- getResourceValue (cached: same id repeated) ----------------------
    let mut cached = Vec::new();
    for _ in 0..20_000u32 {
        let t = Instant::now();
        let _ = table.resolve(bytes, 0x0104_0000, &config);
        cached.push(t.elapsed().as_nanos());
    }
    let (rv_cached_p50, rv_cached_p95) = report("getResourceValue_cached", cached);

    // --- full manager lookup (with reference chasing) ---------------------
    let mut mgr = AssetManager::new();
    mgr.add_apk_assets(ApkAssets::open(&apk_path).unwrap());
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
    let theme = mgr.theme_create();
    mgr.theme_apply_style(theme, 0x0103_013f, false);

    // --- applyStyle (TextView textAppearance) ----------------------------
    let attrs = [0x0101_0095i32, 0x0101_0098, 0x0101_0097, 0x0101_014f];
    let mut out_values = vec![0i32; attrs.len() * STYLE_NUM_ENTRIES];
    let mut out_indices = vec![-1i32; attrs.len()];
    // Warm the theme + bag caches.
    mgr.apply_style(theme, 0, 0x0103_0046, 0, &attrs, &mut out_values, &mut out_indices);
    let mut apply_samples = Vec::new();
    for _ in 0..20_000u32 {
        let t = Instant::now();
        mgr.apply_style(theme, 0, 0x0103_0046, 0, &attrs, &mut out_values, &mut out_indices);
        apply_samples.push(t.elapsed().as_nanos());
    }
    let (apply_p50, apply_p95) = report("applyStyle_textview", apply_samples);

    // --- XmlBlock parse + walk of screen_simple.xml ----------------------
    let xml_bytes = apk.read_entry("res/layout/screen_simple.xml").unwrap_or_default();
    let mut parse_samples = Vec::new();
    let mut walk_samples = Vec::new();
    if !xml_bytes.is_empty() {
        for _ in 0..2000 {
            let t = Instant::now();
            let block = XmlBlock::from_bytes(&xml_bytes, 0, xml_bytes.len()).unwrap();
            parse_samples.push(t.elapsed().as_nanos());
            let t = Instant::now();
            let mut state = acos_framework_natives::res::XmlParserState::new(0, 0);
            loop {
                let ev = state.native_next(&block.doc);
                let _ = state.get_name(&block.doc);
                let _ = state.get_attribute_count(&block.doc);
                if ev == acos_framework_natives::res::END_DOCUMENT {
                    break;
                }
            }
            walk_samples.push(t.elapsed().as_nanos());
        }
    }
    let (xml_parse_p50, xml_parse_p95) = report("xmlblock_parse_screen_simple", parse_samples);
    let (xml_walk_p50, xml_walk_p95) = report("xmlblock_walk_screen_simple", walk_samples);

    // --- resolved sample values (sanity) ---------------------------------
    let rv = mgr.get_resource_value(0x0101_0031, 420, true);
    if let Some((_, v)) = &rv {
        println!(
            "sample colorBackground: type={:#04x} data={:#010x}",
            v.data_type, v.data
        );
    }
    let _ = ResValue::NULL;
    let _ = TypedValueOut::read_style(&out_values[..STYLE_NUM_ENTRIES]);

    // --- write results/res/bench.json ------------------------------------
    let dir = workspace_root().join("results/res");
    let _ = std::fs::create_dir_all(&dir);
    let json = format!(
        "{{\n  \"framework_res_load\": {{\"p50_ns\": {load_p50}, \"p95_ns\": {load_p95}}},\n  \
         \"getResourceValue_uncached\": {{\"p50_ns\": {rv_uncached_p50}, \"p95_ns\": {rv_uncached_p95}}},\n  \
         \"getResourceValue_cached\": {{\"p50_ns\": {rv_cached_p50}, \"p95_ns\": {rv_cached_p95}}},\n  \
         \"applyStyle_textview\": {{\"p50_ns\": {apply_p50}, \"p95_ns\": {apply_p95}}},\n  \
         \"xmlblock_parse_screen_simple\": {{\"p50_ns\": {xml_parse_p50}, \"p95_ns\": {xml_parse_p95}}},\n  \
         \"xmlblock_walk_screen_simple\": {{\"p50_ns\": {xml_walk_p50}, \"p95_ns\": {xml_walk_p95}}}\n}}\n"
    );
    let _ = std::fs::write(dir.join("bench.json"), json);
    println!("wrote results/res/bench.json");
}
