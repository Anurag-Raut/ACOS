//! End-to-end variable-font axis coverage through `RenderContext`
//! (ADR-0019 `T-33`).
//!
//! `fonts.xml` points every `sans-serif` weight entry at the Roboto variable
//! font (`Roboto-Regular.ttf`, `wght` 100–900) and carries the matching
//! `<axis tag="wght">` per entry. Before `T-33` the FreeType A8 path always
//! rasterised the default instance, so a 700-weight paint drew the 400-weight
//! glyphs. These tests draw real text through `canvas_draw_text` and assert:
//!
//! * heavier weights cover strictly more pixels (monotonic ink), and
//! * the same weight is deterministic while a different weight is not.
//!
//! The per-glyph mask tests live in `text/freetype.rs`; this file exercises the
//! whole `context -> effective_variations -> FT_Set_Var_Design_Coordinates`
//! plumbing, including the mask cache.

use acos_render::RenderContext;

const TEXT: &str = "Hello";
const SIZE_PX: f32 = 36.75;

/// Draw `TEXT` at `weight` on a fresh surface and return the raw RGBA frame.
fn draw_weight(ctx: &mut RenderContext, weight: u16) -> (Vec<u8>, usize) {
    let surface = ctx.surface_create(320, 80, 1.0).unwrap();
    ctx.canvas_for_surface(surface).unwrap();
    let paint = ctx.paint_create();
    ctx.paint_set_text_size(paint, SIZE_PX).unwrap();
    ctx.paint_set_color(paint, 0xff00_0000).unwrap();
    let typeface = ctx.typeface_create(None, weight, false).unwrap();
    ctx.paint_set_typeface(paint, typeface).unwrap();
    let text: Vec<u16> = TEXT.encode_utf16().collect();
    ctx.canvas_draw_text(surface, &text, 8.0, 60.0, paint)
        .unwrap();
    ctx.surface_present(surface, 0).unwrap();
    let rgba = ctx.surface_rgba8(surface).unwrap();
    let ink = rgba.chunks_exact(4).filter(|p| p[3] > 0).count();
    (rgba, ink)
}

#[test]
fn variable_weight_renders_strictly_heavier() {
    let mut ctx = RenderContext::new();
    if !ctx.has_fonts() {
        eprintln!("SKIP: no fonts available (set ACOS_FONT_DIR)");
        return;
    }
    // The four weights are all `<font>` entries of the same variable file.
    let weights = [100u16, 400, 700, 900];
    let mut inks = Vec::new();
    for weight in weights {
        let (_rgba, ink) = draw_weight(&mut ctx, weight);
        inks.push((weight, ink));
    }
    for pair in inks.windows(2) {
        let (light_w, light_ink) = pair[0];
        let (heavy_w, heavy_ink) = pair[1];
        assert!(
            heavy_ink > light_ink,
            "wght {heavy_w} ink {heavy_ink} not greater than wght {light_w} ink {light_ink}: {inks:?}"
        );
    }
    eprintln!("variable weight ink pixels (320x80, 36.75px): {inks:?}");
}

#[test]
fn same_weight_is_deterministic_different_weight_is_not() {
    let mut ctx = RenderContext::new();
    if !ctx.has_fonts() {
        eprintln!("SKIP: no fonts available (set ACOS_FONT_DIR)");
        return;
    }
    let (light_a, _) = draw_weight(&mut ctx, 200);
    let (light_b, _) = draw_weight(&mut ctx, 200);
    let (heavy, _) = draw_weight(&mut ctx, 800);
    assert_eq!(
        light_a, light_b,
        "same weight must produce identical frames"
    );
    assert_ne!(light_a, heavy, "different weights must differ");
}
