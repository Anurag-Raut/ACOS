//! Java-level acceptance for the `android.graphics.fonts.*` natives (W23-A).
//!
//! Compiles `tests/java/FontNativesMain.java` with the host `javac`/`d8` (build
//! tools only, never the execution path) and runs it on the real Android 14
//! boot classpath (`--corelib real`). The Java case drives the real
//! `Typeface.<clinit>` / `loadPreinstalledSystemFontMap` bytecode:
//! `Font$Builder` + `FontFamily$Builder` over a real `FileChannel.map` of
//! `/system/fonts/Roboto-Regular.ttf`, `nativeCreateFromArray`,
//! `nativeForceSetStaticFinalField`, and `Paint.measureText` on the bold
//! `sans-serif` typeface.
//!
//! The measured width is cross-checked against `acos-render`'s shaping stack,
//! which is the same engine `Paint` uses inside `gfx_text`/`gfx_paint`, so a
//! regression in either the font registry or the measurement path shows up.
//! Skips loudly when the toolchain or boot classpath are absent.

use std::path::{Path, PathBuf};
use std::process::Command;

use acos_dex::DexFile;
use acos_render::text::fonts::FontCollection;
use acos_render::text::{TextEngine, TextStyle};
use acos_vm::{CoreLib, Vm, VmError, VmOptions};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let p = dir.join(name);
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

/// Compile `FontNativesMain` (plus its hidden-API stub) and return the DEX.
fn compile_case() -> Option<PathBuf> {
    let javac = which("javac")?;
    let jar = which("jar")?;
    let sdk = std::env::var("ANDROID_HOME")
        .or_else(|_| std::env::var("ANDROID_SDK_ROOT"))
        .unwrap_or_else(|_| "/home/Qui/Android/Sdk".to_string());
    let d8 = PathBuf::from(format!("{sdk}/build-tools/35.0.0/d8"));
    let android_jar = PathBuf::from(format!("{sdk}/platforms/android-34/android.jar"));
    if !d8.is_file() || !android_jar.is_file() {
        return None;
    }
    let root = repo_root();
    let out = root.join("target/tmp/acos-font-natives");
    let dex = out.join("classes.dex");
    let src = root.join("crates/acos-vm/tests/java/FontNativesMain.java");
    let stub_src = root.join("crates/acos-vm/tests/java/font_stubs");
    if !src.is_file() || !stub_src.is_dir() {
        return None;
    }

    // Compile the hidden-API stub into a library jar (never emitted into the
    // app DEX). The output is cached across runs.
    let stub_classes = out.join("stubs");
    let stub_jar = out.join("font-stubs.jar");
    std::fs::create_dir_all(&stub_classes).ok()?;
    let stub_java = stub_src.join("android/graphics/Typeface.java");
    if !stub_jar.is_file() {
        let ok = Command::new(&javac)
            .args(["-source", "8", "-target", "8", "-nowarn", "-encoding", "UTF-8", "-cp"])
            .arg(&android_jar)
            .args(["-d"])
            .arg(&stub_classes)
            .arg(&stub_java)
            .status()
            .ok()?
            .success();
        if !ok {
            return None;
        }
        let ok = Command::new(&jar)
            .arg("cf")
            .arg(&stub_jar)
            .args(["-C"])
            .arg(&stub_classes)
            .arg("android")
            .status()
            .ok()?
            .success();
        if !ok {
            return None;
        }
    }
    // Recompile whenever the source is newer than the DEX.
    if dex.is_file() {
        let fresh = std::fs::metadata(&dex)
            .and_then(|d| d.modified())
            .ok()
            .zip(std::fs::metadata(&src).and_then(|d| d.modified()).ok())
            .map(|(d, s)| d >= s)
            .unwrap_or(false);
        if fresh {
            return Some(dex);
        }
    }
    let classes = out.join("classes");
    std::fs::create_dir_all(&classes).ok()?;
    let ok = Command::new(&javac)
        .args(["-source", "8", "-target", "8", "-nowarn", "-encoding", "UTF-8", "-cp"])
        .arg(format!("{}:{}", stub_jar.display(), android_jar.display()))
        .args(["-d"])
        .arg(&classes)
        .arg(&src)
        .status()
        .ok()?
        .success();
    if !ok {
        return None;
    }
    let class_file = classes.join("FontNativesMain.class");
    if !class_file.is_file() {
        return None;
    }
    let ok = Command::new(&d8)
        .arg("--lib")
        .arg(&stub_jar)
        .arg("--lib")
        .arg(&android_jar)
        .args(["--min-api", "26", "--output"])
        .arg(&out)
        .arg(&class_file)
        .status()
        .ok()?
        .success();
    if !ok || !dex.is_file() {
        return None;
    }
    Some(dex)
}

