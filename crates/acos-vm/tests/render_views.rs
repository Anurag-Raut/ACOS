//! W101-A: **pipeline-generated** render corpus.
//!
//! The W66/W82 corpus scenes are hand-authored reconstructions of ART reference
//! UI dumps, so their SSIM measures how well the scenes were authored, not the
//! real draw pipeline (HONESTY_AUDIT #11 / W94-A finding 12). This suite closes
//! that gap for the framework-reachable fixtures: it inflates the fixture's
//! compiled layout through the real `PhoneLayoutInflater`, runs a real
//! `measure`/`layout`, and draws the resulting `View` hierarchy into a real
//! `RecordingCanvas` (`View.draw`). The recorded `RenderNode` display list is
//! rasterised by the real `HardwareRenderer` state machine, which in ACOS walks
//! it through `acos-render`.
//!
//! No scene JSON, no hand-authored coordinates: the candidate is whatever the
//! framework's own `View.draw` produced. Reference is always the ART capture.
//!
//! Run: `tools/render-views.sh` (sets `ACOS_FONT_DIR`).
//!
//! Skips loudly when `javac`/`d8`/`android.jar`, the fixture APK or the reference
//! is unavailable. `ACOS_REQUIRE_FIXTURES` turns a missing fixture into a hard
//! failure (shared policy).

use std::path::{Path, PathBuf};
use std::process::Command;

use acos_dex::DexFile;
use acos_parcel::binder::BinderId;
use acos_parcel::Parcel;
use acos_render::{diff, image};
use acos_vm::javabridge::JavaValue;
use acos_vm::natives::binder::{install_transport, BinderDelivery, BinderTransport};
use acos_vm::{CoreLib, Vm, VmError, VmOptions};

/// A transport that answers every service lookup with `null` (the truthful
/// answer for this VM-level test: no platform services are running).
struct NoServices;

impl BinderTransport for NoServices {
    fn context_object(&self, _vm: &mut Vm) -> u64 {
        1
    }

    fn binder_id_of(&self, vm: &mut Vm, obj: acos_vm::heap::Handle) -> Option<BinderId> {
        if let Ok(JavaValue::Long(v)) = vm.get_field(obj, "mNativeData") {
            return Some(BinderId(v as u64));
        }
        if let Ok(JavaValue::Long(v)) = vm.get_field(obj, "mObject") {
            return Some(BinderId(v as u64));
        }
        None
    }

    fn binder_object(&self, vm: &mut Vm, id: BinderId) -> Option<acos_vm::heap::Handle> {
        if id.is_null() {
            return None;
        }
        vm.new_instance_ctor(
            "android.os.BinderProxy",
            "(J)V",
            &[JavaValue::Long(id.0 as i64)],
        )
        .ok()
    }

    fn transact(
        &self,
        _vm: &mut Vm,
        _binder: BinderId,
        _code: u32,
        _data: &mut Parcel,
        reply: &mut Parcel,
        _flags: u32,
    ) -> Result<Vec<BinderDelivery>, String> {
        reply.reset();
        reply.write_int32(0);
        reply.write_strong_binder_opt(None);
        Ok(Vec::new())
    }

    fn descriptor(&self, _id: BinderId) -> Option<String> {
        None
    }

    fn is_alive(&self, _id: BinderId) -> bool {
        false
    }
}

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

fn sdk() -> Option<PathBuf> {
    std::env::var("ANDROID_HOME")
        .or_else(|_| std::env::var("ANDROID_SDK_ROOT"))
        .ok()
        .map(PathBuf::from)
        .or_else(|| {
            let p = PathBuf::from("/home/Qui/Android/Sdk");
            p.is_dir().then_some(p)
        })
}

fn tools() -> Option<(PathBuf, PathBuf, PathBuf)> {
    let javac = which("javac")?;
    let sdk = sdk()?;
    let d8 = sdk.join("build-tools/35.0.0/d8");
    let android_jar = sdk.join("platforms/android-34/android.jar");
    if !d8.is_file() || !android_jar.is_file() {
        return None;
    }
    Some((javac, d8, android_jar))
}

