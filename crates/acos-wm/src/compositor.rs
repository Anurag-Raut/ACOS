//! The SurfaceFlinger replacement: a layer tree composed into an
//! `acos-render` surface.
//!
//! `IWindowSession.finishDrawing` hands over a `SurfaceControl.Transaction`
//! (here a typed [`Transaction`]) containing the client's rendered buffer for a
//! layer. [`Compositor::apply`] updates the layer record and marks the tree
//! dirty; [`Compositor::present`] composites the visible layers in z-order into
//! the display surface and calls the real `acos-render` [`Surface::present`],
//! which assigns the monotonic `frame_seq`, the content hash and the caller's
//! `cause_id`.
//!
//! ## Damage tracking
//!
//! A present never re-composites the whole display unless it has to. Every
//! mutation records the union of the layer's old and new display-space coverage
//! in [`Compositor::damage`]; `present` re-composites **only** that region.
//! Compositing is per-pixel local, so recompositing a region from the
//! background plus every layer clipped to it is pixel-identical to a full
//! composite (this is the property the `damage_matches_full` proptest checks).
//! The full frame is recomposited on the first present, on
//! [`Compositor::invalidate`], and on `present_forced`.
//!
//! ## Cost
//!
//! The compositor owns one persistent output buffer — the surface's own pixmap —
//! and writes into it in place, so there is no per-present allocation and no
//! `Bitmap → Pixmap` copy. The blit has an opaque row-memcpy fast path and an
//! integer premultiplied `source-over` path, both iterated row-wise over raw
//! bytes with the damage clip applied. Layers fully covered by an opaque layer
//! above them are skipped.
//!
//! Because the pixels are written in place while the previous [`Frame`] holds a
//! *clone* of the buffer (`Surface::present` clones), a captured frame is never
//! aliased by a later composite.

use std::collections::BTreeMap;

use acos_render::{Bitmap, BitmapConfig, Rect, Surface};

use crate::display::DisplaySpec;
use crate::error::{Result, WmError};
use crate::geom::{PointI, RectI};
use crate::wms::WindowId;

/// A rendered client buffer attached to a layer.
pub struct FrameBuffer {
    /// The pixels (premultiplied RGBA8, as `Bitmap` stores them).
    pub bitmap: Bitmap,
    /// The client frame number (its own `frame`/`syncSeqId` counter).
    pub frame_number: u64,
    /// The action that caused the content, propagated into the final frame.
    pub cause_id: i64,
}

/// One surface layer.
pub struct Layer {
    /// Stable layer id (`SurfaceControl` handle).
    pub id: u64,
    /// Debug name (`Surface(name=…)/@0x…` style).
    pub name: String,
    /// Owning window, when this layer is a window surface.
    pub window: Option<WindowId>,
    /// Z-order; larger paints later (on top).
    pub z: i64,
    /// Visible flag.
    pub visible: bool,
    /// Layer alpha `0.0..=1.0`.
    pub alpha: f32,
    /// Position in display coordinates.
    pub position: PointI,
    /// Optional crop in buffer coordinates.
    pub crop: Option<RectI>,
    /// `SurfaceControl.Transaction.setTransform` hint (0 = identity).
    pub transform: u32,
    /// `SurfaceControl.Transaction.setBuffer` content.
    pub buffer: Option<FrameBuffer>,
    /// `true` when the layer is fully opaque (enables the opaque fast path).
    pub opaque: bool,
    /// `SurfaceControl.Transaction` generation that last touched the layer.
    pub last_transaction: u64,
}

/// A typed `SurfaceControl.Transaction` commit (the BLAST handoff).
#[derive(Debug, Clone, PartialEq)]
pub enum Transaction {
    /// `setBuffer(layer, buffer, frameNumber)`.
    SetBuffer {
        /// Target layer.
        layer: u64,
        /// Client frame number.
        frame_number: u64,
        /// The action that produced the buffer.
        cause_id: i64,
        /// Buffer width.
        width: u32,
        /// Buffer height.
        height: u32,
        /// Opaque (all pixels alpha == 255).
        opaque: bool,
    },
    /// `setPosition(layer, x, y)`.
    SetPosition {
        /// Target layer.
        layer: u64,
        /// New position.
        position: PointI,
    },
    /// `setLayer(layer, z)`.
    SetLayer {
        /// Target layer.
        layer: u64,
        /// New z-order.
        z: i64,
    },
    /// `setAlpha(layer, alpha)`.
    SetAlpha {
        /// Target layer.
        layer: u64,
        /// New alpha.
        alpha: f32,
    },
    /// `setVisibility(layer, visible)`.
    SetVisibility {
        /// Target layer.
        layer: u64,
        /// New visibility.
        visible: bool,
    },
    /// `setCrop(layer, crop)`.
    SetCrop {
        /// Target layer.
        layer: u64,
        /// New crop.
        crop: Option<RectI>,
    },
    /// `setTransform(layer, hint)`.
    SetTransform {
        /// Target layer.
        layer: u64,
        /// New transform hint (0 = identity).
        transform: u32,
    },
}

