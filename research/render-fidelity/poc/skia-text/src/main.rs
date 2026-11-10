//! Feasibility PoC: can version-pinned Skia CPU rasterisation + FreeType glyph
//! rendering reproduce Android 14's text pixels?
//!
//! Renders "Hello ACOS" with the exact Roboto font from the Android 14 system
//! image, sweeping FontHinting x subpixel x edging, and writes one RGBA PNG per
//! configuration (white glyphs, straight alpha on transparent). The alignment
//! and residual analysis against the real device screenshot is done by
//! `../diff_text.py`.
//!
//! Usage: skia-text-poc <font.ttf> <out_dir> [text]

#![allow(deprecated)]

use skia_safe::font::Edging;
use skia_safe::{
    surfaces, Color, Data, EncodedImageFormat, Font, FontHinting, FontMgr, Paint,
};

const W: i32 = 480;
const H: i32 = 140;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: skia-text-poc <font.ttf> <out_dir> [text]");
        std::process::exit(2);
    }
    let font_path = &args[1];
    let out_dir = &args[2];
    let text = args.get(3).map(String::as_str).unwrap_or("Hello ACOS");
    let size = 36.75f32;

    std::fs::create_dir_all(out_dir).unwrap();
    let bytes = std::fs::read(font_path).unwrap();
    let mgr = FontMgr::new();
    let tf = mgr
        .new_from_data(Data::new_copy(&bytes), None)
        .expect("SkFontMgr::makeFromData returned None");
    println!(
        "font={} family={:?} ps={:?} upem={:?}",
        font_path,
        tf.family_name(),
        tf.post_script_name(),
        tf.units_per_em()
    );

    let hintings = [FontHinting::None, FontHinting::Slight, FontHinting::Normal, FontHinting::Full];
    let edgings = [Edging::AntiAlias, Edging::SubpixelAntiAlias];
    let subpixels = [false, true];
    let autohints = [false, true];

    // Metrics at the default hinting/edging for reference.
    {
        let font = Font::from_typeface(tf.clone(), size);
        let fh = FontHinting::Slight;
        let mut f2 = font.clone();
        f2.set_hinting(fh);
        let (adv, bounds) = f2.measure_str(text, None);
        let (spacing, metrics) = f2.metrics();
        println!(
            "measure: advance={adv:.3} bounds={bounds:?} spacing={spacing:.3} ascent={:.3} descent={:.3} top={:.3} bottom={:.3} leading={:.3}",
            metrics.ascent, metrics.descent, metrics.top, metrics.bottom, metrics.leading
        );
    }

    let mut n = 0;
    for &h in &hintings {
        for &sp in &subpixels {
            for &e in &edgings {
                for &ah in &autohints {
                    let mut font = Font::from_typeface(tf.clone(), size);
                    font.set_hinting(h);
                    font.set_subpixel(sp);
                    font.set_edging(e);
                    font.set_force_auto_hinting(ah);
                    font.set_baseline_snap(false);
                    font.set_linear_metrics(false);

                    let mut surface = surfaces::raster_n32_premul((W, H)).unwrap();
                    surface.canvas().clear(Color::TRANSPARENT);
                    let mut paint = Paint::default();
                    paint.set_anti_alias(true);
                    paint.set_color(Color::WHITE);
                    let (advance, bounds) = font.measure_str(text, Some(&paint));
                    surface
                        .canvas()
                        .draw_str(text, (20.0f32, 100.0f32), &font, &paint);
                    let img = surface.image_snapshot();
                    let name = format!(
                        "h{}_sp{}_e{}_ah{}",
                        hn(h),
                        sp as u8,
                        en(e),
                        ah as u8
                    );
                    let data = img
                        .encode_to_data(EncodedImageFormat::PNG)
                        .expect("png encode");
                    std::fs::write(format!("{out_dir}/{name}.png"), data.as_bytes()).unwrap();
                    println!(
                        "cfg {name}: advance={advance:.3} bounds={bounds:?}"
                    );
                    n += 1;
                }
            }
        }
    }
    println!("wrote {n} configurations to {out_dir}");
}

fn hn(h: FontHinting) -> &'static str {
    match h {
        FontHinting::None => "none",
        FontHinting::Slight => "slight",
        FontHinting::Normal => "normal",
        FontHinting::Full => "full",
    }
}

fn en(e: Edging) -> &'static str {
    match e {
        Edging::Alias => "alias",
        Edging::AntiAlias => "aa",
        Edging::SubpixelAntiAlias => "lcd",
    }
}
