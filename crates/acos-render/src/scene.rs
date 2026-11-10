//! A small JSON scene format for tests and benchmarks that does not need the
//! DEX VM, plus the renderer that turns it into a `RenderNode` tree and
//! rasterises it.
//!
//! The format is intentionally close to the `RenderNode`/`Canvas` API so the
//! same display-list machinery the framework natives drive is exercised.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::context::RenderContext;
use crate::error::{RenderError, RenderResult};
use crate::geom::RectF;
use crate::handle::Handle;
use crate::paint::TileMode;
use crate::path::Path as AcosPath;
use crate::render_node::DisplayOp;
use crate::surface::Frame;
use crate::text::fonts::FontCollection;

/// Top-level scene description.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Scene {
    pub width: u32,
    pub height: u32,
    #[serde(default = "default_density")]
    pub density: f32,
    /// Optional background colour (ARGB hex or "#RRGGBB").
    #[serde(default)]
    pub background: Option<String>,
    pub root: SceneNode,
}

fn default_density() -> f32 {
    1.0
}

/// A render node in the scene.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct SceneNode {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub bounds: Option<[i32; 4]>,
    #[serde(default = "one")]
    pub alpha: f32,
    #[serde(default, rename = "translate")]
    pub translation: Option<[f32; 2]>,
    #[serde(default)]
    pub scale: Option<[f32; 2]>,
    #[serde(default)]
    pub rotation: f32,
    #[serde(default)]
    pub pivot: Option<[f32; 2]>,
    #[serde(default, rename = "clipToBounds")]
    pub clip_to_bounds: bool,
    #[serde(default)]
    pub elevation: f32,
    #[serde(default)]
    pub outline: Option<OutlineSpec>,
    #[serde(default)]
    pub ops: Vec<OpSpec>,
    #[serde(default)]
    pub children: Vec<SceneNode>,
}

