//! Text-metric parity against the API-34 ART reference captures (ADR-0019
//! `T-33`).
//!
//! The emulator `uiautomator` dumps give the bounds of each `wrap_content`
//! `TextView`/`Button` at 420 dpi (density 2.625). For a `wrap_content`
//! TextView the width is the shaped advance of the string and the height is
//! `descent - ascent` plus the framework's font padding. This test measures the
//! same strings through `RenderContext` (the guest-metric path) and writes the
//! deltas to `research/render-fidelity/TEXT_METRICS.md`.
//!
//! It deliberately does **not** assert tight bounds: the goal recorded in M3 is
//! *exact text bounds*, and today only the advance/height parity is measured.
//! A missing `utf8` font set or `ACOS_RENDER_SKIP_GOLDEN=1` skips the test.
//!
//! Tiers: the bounds are `art-reference (emulator, API 34, 420 dpi)`. The
//! `fixtures/reference-device/` captures are API 35 / 400 dpi and are used only
//! as a sanity check, never for pixel parity.

use std::path::PathBuf;

use acos_render::text::{TextEngine, TextStyle};
use acos_render::RenderContext;

const DENSITY: f32 = 2.625;

/// Benchmark numbers live in `results/render/text-draw-bench.md`; this is the
/// summary appended to the generated parity report so `TEXT_METRICS.md` is
/// self-contained.
const BENCH_SECTION: &str = r#"
## Text-draw speed (T-33 regression check)

Axis support must not cost steady-state draw speed. Same host, 3 runs each,
`"Hello ACOS"` at 36.75px, 60 iterations:

| Path | before p50 | after p50 | delta |
|---|---:|---:|---:|
| FreeType A8 mask | 12.20 us | 12.31 us | +0.9% |
| `ttf-parser` outline + tiny-skia | 61.65 us | 66.20 us | +7.4% |

The mask path is the ADR-0019 text path and stays at ~12 us (**5.4x** the
outline path; ADR-0019 recorded 4.8x within host noise). Raw runs and method:
`../../results/render/text-draw-bench.md`.

## Known gaps (honest tier labels)

* **Fallback selection** is script-agnostic: `FontCollection::fallback_chain`
  puts the requested family first and then walks the `fonts.xml` order, and
  `font_for_char` takes the first family with a glyph. The `<family lang="...">`
  attribute is parsed into `Family::lang` but not used to order the chain by
  script/locale. The ART fixtures are all en-US Latin, so the golden values do
  not exercise fallback; CJK/Indic locale ordering is untested here.
* **Vertical metrics under variation**: `font_metrics_for` reads the default
  instance's `hhea`/`OS/2` values and does not re-read them after
  `FT_Set_Var_Design_Coordinates`. For Roboto's `ital`/`wdth`/`wght` axes the
  vertical metrics do not change, so the table above is unaffected; an `opsz`
  face would need instance-aware metrics.
* **Height**: `top`/`bottom` are set equal to `ascent`/`descent`, so the View
  height delta (13-16%) is the known font-padding gap, not a shaping error.
"#;

fn workspace_path(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel)
}

fn reference_xml(app: &str) -> Option<String> {
    let p = workspace_path(&format!(
        "../../fixtures/reference/{app}/art-api34-x86_64/ui_initial.xml"
    ));
    std::fs::read_to_string(p).ok()
}

/// Bounds `[x0, y0, x1, y1]` of the node whose `text` attribute matches.
fn bounds_for_text(xml: &str, text: &str) -> Option<[i32; 4]> {
    let marker = format!("text=\"{text}\"");
    let idx = xml.find(&marker)?;
    let rest = &xml[idx..];
    let b = rest.find("bounds=\"")?;
    bounds_from(&rest[b + 8..])
}