impl std::fmt::Debug for FrameBuffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FrameBuffer")
            .field("width", &self.bitmap.width())
            .field("height", &self.bitmap.height())
            .field("frame_number", &self.frame_number)
            .field("cause_id", &self.cause_id)
            .finish()
    }
}

impl std::fmt::Debug for Layer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Layer")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("window", &self.window)
            .field("z", &self.z)
            .field("visible", &self.visible)
            .field("alpha", &self.alpha)
            .field("position", &self.position)
            .field("opaque", &self.opaque)
            .field("transform", &self.transform)
            .field("has_buffer", &self.buffer.is_some())
            .finish()
    }
}

/// The composed display and its layers.
pub struct Compositor {
    /// Display geometry.
    pub display: DisplaySpec,
    /// The output surface driving the `acos-render` frame contract.
    pub surface: Surface,
    /// All layers by id.
    layers: BTreeMap<u64, Layer>,
    /// Next layer id.
    next_layer: u64,
    /// Composite generation (bumped on every applied transaction).
    generation: u64,
    /// Pending damage in display coordinates; `None` means the tree is clean.
    damage: Option<RectI>,
    /// Frames presented.
    presents: u64,
    /// Presents skipped because nothing was dirty.
    skipped: u64,
    /// Last cause id.
    last_cause: i64,
}

impl std::fmt::Debug for Compositor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Compositor")
            .field("display", &self.display)
            .field("layers", &self.layers.len())
            .field("generation", &self.generation)
            .field("damage", &self.damage)
            .field("presents", &self.presents)
            .field("skipped", &self.skipped)
            .finish()
    }
}

impl Compositor {
    /// Create a compositor for `display`.
    pub fn new(display: DisplaySpec) -> Result<Self> {
        let surface = Surface::new(
            display.frame.width() as u32,
            display.frame.height() as u32,
            display.density,
        )?;
        let full = display.frame;
        Ok(Self {
            display,
            surface,
            layers: BTreeMap::new(),
            next_layer: 1,
            generation: 0,
            // The first present paints the whole (black) display.
            damage: Some(full),
            presents: 0,
            skipped: 0,
            last_cause: 0,
        })
    }

    /// Create a layer and return its id.
    ///
    /// A fresh layer has no buffer, so it covers nothing: it needs no damage.
    /// The later buffer/geometry commit records the damage.
    pub fn create_layer(
        &mut self,
        window: Option<WindowId>,
        name: impl Into<String>,
        z: i64,
    ) -> u64 {
        let id = self.next_layer;
        self.next_layer += 1;
        self.layers.insert(
            id,
            Layer {
                id,
                name: name.into(),
                window,
                z,
                visible: true,
                alpha: 1.0,
                position: PointI::new(0, 0),
                crop: None,
                transform: 0,
                buffer: None,
                opaque: false,
                last_transaction: 0,
            },
        );
        self.generation += 1;
        id
    }

    /// Borrow a layer.
    #[must_use]
    pub fn layer(&self, id: u64) -> Option<&Layer> {
        self.layers.get(&id)
    }

    /// Borrow a layer mutably.
    ///
    /// Mutating a layer directly bypasses damage tracking; call
    /// [`Compositor::invalidate`] afterwards if the next present must reflect
    /// the change.
    pub fn layer_mut(&mut self, id: u64) -> Option<&mut Layer> {
        self.layers.get_mut(&id)
    }

    /// The layer id for a window, if any.
    #[must_use]
    pub fn layer_for_window(&self, window: WindowId) -> Option<u64> {
        self.layers
            .values()
            .find(|l| l.window == Some(window))
            .map(|l| l.id)
    }

    /// Remove a layer (window removed). Returns the removed layer.
    pub fn remove_layer(&mut self, id: u64) -> Option<Layer> {
        let removed = self.layers.remove(&id)?;
        self.generation += 1;
        let covered = layer_coverage(&removed, self.display.frame);
        self.add_damage(covered);
        Some(removed)
    }

    /// Directly attach a buffer to a layer (used by the frame-handoff path).
    pub fn set_buffer(
        &mut self,
        layer: u64,
        bitmap: Bitmap,
        frame_number: u64,
        cause_id: i64,
    ) -> Result<()> {
        let old = self.layer_coverage(layer);
        let opaque = detect_opaque(&bitmap);
        {
            let l = self
                .layers
                .get_mut(&layer)
                .ok_or(WmError::ReleasedSurface(layer))?;
            l.opaque = opaque;
            l.buffer = Some(FrameBuffer {
                bitmap,
                frame_number,
                cause_id,
            });
        }
        let new = self.layer_coverage(layer);
        self.generation += 1;
        self.add_damage(old.union(new));
        Ok(())
    }

