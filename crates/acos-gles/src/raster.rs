//! RGBA8888 framebuffer + scanline triangle rasteriser with the fixed-function
//! pipeline semantics GLES2 guarantees (ADR-0023).
//!
//! The rasteriser is the only place pixels are produced. It owns no GL state:
//! the caller passes a [`RasterState`] snapshot and a fragment closure, so the
//! same code rasterises to the default framebuffer (an EGL surface) and to an
//! FBO colour attachment.
//!
//! Semantics implemented here:
//!
//! * window-space mapping from clip space (viewport, Y flip so row 0 is the top
//!   of the image, `z` mapped to `[0,1]` depth range),
//! * edge-function rasterisation on pixel centres with top-left fill,
//! * perspective-correct varying interpolation (`1/w` weighting),
//! * depth test/`depthMask`, the `GL_LESS`-family compare functions,
//! * per-channel `colorMask` and GLES2 blend factors,
//! * scissor clipping,
//! * back-face culling (`CCW`/`CW` front faces).

use crate::glsl::Value;

/// `GL_UNSIGNED_BYTE`-style RGBA colour buffer plus optional depth and 8-bit
/// stencil buffers.
#[derive(Clone, Debug)]
#[allow(missing_docs)]
pub struct Framebuffer {
    pub width: u32,
    pub height: u32,
    /// RGBA8888, row-major, `width * 4` bytes per row.
    pub color: Vec<u8>,
    /// `None` until a depth buffer is requested (`glDepthMask`/`glEnable`).
    pub depth: Option<Vec<f32>>,
    /// 8-bit stencil buffer (Skia's clip path needs it).
    pub stencil: Option<Vec<u8>>,
}

impl Framebuffer {
    /// Allocate a cleared (all-zero = transparent black) framebuffer.
    #[must_use]
    pub fn new(width: u32, height: u32) -> Framebuffer {
        Framebuffer {
            width,
            height,
            color: vec![0u8; width as usize * height as usize * 4],
            depth: Some(vec![1.0; width as usize * height as usize]),
            stencil: Some(vec![0u8; width as usize * height as usize]),
        }
    }

    /// Number of bytes in the colour buffer (checked geometry).
    #[must_use]
    pub fn byte_len(width: u32, height: u32) -> Option<usize> {
        (width as usize)
            .checked_mul(height as usize)?
            .checked_mul(4)
    }

    /// Replace the colour + depth storage for a new geometry.
    pub fn resize(&mut self, width: u32, height: u32) {
        if self.width == width && self.height == height {
            return;
        }
        self.width = width;
        self.height = height;
        self.color = vec![0u8; width as usize * height as usize * 4];
        self.depth = Some(vec![1.0; width as usize * height as usize]);
        self.stencil = Some(vec![0u8; width as usize * height as usize]);
    }

    /// Straight (non-premultiplied) RGBA8888 at `(x, y)`.
    #[must_use]
    pub fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
        if x >= self.width || y >= self.height {
            return [0, 0, 0, 0];
        }
        let i = (y as usize * self.width as usize + x as usize) * 4;
        [
            self.color[i],
            self.color[i + 1],
            self.color[i + 2],
            self.color[i + 3],
        ]
    }

    /// Write straight RGBA8888 at `(x, y)`.
    pub fn set_pixel(&mut self, x: u32, y: u32, px: [u8; 4]) {
        if x >= self.width || y >= self.height {
            return;
        }
        let i = (y as usize * self.width as usize + x as usize) * 4;
        self.color[i..i + 4].copy_from_slice(&px);
    }

    /// Fill the whole colour buffer with one RGBA8888 value.
    pub fn fill(&mut self, px: [u8; 4]) {
        for chunk in self.color.chunks_exact_mut(4) {
            chunk.copy_from_slice(&px);
        }
    }

    /// Fill a rectangle with one RGBA8888 value (scissor/clear support).
    pub fn fill_rect(&mut self, rect: (i32, i32, i32, i32), px: [u8; 4]) {
        let (x0, y0, x1, y1) = clamp_rect(rect, self.width, self.height);
        for y in y0..y1 {
            let row = (y as usize * self.width as usize) * 4;
            for x in x0..x1 {
                let i = row + x as usize * 4;
                self.color[i..i + 4].copy_from_slice(&px);
            }
        }
    }
}