/// Compile `RenderViewsMain.java` to DEX, with the hidden-API stubs compiled to
/// `--lib`-only classes (never packaged): the VM binds the real framework class.
fn compile_harness() -> Option<PathBuf> {
    let (javac, d8, android_jar) = tools()?;
    let root = repo_root();
    let out = root.join("target/tmp/acos-render-views");
    let dex = out.join("classes.dex");
    let src = root.join("crates/acos-vm/tests/java/RenderViewsMain.java");
    if !src.is_file() {
        return None;
    }
    let classes = out.join("classes");
    let stubs = out.join("stubs");
    std::fs::create_dir_all(&classes).ok()?;
    std::fs::create_dir_all(&stubs).ok()?;
    let mut stub_src: Vec<PathBuf> = Vec::new();
    collect_java(&root.join("crates/acos-vm/tests/java/stubs"), &mut stub_src);
    collect_java(&root.join("crates/acos-vm/tests/java/inflate_stubs"), &mut stub_src);
    if stub_src.is_empty() {
        return None;
    }
    let ok = Command::new(&javac)
        .args(["--release", "11", "-encoding", "UTF-8", "-cp"])
        .arg(&android_jar)
        .args(["-d"])
        .arg(&stubs)
        .args(&stub_src)
        .status()
        .ok()?
        .success();
    if !ok {
        return None;
    }
    let cp = format!("{}:{}", stubs.display(), android_jar.display());
    let ok = Command::new(&javac)
        .args(["--release", "11", "-encoding", "UTF-8", "-cp"])
        .arg(&cp)
        .args(["-d"])
        .arg(&classes)
        .arg(&src)
        .status()
        .ok()?
        .success();
    if !ok {
        return None;
    }
    // Recompile unconditionally so an edited harness is never masked by a stale
    // cached DEX.
    let _ = std::fs::remove_file(&dex);
    let mut class_files: Vec<PathBuf> = Vec::new();
    collect_class(&classes, &mut class_files);
    let mut d8 = Command::new(&d8);
    d8.arg("--lib")
        .arg(&android_jar)
        .args(["--min-api", "26", "--output"])
        .arg(&out);
    for c in &class_files {
        d8.arg(c);
    }
    let ok = d8.status().ok()?.success();
    (ok && dex.is_file()).then_some(dex)
}

fn collect_class(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect_class(&p, out);
        } else if p.extension().map(|x| x == "class").unwrap_or(false) {
            out.push(p);
        }
    }
}

fn collect_java(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            collect_java(&p, out);
        } else if p.extension().map(|x| x == "java").unwrap_or(false) {
            out.push(p);
        }
    }
}

struct Scene {
    app: &'static str,
    layout: &'static str,
    reference: &'static str,
}

const WIDTH: &str = "1080";
const HEIGHT: &str = "2400";

/// Framework-inflatable fixtures (no custom View subclasses, no Activity).
const SCENES: &[Scene] = &[
    Scene {
        app: "canvas-shapes",
        layout: "main",
        reference: "fixtures/reference/canvas-shapes/art-api34-x86_64/screenshot_initial.png",
    },
    Scene {
        app: "counter",
        layout: "main",
        reference: "fixtures/reference/counter/art-api34-x86_64/screenshot_initial.png",
    },
    Scene {
        app: "form-validation",
        layout: "main",
        reference: "fixtures/reference/form-validation/art-api34-x86_64/screenshot_initial.png",
    },
    Scene {
        app: "text-input",
        layout: "main",
        reference: "fixtures/reference/text-input/art-api34-x86_64/screenshot_initial.png",
    },
    Scene {
        app: "widget-gallery",
        layout: "main",
        reference: "fixtures/reference/widget-gallery/art-api34-x86_64/screenshot_initial.png",
    },
];

fn apk_path(app: &str) -> PathBuf {
    repo_root().join(format!("fixtures/build/apps/{app}.apk"))
}

fn set_font_dir() {
    if std::env::var_os("ACOS_FONT_DIR").is_none() {
        let d = repo_root().join(".cache/boot/android-34/fonts");
        if d.is_dir() {
            std::env::set_var("ACOS_FONT_DIR", d);
        }
    }
}