fn one() -> f32 {
    1.0
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum OutlineSpec {
    Rect { bounds: [f32; 4] },
    RoundRect { bounds: [f32; 4], radius: f32 },
}

/// A drawing operation.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "op", rename_all = "camelCase")]
pub enum OpSpec {
    Rect {
        rect: [f32; 4],
        #[serde(default)]
        paint: Option<PaintSpec>,
    },
    RoundRect {
        rect: [f32; 4],
        #[serde(default)]
        rx: f32,
        #[serde(default)]
        ry: f32,
        #[serde(default)]
        paint: Option<PaintSpec>,
    },
    Oval {
        rect: [f32; 4],
        #[serde(default)]
        paint: Option<PaintSpec>,
    },
    Circle {
        cx: f32,
        cy: f32,
        r: f32,
        #[serde(default)]
        paint: Option<PaintSpec>,
    },
    Line {
        x0: f32,
        y0: f32,
        x1: f32,
        y1: f32,
        #[serde(default)]
        paint: Option<PaintSpec>,
    },
    Text {
        text: String,
        x: f32,
        y: f32,
        #[serde(default)]
        paint: Option<PaintSpec>,
    },
    Color { color: String },
    Path {
        commands: Vec<serde_json::Value>,
        #[serde(default)]
        paint: Option<PaintSpec>,
    },
    Bitmap {
        path: String,
        dst: [f32; 4],
        #[serde(default)]
        src: Option<[f32; 4]>,
        #[serde(default)]
        filter: bool,
    },
}

/// A paint description.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct PaintSpec {
    #[serde(default)]
    pub color: Option<String>,
    /// `fill` (default), `stroke`, `fillStroke`.
    #[serde(default)]
    pub style: Option<String>,
    #[serde(default, rename = "strokeWidth")]
    pub stroke_width: Option<f32>,
    #[serde(default, rename = "antiAlias")]
    pub anti_alias: Option<bool>,
    #[serde(default, rename = "textSize")]
    pub text_size: Option<f32>,
    #[serde(default)]
    pub align: Option<String>,
    #[serde(default, rename = "letterSpacing")]
    pub letter_spacing: Option<f32>,
    #[serde(default, rename = "fakeBold")]
    pub fake_bold: bool,
    #[serde(default)]
    pub underline: bool,
    #[serde(default)]
    pub typeface: Option<TypefaceSpec>,
    #[serde(default)]
    pub shader: Option<ShaderSpecJson>,
    #[serde(default)]
    pub blur: Option<f32>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct TypefaceSpec {
    #[serde(default)]
    pub family: Option<String>,
    #[serde(default = "default_weight")]
    pub weight: u16,
    #[serde(default)]
    pub italic: bool,
}

fn default_weight() -> u16 {
    400
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ShaderSpecJson {
    Linear {
        start: [f32; 2],
        end: [f32; 2],
        colors: Vec<String>,
        #[serde(default)]
        positions: Option<Vec<f32>>,
        #[serde(default)]
        tile: Option<String>,
    },
    Radial {
        center: [f32; 2],
        radius: f32,
        colors: Vec<String>,
        #[serde(default)]
        positions: Option<Vec<f32>>,
        #[serde(default)]
        tile: Option<String>,
    },
    Sweep {
        center: [f32; 2],
        colors: Vec<String>,
        #[serde(default)]
        positions: Option<Vec<f32>>,
    },
    /// `android.graphics.BitmapShader` — tiles a bitmap in x and y with the
    /// given tile mode (W70-A). `path` is resolved relative to the scene file.
    Bitmap {
        path: String,
        #[serde(default)]
        tile: Option<String>,
    },
}

/// Result of rendering a scene.
pub struct SceneRender {
    pub seq: u64,
    pub frame: Frame,
    pub node_count: usize,
    pub unsupported_ops: Vec<String>,
    pub frame_class: crate::surface::FrameClass,
}

impl Scene {
    pub fn from_json(json: &str) -> RenderResult<Scene> {
        serde_json::from_str(json).map_err(|e| RenderError::Decode {
            msg: format!("scene JSON: {e}"),
        })
    }

    pub fn load(path: &Path) -> RenderResult<Scene> {
        let text = std::fs::read_to_string(path).map_err(|e| RenderError::Io {
            msg: format!("reading scene {}: {e}", path.display()),
        })?;
        let mut scene = Self::from_json(&text)?;
        // Resolve relative asset paths against the scene file's directory.
        if let Some(dir) = path.parent() {
            resolve_assets(&mut scene.root, dir);
        }
        Ok(scene)
    }

    /// Build the render-node tree and render it.
    pub fn render(&self, ctx: &mut RenderContext, cause_id: i64) -> RenderResult<SceneRender> {
        let surface = ctx.surface_create(self.width, self.height, self.density)?;
        let (root, count) = self.build_tree(ctx, &self.root)?;
        if let Some(bg) = &self.background {
            let color = parse_color(bg).ok_or_else(|| RenderError::invalid("bad background colour"))?;
            // Background is the first display-list op so the per-frame clear
            // does not wipe it.
            if let Some(node) = ctx.render_nodes.get_mut(root) {
                node.display_list.insert(0, DisplayOp::DrawColor(color));
            }
        }
        let seq = ctx.surface_render_node(surface, root, cause_id)?;
        let frame = ctx.surface_capture_fresh(surface, 0)?;
        let unsupported_ops = ctx.surface_capture(surface)?.map(|f| f.unsupported_ops).unwrap_or_default();
        Ok(SceneRender {
            seq,
            frame_class: frame.class,
            frame,
            node_count: count,
            unsupported_ops,
        })
    }

    /// Build the render-node tree, returning (root handle, node count).
    pub fn build_tree(&self, ctx: &mut RenderContext, root: &SceneNode) -> RenderResult<(Handle, usize)> {
        build_node(ctx, root, None)
    }
}

fn build_node(ctx: &mut RenderContext, node: &SceneNode, _parent: Option<Handle>) -> RenderResult<(Handle, usize)> {
    let h = ctx.render_node_create(node.name.as_deref().unwrap_or("node"));
    let mut count = 1;
    if let Some(b) = node.bounds {
        ctx.render_node_set_bounds(h, b[0], b[1], b[2], b[3])?;
    }
    if node.alpha != 1.0 {
        ctx.render_node_set_alpha(h, node.alpha)?;
    }
    if let Some(t) = node.translation {
        ctx.render_node_set_translation(h, t[0], t[1])?;
    }
    if let Some(s) = node.scale {
        ctx.render_node_set_scale(h, s[0], s[1])?;
    }
    if node.rotation != 0.0 {
        ctx.render_node_set_rotation(h, node.rotation)?;
    }
    if let Some(p) = node.pivot {
        ctx.render_node_set_pivot(h, p[0], p[1])?;
    }
    if node.clip_to_bounds {
        ctx.render_node_set_clip_to_bounds(h, true)?;
    }
    if node.elevation != 0.0 {
        ctx.render_node_set_elevation(h, node.elevation)?;
    }
    if let Some(o) = &node.outline {
        match o {
            OutlineSpec::Rect { bounds } => {
                ctx.render_node_set_outline_rect(
                    h,
                    RectF::new(bounds[0], bounds[1], bounds[2], bounds[3]),
                )?;
            }
            OutlineSpec::RoundRect { bounds, radius } => {
                ctx.render_node_set_outline_round_rect(
                    h,
                    bounds[0] as i32,
                    bounds[1] as i32,
                    bounds[2] as i32,
                    bounds[3] as i32,
                    *radius,
                    *radius,
                )?;
            }
        }
    }
    for op in &node.ops {
        if let Some(display) = build_op(ctx, op)? {
            ctx.render_node_push_op(h, display)?;
        }
    }
    for child in &node.children {
        let (ch, c) = build_node(ctx, child, Some(h))?;
        ctx.render_node_add_child(h, ch)?;
        count += c;
    }
    Ok((h, count))
}

fn build_op(ctx: &mut RenderContext, op: &OpSpec) -> RenderResult<Option<DisplayOp>> {
    Ok(match op {
        OpSpec::Rect { rect, paint } => {
            let p = build_paint(ctx, paint.as_ref())?;
            Some(DisplayOp::DrawRect(rect4(rect), p))
        }
        OpSpec::RoundRect { rect, rx, ry, paint } => {
            let p = build_paint(ctx, paint.as_ref())?;
            Some(DisplayOp::DrawRoundRect(rect4(rect), *rx, *ry, p))
        }
        OpSpec::Oval { rect, paint } => {
            let p = build_paint(ctx, paint.as_ref())?;
            Some(DisplayOp::DrawOval(rect4(rect), p))
        }
        OpSpec::Circle { cx, cy, r, paint } => {
            let p = build_paint(ctx, paint.as_ref())?;
            Some(DisplayOp::DrawCircle(*cx, *cy, *r, p))
        }
        OpSpec::Line { x0, y0, x1, y1, paint } => {
            let p = build_paint(ctx, paint.as_ref())?;
            Some(DisplayOp::DrawLine(*x0, *y0, *x1, *y1, p))
        }
        OpSpec::Text { text, x, y, paint } => {
            let p = build_paint(ctx, paint.as_ref())?;
            Some(DisplayOp::DrawText(text.encode_utf16().collect(), *x, *y, p))
        }
        OpSpec::Color { color } => {
            let c = parse_color(color).ok_or_else(|| RenderError::invalid("bad colour"))?;
            Some(DisplayOp::DrawColor(c))
        }
        OpSpec::Path { commands, paint } => {
            let path = build_path(commands)?;
            let ph = ctx.paths.insert(path);
            let p = build_paint(ctx, paint.as_ref())?;
            Some(DisplayOp::DrawPath(ph, p))
        }
        OpSpec::Bitmap { path, dst, src: _, filter } => {
            let bytes = std::fs::read(path).map_err(|e| RenderError::Io {
                msg: format!("reading bitmap {path}: {e}"),
            })?;
            let bh = ctx.bitmap_create_from_bytes(&bytes)?;
            Some(DisplayOp::DrawBitmap(
                bh,
                None,
                rect4(dst),
                None,
                *filter,
            ))
        }
    })
}

fn build_paint(ctx: &mut RenderContext, spec: Option<&PaintSpec>) -> RenderResult<Handle> {
    let h = ctx.paint_create();
    let Some(spec) = spec else {
        return Ok(h);
    };
    if let Some(c) = &spec.color {
        let color = parse_color(c).ok_or_else(|| RenderError::invalid("bad paint colour"))?;
        ctx.paint_set_color(h, color)?;
    }
    if let Some(style) = &spec.style {
        let ordinal = match style.as_str() {
            "fill" => 0,
            "stroke" => 1,
            "fillStroke" | "fill_stroke" => 2,
            _ => return Err(RenderError::invalid("bad paint style")),
        };
        ctx.paint_set_style(h, ordinal)?;
    }
    if let Some(w) = spec.stroke_width {
        ctx.paint_set_stroke_width(h, w)?;
    }
    // Android's `Paint()` has ANTI_ALIAS_FLAG set; only override the
    // `Paint::default()` value when the scene asks explicitly (ADR-0019).
    if let Some(anti_alias) = spec.anti_alias {
        ctx.paint_set_anti_alias(h, anti_alias)?;
    }
    if let Some(size) = spec.text_size {
        ctx.paint_set_text_size(h, size)?;
    }
    if let Some(ls) = spec.letter_spacing {
        ctx.paint_set_letter_spacing(h, ls)?;
    }
    if spec.fake_bold {
        ctx.paint_set_fake_bold(h, true)?;
    }
    if spec.underline {
        ctx.paint_set_underline(h, true)?;
    }
    if let Some(align) = &spec.align {
        let ordinal = match align.as_str() {
            "left" => 0,
            "center" => 1,
            "right" => 2,
            _ => return Err(RenderError::invalid("bad text align")),
        };
        ctx.paint_set_text_align(h, ordinal)?;
    }
    if let Some(tf) = &spec.typeface {
        if ctx.has_fonts() {
            match ctx.typeface_create(tf.family.as_deref(), tf.weight, tf.italic) {
                Ok(t) => ctx.paint_set_typeface(h, t)?,
                Err(e) => return Err(e),
            }
        }
    }
    if let Some(blur) = spec.blur {
        ctx.paint_set_mask_filter_blur(h, blur, 0)?;
    }
    if let Some(shader) = &spec.shader {
        apply_shader(ctx, h, shader)?;
    }
    Ok(h)
}

fn apply_shader(ctx: &mut RenderContext, paint: Handle, shader: &ShaderSpecJson) -> RenderResult<()> {
    match shader {
        ShaderSpecJson::Linear {
            start,
            end,
            colors,
            positions,
            tile,
        } => {
            let colors = parse_colors(colors)?;
            let tile = parse_tile(tile.as_deref());
            ctx.paint_set_shader_linear(
                paint,
                start[0],
                start[1],
                end[0],
                end[1],
                colors,
                positions.clone(),
                tile,
                None,
            )
        }
        ShaderSpecJson::Radial {
            center,
            radius,
            colors,
            positions,
            tile,
        } => {
            let colors = parse_colors(colors)?;
            let tile = parse_tile(tile.as_deref());
            ctx.paint_set_shader_radial(
                paint,
                center[0],
                center[1],
                *radius,
                colors,
                positions.clone(),
                tile,
                None,
            )
        }
        ShaderSpecJson::Sweep {
            center,
            colors,
            positions,
        } => {
            let colors = parse_colors(colors)?;
            ctx.paint_set_shader_sweep(paint, center[0], center[1], colors, positions.clone(), None)
        }
        ShaderSpecJson::Bitmap { path, tile } => {
            let bytes = std::fs::read(path).map_err(|e| RenderError::Io {
                msg: format!("reading shader bitmap {path}: {e}"),
            })?;
            let bitmap = ctx.bitmap_create_from_bytes(&bytes)?;
            let tile = parse_tile(tile.as_deref());
            ctx.paint_set_shader_bitmap(paint, bitmap, tile, tile, None)
        }
    }
}

fn parse_tile(s: Option<&str>) -> TileMode {
    match s {
        Some("repeat") => TileMode::Repeat,
        Some("mirror") => TileMode::Mirror,
        _ => TileMode::Clamp,
    }
}

fn parse_colors(colors: &[String]) -> RenderResult<Vec<u32>> {
    colors
        .iter()
        .map(|c| parse_color(c).ok_or_else(|| RenderError::invalid(format!("bad colour {c}"))))
        .collect()
}

fn build_path(commands: &[serde_json::Value]) -> RenderResult<AcosPath> {
    let mut path = AcosPath::new();
    for cmd in commands {
        let arr = cmd
            .as_array()
            .ok_or_else(|| RenderError::invalid("path command must be an array"))?;
        let op = arr.first().and_then(|v| v.as_str()).unwrap_or("");
        let n: Vec<f32> = arr[1..].iter().filter_map(|v| v.as_f64()).map(|v| v as f32).collect();
        match op {
            "M" => path.move_to(n[0], n[1]),
            "L" => path.line_to(n[0], n[1]),
            "Q" => path.quad_to(n[0], n[1], n[2], n[3]),
            "C" => path.cubic_to(n[0], n[1], n[2], n[3], n[4], n[5]),
            "Z" => path.close(),
            _ => return Err(RenderError::invalid(format!("unknown path op {op}"))),
        }
    }
    Ok(path)
}

fn rect4(r: &[f32; 4]) -> RectF {
    RectF::new(r[0], r[1], r[2], r[3])
}

/// Parse `#RGB`, `#RRGGBB`, `#AARRGGBB` or a decimal ARGB string.
pub fn parse_color(s: &str) -> Option<u32> {
    let s = s.trim();
    if let Some(hex) = s.strip_prefix('#') {
        let v = u32::from_str_radix(hex, 16).ok()?;
        return Some(match hex.len() {
            6 => 0xff00_0000 | v,
            8 => v,
            3 => {
                let r = (v >> 8) & 0xf;
                let g = (v >> 4) & 0xf;
                let b = v & 0xf;
                0xff00_0000 | (r << 20) | (r << 16) | (g << 12) | (g << 8) | (b << 4) | b
            }
            _ => return None,
        });
    }
    s.parse::<u32>().ok()
}

fn resolve_assets(node: &mut SceneNode, dir: &Path) {
    for op in node.ops.iter_mut() {
        if let OpSpec::Bitmap { path, .. } = op {
            let p = PathBuf::from(&*path);
            if p.is_relative() {
                *path = dir.join(p).to_string_lossy().to_string();
            }
        }
        let paint = match op {
            OpSpec::Rect { paint, .. }
            | OpSpec::RoundRect { paint, .. }
            | OpSpec::Oval { paint, .. }
            | OpSpec::Circle { paint, .. }
            | OpSpec::Line { paint, .. }
            | OpSpec::Text { paint, .. }
            | OpSpec::Path { paint, .. } => paint.as_mut(),
            OpSpec::Color { .. } | OpSpec::Bitmap { .. } => None,
        };
        if let Some(PaintSpec {
            shader: Some(ShaderSpecJson::Bitmap { path, .. }),
            ..
        }) = paint
        {
            let p = PathBuf::from(&*path);
            if p.is_relative() {
                *path = dir.join(p).to_string_lossy().to_string();
            }
        }
    }
    for c in node.children.iter_mut() {
        resolve_assets(c, dir);
    }
}

/// Convenience: render a JSON scene with a fresh context.
pub fn render_json(json: &str, cause_id: i64) -> RenderResult<SceneRender> {
    let mut ctx = RenderContext::new();
    let scene = Scene::from_json(json)?;
    scene.render(&mut ctx, cause_id)
}

/// Load only a font collection, loudly reporting if absent.
pub fn require_fonts() -> Option<FontCollection> {
    FontCollection::load()
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL: &str = r##"{
      "width": 100, "height": 50, "density": 1.0,
      "background": "#FFFFFFFF",
      "root": {
        "name": "root", "bounds": [0,0,100,50],
        "ops": [
          {"op": "rect", "rect": [10,10,40,40], "paint": {"color": "#FF0000"}},
          {"op": "circle", "cx": 70, "cy": 25, "r": 15, "paint": {"color": "#0000FFFF"}}
        ]
      }
    }"##;

    #[test]
    fn minimal_scene_renders_content() {
        let scene = Scene::from_json(MINIMAL).unwrap();
        let mut ctx = RenderContext::new();
        let out = scene.render(&mut ctx, 1).unwrap();
        assert!(out.node_count >= 1);
        assert_eq!((out.frame.width, out.frame.height), (100, 50));
        assert_eq!(out.frame_class, crate::surface::FrameClass::Content);
        let px = out.frame.pixels.pixel(20, 20).unwrap();
        assert_eq!((px.red(), px.green(), px.blue()), (255, 0, 0));
    }

    #[test]
    fn parse_colours() {
        assert_eq!(parse_color("#ff0000"), Some(0xffff0000));
        assert_eq!(parse_color("#80ff0000"), Some(0x80ff0000));
        assert_eq!(parse_color("#f00"), Some(0xffff0000));
    }
}