fn clamp_rect(rect: (i32, i32, i32, i32), w: u32, h: u32) -> (i32, i32, i32, i32) {
    let (x0, y0, x1, y1) = rect;
    (
        x0.max(0),
        y0.max(0),
        x1.min(w as i32),
        y1.min(h as i32),
    )
}

/// A GLES2 depth compare function.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[allow(missing_docs)]
pub enum DepthFunc {
    Never,
    Less,
    Equal,
    Lequal,
    Greater,
    Notequal,
    Gequal,
    Always,
}

impl DepthFunc {
    fn passes(self, src: f32, dst: f32) -> bool {
        match self {
            DepthFunc::Never => false,
            DepthFunc::Less => src < dst,
            DepthFunc::Equal => (src - dst).abs() < f32::EPSILON,
            DepthFunc::Lequal => src <= dst,
            DepthFunc::Greater => src > dst,
            DepthFunc::Notequal => (src - dst).abs() >= f32::EPSILON,
            DepthFunc::Gequal => src >= dst,
            DepthFunc::Always => true,
        }
    }
}

/// GLES2 blend factors.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[allow(missing_docs)]
pub enum BlendFactor {
    Zero,
    One,
    SrcColor,
    OneMinusSrcColor,
    SrcAlpha,
    OneMinusSrcAlpha,
    DstAlpha,
    OneMinusDstAlpha,
    DstColor,
    OneMinusDstColor,
    SrcAlphaSaturate,
}

impl BlendFactor {
    fn rgb(self, src: [f32; 4], dst: [f32; 4]) -> f32 {
        self.channel(src, dst, 0)
    }

    fn alpha(self, src: [f32; 4], dst: [f32; 4]) -> f32 {
        self.channel(src, dst, 3)
    }

    fn channel(self, src: [f32; 4], dst: [f32; 4], c: usize) -> f32 {
        match self {
            BlendFactor::Zero => 0.0,
            BlendFactor::One => 1.0,
            BlendFactor::SrcColor => src[c],
            BlendFactor::OneMinusSrcColor => 1.0 - src[c],
            BlendFactor::SrcAlpha => src[3],
            BlendFactor::OneMinusSrcAlpha => 1.0 - src[3],
            BlendFactor::DstAlpha => dst[3],
            BlendFactor::OneMinusDstAlpha => 1.0 - dst[3],
            BlendFactor::DstColor => dst[c],
            BlendFactor::OneMinusDstColor => 1.0 - dst[c],
            BlendFactor::SrcAlphaSaturate => {
                let s = src[3].min(1.0 - dst[3]);
                if c == 3 {
                    1.0
                } else {
                    s
                }
            }
        }
    }
}

/// Blend configuration (`glEnable(GL_BLEND)` + `glBlendFunc[Separate]`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[allow(missing_docs)]
pub struct Blend {
    pub src_rgb: BlendFactor,
    pub dst_rgb: BlendFactor,
    pub src_alpha: BlendFactor,
    pub dst_alpha: BlendFactor,
}

impl Blend {
    /// `glBlendFunc(src, dst)`.
    #[must_use]
    pub fn same(src: BlendFactor, dst: BlendFactor) -> Blend {
        Blend {
            src_rgb: src,
            dst_rgb: dst,
            src_alpha: src,
            dst_alpha: dst,
        }
    }
}

/// Cull mode.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[allow(missing_docs)]
pub enum Cull {
    Front,
    Back,
    FrontAndBack,
}

/// A GLES2 stencil operation.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[allow(missing_docs)]
pub enum StencilOp {
    Keep,
    Zero,
    Replace,
    Incr,
    Decr,
    Invert,
    IncrWrap,
    DecrWrap,
}

