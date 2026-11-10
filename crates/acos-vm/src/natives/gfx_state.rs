//! Shared per-thread `acos-render` context for the `android.graphics.*` natives.
//!
//! The framework passes opaque `jlong` native pointers between natives. W19-B's
//! `gfx_paint`/`gfx_path`/`gfx_matrix` own the *authoritative* handle tables for
//! those types (generation-checked, in [`super::gfx_handles`]); this module owns
//! the one [`acos_render::RenderContext`] that the canvas/node/bitmap/hwui
//! natives rasterise into.
//!
//! The two worlds meet at **record time**: `gfx_canvas` resolves a `Paint` /
//! `Path` / `Matrix` jlong through W19-B's accessors and copies the value into
//! the render context, then stores that frozen copy's handle in the
//! [`acos_render::DisplayOp`]. A later `Paint.setColor` (or `Path.lineTo`)
//! therefore cannot alter an already-recorded op — the reviewer's risk 2.
//!
//! One context per host thread keeps the interpreter lock-free (`Vm` is
//! `!Send`).

use std::cell::RefCell;

use acos_render::context::RenderContext;
use acos_render::handle::Handle as RHandle;
use acos_render::paint::{
    Paint, PaintStyle, StrokeCap, StrokeJoin, TextAlign,
};

use crate::error::{VmError, VmResult};
use crate::natives::gfx_paint::{self, PaintRec};

thread_local! {
    static CTX: RefCell<RenderContext> = RefCell::new(RenderContext::new());
}

/// Run `f` with this thread's renderer context.
pub(crate) fn with_ctx<R>(f: impl FnOnce(&mut RenderContext) -> R) -> R {
    CTX.with(|c| f(&mut c.borrow_mut()))
}

/// A raw `jlong` as an `acos_render::Handle`.
#[inline]
pub(crate) fn rh(j: i64) -> RHandle {
    RHandle(j as u64)
}

/// An `acos_render::Handle` as a raw `jlong`.
#[inline]
pub(crate) fn gj(h: RHandle) -> i64 {
    h.0 as i64
}

/// A structured stale/null graphics-handle error (`STALE_HANDLE`).
pub(crate) fn stale(what: &str, handle: i64) -> VmError {
    VmError::StaleHandle {
        what: format!("{what} handle 0x{handle:016x}"),
    }
}

/// Map an `acos-render` error onto a structured VM error. A bad handle becomes
/// `STALE_HANDLE` (never a panic / silent default); everything else is
/// surfaced as an unsupported op naming `op`.
pub(crate) fn render_err(op: &str, e: acos_render::RenderError) -> VmError {
    match e {
        acos_render::RenderError::BadHandle => VmError::StaleHandle {
            what: op.to_string(),
        },
        other => VmError::unsupported_api(format!("{op} ({})", other.code()), "acos-render"),
    }
}

/// Convert W19-B's primitive `PaintRec` into the renderer's `Paint`.
///
/// Shader/colour-filter/mask-filter/path-effect handles and the concrete
/// `Typeface` object are deliberately not carried across: `gfx_paint` keeps
/// those in its own tables and the renderer resolves fonts through its own
/// `FontCollection`. This is a documented fidelity limit, not a silent drop —
/// the fields the canvas path consumes (colour, alpha, style, stroke, text
/// size/scale/skew/align/spacing, flags, shadow) are mapped exactly.
pub(crate) fn render_paint_from(rec: &PaintRec) -> Paint {
    let mut p = Paint {
        color: rec.color,
        alpha: rec.alpha.clamp(0, 255) as u32,
        ..Paint::default()
    };
    if let Some(s) = PaintStyle::from_ordinal(rec.style) {
        p.style = s;
    }
    p.stroke_width = rec.stroke_width;
    p.stroke_miter = rec.stroke_miter;
    if let Some(c) = StrokeCap::from_ordinal(rec.stroke_cap) {
        p.stroke_cap = c;
    }
    if let Some(j) = StrokeJoin::from_ordinal(rec.stroke_join) {
        p.stroke_join = j;
    }
    if let Some(a) = TextAlign::from_ordinal(rec.text_align) {
        p.text_align = a;
    }
    p.anti_alias = rec.flags & 0x01 != 0;
    p.filter_bitmap = rec.flags & 0x02 != 0;
    p.dither = rec.flags & 0x04 != 0;
    p.underline = rec.flags & 0x08 != 0;
    p.strike_thru = rec.flags & 0x10 != 0;
    p.fake_bold = rec.flags & 0x20 != 0;
    p.linear_text = rec.flags & 0x40 != 0;
    p.subpixel_text = rec.flags & 0x80 != 0;
    p.hinting = rec.flags & 0x200 != 0;
    p.text_size = rec.text_size;
    p.text_scale_x = rec.text_scale_x;
    p.text_skew_x = rec.text_skew_x;
    p.letter_spacing = rec.letter_spacing;
    p.word_spacing = rec.word_spacing;
    p.font_feature_settings = rec.font_features.clone();
    p.font_variation_settings = rec.font_variations.clone();
    if let Some((radius, dx, dy, color)) = rec.shadow {
        p.shadow_layer = Some(acos_render::paint::ShadowLayer {
            radius,
            dx,
            dy,
            color,
        });
    }
    p
}

/// Snapshot a framework `Paint` jlong into a frozen renderer paint, returning
/// the `jlong` of the frozen copy. Record-time capture, per the advisor review.
pub(crate) fn snapshot_paint(paint: i64) -> VmResult<i64> {
    let rec = gfx_paint::paint_rec(paint)?;
    let mut rendered = render_paint_from(&rec);
    if rec.shader != 0 {
        match gfx_paint::shader_spec(rec.shader) {
            Some(spec) => rendered.shader = Some(spec),
            None => with_ctx(|c| {
                c.unsupported("Paint shader handle did not resolve to a ShaderSpec")
                    .ok();
            }),
        }
    }
    Ok(with_ctx(|c| gj(c.paints.insert(rendered))))
}

/// Snapshot a framework `Path` jlong into a frozen renderer path.
pub(crate) fn snapshot_path(path: i64) -> VmResult<i64> {
    let p = super::gfx_path::path_clone(path)?;
    Ok(with_ctx(|c| gj(c.paths.insert(p))))
}