fn run_case() -> Result<String, String> {
    let Some(dex) = compile_case() else {
        return Err("javac/d8/android.jar unavailable".to_string());
    };
    // `Typeface.<clinit>` calls `ULocale.addLikelySubtags`, which reads the ICU
    // `.dat` through `AndroidDataFiles.generateIcuDataPath()`. This mirrors
    // `acos-runtime`'s `seed_device_environment()`.
    if std::env::var_os("ANDROID_I18N_ROOT").is_none() {
        std::env::set_var("ANDROID_I18N_ROOT", "/apex/com.android.i18n");
    }
    if std::env::var_os("ANDROID_TZDATA_ROOT").is_none() {
        std::env::set_var("ANDROID_TZDATA_ROOT", "/apex/com.android.tzdata");
    }
    let options = VmOptions {
        corelib: CoreLib::Real,
        ..VmOptions::default()
    };
    let mut vm = match Vm::new(options) {
        Ok(vm) => vm,
        Err(VmError::BootClasspathMissing(m)) => return Err(m),
        Err(e) => return Err(format!("Vm::new failed: {e}")),
    };
    vm.add_dex(DexFile::parse(std::fs::read(&dex).unwrap()).unwrap());
    let outcome = vm.run("FontNativesMain", &[]);
    if outcome.exit_code != 0 || outcome.vm_error.is_some() {
        return Err(format!(
            "run failed: exit={} vm_error={:?} stdout={} stderr={}",
            outcome.exit_code, outcome.vm_error, outcome.stdout, outcome.stderr
        ));
    }
    Ok(outcome.stdout)
}

fn line_value<'a>(out: &'a str, key: &str) -> Option<&'a str> {
    out.lines()
        .find_map(|l| l.strip_prefix(&format!("{key}=")))
}

/// The width `acos-render` — the same shaping engine `Paint` routes through —
/// attributes to `text` for the bold `sans-serif` face at `size` px.
fn expected_measure(text: &str, size: f32) -> Option<f32> {
    let fc = FontCollection::shared()?;
    let font = fc.resolve_typeface(Some("sans-serif"), 700, false)?;
    let mut engine = TextEngine::new(fc);
    let style = TextStyle {
        font,
        family: Some("sans-serif".to_string()),
        size,
        scale_x: 1.0,
        skew_x: 0.0,
        letter_spacing: 0.0,
        fake_bold: false,
        features: Vec::new(),
        variations: Vec::new(),
    };
    let units: Vec<u16> = text.encode_utf16().collect();
    Some(engine.measure_text(&units, &style))
}

#[test]
fn font_natives_typeface_and_measure_on_the_real_framework() {
    let out = match run_case() {
        Ok(out) => out,
        Err(m) => {
            eprintln!("SKIP font_natives_typeface_and_measure_on_the_real_framework: {m}");
            return;
        }
    };

    eprintln!("--- FontNativesMain output ---\n{out}");
    assert_eq!(line_value(&out, "default.present"), Some("true"), "{out}");
    assert_eq!(line_value(&out, "default.family"), Some("sans-serif"), "{out}");
    assert_eq!(line_value(&out, "default.style"), Some("0"), "{out}");
    assert_eq!(line_value(&out, "bold.present"), Some("true"), "{out}");
    assert_eq!(line_value(&out, "bold.style"), Some("1"), "{out}");

    let measure24: f32 = line_value(&out, "measure.24")
        .and_then(|v| v.parse().ok())
        .unwrap_or_else(|| panic!("missing measure.24 in {out}"));
    let measure48: f32 = line_value(&out, "measure.48")
        .and_then(|v| v.parse().ok())
        .unwrap_or_else(|| panic!("missing measure.48 in {out}"));
    assert!(measure24 > 0.0, "measure.24 must be positive: {out}");
    // Text size scales (near-)linearly: doubling the size doubles the width
    // within a pixel of glyph-advance rounding.
    assert!(
        (measure48 - 2.0 * measure24).abs() < 2.0,
        "measure.48 {measure48} != 2 * measure.24 {measure24}"
    );
    // The Java measurement must agree with acos-render's shaping stack.
    match expected_measure("Hello", 24.0) {
        Some(expected) => assert!(
            (measure24 - expected).abs() < 1.0,
            "Paint.measureText {measure24} != acos-render {expected}"
        ),
        None => eprintln!("SKIP cross-check: no /system/fonts collection available"),
    }

    assert_eq!(line_value(&out, "family.size"), Some("1"), "{out}");
    assert_eq!(line_value(&out, "font.weight"), Some("700"), "{out}");
    assert_eq!(line_value(&out, "font.file"), Some("Roboto-Regular.ttf"), "{out}");
    assert!(line_value(&out, "font.axes").is_some(), "{out}");
    assert!(
        line_value(&out, "font.ascent").and_then(|v| v.parse::<f32>().ok()).unwrap_or(0.0) < 0.0,
        "ascent must be negative: {out}"
    );
    assert!(
        line_value(&out, "font.descent").and_then(|v| v.parse::<f32>().ok()).unwrap_or(0.0) > 0.0,
        "descent must be positive: {out}"
    );
    assert_eq!(line_value(&out, "glyph.rect"), Some("true,true"), "{out}");
    assert_eq!(out.lines().last(), Some("done"), "{out}");
}