    /// Apply a typed transaction.
    pub fn apply(&mut self, txn: &Transaction) -> Result<()> {
        let generation = self.generation + 1;
        match txn {
            Transaction::SetBuffer {
                layer,
                frame_number,
                cause_id,
                width,
                height,
                opaque,
            } => {
                let old = self.layer_coverage(*layer);
                let l = self
                    .layers
                    .get_mut(layer)
                    .ok_or(WmError::ReleasedSurface(*layer))?;
                // The buffer content itself is supplied separately (it is a
                // renderer handle, not bytes in the transaction); we only record
                // the metadata here and synthesise a solid buffer when the
                // caller did not provide pixels.
                if l.buffer.is_none() {
                    let mut bmp = Bitmap::new(*width, *height, BitmapConfig::Rgba8888)?;
                    bmp.erase(0xff00_0000);
                    l.buffer = Some(FrameBuffer {
                        bitmap: bmp,
                        frame_number: *frame_number,
                        cause_id: *cause_id,
                    });
                } else if let Some(buf) = l.buffer.as_mut() {
                    buf.frame_number = *frame_number;
                    buf.cause_id = *cause_id;
                }
                l.opaque = *opaque;
                l.last_transaction = generation;
                let new = self.layer_coverage(*layer);
                self.add_damage(old.union(new));
            }
            Transaction::SetPosition { layer, position } => {
                let old = self.layer_coverage(*layer);
                let l = self
                    .layers
                    .get_mut(layer)
                    .ok_or(WmError::ReleasedSurface(*layer))?;
                l.position = *position;
                l.last_transaction = generation;
                let new = self.layer_coverage(*layer);
                self.add_damage(old.union(new));
            }
            Transaction::SetLayer { layer, z } => {
                let old = self.layer_coverage(*layer);
                let l = self
                    .layers
                    .get_mut(layer)
                    .ok_or(WmError::ReleasedSurface(*layer))?;
                l.z = *z;
                l.last_transaction = generation;
                let new = self.layer_coverage(*layer);
                self.add_damage(old.union(new));
            }
            Transaction::SetAlpha { layer, alpha } => {
                let old = self.layer_coverage(*layer);
                let l = self
                    .layers
                    .get_mut(layer)
                    .ok_or(WmError::ReleasedSurface(*layer))?;
                l.alpha = alpha.clamp(0.0, 1.0);
                l.last_transaction = generation;
                let new = self.layer_coverage(*layer);
                self.add_damage(old.union(new));
            }
            Transaction::SetVisibility { layer, visible } => {
                let old = self.layer_coverage(*layer);
                let l = self
                    .layers
                    .get_mut(layer)
                    .ok_or(WmError::ReleasedSurface(*layer))?;
                l.visible = *visible;
                l.last_transaction = generation;
                let new = self.layer_coverage(*layer);
                self.add_damage(old.union(new));
            }
            Transaction::SetCrop { layer, crop } => {
                let old = self.layer_coverage(*layer);
                let l = self
                    .layers
                    .get_mut(layer)
                    .ok_or(WmError::ReleasedSurface(*layer))?;
                l.crop = *crop;
                l.last_transaction = generation;
                let new = self.layer_coverage(*layer);
                self.add_damage(old.union(new));
            }
            Transaction::SetTransform { layer, transform } => {
                let old = self.layer_coverage(*layer);
                let l = self
                    .layers
                    .get_mut(layer)
                    .ok_or(WmError::ReleasedSurface(*layer))?;
                l.transform = *transform;
                l.last_transaction = generation;
                let new = self.layer_coverage(*layer);
                self.add_damage(old.union(new));
            }
        }
        self.generation = generation;
        Ok(())
    }

    /// Layers in paint order (ascending z, then id for determinism).
    #[must_use]
    pub fn paint_order(&self) -> Vec<&Layer> {
        let mut v: Vec<&Layer> = self.layers.values().collect();
        v.sort_by_key(|l| (l.z, l.id));
        v
    }

    /// Mark the whole display as damaged (structure changed outside the tracked
    /// mutations, e.g. through [`Compositor::layer_mut`]).
    pub fn invalidate(&mut self) {
        self.damage = Some(self.display.frame);
    }

    /// The pending damage region, if any.
    #[must_use]
    pub fn pending_damage(&self) -> Option<RectI> {
        self.damage
    }

    /// Union `r` (clipped to the display) into the pending damage.
    fn add_damage(&mut self, r: RectI) {
        let r = r.intersect(self.display.frame);
        if r.is_empty() {
            return;
        }
        self.damage = Some(match self.damage {
            Some(d) if !d.is_empty() => d.union(r),
            _ => r,
        });
    }