impl StencilOp {
    /// Apply the operation to `stored` (masked later by `write_mask`).
    #[must_use]
    pub fn apply(self, stored: u8, reference: u8) -> u8 {
        match self {
            StencilOp::Keep => stored,
            StencilOp::Zero => 0,
            StencilOp::Replace => reference,
            StencilOp::Incr => stored.saturating_add(1),
            StencilOp::Decr => stored.saturating_sub(1),
            StencilOp::Invert => !stored,
            StencilOp::IncrWrap => stored.wrapping_add(1),
            StencilOp::DecrWrap => stored.wrapping_sub(1),
        }
    }
}

/// Per-face stencil state (`glStencilFunc[Separate]` + `glStencilOp[Separate]`).
#[derive(Clone, Copy, Debug)]
#[allow(missing_docs)]
pub struct StencilFace {
    pub func: DepthFunc,
    pub reference: i32,
    pub value_mask: u32,
    pub write_mask: u32,
    pub sfail: StencilOp,
    pub dpfail: StencilOp,
    pub dppass: StencilOp,
}

impl StencilFace {
    /// `GL_ALWAYS`, ref 0, masks 0xFF, all ops `KEEP` (the GLES2 initial state).
    #[must_use]
    pub fn always() -> StencilFace {
        StencilFace {
            func: DepthFunc::Always,
            reference: 0,
            value_mask: 0xFF,
            write_mask: 0xFF,
            sfail: StencilOp::Keep,
            dpfail: StencilOp::Keep,
            dppass: StencilOp::Keep,
        }
    }
}

/// Fixed-function state for one draw.
#[derive(Clone, Copy, Debug)]
#[allow(missing_docs)]
pub struct RasterState {
    /// `(x, y, width, height)` in window coordinates.
    pub viewport: (i32, i32, u32, u32),
    /// `(x, y, width, height)` scissor box, `None` = disabled.
    pub scissor: Option<(i32, i32, u32, u32)>,
    pub blend: Option<Blend>,
    pub depth_test: bool,
    pub depth_func: DepthFunc,
    pub depth_mask: bool,
    /// RGBA write mask.
    pub color_mask: [bool; 4],
    pub cull: Option<Cull>,
    /// `true` = counter-clockwise is front-facing.
    pub front_ccw: bool,
    /// `glEnable(GL_STENCIL_TEST)`.
    pub stencil_test: bool,
    /// Front-facing stencil state.
    pub stencil_front: StencilFace,
    /// Back-facing stencil state.
    pub stencil_back: StencilFace,
}

impl Default for RasterState {
    fn default() -> RasterState {
        RasterState {
            viewport: (0, 0, 0, 0),
            scissor: None,
            blend: None,
            depth_test: false,
            depth_func: DepthFunc::Less,
            depth_mask: true,
            color_mask: [true; 4],
            cull: None,
            front_ccw: true,
            stencil_test: false,
            stencil_front: StencilFace::always(),
            stencil_back: StencilFace::always(),
        }
    }
}

/// A projected vertex: screen-space position plus interpolatable varyings.
#[derive(Clone, Debug)]
#[allow(missing_docs)]
pub struct ScreenVertex {
    pub x: f32,
    pub y: f32,
    pub z: f32,
    /// `1/w` for perspective-correct interpolation.
    pub inv_w: f32,
    pub varyings: Vec<Value>,
}

/// Project one clip-space vertex into window space for `state`.
#[must_use]
pub fn project(clip: [f32; 4], varyings: Vec<Value>, state: &RasterState) -> ScreenVertex {
    let w = if clip[3].abs() < 1e-9 { 1.0 } else { clip[3] };
    let inv_w = 1.0 / w;
    let (vx, vy, vw, vh) = state.viewport;
    let ndc_x = clip[0] * inv_w;
    let ndc_y = clip[1] * inv_w;
    let ndc_z = clip[2] * inv_w;
    ScreenVertex {
        x: vx as f32 + (ndc_x * 0.5 + 0.5) * vw as f32,
        // Window space puts +Y up; image rows run top-down.
        y: vy as f32 + (0.5 - ndc_y * 0.5) * vh as f32,
        z: ndc_z * 0.5 + 0.5,
        inv_w,
        varyings,
    }
}