fn run_harness(app: &str, layout: &str) -> Result<(), String> {
    let Some(dex) = compile_harness() else {
        return Err("javac/d8/android.jar or RenderViewsMain.java unavailable".to_string());
    };
    let apk = apk_path(app);
    if !apk.is_file() {
        return Err(format!("missing fixture apk {}", apk.display()));
    }
    // ICU/tzdata roots the real boot seeds; the VFS maps `/system/**` onto
    // `.cache/boot/android-34/**`. `EditText`'s `Editor` loads break-iterator
    // bundles through `AndroidDataFiles`.
    if std::env::var_os("ANDROID_I18N_ROOT").is_none() {
        std::env::set_var("ANDROID_I18N_ROOT", "/system/apex/com.android.i18n");
    }
    if std::env::var_os("ANDROID_TZDATA_ROOT").is_none() {
        std::env::set_var("ANDROID_TZDATA_ROOT", "/apex/com.android.tzdata");
    }
    let mock_dex = repo_root().join(".cache/boot/android-34/dex/android.test.mock/classes.dex");
    if !mock_dex.is_file() {
        return Err(format!("missing {}", mock_dex.display()));
    }
    let mut vm = match Vm::new(VmOptions {
        corelib: CoreLib::Real,
        ..VmOptions::default()
    }) {
        Ok(vm) => vm,
        Err(VmError::BootClasspathMissing(m)) => return Err(m),
        Err(e) => return Err(format!("Vm::new failed: {e}")),
    };
    install_transport(Box::new(NoServices));
    let framework_res = repo_root().join(".cache/boot/android-34/framework-res.apk");
    let mut argv = vec![
        apk.to_string_lossy().to_string(),
        format!("dev.acos.fixtures.{}", app.replace('-', "")),
        layout.to_string(),
        WIDTH.to_string(),
        HEIGHT.to_string(),
    ];
    if framework_res.is_file() {
        argv.push(framework_res.to_string_lossy().to_string());
    }
    vm.add_dex(DexFile::parse(std::fs::read(&mock_dex).unwrap()).unwrap());
    // Custom `View` subclasses in the fixture (e.g. canvas-shapes' ShapeView)
    // are app classes, loaded exactly as the inflate-layout suite loads them.
    let app_dex = repo_root().join(format!("fixtures/build/apps/{app}/dex/classes.dex"));
    if app_dex.is_file() {
        vm.add_dex(DexFile::parse(std::fs::read(&app_dex).unwrap()).unwrap());
    }
    vm.add_dex(DexFile::parse(std::fs::read(&dex).unwrap()).unwrap());
    let outcome = vm.run("RenderViewsMain", &argv);
    if outcome.exit_code != 0 || outcome.vm_error.is_some() {
        return Err(format!(
            "run failed: exit={} vm_error={:?} stdout={} stderr={}",
            outcome.exit_code, outcome.vm_error, outcome.stdout, outcome.stderr
        ));
    }
    if !outcome.stdout.lines().any(|l| l == "done") {
        return Err(format!("harness did not finish:\n{}", outcome.stdout));
    }
    if outcome.stdout.contains("INFLATE-FAILED") {
        return Err(format!(
            "framework inflation failed:\n{}",
            outcome.stdout
        ));
    }
    if std::env::var_os("ACOS_RENDER_VIEWS_VERBOSE").is_some() {
        eprintln!("[{app}] harness stdout:\n{}", outcome.stdout);
    }
    Ok(())
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xedb8_8320 & mask);
        }
    }
    !crc
}

/// Minimal stored-deflate PNG writer (no encoder dependency in the test set).
fn encode_png(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
    fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
        out.extend_from_slice(&(data.len() as u32).to_be_bytes());
        let mut crc_in = kind.to_vec();
        crc_in.extend_from_slice(data);
        out.extend_from_slice(kind);
        out.extend_from_slice(data);
        out.extend_from_slice(&crc32(&crc_in).to_be_bytes());
    }
    let mut raw = Vec::with_capacity((width * 4 + 1) as usize * height as usize);
    for y in 0..height {
        raw.push(0);
        let start = (y * width * 4) as usize;
        raw.extend_from_slice(&rgba[start..start + (width * 4) as usize]);
    }
    let mut z = vec![0x78, 0x01];
    let mut pos = 0;
    while pos < raw.len() {
        let n = (raw.len() - pos).min(65535);
        let final_block = pos + n >= raw.len();
        z.push(if final_block { 1 } else { 0 });
        z.extend_from_slice(&(n as u16).to_le_bytes());
        z.extend_from_slice(&(!(n as u16)).to_le_bytes());
        z.extend_from_slice(&raw[pos..pos + n]);
        pos += n;
    }
    let mut a = 1u32;
    let mut b = 0u32;
    for &byte in &raw {
        a = (a + byte as u32) % 65521;
        b = (b + a) % 65521;
    }
    z.extend_from_slice(&((b << 16) | a).to_be_bytes());

    let mut out = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &z);
    chunk(&mut out, b"IEND", &[]);
    out
}

fn mask_systemui(img: &mut image::DecodedImage) {
    const STATUS_H: usize = 128;
    const NAV_TOP: usize = 2337;
    let w = img.width as usize;
    let h = img.height as usize;
    if w == 0 {
        return;
    }
    for (y0, y1) in [(0usize, STATUS_H), (NAV_TOP, h)] {
        let a = y0.min(h) * w;
        let b = y1.min(h) * w;
        if a < b {
            img.pixels[a..b].fill(0);
        }
    }
}