    /// Atomically produce a frame if anything changed.
    ///
    /// Returns `Ok(None)` when the tree is clean, so callers can never confuse a
    /// stale capture with a fresh one (the protocol's freshness rule).
    pub fn present(&mut self, cause_id: i64) -> Result<Option<u64>> {
        let Some(damage) = self.damage.take() else {
            self.skipped += 1;
            return Ok(None);
        };
        let dmg = damage.intersect(self.display.frame);
        if dmg.is_empty() {
            self.skipped += 1;
            return Ok(None);
        }
        self.compose(dmg)?;
        let seq = self.surface.present_damage(
            cause_id,
            Some(Rect::new(dmg.left, dmg.top, dmg.right, dmg.bottom)),
            Vec::new(),
        );
        self.presents += 1;
        self.last_cause = cause_id;
        Ok(Some(seq))
    }

    /// Composite pending damage into the working buffer **without** producing a
    /// frame.
    ///
    /// This is the exact step [`Compositor::present`] runs before the
    /// `acos-render` commit. It is exposed (hidden) so the benchmark can measure
    /// the compositor's own cost separately from the surface commit; production
    /// callers must use `present`, which is atomic.
    #[doc(hidden)]
    pub fn compose_pending_for_bench(&mut self) -> Result<bool> {
        let Some(damage) = self.damage.take() else {
            return Ok(false);
        };
        let dmg = damage.intersect(self.display.frame);
        if dmg.is_empty() {
            return Ok(false);
        }
        self.compose(dmg)?;
        Ok(true)
    }

    /// Force a present even when clean (used by an explicit capture request).
    pub fn present_forced(&mut self, cause_id: i64) -> Result<u64> {
        self.invalidate();
        self.present(cause_id)?
            .ok_or_else(|| WmError::Unsupported("present_forced produced no frame".into()))
    }

    /// Re-composite `damage` (already clipped to the display) into the surface's
    /// persistent pixmap.
    fn compose(&mut self, damage: RectI) -> Result<()> {
        let display = self.display.frame;
        let w = display.width() as usize;
        let h = display.height() as usize;

        // Plan (layer, damage ∩ coverage) pairs in ascending paint order,
        // skipping layers fully covered by an opaque layer above them. Planning
        // uses only immutable borrows, so the pixel buffer can be borrowed
        // mutably below.
        let mut plan: Vec<(u64, RectI)> = Vec::new();
        let mut opaque_above: Vec<RectI> = Vec::new();
        for layer in self.paint_order().into_iter().rev() {
            let inter = layer_coverage(layer, display).intersect(damage);
            if inter.is_empty() {
                continue;
            }
            let covered = opaque_above.iter().any(|o| o.intersect(inter) == inter);
            if !covered {
                plan.push((layer.id, inter));
            }
            // Only a buffer that is opaque *and* painted at full layer alpha
            // hides the layers below it.
            let la = (layer.alpha.clamp(0.0, 1.0) * 255.0 + 0.5) as u32;
            if layer.opaque && la == 255 {
                opaque_above.push(inter);
            }
        }
        plan.reverse();

        let data = self.surface.pixmap_mut().data_mut();
        fill_rect(data, w, h, damage, BLACK);
        for (id, rect) in &plan {
            let Some(layer) = self.layers.get(id) else {
                continue;
            };
            let Some(buf) = layer.buffer.as_ref() else {
                continue;
            };
            blit_into(
                data,
                w,
                h,
                &buf.bitmap,
                layer.position,
                layer.alpha,
                layer.crop,
                *rect,
                layer.opaque,
            );
        }
        Ok(())
    }

    /// The latest presented frame.
    #[must_use]
    pub fn capture(&self) -> Option<acos_render::Frame> {
        self.surface.capture()
    }

    /// The frame if strictly newer than `after_seq`.
    pub fn capture_fresh(&self, after_seq: u64) -> Result<acos_render::Frame> {
        self.surface.capture_fresh(after_seq).map_err(Into::into)
    }

    /// Last presented sequence.
    #[must_use]
    pub fn frame_seq(&self) -> u64 {
        self.surface.last_seq()
    }

    /// Composite generation.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// `true` when a present would produce a new frame.
    #[must_use]
    pub fn is_dirty(&self) -> bool {
        self.damage.is_some()
    }

    /// Number of frames actually presented.
    #[must_use]
    pub fn presents(&self) -> u64 {
        self.presents
    }

    /// Number of presents skipped because nothing changed.
    #[must_use]
    pub fn skipped(&self) -> u64 {
        self.skipped
    }

    /// Last cause id.
    #[must_use]
    pub fn last_cause(&self) -> i64 {
        self.last_cause
    }

    /// Coverage of `layer` in display space (empty when it paints nothing).
    fn layer_coverage(&self, layer: u64) -> RectI {
        self.layers
            .get(&layer)
            .map_or(RectI::default(), |l| layer_coverage(l, self.display.frame))
    }
}

/// Opaque black, premultiplied RGBA8 (the display background).
const BLACK: [u8; 4] = [0, 0, 0, 255];

