//! W66-A renderer fidelity corpus.
//!
//! Renders one `acos-render` scene per fixture app and scores it against the
//! authoritative **art-reference** capture (API-34 `google_apis` x86_64,
//! 1080x2400 @ 420 dpi) with the same [`acos_render::diff`] engine that
//! `acos frame-diff` uses. The scene is a DisplayList/Canvas reconstruction of
//! the app's View hierarchy (the VM still cannot drive the framework draw path,
//! see `results/render/M3_LOG.md` §3), so this measures the *renderer* — text,
//! drawables, shadows, blending and AA — not the VM lifecycle.
//!
//! Naming is deliberate: `reference` is always the ART capture, `candidate` is
//! the ACOS render. A `physical-device` capture is never substituted.
//!
//! Reproduce:
//!   ACOS_FONT_DIR=<android fonts> cargo test -p acos-render --test corpus -- --nocapture
//!
//! The test writes `results/render/corpus-metrics.json` and the candidate PNGs.

use std::path::{Path, PathBuf};

use acos_render::{diff, image, PngSetting, RenderContext, Scene};

struct Case {
    app: &'static str,
    /// Fidelity tier: every row here is `art-reference`.
    reference: &'static str,
    /// Upper bounds. Calibrated from the measured values with headroom so a
    /// font/AA revision does not fail while a real regression does.
    max_mae: f64,
    min_mean_ssim: f64,
    max_differing: f64,
}

// Thresholds are *measured* (see `results/render/corpus-metrics.json`), with
// roughly 25-40% headroom on MAE and a 0.005 SSIM floor margin.
const CASES: &[Case] = &[
    Case {
        app: "hello-views",
        reference: "fixtures/reference/hello-views/art-api34-x86_64/screenshot_initial.png",
        max_mae: 1.0,
        min_mean_ssim: 0.985,
        max_differing: 3.0,
    },
    Case {
        app: "counter",
        reference: "fixtures/reference/counter/art-api34-x86_64/screenshot_initial.png",
        max_mae: 2.0,
        min_mean_ssim: 0.980,
        max_differing: 4.0,
    },
    Case {
        app: "two-activity",
        reference: "fixtures/reference/two-activity/art-api34-x86_64/screenshot_initial.png",
        max_mae: 2.0,
        min_mean_ssim: 0.980,
        max_differing: 4.0,
    },
    Case {
        app: "sqlite-crud",
        reference: "fixtures/reference/sqlite-crud/art-api34-x86_64/screenshot_initial.png",
        max_mae: 2.0,
        min_mean_ssim: 0.980,
        max_differing: 4.0,
    },
    Case {
        app: "canvas-shapes",
        reference: "fixtures/reference/canvas-shapes/art-api34-x86_64/screenshot_initial.png",
        max_mae: 2.0,
        min_mean_ssim: 0.980,
        max_differing: 4.0,
    },
    // W82-A widget corpus (ListView/EditText/Button/CheckBox/Switch/RadioButton/
    // ProgressBar/AlertDialog/Toast/ellipsize/RTL). Thresholds are measured
    // (results/render/W82A_CORPUS.md) with headroom; the residual is documented
    // there (SystemUI chrome + vector-widget + ellipsize parity).
    Case {
        app: "form-validation",
        reference: "fixtures/reference/form-validation/art-api34-x86_64/screenshot_initial.png",
        max_mae: 1.3,
        min_mean_ssim: 0.983,
        max_differing: 3.0,
    },
    Case {
        app: "form-validation-error",
        reference: "fixtures/reference/form-validation/art-api34-x86_64/screenshot_after_empty.png",
        max_mae: 3.0,
        min_mean_ssim: 0.968,
        max_differing: 8.0,
    },
    Case {
        app: "list-scroll",
        reference: "fixtures/reference/list-scroll/art-api34-x86_64/screenshot_initial.png",
        max_mae: 4.0,
        min_mean_ssim: 0.950,
        max_differing: 5.5,
    },
    Case {
        app: "list-scroll-scrolled",
        reference: "fixtures/reference/list-scroll/art-api34-x86_64/screenshot_scrolled.png",
        max_mae: 5.0,
        min_mean_ssim: 0.908,
        max_differing: 8.0,
    },
    Case {
        app: "text-input",
        reference: "fixtures/reference/text-input/art-api34-x86_64/screenshot_typed.png",
        max_mae: 5.5,
        min_mean_ssim: 0.930,
        max_differing: 7.0,
    },
    Case {
        app: "dialog-menu-dialog",
        reference: "fixtures/reference/dialog-menu/art-api34-x86_64/screenshot_dialog.png",
        max_mae: 9.0,
        min_mean_ssim: 0.930,
        max_differing: 35.0,
    },
    Case {
        app: "widget-gallery",
        reference: "fixtures/reference/widget-gallery/art-api34-x86_64/screenshot_initial.png",
        max_mae: 5.5,
        min_mean_ssim: 0.935,
        max_differing: 7.5,
    },
];

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn out_dir() -> PathBuf {
    let d = repo_root().join("results/render/corpus");
    let _ = std::fs::create_dir_all(&d);
    d
}

