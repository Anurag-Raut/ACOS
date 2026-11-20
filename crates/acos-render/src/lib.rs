//! # acos-render
//!
//! ACOS's offscreen software 2D renderer: a Rust replacement for Android's
//! HWUI + Skia + minikin graphics stack.
//!
//! The Android 14 framework executes real `android.graphics.*` Java bytecode on
//! ACOS's Rust VM; those classes bottom out in `native` methods
//! (`Canvas.nDrawRect`, `Paint.nSetColor`, `RenderNode.nSetLeftTopRightBottom`,
//! `Bitmap.nativeCreate`, `Paint.nGetTextAdvances`, …). This crate implements
//! the semantics behind those natives:
//!
//! * **Rasterisation** uses [`tiny_skia`], a Rust port of Skia's software
//!   rasteriser, so AA/blend behaviour stays close to Android's software canvas.
//! * **Text** shapes with `rustybuzz` (HarfBuzz port) and rasterises glyphs with
//!   **FreeType** (`FT_LOAD_NO_HINTING | FT_LOAD_RENDER`, grayscale A8 masks,
//!   ADR-0019); `ttf-parser` supplies outlines (the path fallback) and metrics,
//!   with real Android font resolution from `fonts.xml`.
//! * **Frames** are offscreen surfaces with a strict freshness contract
//!   (`frame_seq`, production time, causal id, content hash) so an agent never
//!   receives a stale frame labelled fresh.
//!
//! ## Design constraints
//!
//! * No GPU, no fixed large mappings: a headless task allocates nothing beyond
//!   the objects it creates.
//! * Fonts are `mmap`ed once and shared `Arc` across every surface/context.
//! * Objects live in generation-checked handle tables owned by
//!   [`RenderContext`]; the public API takes integer [`Handle`]s and primitives
//!   only, so the JNI binding layer can map framework `jlong`s onto them.
//! * Unsupported operations are never silently skipped: they are recorded in
//!   the frame's `unsupported_ops` list or escalated in strict mode.

// The public API deliberately mirrors the `android.graphics` native signatures
// (many primitive coordinates/colours per call), so the argument-count lint is
// not useful here.
#![allow(clippy::too_many_arguments)]

pub mod bitmap;
pub mod canvas;
pub mod color;
pub mod context;
pub mod diff;
pub mod error;
pub mod geom;
pub mod handle;
pub mod hash;
pub mod image;
pub mod matrix;
/// P2: streaming MD5, for the published `content_hash` without a `md5sum` fork.
pub mod md5;
pub mod paint;
pub mod path;
pub mod region;
pub mod render_node;
pub mod scene;
pub mod surface;
pub mod text;

pub use bitmap::{Bitmap, BitmapConfig};
pub use canvas::{Canvas, ClipOp};
pub use color::{AndroidBlendMode, PorterDuffMode};
pub use context::{RenderContext, RenderNodeInfo};
pub use diff::{diff, DiffOptions, DiffReport};
pub use error::{RenderError, RenderResult};
pub use geom::{NinePatchChunk, Outline, PointF, Rect, RectF};
pub use handle::{Handle, HandleTable};
pub use matrix::{Matrix3x3, ScaleToFit};
pub use paint::{
    BlurStyle, ColorFilterSpec, MaskFilterSpec, Paint, PaintStyle, PathEffectSpec,
    RenderEffectSpec, ShaderSpec, StrokeCap, StrokeJoin, TextAlign, TileMode,
};
pub use path::{FillType, Path, PathMeasure, PathOp, Verb};
pub use region::{Region, RegionOp};
pub use render_node::{DisplayOp, EllipsizeMode, LayerType, RenderNode};
pub use scene::{Scene, SceneRender};
pub use surface::{Frame, FrameClass, PngSetting, Surface};
pub use text::fonts::{FontCollection, FontRef};
pub use text::layout::{BreakStrategy, LineBreak, LineBreaker, ParagraphConstraints};
pub use text::{FontMetrics, FontMetricsInt, ShapedText, TextStyle};

/// The renderer's own version string, surfaced in evidence metadata.
pub const ACOS_RENDER_VERSION: &str = env!("CARGO_PKG_VERSION");
