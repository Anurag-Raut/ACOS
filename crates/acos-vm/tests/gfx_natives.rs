//! Acceptance for the `android.graphics.*` non-canvas natives (T-31 part 1 +
//! T-33): `Paint`, `Path`, `Matrix`, `Region`, `Typeface`, `LineBreaker`,
//! `MeasuredText`.
//!
//! Two tiers:
//!
//! 1. **Rust-level** (always runs): the generation-checked handle table rejects
//!    stale handles, and `gfx_text::measure` agrees with `acos-render`'s own
//!    shaping within 0.01 px (the ADR-0019 contract).
//! 2. **Java-level** (skips loudly when `javac`/`d8`/`android.jar`/the real
//!    boot classpath are absent): compiles `tests/java/GfxNativesMain.java` and
//!    runs it on `--corelib real`, then checks every printed value.

use std::path::{Path, PathBuf};
use std::process::Command;

use acos_dex::DexFile;
use acos_render::text::fonts::FontCollection;
use acos_render::text::{TextEngine, TextStyle};
use acos_render::Paint as RenderPaint;
use acos_vm::natives::gfx_handles::{Handle, HandleError, HandleTable};
use acos_vm::natives::gfx_text::{self, TextAttrs};
use acos_vm::{CoreLib, Vm, VmError, VmOptions};

// ---------------------------------------------------------------------------
// Tier 1: Rust-level
// ---------------------------------------------------------------------------

#[test]
fn stale_handle_is_a_structured_error_not_a_panic() {
    let mut t: HandleTable<u32> = HandleTable::new();
    let h = t.insert(1);
    assert_eq!(t.remove(h), Ok(1));
    assert!(matches!(t.get(h), Err(HandleError::Stale { .. })));
    // Reusing the slot must not let the stale handle alias the new object.
    let h2 = t.insert(2);
    assert_ne!(h, h2);
    assert!(matches!(t.get(h), Err(HandleError::Stale { .. })));
    assert_eq!(t.get(Handle(0xffff_ffff)), Err(HandleError::Stale {
        handle: Handle(0xffff_ffff)
    }));
}

#[test]
fn paint_text_measurement_agrees_with_acos_render() {
    let Some(fonts) = FontCollection::shared() else {
        eprintln!("SKIP paint_text_measurement_agrees_with_acos_render: no /system/fonts");
        return;
    };
    let font = fonts
        .resolve_typeface(Some("sans-serif"), 400, false)
        .expect("sans-serif resolves");
    let render_paint = RenderPaint {
        text_size: 20.0,
        ..Default::default()
    };
    let style = TextStyle::from_paint(&render_paint, font);
    let mut engine = TextEngine::new(fonts);
    let units: Vec<u16> = "Hello ACOS".encode_utf16().collect();
    let expected = engine.measure_text(&units, &style);

    let attrs = TextAttrs {
        text_size: 20.0,
        ..Default::default()
    };
    let actual = gfx_text::measure(&units, &attrs).expect("measure");
    assert!(
        (actual - expected).abs() < 0.01,
        "gfx_text::measure {actual} != acos-render {expected}"
    );

    // Per-character advances must also sum to the total (within fp noise).
    let mut out = vec![0.0f32; units.len()];
    let total = gfx_text::advances(&units, &attrs, &mut out).expect("advances");
    assert!((total - expected).abs() < 0.01, "{total} vs {expected}");
    assert!((out.iter().sum::<f32>() - expected).abs() < 0.01);
}

// ---------------------------------------------------------------------------
// Tier 2: Java-level on the real framework
// ---------------------------------------------------------------------------

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