/// Bounds `[x0, y0, x1, y1]` of the node whose `resource-id` matches.
fn bounds_for_id(xml: &str, id: &str) -> Option<[i32; 4]> {
    let marker = format!("resource-id=\"{id}\"");
    let idx = xml.find(&marker)?;
    let rest = &xml[idx..];
    let b = rest.find("bounds=\"")?;
    bounds_from(&rest[b + 8..])
}

fn bounds_from(rest: &str) -> Option<[i32; 4]> {
    let end = rest.find('"')?;
    let raw = &rest[..end];
    let mut nums = Vec::new();
    let mut cur = String::new();
    for ch in raw.chars() {
        if ch == '-' || ch.is_ascii_digit() {
            cur.push(ch);
        } else if !cur.is_empty() {
            if let Ok(v) = cur.parse::<i32>() {
                nums.push(v);
            }
            cur.clear();
        }
    }
    if !cur.is_empty() {
        if let Ok(v) = cur.parse::<i32>() {
            nums.push(v);
        }
    }
    if nums.len() >= 4 {
        Some([nums[0], nums[1], nums[2], nums[3]])
    } else {
        None
    }
}

struct Case {
    app: &'static str,
    label: &'static str,
    text: &'static str,
    size_sp: f32,
    /// `(resource-id, text)` selector; resource-id wins when non-empty.
    id: &'static str,
    by_text: &'static str,
}

const CASES: &[Case] = &[
    Case {
        app: "hello-views",
        label: "title",
        text: "Hello ACOS",
        size_sp: 14.0,
        id: "dev.acos.fixtures.helloviews:id/title",
        by_text: "",
    },
    Case {
        app: "hello-views",
        label: "result",
        text: "clicks: 0",
        size_sp: 14.0,
        id: "dev.acos.fixtures.helloviews:id/result",
        by_text: "",
    },
    Case {
        app: "counter",
        label: "count",
        text: "0",
        size_sp: 14.0,
        id: "dev.acos.fixtures.counter:id/count",
        by_text: "",
    },
    Case {
        app: "hello-views",
        label: "app-title",
        text: "Hello Views",
        size_sp: 20.0,
        id: "",
        by_text: "Hello Views",
    },
    Case {
        app: "counter",
        label: "app-title",
        text: "Counter",
        size_sp: 20.0,
        id: "",
        by_text: "Counter",
    },
    Case {
        app: "todo-list",
        label: "app-title",
        text: "Todo List",
        size_sp: 20.0,
        id: "",
        by_text: "Todo List",
    },
];

struct Measured {
    app: &'static str,
    label: &'static str,
    text: &'static str,
    size_sp: f32,
    ref_bounds: [i32; 4],
    ref_w: i32,
    ref_h: i32,
    acos_advance: f32,
    acos_ascent: f32,
    acos_descent: f32,
    acos_leading: f32,
    acos_height: f32,
    acos_tight: Option<(f32, f32)>,
}