#[test]
fn fixture_scenes_match_art_reference() {
    let mut rows = Vec::new();
    for case in CASES {
        let ref_path = repo_root().join(case.reference);
        if !ref_path.exists() {
            eprintln!(
                "SKIP {}: missing reference {} (capture with tools/oracle/)",
                case.app,
                ref_path.display()
            );
            continue;
        }
        let scene_path =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("tests/scenes/{}.json", case.app));
        assert!(
            scene_path.exists(),
            "missing corpus scene {}",
            scene_path.display()
        );
        let scene = Scene::load(&scene_path).expect("load scene");
        let mut ctx = RenderContext::new();
        let out = scene.render(&mut ctx, 1).expect("render scene");
        assert!(
            out.unsupported_ops.is_empty(),
            "{} rendered with unsupported ops: {:?}",
            case.app,
            out.unsupported_ops
        );

        let candidate_bytes = out.frame.png(PngSetting::FastFiltered);
        let candidate_path = out_dir().join(format!("{}.png", case.app));
        std::fs::write(&candidate_path, &candidate_bytes).expect("write candidate PNG");

        let reference =
            image::decode(&std::fs::read(&ref_path).expect("read reference")).expect("decode ref");
        let got = image::decode_png(&candidate_bytes).expect("decode candidate");
        let report = diff::diff(&reference, &got, &diff::DiffOptions::default()).expect("diff");

        eprintln!(
            "{:<14} mean={:.4} rmse={:.4} psnr={:.2} differing={:.3}% ssim(mean={:.4} global={:.4}) blocks={:.2}%",
            case.app,
            report.mean_abs_error,
            report.rmse,
            report.psnr_db.unwrap_or(f64::INFINITY),
            report.differing_fraction * 100.0,
            report.mean_ssim,
            report.global_ssim,
            report.blocks.changed_fraction * 100.0,
        );

        rows.push(serde_json::json!({
            "app": case.app,
            "tier": "art-reference",
            "reference": case.reference,
            "scene": format!("crates/acos-render/tests/scenes/{}.json", case.app),
            "width": out.frame.width,
            "height": out.frame.height,
            "unsupportedOps": out.unsupported_ops,
            "meanAbsError": report.mean_abs_error,
            "rmse": report.rmse,
            "maxAbsError": report.max_abs_error,
            "psnrDb": report.psnr_db,
            "differingFraction": report.differing_fraction,
            "meanSsim": report.mean_ssim,
            "globalSsim": report.global_ssim,
            "blocksChangedFraction": report.blocks.changed_fraction,
            "changedBbox": report.changed_bbox,
            "thresholds": {
                "maxMae": case.max_mae,
                "minMeanSsim": case.min_mean_ssim,
                "maxDifferingPct": case.max_differing,
            },
        }));

        assert!(
            report.mean_abs_error <= case.max_mae,
            "{} MAE {:.4} exceeded threshold {:.4}",
            case.app,
            report.mean_abs_error,
            case.max_mae
        );
        assert!(
            report.mean_ssim >= case.min_mean_ssim,
            "{} mean SSIM {:.4} fell below {:.4}",
            case.app,
            report.mean_ssim,
            case.min_mean_ssim
        );
        assert!(
            report.differing_fraction * 100.0 <= case.max_differing,
            "{} differing {:.3}% exceeded {:.3}%",
            case.app,
            report.differing_fraction * 100.0,
            case.max_differing
        );
        assert!(!report.identical, "{} frame was pixel-identical to reference", case.app);
    }

    assert!(!rows.is_empty(), "no corpus references were available");
    let aggregate = serde_json::json!({
        "wave": "W66-A",
        "corpus": "fixture-apps",
        "note": "candidate = acos-render scene reconstruction; reference = ART API-34 emulator",
        "scenes": rows,
    });
    let out = out_dir().join("corpus-metrics.json");
    std::fs::write(&out, serde_json::to_vec_pretty(&aggregate).expect("serialize")).expect("write");
    eprintln!("wrote {}", rel(&out));
}

fn rel(p: &Path) -> String {
    p.strip_prefix(repo_root()).unwrap_or(p).display().to_string()
}