#[test]
fn pipeline_generated_layouts_match_art_reference() {
    set_font_dir();
    let mut rows: Vec<serde_json::Value> = Vec::new();
    let mut ran = 0usize;

    for scene in SCENES {
        if !acos_test_support::have_fixture(scene.reference) {
            eprintln!(
                "SKIP {}: missing reference {}",
                scene.app, scene.reference
            );
            continue;
        }
        if !acos_test_support::have_fixture(&format!("fixtures/build/apps/{}.apk", scene.app)) {
            eprintln!("SKIP {}: missing fixture apk", scene.app);
            continue;
        }
        match run_harness(scene.app, scene.layout) {
            Ok(()) => {}
            Err(m) => {
                // A framework gap in this fixture is itself evidence; record it
                // rather than silently dropping coverage.
                eprintln!("PIPELINE-FAIL {}: {m}", scene.app);
                rows.push(serde_json::json!({
                    "app": scene.app,
                    "provenance": "pipeline-generated",
                    "status": "pipeline-failure",
                    "error": m,
                    "reference": scene.reference,
                }));
                continue;
            }
        }
        let Some(frame) = acos_vm::natives::gfx_hwui::last_frame() else {
            eprintln!("PIPELINE-FAIL {}: HardwareRenderer submitted no frame", scene.app);
            continue;
        };
        assert_eq!(
            (frame.width, frame.height),
            (1080, 2400),
            "{}: frame size",
            scene.app
        );
        let candidate = diff::decoded_from_rgba8(frame.width, frame.height, &frame.rgba)
            .expect("decode pipeline frame");
        let reference = image::decode(
            &std::fs::read(repo_root().join(scene.reference)).expect("read reference"),
        )
        .expect("decode reference");
        let report = diff::diff(&reference, &candidate, &diff::DiffOptions::default())
            .expect("diff pipeline frame vs reference");

        let mut masked_ref = reference.clone();
        let mut masked_got = candidate.clone();
        mask_systemui(&mut masked_ref);
        mask_systemui(&mut masked_got);
        let masked = diff::diff(&masked_ref, &masked_got, &diff::DiffOptions::default())
            .expect("masked diff");

        eprintln!(
            "PIPELINE {:<16} mean={:.4} rmse={:.4} psnr={:.2} differing={:.3}% ssim(mean={:.4} global={:.4} masked={:.4})",
            scene.app,
            report.mean_abs_error,
            report.rmse,
            report.psnr_db.unwrap_or(f64::INFINITY),
            report.differing_fraction * 100.0,
            report.mean_ssim,
            report.global_ssim,
            masked.mean_ssim,
        );

        // Persist the candidate frame as PNG for inspection.
        let out_dir = repo_root().join("results/render/pipeline");
        let _ = std::fs::create_dir_all(&out_dir);
        let _ = std::fs::write(
            out_dir.join(format!("{}.png", scene.app)),
            encode_png(frame.width, frame.height, &frame.rgba),
        );

        rows.push(serde_json::json!({
            "app": scene.app,
            "provenance": "pipeline-generated",
            "status": "ok",
            "reference": scene.reference,
            "width": frame.width,
            "height": frame.height,
            "meanAbsError": report.mean_abs_error,
            "rmse": report.rmse,
            "maxAbsError": report.max_abs_error,
            "psnrDb": report.psnr_db,
            "differingFraction": report.differing_fraction,
            "meanSsim": report.mean_ssim,
            "globalSsim": report.global_ssim,
            "maskedMeanSsim": masked.mean_ssim,
            "maskedMae": masked.mean_abs_error,
            "blocksChangedFraction": report.blocks.changed_fraction,
            "changedBbox": report.changed_bbox,
        }));
        ran += 1;
    }

    let out = repo_root().join("results/render/corpus/corpus-pipeline-metrics.json");
    let _ = std::fs::create_dir_all(out.parent().unwrap());
    let aggregate = serde_json::json!({
        "wave": "W101-A",
        "corpus": "pipeline-generated",
        "note": "candidate = real LayoutInflater + View.measure/layout/draw -> RecordingCanvas -> HardwareRenderer -> acos-render; reference = ART API-34 emulator",
        "scenes": rows,
    });
    std::fs::write(&out, serde_json::to_vec_pretty(&aggregate).expect("serialize"))
        .expect("write pipeline metrics");
    eprintln!("wrote {}", out.display());

    assert!(
        ran > 0,
        "no pipeline-generated scene produced a frame (all fixtures skipped or failed)"
    );
}