#[test]
fn text_metrics_parity_vs_art() {
    if std::env::var("ACOS_RENDER_SKIP_GOLDEN").is_ok() {
        return;
    }
    let mut ctx = RenderContext::new();
    if acos_test_support::require_prereq(ctx.has_fonts(), "text_metrics_parity_vs_art", "no fonts available (set ACOS_FONT_DIR)").is_none() {
        return;
    }
    // A separate engine so the tight-bounds column can be read directly.
    let Some(fonts) = acos_test_support::require_some(acos_render::FontCollection::shared(), "text_metrics_parity_vs_art", "no shared font collection") else {
        return;
    };
    let font = fonts
        .resolve_typeface(None, 400, false)
        .expect("default sans-serif");

    let mut rows = Vec::new();
    for case in CASES {
        let Some(xml) = acos_test_support::require_some(reference_xml(case.app), "text_metrics_parity_vs_art", &format!("no reference dump for {}", case.app)) else {
            return;
        };
        let bounds = if case.id.is_empty() {
            bounds_for_text(&xml, case.by_text)
        } else {
            bounds_for_id(&xml, case.id)
        };
        let Some(bounds) = bounds else {
            acos_test_support::require_prereq(false, "text_metrics_parity_vs_art", &format!("bounds for {}/{} not found", case.app, case.label));
            continue;
        };

        let size_px = case.size_sp * DENSITY;
        let paint = ctx.paint_create();
        ctx.paint_set_text_size(paint, size_px).unwrap();
        let typeface = ctx.typeface_create(None, 400, false).unwrap();
        ctx.paint_set_typeface(paint, typeface).unwrap();
        let text: Vec<u16> = case.text.encode_utf16().collect();
        let advance = ctx.paint_measure_text(paint, &text).unwrap();
        let metrics = ctx.paint_get_font_metrics(paint).unwrap();

        // Tight glyph bounds (ink box) from the shape/bounds API.
        let style = TextStyle {
            font: font.clone(),
            family: None,
            size: size_px,
            scale_x: 1.0,
            skew_x: 0.0,
            letter_spacing: 0.0,
            fake_bold: false,
            features: Vec::new(),
            variations: Vec::new(),
        };
        let mut engine = TextEngine::new(fonts.clone());
        let tight = engine
            .text_bounds(&text, &style)
            .map(|r| (r.width(), r.height()));

        rows.push(Measured {
            app: case.app,
            label: case.label,
            text: case.text,
            size_sp: case.size_sp,
            ref_bounds: bounds,
            ref_w: bounds[2] - bounds[0],
            ref_h: bounds[3] - bounds[1],
            acos_advance: advance,
            acos_ascent: metrics.ascent,
            acos_descent: metrics.descent,
            acos_leading: metrics.leading,
            // The `wrap_content` TextView height with the default
            // `includeFontPadding`: `FontMetricsInt.bottom - top`. This is the
            // quantity the ART reference bounds measure, not `descent - ascent`.
            acos_height: metrics.bottom.ceil() - metrics.top.floor(),
            acos_tight: tight,
        });
    }

    if acos_test_support::require_prereq(!(rows.is_empty()), "text_metrics_parity_vs_art", "no reference bounds resolved").is_none() {
        return;
    }

    write_report(&rows);
    for r in &rows {
        let dw = (r.acos_advance - r.ref_w as f32).abs() / r.ref_w as f32 * 100.0;
        eprintln!(
            "{:11} {:9} {:14?} {:>4}sp advance={:8.2} ref_w={:>4} dw={:5.1}% h={:6.2} ref_h={:>3} tight={:?}",
            r.app, r.label, r.text, r.size_sp, r.acos_advance, r.ref_w, dw, r.acos_height, r.ref_h, r.acos_tight
        );
    }

    // The wrap_content advance must stay within the ADR-0019 envelope; this is
    // already true at 14sp (0.3%) and is what the renderer reproduces.
    for r in &rows {
        if r.size_sp == 14.0 {
            let dw = (r.acos_advance - r.ref_w as f32).abs() / r.ref_w as f32;
            assert!(dw < 0.05, "{} advance delta {dw:.3}", r.text);
        }
        // W185-A: the Skia `top`/`bottom` metrics make the wrap_content TextView
        // height match the ART reference within the 2 px acceptance bound.
        let dh = (r.acos_height - r.ref_h as f32).abs();
        assert!(
            dh <= 2.0,
            "{} height {:.1} vs ref {} (+{dh:.1}px)",
            r.text,
            r.acos_height,
            r.ref_h
        );
    }
}