fn edge(ax: f32, ay: f32, bx: f32, by: f32, px: f32, py: f32) -> f32 {
    (bx - ax) * (py - ay) - (by - ay) * (px - ax)
}

/// Interpolate `a` at weights `(w0, w1, w2)` with perspective correction,
/// writing into a reused buffer (no per-fragment allocation).
fn interpolate_into(
    out: &mut Vec<Value>,
    a: &ScreenVertex,
    b: &ScreenVertex,
    c: &ScreenVertex,
    w: [f32; 3],
    iw: f32,
) {
    out.clear();
    for i in 0..a.varyings.len() {
        let va = &a.varyings[i];
        let vb = b.varyings.get(i).unwrap_or(va);
        let vc = c.varyings.get(i).unwrap_or(va);
        out.push(match va {
            Value::V(ca, n) => {
                let cb = vb.to_vec4();
                let cc = vc.to_vec4();
                let mut r = [0.0f32; 4];
                for k in 0..*n {
                    let num = w[0] * ca[k] * a.inv_w + w[1] * cb[k] * b.inv_w + w[2] * cc[k] * c.inv_w;
                    r[k] = if iw == 0.0 { ca[k] } else { num / iw };
                }
                Value::V(r, *n)
            }
            Value::F(_) | Value::I(_) | Value::B(_) | Value::S(_) => {
                // Scalars interpolate like a 1-wide vector.
                let fa = va.to_f32();
                let fb = vb.to_f32();
                let fc = vc.to_f32();
                let num = w[0] * fa * a.inv_w + w[1] * fb * b.inv_w + w[2] * fc * c.inv_w;
                Value::F(if iw == 0.0 { fa } else { num / iw })
            }
            other => other.clone(),
        });
    }
}