/// The display-space rect a layer paints into (clipped to `display`).
fn layer_coverage(l: &Layer, display: RectI) -> RectI {
    if !l.visible || l.alpha <= 0.0 {
        return RectI::default();
    }
    let Some(buf) = l.buffer.as_ref() else {
        return RectI::default();
    };
    let src = source_rect(
        buf.bitmap.width() as i32,
        buf.bitmap.height() as i32,
        l.crop,
    );
    if src.is_empty() {
        return RectI::default();
    }
    let dest = RectI::new(
        l.position.x,
        l.position.y,
        l.position.x + src.width(),
        l.position.y + src.height(),
    );
    dest.intersect(display)
}

/// The buffer-space source rect, honouring crop (clamped to the buffer).
fn source_rect(w: i32, h: i32, crop: Option<RectI>) -> RectI {
    let full = RectI::new(0, 0, w, h);
    match crop {
        Some(c) => RectI::new(c.left.max(0), c.top.max(0), c.right, c.bottom).intersect(full),
        None => full,
    }
}

/// `true` when every pixel is fully opaque (enables the memcpy fast path).
fn detect_opaque(bitmap: &Bitmap) -> bool {
    if !bitmap.has_alpha() {
        return true;
    }
    bitmap.pixels().iter().all(|&w| (w >> 24) == 255)
}

/// Exact `round(x / 255)` for `x <= 255 * 255`, without a division.
#[inline]
fn div255(x: u32) -> u32 {
    let t = x + 128;
    (t + (t >> 8)) >> 8
}

/// Fill `r` (display space, clipped to the buffer) with `px`.
fn fill_rect(dst: &mut [u8], dst_w: usize, dst_h: usize, r: RectI, px: [u8; 4]) {
    if r.is_empty() {
        return;
    }
    let _ = dst_h;
    let n = r.width() as usize * 4;
    for y in r.top..r.bottom {
        let off = (y as usize * dst_w + r.left as usize) * 4;
        for c in dst[off..off + n].chunks_exact_mut(4) {
            c.copy_from_slice(&px);
        }
    }
}