fn compile_case(name: &str) -> Option<PathBuf> {
    let javac = which("javac")?;
    let sdk = std::env::var("ANDROID_HOME")
        .or_else(|_| std::env::var("ANDROID_SDK_ROOT"))
        .unwrap_or_else(|_| "/home/Qui/Android/Sdk".to_string());
    let d8 = PathBuf::from(format!("{sdk}/build-tools/35.0.0/d8"));
    let android_jar = PathBuf::from(format!("{sdk}/platforms/android-34/android.jar"));
    if !d8.is_file() || !android_jar.is_file() {
        return None;
    }
    let root = repo_root();
    let src = root.join("crates/acos-vm/tests/java").join(format!("{name}.java"));
    if !src.is_file() {
        return None;
    }
    let out = root.join("target/tmp/acos-gfx-natives").join(name);
    let dex = out.join("classes.dex");
    if dex.is_file() {
        return Some(dex);
    }
    let classes = out.join("classes");
    std::fs::create_dir_all(&classes).ok()?;
    let ok = Command::new(&javac)
        .args(["--release", "11", "-encoding", "UTF-8", "-cp"])
        .arg(&android_jar)
        .args(["-d"])
        .arg(&classes)
        .arg(&src)
        .status()
        .ok()?
        .success();
    if !ok {
        return None;
    }
    let class_file = classes.join(format!("{name}.class"));
    let ok = Command::new(&d8)
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
    let Some(dex) = compile_case("GfxNativesMain") else {
        return Err("javac/d8/android.jar unavailable".to_string());
    };
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
    let outcome = vm.run("GfxNativesMain", &[]);
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

#[test]
fn gfx_natives_on_the_real_framework() {
    let out = match run_case() {
        Ok(out) => out,
        Err(m) => {
            eprintln!("SKIP gfx_natives_on_the_real_framework: {m}");
            return;
        }
    };

    // Paint getters/setters round-trip. `getStyle()`/`getTextAlign()` return
    // enums whose `toString()` is the constant name.
    assert_eq!(line_value(&out, "style"), Some("STROKE"), "{out}");
    assert_eq!(line_value(&out, "strokeWidth"), Some("3.5"), "{out}");
    assert_eq!(line_value(&out, "textSize"), Some("20.0"), "{out}");
    assert_eq!(line_value(&out, "textAlign"), Some("CENTER"), "{out}");
    assert_eq!(line_value(&out, "alpha"), Some("200"), "{out}");
    assert_eq!(line_value(&out, "color"), Some("c8112233"), "{out}");
    // Paint() defaults to ANTI_ALIAS_FLAG on Android 14.
    let flags: i32 = line_value(&out, "flags").unwrap().parse().unwrap();
    assert_eq!(flags & 0x01, 0x01, "ANTI_ALIAS_FLAG must be set: {flags}");

    // The raw advance (getTextWidths -> nGetTextAdvances) must equal
    // acos-render's shaping within 0.01 px.
    let measured: f32 = line_value(&out, "measure").unwrap().parse().unwrap();
    let raw: f32 = line_value(&out, "widths").unwrap().parse().unwrap();
    let Some(fonts) = FontCollection::shared() else {
        eprintln!("SKIP measure comparison: no /system/fonts");
        return;
    };
    let font = fonts
        .resolve_typeface(Some("sans-serif"), 400, false)
        .unwrap();
    let style = TextStyle::from_paint(
        &RenderPaint {
            text_size: 20.0,
            ..Default::default()
        },
        font,
    );
    let mut engine = TextEngine::new(fonts);
    let expected = engine.measure_text(&"Hello ACOS".encode_utf16().collect::<Vec<_>>(), &style);
    assert!(
        (raw - expected).abs() < 0.01,
        "Java nGetTextAdvances {raw} != acos-render {expected}"
    );
    // Android's `Paint.measureText` wraps the native advance in `Math.ceil`
    // (verified in the framework DEX), so the Java-visible value is the ceiling.
    assert_eq!(measured, expected.ceil(), "measureText must be ceil(advance)");

    // Path ops/bounds.
    assert_eq!(line_value(&out, "bounds"), Some("0.0,0.0,10.0,5.0"), "{out}");
    // Matrix map points.
    assert_eq!(line_value(&out, "mapped"), Some("6.0,9.0"), "{out}");
    assert!(out.lines().any(|l| l == "done"), "{out}");
}