/// Rasterise one triangle.
///
/// `fragment` receives the interpolated varyings and the fragment coordinate
/// `(x + 0.5, y + 0.5, z, 1/w)`; returning `None` discards the fragment.
/// Returns the number of fragments that passed the depth test and were shaded.
pub fn triangle<F>(
    fb: &mut Framebuffer,
    state: &RasterState,
    verts: [&ScreenVertex; 3],
    mut fragment: F,
) -> usize
where
    F: FnMut(&[Value], [f32; 4]) -> Option<[f32; 4]>,
{
    let [a, b, c] = verts;
    let area = edge(a.x, a.y, b.x, b.y, c.x, c.y);
    if area.abs() < 1e-9 {
        return 0;
    }
    // Window space has Y pointing down, so a counter-clockwise triangle in clip
    // space has a negative window-space area.
    let ccw = area < 0.0;
    let is_front = ccw == state.front_ccw;
    if let Some(cull) = state.cull {
        let drop = match cull {
            Cull::Front => is_front,
            Cull::Back => !is_front,
            Cull::FrontAndBack => true,
        };
        if drop {
            return 0;
        }
    }

    let (vp_x, vp_y, vp_w, vp_h) = state.viewport;
    let mut min_x = a.x.min(b.x).min(c.x).floor().max(0.0);
    let mut min_y = a.y.min(b.y).min(c.y).floor().max(0.0);
    let mut max_x = a.x.max(b.x).max(c.x).ceil();
    let mut max_y = a.y.max(b.y).max(c.y).ceil();
    // Viewport bounds always clip in GLES.
    min_x = min_x.max(vp_x as f32);
    min_y = min_y.max(vp_y as f32);
    max_x = max_x.min((vp_x + vp_w as i32) as f32);
    max_y = max_y.min((vp_y + vp_h as i32) as f32);
    if let Some((sx, sy, sw, sh)) = state.scissor {
        min_x = min_x.max(sx as f32);
        min_y = min_y.max(sy as f32);
        max_x = max_x.min((sx + sw as i32) as f32);
        max_y = max_y.min((sy + sh as i32) as f32);
    }
    max_x = max_x.min(fb.width as f32);
    max_y = max_y.min(fb.height as f32);
    if max_x <= min_x || max_y <= min_y {
        return 0;
    }
    let (x0, x1) = (min_x as i32, max_x as i32);
    let (y0, y1) = (min_y as i32, max_y as i32);
    let inv_area = 1.0 / area;
    let mut shaded = 0usize;
    let mut varyings: Vec<Value> = Vec::new();
    // Edge functions are affine in `x`, so step them along each scanline
    // instead of re-evaluating three multiplies per pixel.
    let step0 = b.y - c.y;
    let step1 = c.y - a.y;
    let step2 = a.y - b.y;
    let px0 = x0 as f32 + 0.5;

    for y in y0..y1 {
        let py = y as f32 + 0.5;
        let mut e0 = edge(b.x, b.y, c.x, c.y, px0, py);
        let mut e1 = edge(c.x, c.y, a.x, a.y, px0, py);
        let mut e2 = edge(a.x, a.y, b.x, b.y, px0, py);
        for x in x0..x1 {
            let w0 = e0 * inv_area;
            let w1 = e1 * inv_area;
            let w2 = e2 * inv_area;
            e0 += step0;
            e1 += step1;
            e2 += step2;
            if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 {
                continue;
            }
            let px = x as f32 + 0.5;
            let z = w0 * a.z + w1 * b.z + w2 * c.z;
            if !(0.0..=1.0).contains(&z) {
                continue;
            }
            let depth_index = y as usize * fb.width as usize + x as usize;
            // GL order: stencil test → depth test; the stencil op runs on
            // every fragment that reaches the buffer, pass or fail.
            let (stencil_pass, depth_pass) = {
                let depth_pass = if state.depth_test {
                    let stored = fb
                        .depth
                        .as_ref()
                        .and_then(|d| d.get(depth_index).copied())
                        .unwrap_or(1.0);
                    state.depth_func.passes(z, stored)
                } else {
                    true
                };
                let stencil_pass = if state.stencil_test {
                    match fb.stencil.as_ref().and_then(|s| s.get(depth_index).copied()) {
                        Some(stored) => {
                            let face = if is_front {
                                state.stencil_front
                            } else {
                                state.stencil_back
                            };
                            let sref = (face.reference as u32) & face.value_mask;
                            let sval = u32::from(stored) & face.value_mask;
                            let pass = face.func.passes(sref as f32, sval as f32);
                            let op = if !pass {
                                face.sfail
                            } else if !depth_pass {
                                face.dpfail
                            } else {
                                face.dppass
                            };
                            let new = op.apply(stored, face.reference as u8);
                            if let Some(s) = fb.stencil.as_mut() {
                                if depth_index < s.len() {
                                    s[depth_index] =
                                        (stored & !(face.write_mask as u8))
                                            | (new & face.write_mask as u8);
                                }
                            }
                            pass
                        }
                        None => true,
                    }
                } else {
                    true
                };
                (stencil_pass, depth_pass)
            };
            if !stencil_pass || !depth_pass {
                continue;
            }
            let iw = w0 * a.inv_w + w1 * b.inv_w + w2 * c.inv_w;
            interpolate_into(&mut varyings, a, b, c, [w0, w1, w2], iw);
            let frag_coord = [px, py, z, if iw == 0.0 { 1.0 } else { 1.0 / iw }];
            let Some(src) = fragment(&varyings, frag_coord) else {
                continue;
            };
            shaded += 1;
            if state.depth_mask {
                if let Some(d) = fb.depth.as_mut() {
                    if depth_index < d.len() {
                        d[depth_index] = z;
                    }
                }
            }
            let i = depth_index * 4;
            let mut dst = [
                f32::from(fb.color[i]) / 255.0,
                f32::from(fb.color[i + 1]) / 255.0,
                f32::from(fb.color[i + 2]) / 255.0,
                f32::from(fb.color[i + 3]) / 255.0,
            ];
            let out = match state.blend {
                Some(blend) => {
                    let mut r = [0.0f32; 4];
                    for k in 0..3 {
                        r[k] = src[k] * blend.src_rgb.rgb(src, dst) + dst[k] * blend.dst_rgb.rgb(src, dst);
                    }
                    r[3] = src[3] * blend.src_alpha.alpha(src, dst)
                        + dst[3] * blend.dst_alpha.alpha(src, dst);
                    r
                }
                None => src,
            };
            for k in 0..4 {
                if state.color_mask[k] {
                    dst[k] = out[k].clamp(0.0, 1.0);
                    fb.color[i + k] = (dst[k] * 255.0 + 0.5) as u8;
                }
            }
        }
    }
    shaded
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(w: u32, h: u32) -> RasterState {
        RasterState {
            viewport: (0, 0, w, h),
            ..RasterState::default()
        }
    }

    #[test]
    fn rasterises_a_triangle_to_the_expected_pixels() {
        let mut fb = Framebuffer::new(8, 8);
        let a = project([-1.0, -1.0, 0.0, 1.0], vec![], &state(8, 8));
        let b = project([1.0, -1.0, 0.0, 1.0], vec![], &state(8, 8));
        let cc = project([-1.0, 1.0, 0.0, 1.0], vec![], &state(8, 8));
        let n = triangle(&mut fb, &state(8, 8), [&a, &b, &cc], |_, _| {
            Some([1.0, 0.0, 0.0, 1.0])
        });
        // Lower-left half of an 8x8 buffer: 36 pixel centres.
        assert_eq!(n, 36, "exact fragment count");
        assert_eq!(fb.pixel(0, 0), [255, 0, 0, 255], "top-left is inside");

        assert_eq!(fb.pixel(7, 0), [0, 0, 0, 0], "right of the hypotenuse");
        assert_eq!(fb.pixel(7, 7), [255, 0, 0, 255], "bottom-right is inside");
        let lit = fb.color.chunks_exact(4).filter(|p| p[3] != 0).count();
        assert_eq!(lit, 36);
    }

    #[test]
    fn honours_scissor_blend_and_color_mask() {
        let mut fb = Framebuffer::new(4, 4);
        fb.fill([0, 0, 255, 255]);
        let st = RasterState {
            viewport: (0, 0, 4, 4),
            scissor: Some((0, 0, 2, 4)),
            blend: Some(Blend::same(BlendFactor::SrcAlpha, BlendFactor::OneMinusSrcAlpha)),
            ..RasterState::default()
        };
        let a = project([-1.0, -1.0, 0.0, 1.0], vec![], &st);
        let b = project([3.0, -1.0, 0.0, 1.0], vec![], &st);
        let c = project([-1.0, 3.0, 0.0, 1.0], vec![], &st);
        triangle(&mut fb, &st, [&a, &b, &c], |_, _| {
            Some([1.0, 0.0, 0.0, 0.5])
        });
        // Left half blended with 50% alpha over blue; right half untouched.
        // `dst.a = 0.5*0.5 + 1.0*0.5 = 0.75`, `dst.rgb = 0.5*src + 0.5*dst`.
        assert_eq!(fb.pixel(0, 0), [128, 0, 128, 191]);
        assert_eq!(fb.pixel(3, 0), [0, 0, 255, 255]);
    }

    #[test]
    fn depth_test_keeps_the_nearest_fragment() {
        let mut fb = Framebuffer::new(4, 4);
        let st = RasterState {
            viewport: (0, 0, 4, 4),
            depth_test: true,
            ..RasterState::default()
        };
        let mut quad = |z: f32, color: [f32; 4]| {
            let a = project([-1.0, -1.0, z, 1.0], vec![], &st);
            let b = project([3.0, -1.0, z, 1.0], vec![], &st);
            let c = project([-1.0, 3.0, z, 1.0], vec![], &st);
            triangle(&mut fb, &st, [&a, &b, &c], |_, _| Some(color))
        };
        assert!(quad(-0.5, [1.0, 0.0, 0.0, 1.0]) > 0);
        // A farther fragment must not overwrite the nearer one.
        assert_eq!(quad(0.5, [0.0, 1.0, 0.0, 1.0]), 0);
        assert_eq!(fb.pixel(0, 0), [255, 0, 0, 255]);
    }

    #[test]
    fn stencil_test_masks_fragments() {
        let mut fb = Framebuffer::new(4, 4);
        // Left half of the stencil buffer is 1, right half is 0.
        if let Some(s) = fb.stencil.as_mut() {
            for y in 0..4usize {
                for x in 0..4usize {
                    s[y * 4 + x] = if x < 2 { 1 } else { 0 };
                }
            }
        }
        let st = RasterState {
            viewport: (0, 0, 4, 4),
            stencil_test: true,
            stencil_front: StencilFace {
                func: DepthFunc::Equal,
                reference: 1,
                value_mask: 0xFF,
                write_mask: 0xFF,
                sfail: StencilOp::Keep,
                dpfail: StencilOp::Keep,
                dppass: StencilOp::Keep,
            },
            ..RasterState::default()
        };
        let a = project([-1.0, -1.0, 0.0, 1.0], vec![], &st);
        let b = project([3.0, -1.0, 0.0, 1.0], vec![], &st);
        let c = project([-1.0, 3.0, 0.0, 1.0], vec![], &st);
        triangle(&mut fb, &st, [&a, &b, &c], |_, _| Some([1.0, 0.0, 0.0, 1.0]));
        assert_eq!(fb.pixel(0, 0), [255, 0, 0, 255], "stencil==1 passes");
        assert_eq!(fb.pixel(3, 0), [0, 0, 0, 0], "stencil==0 is masked");
    }

    #[test]
    fn stencil_invert_op_runs_on_failed_test() {
        let mut fb = Framebuffer::new(2, 1);
        if let Some(s) = fb.stencil.as_mut() {
            s[0] = 0x0F;
            s[1] = 0x00;
        }
        let st = RasterState {
            viewport: (0, 0, 2, 1),
            stencil_test: true,
            stencil_front: StencilFace {
                func: DepthFunc::Equal,
                reference: 0,
                value_mask: 0xFF,
                write_mask: 0xFF,
                sfail: StencilOp::Invert,
                dpfail: StencilOp::Keep,
                dppass: StencilOp::Keep,
            },
            ..RasterState::default()
        };
        let a = project([-1.0, -1.0, 0.0, 1.0], vec![], &st);
        let b = project([1.0, -1.0, 0.0, 1.0], vec![], &st);
        let c = project([-1.0, 1.0, 0.0, 1.0], vec![], &st);
        triangle(&mut fb, &st, [&a, &b, &c], |_, _| Some([1.0, 1.0, 1.0, 1.0]));
        let s = fb.stencil.as_ref().unwrap();
        assert_eq!(s[0], 0xF0, "failing fragment inverts the stored value");
        assert_eq!(s[1], 0x00, "passing fragment keeps it");
    }

    #[test]
    fn interpolates_varyings_perspective_correctly() {
        let mut fb = Framebuffer::new(2, 1);
        let st = RasterState {
            viewport: (0, 0, 2, 1),
            ..RasterState::default()
        };
        let mut a = project([-1.0, -1.0, 0.0, 1.0], vec![Value::F(0.0)], &st);
        let mut b = project([3.0, -1.0, 0.0, 1.0], vec![Value::F(1.0)], &st);
        let mut c = project([-1.0, 3.0, 0.0, 1.0], vec![Value::F(0.0)], &st);
        a.inv_w = 1.0;
        b.inv_w = 1.0;
        c.inv_w = 1.0;
        let seen = std::cell::RefCell::new(Vec::new());
        triangle(&mut fb, &st, [&a, &b, &c], |vary, _| {
            seen.borrow_mut().push(vary[0].to_f32());
            Some([0.0, 0.0, 0.0, 1.0])
        });
        let s = seen.into_inner();
        assert_eq!(s.len(), 2);
        assert!(s[0] < s[1], "varying increases left to right: {s:?}");
    }
}