/// Write the measured table next to the other render evidence.
fn write_report(rows: &[Measured]) {
    let dir = workspace_path("../../research/render-fidelity");
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let mut body = String::from(
        "# Text-metric parity: ACOS `RenderContext` vs ART reference (T-33)\n\n\
**Tier:** `art-reference (emulator, API 34, google_apis x86_64, 420 dpi, density 2.625)`.\n\
Reference bounds are read from `fixtures/reference/<app>/art-api34-x86_64/ui_initial.xml`\n\
at test time, so the numbers below are sourced, not hard-coded. The\n\
`fixtures/reference-device/` (API 35, 400 dpi) captures are **not** used here:\n\
a different density is only a sanity check, never pixel parity.\n\n\
Measured by `cargo test -p acos-render --test text_metrics_parity`.\n\n\
For a `wrap_content` TextView the View width equals the shaped advance; the\n\
View height is `descent - ascent` plus the framework font padding, so the\n\
height column is expected to be a bounded underestimate. `tight` is the\n\
glyph-ink bounding box (`TextEngine::text_bounds`); M3 targets *exact text\n\
bounds* and this column is the current state of that goal.\n\n",
    );
    body.push_str(
        "| app | label | text | size (sp) | ref bounds | ref W | ACOS advance | \u{394}W | ACOS ascent | descent | leading | W+H | ref H | tight (w\u{00d7}h) |\n",
    );
    body.push_str("|---|---|---|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|---|\n");
    for r in rows {
        let dw = (r.acos_advance - r.ref_w as f32).abs() / r.ref_w as f32 * 100.0;
        let tight = match r.acos_tight {
            Some((w, h)) => format!("{w:.1}\u{00d7}{h:.1}"),
            None => "-".to_string(),
        };
        body.push_str(&format!(
            "| {} | {} | `{}` | {:.0} | {} | {} | {:.2} | {:.1}% | {:.2} | {:.2} | {:.2} | {:.2} | {} | {} |\n",
            r.app,
            r.label,
            r.text,
            r.size_sp,
            fmt_bounds(r.ref_bounds),
            r.ref_w,
            r.acos_advance,
            dw,
            r.acos_ascent,
            r.acos_descent,
            r.acos_leading,
            r.acos_height,
            r.ref_h,
            tight,
        ));
    }
    body.push_str(
        "\n## Reading the table\n\n\
* At 14sp the advance matches the device to ~0.3% (`Hello ACOS` 188.45 vs 189 px,\n\
  `clicks: 0` 132.61 vs 133 px). This is the FreeType-mask + rustybuzz path of\n\
  ADR-0019 and the basis of the accepted per-glyph coverage error.\n\
* The `W+H` column is the `wrap_content` TextView height with the default\n\
  `includeFontPadding`, i.e. `FontMetricsInt.bottom - top`. W185-A derives\n\
  `top`/`bottom` from the face bounding box (`head` yMax/yMin) exactly like\n\
  Skia's FreeType backend, so the View height tracks ART to within 2 px (the\n\
  reference captures resolve 14sp to 37 px via `getDimensionPixelSize`).\n\
* Subpixel positioning: advances stay fractional (linear metrics, Android's\n\
  default `subpixel positioning on`); A8 masks are placed on the rounded integer\n\
  pen (`(x).round() + bitmap_left`), matching the measured PoC in\n\
  `REPORT.md` \u{00a7}3.2. No advance rounding is applied.\n\
* The 20sp app-title rows are the ActionBar title (inferred from the 71 px\n\
  reference height); the title is framework decor, not an ACOS `TextView`, so\n\
  it is reported as a size sanity check.\n\n\
## Variable-font axes\n\n\
`wght`/`wdth`/`ital` from `fonts.xml` plus `Paint.setFontVariationSettings` are\n\
now applied to FreeType rasterisation with `FT_Set_Var_Design_Coordinates`, using\n\
the same design-space values `rustybuzz` shapes with (`effective_variations`).\n\
The A8 mask cache key includes the axis coordinates, and the shaping cache key\n\
now includes the effective variations, so `wght=400` and `wght=700` never\n\
collide. `cargo test -p acos-render` covers strictly-increasing coverage for\n\
`wght` 100..900 and instance-cache correctness.\n",
    );
    body.push_str(BENCH_SECTION);
    let _ = std::fs::write(dir.join("TEXT_METRICS.md"), body);
}

fn fmt_bounds(b: [i32; 4]) -> String {
    format!("[{},{}][{},{}]", b[0], b[1], b[2], b[3])
}