/// Source-over blit of one layer into `dst`, restricted to `rect` (a display
/// space rect already clipped to both the layer coverage and the damage).
///
/// `src.pixels()` is premultiplied RGBA8 stored as `r | g<<8 | b<<16 | a<<24`;
/// on a little-endian host its bytes are already the `[r,g,b,a]` the pixmap
/// stores, so the opaque path is a plain row memcpy.
#[allow(clippy::too_many_arguments)]
fn blit_into(
    dst: &mut [u8],
    dst_w: usize,
    dst_h: usize,
    src: &Bitmap,
    position: PointI,
    alpha: f32,
    crop: Option<RectI>,
    rect: RectI,
    opaque: bool,
) {
    let _ = dst_h;
    if rect.is_empty() {
        return;
    }
    let src_rect = source_rect(src.width() as i32, src.height() as i32, crop);
    if src_rect.is_empty() {
        return;
    }
    let la = (alpha.clamp(0.0, 1.0) * 255.0 + 0.5) as u32;
    if la == 0 {
        return;
    }
    let src_w = src.width() as usize;
    let sx0 = src_rect.left + (rect.left - position.x);
    let sy0 = src_rect.top + (rect.top - position.y);
    let row_bytes = rect.width() as usize * 4;
    let src_bytes: &[u8] = bytemuck::cast_slice(src.pixels());

    // Fast path: fully opaque content at full layer alpha is a row memcpy.
    if opaque && la == 255 {
        for dy in rect.top..rect.bottom {
            let sy = sy0 + (dy - rect.top);
            let so = (sy as usize * src_w + sx0 as usize) * 4;
            let dofs = (dy as usize * dst_w + rect.left as usize) * 4;
            dst[dofs..dofs + row_bytes].copy_from_slice(&src_bytes[so..so + row_bytes]);
        }
        return;
    }

    // General premultiplied source-over, row-wise and bounds-check free.
    for dy in rect.top..rect.bottom {
        let sy = sy0 + (dy - rect.top);
        let so = (sy as usize * src_w + sx0 as usize) * 4;
        let dofs = (dy as usize * dst_w + rect.left as usize) * 4;
        let srow = &src_bytes[so..so + row_bytes];
        let drow = &mut dst[dofs..dofs + row_bytes];
        for (d, s) in drow.chunks_exact_mut(4).zip(srow.chunks_exact(4)) {
            let sa = if la == 255 {
                s[3] as u32
            } else {
                div255(s[3] as u32 * la)
            };
            if sa == 255 {
                d.copy_from_slice(s);
                continue;
            }
            if sa == 0 {
                continue;
            }
            let inv = 255 - sa;
            let (sr, sg, sb) = if la == 255 {
                (s[0] as u32, s[1] as u32, s[2] as u32)
            } else {
                (
                    div255(s[0] as u32 * la),
                    div255(s[1] as u32 * la),
                    div255(s[2] as u32 * la),
                )
            };
            d[0] = (sr + div255(d[0] as u32 * inv)) as u8;
            d[1] = (sg + div255(d[1] as u32 * inv)) as u8;
            d[2] = (sb + div255(d[2] as u32 * inv)) as u8;
            d[3] = (sa + div255(d[3] as u32 * inv)) as u8;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::display::DisplaySpec;
    use proptest::prelude::*;

    fn small_compositor(w: u32, h: u32) -> Compositor {
        let mut d = DisplaySpec::reference_1080x2400();
        d.frame = RectI::new(0, 0, w as i32, h as i32);
        Compositor::new(d).unwrap()
    }

    /// Assert the surface equals a naive full composite, reporting the first
    /// differing pixel instead of dumping two 10 MB arrays.
    fn assert_matches_full(c: &Compositor, ctx: &str) {
        let got = c.surface.pixmap().data();
        let want = composite_full(c);
        if got != want.as_slice() {
            let idx = got
                .iter()
                .zip(want.iter())
                .position(|(a, b)| a != b)
                .unwrap_or(0);
            let w = c.display.frame.width() as usize;
            let px = idx / 4;
            let (x, y) = (px % w, px / w);
            let end = (idx + 4).min(got.len());
            let wend = (idx + 4).min(want.len());
            panic!(
                "{ctx}: first mismatch at byte {idx} (pixel {x},{y}): got {:?} want {:?}",
                &got[idx..end],
                &want[idx..wend]
            );
        }
    }

    /// A naive full composite of the current tree, ignoring damage and occlusion
    /// skips. This is the reference the damage path must match exactly.
    fn composite_full(c: &Compositor) -> Vec<u8> {
        let w = c.display.frame.width() as usize;
        let h = c.display.frame.height() as usize;
        let mut data = vec![0u8; w * h * 4];
        fill_rect(&mut data, w, h, c.display.frame, BLACK);
        for layer in c.paint_order() {
            let Some(buf) = layer.buffer.as_ref() else {
                continue;
            };
            if !layer.visible || layer.alpha <= 0.0 {
                continue;
            }
            let cov = layer_coverage(layer, c.display.frame);
            if cov.is_empty() {
                continue;
            }
            blit_into(
                &mut data,
                w,
                h,
                &buf.bitmap,
                layer.position,
                layer.alpha,
                layer.crop,
                cov,
                layer.opaque,
            );
        }
        data
    }

    #[test]
    fn two_layers_z_order_opaque_wins() {
        let mut c = small_compositor(8, 8);
        let red = c.create_layer(None, "red", 0);
        let blue = c.create_layer(None, "blue", 1);
        let mut r = Bitmap::new(8, 8, BitmapConfig::Rgba8888).unwrap();
        r.erase(0xffff0000);
        let mut b = Bitmap::new(4, 4, BitmapConfig::Rgba8888).unwrap();
        b.erase(0xff0000ff);
        c.set_buffer(red, r, 1, 10).unwrap();
        c.set_buffer(blue, b, 1, 10).unwrap();
        c.apply(&Transaction::SetPosition {
            layer: blue,
            position: PointI::new(2, 2),
        })
        .unwrap();
        let seq = c.present(10).unwrap().unwrap();
        assert_eq!(seq, 1);
        let frame = c.capture().unwrap();
        let px = |x: u32, y: u32| {
            let p = frame.pixels.pixel(x, y).unwrap();
            (p.red(), p.green(), p.blue(), p.alpha())
        };
        assert_eq!(px(0, 0), (255, 0, 0, 255), "outside blue stays red");
        assert_eq!(px(3, 3), (0, 0, 255, 255), "blue overlaps on top");
    }

    #[test]
    fn present_is_skipped_without_damage() {
        let mut c = small_compositor(4, 4);
        let l = c.create_layer(None, "l", 0);
        let mut b = Bitmap::new(4, 4, BitmapConfig::Rgba8888).unwrap();
        b.erase(0xff010203);
        c.set_buffer(l, b, 1, 1).unwrap();
        assert_eq!(c.present(1).unwrap(), Some(1));
        assert_eq!(c.present(1).unwrap(), None, "no damage => no new frame");
        assert_eq!(c.skipped(), 1);
        c.set_buffer(l, Bitmap::new(4, 4, BitmapConfig::Rgba8888).unwrap(), 2, 2)
            .unwrap();
        assert_eq!(c.present(2).unwrap(), Some(2));
        assert_eq!(c.frame_seq(), 2);
    }

    #[test]
    fn cause_id_is_propagated() {
        let mut c = small_compositor(2, 2);
        let l = c.create_layer(None, "l", 0);
        let mut b = Bitmap::new(2, 2, BitmapConfig::Rgba8888).unwrap();
        b.erase(0xff112233);
        c.set_buffer(l, b, 1, 77).unwrap();
        c.present(77).unwrap();
        assert_eq!(c.capture().unwrap().cause_id, 77);
        assert_eq!(c.last_cause(), 77);
    }

    #[test]
    fn damage_tracking_keeps_only_the_changed_region() {
        let mut c = small_compositor(64, 64);
        let l = c.create_layer(None, "l", 0);
        let mut b = Bitmap::new(20, 20, BitmapConfig::Rgba8888).unwrap();
        b.erase(0xff102030);
        c.set_buffer(l, b, 1, 1).unwrap();
        c.present(1).unwrap();
        // Move the layer right by 5px: only the old∪new strip is damaged.
        c.apply(&Transaction::SetPosition {
            layer: l,
            position: PointI::new(5, 0),
        })
        .unwrap();
        assert_eq!(c.pending_damage(), Some(RectI::new(0, 0, 25, 20)));
        c.present(1).unwrap();
        assert_matches_full(&c, "after move");
    }

    #[test]
    fn removed_layer_area_is_repainted() {
        let mut c = small_compositor(32, 32);
        let under = c.create_layer(None, "under", 0);
        let over = c.create_layer(None, "over", 1);
        let mut u = Bitmap::new(32, 32, BitmapConfig::Rgba8888).unwrap();
        u.erase(0xff00ff00);
        let mut o = Bitmap::new(10, 10, BitmapConfig::Rgba8888).unwrap();
        o.erase(0xffff0000);
        c.set_buffer(under, u, 1, 1).unwrap();
        c.set_buffer(over, o, 1, 1).unwrap();
        c.apply(&Transaction::SetPosition {
            layer: over,
            position: PointI::new(4, 4),
        })
        .unwrap();
        c.present(1).unwrap();
        assert_matches_full(&c, "after draw");
        c.remove_layer(over);
        c.present(1).unwrap();
        assert_matches_full(&c, "after remove");
        // The former over-layer area is now green.
        let d = c.surface.pixmap().data();
        let idx = (8 * 32 + 8) * 4;
        assert_eq!(&d[idx..idx + 4], &[0, 255, 0, 255]);
    }

    #[test]
    fn translucent_alpha_does_not_occlude_the_layer_below() {
        let mut c = small_compositor(8, 8);
        let under = c.create_layer(None, "under", 0);
        let over = c.create_layer(None, "over", 1);
        let mut u = Bitmap::new(8, 8, BitmapConfig::Rgba8888).unwrap();
        u.erase(0xff00_00ff);
        let mut o = Bitmap::new(8, 8, BitmapConfig::Rgba8888).unwrap();
        o.erase(0xffff_0000);
        c.set_buffer(under, u, 1, 1).unwrap();
        c.set_buffer(over, o, 1, 1).unwrap();
        // `over` has an opaque buffer but is painted at 50%: it must not skip
        // `under` in the occluder pass.
        c.apply(&Transaction::SetAlpha {
            layer: over,
            alpha: 0.5,
        })
        .unwrap();
        c.present(1).unwrap();
        assert_matches_full(&c, "translucent opaque-buffer top");
    }

    #[test]
    fn negative_position_and_crop_stay_in_bounds() {
        let mut c = small_compositor(20, 20);
        let l = c.create_layer(None, "l", 0);
        let mut b = Bitmap::new(8, 8, BitmapConfig::Rgba8888).unwrap();
        b.erase(0xffffffff);
        c.set_buffer(l, b, 1, 1).unwrap();
        c.apply(&Transaction::SetPosition {
            layer: l,
            position: PointI::new(-4, -4),
        })
        .unwrap();
        c.apply(&Transaction::SetCrop {
            layer: l,
            crop: Some(RectI::new(2, 2, 6, 6)),
        })
        .unwrap();
        c.present(1).unwrap();
        assert_matches_full(&c, "negative crop");
    }

    #[derive(Debug, Clone)]
    struct BufSpec {
        w: u32,
        h: u32,
        base: u32,
        blot: Option<(i32, i32, u32, u32, u32)>,
    }

    impl BufSpec {
        fn build(&self) -> Bitmap {
            let mut b = Bitmap::new(self.w, self.h, BitmapConfig::Rgba8888).unwrap();
            b.erase(self.base);
            if let Some((x, y, w, h, c)) = self.blot {
                for yy in y..y + h as i32 {
                    for xx in x..x + w as i32 {
                        b.set_pixel(xx, yy, c);
                    }
                }
            }
            b
        }
    }

    fn buf_spec() -> impl Strategy<Value = BufSpec> {
        (
            1u32..12,
            1u32..12,
            any::<u32>(),
            prop::option::of((0i32..8, 0i32..8, 1u32..7, 1u32..7, any::<u32>())),
        )
            .prop_map(|(w, h, base, blot)| BufSpec { w, h, base, blot })
    }

    #[derive(Debug, Clone)]
    enum Op {
        SetBuffer {
            layer: u8,
            buf: u8,
        },
        Position {
            layer: u8,
            x: i32,
            y: i32,
        },
        Alpha {
            layer: u8,
            a: u8,
        },
        Z {
            layer: u8,
            z: i8,
        },
        Visible {
            layer: u8,
            v: bool,
        },
        Crop {
            layer: u8,
            crop: Option<(i32, i32, i32, i32)>,
        },
        Create {
            z: i8,
        },
        Remove {
            layer: u8,
        },
    }

    fn op() -> impl Strategy<Value = Op> {
        prop_oneof![
            (0u8..4, 0u8..4).prop_map(|(layer, buf)| Op::SetBuffer { layer, buf }),
            (0u8..8, -8i32..40, -8i32..40).prop_map(|(layer, x, y)| Op::Position { layer, x, y }),
            (0u8..8, 0u8..=255).prop_map(|(layer, a)| Op::Alpha { layer, a }),
            (0u8..8, -2i8..3).prop_map(|(layer, z)| Op::Z { layer, z }),
            (0u8..8, any::<bool>()).prop_map(|(layer, v)| Op::Visible { layer, v }),
            (
                0u8..8,
                prop::option::of((-2i32..10, -2i32..10, 0i32..14, 0i32..14))
            )
                .prop_map(|(layer, crop)| Op::Crop { layer, crop }),
            (-1i8..3).prop_map(|z| Op::Create { z }),
            (0u8..8).prop_map(|layer| Op::Remove { layer }),
        ]
    }

    fn pick(ids: &[u64], i: u8) -> Option<u64> {
        if ids.is_empty() {
            None
        } else {
            Some(ids[i as usize % ids.len()])
        }
    }

    fn apply_op(c: &mut Compositor, op: &Op, bufs: &[Bitmap], ids: &mut Vec<u64>) {
        match op {
            Op::SetBuffer { layer, buf } => {
                if let Some(id) = pick(ids, *layer) {
                    let _ = c.set_buffer(id, bufs[*buf as usize % bufs.len()].clone(), 1, 55);
                }
            }
            Op::Position { layer, x, y } => {
                if let Some(id) = pick(ids, *layer) {
                    let _ = c.apply(&Transaction::SetPosition {
                        layer: id,
                        position: PointI::new(*x, *y),
                    });
                }
            }
            Op::Alpha { layer, a } => {
                if let Some(id) = pick(ids, *layer) {
                    let _ = c.apply(&Transaction::SetAlpha {
                        layer: id,
                        alpha: *a as f32 / 255.0,
                    });
                }
            }
            Op::Z { layer, z } => {
                if let Some(id) = pick(ids, *layer) {
                    let _ = c.apply(&Transaction::SetLayer {
                        layer: id,
                        z: *z as i64,
                    });
                }
            }
            Op::Visible { layer, v } => {
                if let Some(id) = pick(ids, *layer) {
                    let _ = c.apply(&Transaction::SetVisibility {
                        layer: id,
                        visible: *v,
                    });
                }
            }
            Op::Crop { layer, crop } => {
                if let Some(id) = pick(ids, *layer) {
                    let cr = crop.map(|(l, t, r, b)| RectI::new(l, t, r, b));
                    let _ = c.apply(&Transaction::SetCrop {
                        layer: id,
                        crop: cr,
                    });
                }
            }
            Op::Create { z } => {
                if ids.len() < 8 {
                    let id = c.create_layer(None, "n", *z as i64);
                    let _ = c.set_buffer(id, bufs[ids.len() % bufs.len()].clone(), 1, 55);
                    let _ = c.apply(&Transaction::SetPosition {
                        layer: id,
                        position: PointI::new(ids.len() as i32 * 2 - 4, -3),
                    });
                    ids.push(id);
                }
            }
            Op::Remove { layer } => {
                if ids.len() > 1 {
                    let i = *layer as usize % ids.len();
                    let id = ids.remove(i);
                    let _ = c.remove_layer(id);
                }
            }
        }
    }

    fn run_case(pool: &[BufSpec], seed_ops: &[Op], ops: &[Op]) {
        let bufs: Vec<Bitmap> = pool.iter().map(BufSpec::build).collect();
        let mut c = small_compositor(32, 32);
        let mut ids = vec![
            c.create_layer(None, "a", 0),
            c.create_layer(None, "b", 1),
            c.create_layer(None, "c", 2),
        ];
        for op in seed_ops {
            apply_op(&mut c, op, &bufs, &mut ids);
        }
        let _ = c.present(1);
        assert_matches_full(&c, "seed");
        for (step, op) in ops.iter().enumerate() {
            apply_op(&mut c, op, &bufs, &mut ids);
            let _ = c.present(1);
            assert_matches_full(&c, &format!("step {step} op {op:?}"));
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(600))]
        #[test]
        fn damage_matches_full(
            pool in prop::collection::vec(buf_spec(), 1..4),
            seed_ops in prop::collection::vec(op(), 1..10),
            ops in prop::collection::vec(op(), 1..30),
        ) {
            run_case(&pool, &seed_ops, &ops);
        }
    }
}
