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
//! Damage is explicit: `present` returns `Ok(None)` when nothing changed, so a
//! capture with no input cannot be labelled fresh.

use std::collections::BTreeMap;

use acos_render::{Bitmap, BitmapConfig, Surface};

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
    /// `true` since the last present.
    dirty: bool,
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
            .field("dirty", &self.dirty)
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
        Ok(Self {
            display,
            surface,
            layers: BTreeMap::new(),
            next_layer: 1,
            generation: 0,
            dirty: true,
            presents: 0,
            skipped: 0,
            last_cause: 0,
        })
    }

    /// Create a layer and return its id.
    pub fn create_layer(&mut self, window: Option<WindowId>, name: impl Into<String>, z: i64) -> u64 {
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
        self.dirty = true;
        id
    }

    /// Borrow a layer.
    #[must_use]
    pub fn layer(&self, id: u64) -> Option<&Layer> {
        self.layers.get(&id)
    }

    /// Borrow a layer mutably.
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
        let removed = self.layers.remove(&id);
        if removed.is_some() {
            self.generation += 1;
            self.dirty = true;
        }
        removed
    }

    /// Directly attach a buffer to a layer (used by the frame-handoff path).
    pub fn set_buffer(
        &mut self,
        layer: u64,
        bitmap: Bitmap,
        frame_number: u64,
        cause_id: i64,
    ) -> Result<()> {
        let l = self
            .layers
            .get_mut(&layer)
            .ok_or(WmError::ReleasedSurface(layer))?;
        l.opaque = !bitmap.has_alpha();
        l.buffer = Some(FrameBuffer {
            bitmap,
            frame_number,
            cause_id,
        });
        self.generation += 1;
        self.dirty = true;
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
            }
            Transaction::SetPosition { layer, position } => {
                let l = self
                    .layers
                    .get_mut(layer)
                    .ok_or(WmError::ReleasedSurface(*layer))?;
                l.position = *position;
                l.last_transaction = generation;
            }
            Transaction::SetLayer { layer, z } => {
                let l = self
                    .layers
                    .get_mut(layer)
                    .ok_or(WmError::ReleasedSurface(*layer))?;
                l.z = *z;
                l.last_transaction = generation;
            }
            Transaction::SetAlpha { layer, alpha } => {
                let l = self
                    .layers
                    .get_mut(layer)
                    .ok_or(WmError::ReleasedSurface(*layer))?;
                l.alpha = alpha.clamp(0.0, 1.0);
                l.last_transaction = generation;
            }
            Transaction::SetVisibility { layer, visible } => {
                let l = self
                    .layers
                    .get_mut(layer)
                    .ok_or(WmError::ReleasedSurface(*layer))?;
                l.visible = *visible;
                l.last_transaction = generation;
            }
            Transaction::SetCrop { layer, crop } => {
                let l = self
                    .layers
                    .get_mut(layer)
                    .ok_or(WmError::ReleasedSurface(*layer))?;
                l.crop = *crop;
                l.last_transaction = generation;
            }
            Transaction::SetTransform { layer, transform } => {
                let l = self
                    .layers
                    .get_mut(layer)
                    .ok_or(WmError::ReleasedSurface(*layer))?;
                l.transform = *transform;
                l.last_transaction = generation;
            }
        }
        self.generation = generation;
        self.dirty = true;
        Ok(())
    }

    /// Layers in paint order (ascending z, then id for determinism).
    #[must_use]
    pub fn paint_order(&self) -> Vec<&Layer> {
        let mut v: Vec<&Layer> = self.layers.values().collect();
        v.sort_by_key(|l| (l.z, l.id));
        v
    }

    /// Atomically produce a frame if anything changed.
    ///
    /// Returns `Ok(None)` when the tree is clean, so callers can never confuse a
    /// stale capture with a fresh one (the protocol's freshness rule).
    pub fn present(&mut self, cause_id: i64) -> Result<Option<u64>> {
        if !self.dirty {
            self.skipped += 1;
            return Ok(None);
        }
        self.compose()?;
        let seq = self.surface.present(cause_id, Vec::new());
        self.dirty = false;
        self.presents += 1;
        self.last_cause = cause_id;
        Ok(Some(seq))
    }

    /// Force a present even when clean (used by an explicit capture request).
    pub fn present_forced(&mut self, cause_id: i64) -> Result<u64> {
        self.dirty = true;
        self.present(cause_id)?
            .ok_or_else(|| WmError::Unsupported("present_forced produced no frame".into()))
    }

    fn compose(&mut self) -> Result<()> {
        let w = self.display.frame.width();
        let h = self.display.frame.height();
        let mut out = Bitmap::new(w as u32, h as u32, BitmapConfig::Rgba8888)?;
        // Background: opaque black (the display always scans out something).
        out.erase(0xff00_0000);
        let order: Vec<(u64, PointI, f32, Option<RectI>, bool)> = self
            .paint_order()
            .into_iter()
            .filter(|l| l.visible)
            .map(|l| (l.id, l.position, l.alpha, l.crop, l.buffer.is_some()))
            .collect();
        for (id, position, alpha, crop, has_buffer) in order {
            if !has_buffer {
                continue;
            }
            let layer = self
                .layers
                .get(&id)
                .ok_or(WmError::ReleasedSurface(id))?;
            let buffer = layer
                .buffer
                .as_ref()
                .ok_or(WmError::ReleasedSurface(id))?;
            blit(&mut out, &buffer.bitmap, position, alpha, crop);
        }
        self.surface.pixmap = out.to_pixmap()?;
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
        self.dirty
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
}

/// Source-over blit of `src` into `dst` at `position`, honouring `alpha`/`crop`.
fn blit(dst: &mut Bitmap, src: &Bitmap, position: PointI, alpha: f32, crop: Option<RectI>) {
    let alpha = alpha.clamp(0.0, 1.0);
    let (sx0, sy0, sx1, sy1) = match crop {
        Some(c) => (c.left.max(0), c.top.max(0), c.right, c.bottom),
        None => (0, 0, src.width() as i32, src.height() as i32),
    };
    for sy in sy0..sy1.min(src.height() as i32) {
        for sx in sx0..sx1.min(src.width() as i32) {
            let dx = position.x + (sx - sx0);
            let dy = position.y + (sy - sy0);
            if dx < 0 || dy < 0 || dx >= dst.width() as i32 || dy >= dst.height() as i32 {
                continue;
            }
            let s = src.get_pixel(sx, sy);
            let sa = ((s >> 24) & 0xff) as f32 / 255.0 * alpha;
            if sa <= 0.0 {
                continue;
            }
            let d = dst.get_pixel(dx, dy);
            let out = blend_argb(s, sa, d);
            dst.set_pixel(dx, dy, out);
        }
    }
}

/// Non-premultiplied source-over in ARGB space.
fn blend_argb(src: u32, src_alpha: f32, dst: u32) -> u32 {
    let sr = ((src >> 16) & 0xff) as f32;
    let sg = ((src >> 8) & 0xff) as f32;
    let sb = (src & 0xff) as f32;
    let da = ((dst >> 24) & 0xff) as f32 / 255.0;
    let dr = ((dst >> 16) & 0xff) as f32;
    let dg = ((dst >> 8) & 0xff) as f32;
    let db = (dst & 0xff) as f32;
    let out_a = src_alpha + da * (1.0 - src_alpha);
    let mix = |s: f32, d: f32| -> u32 {
        if out_a <= 0.0 {
            0
        } else {
            (((s * src_alpha + d * da * (1.0 - src_alpha)) / out_a).clamp(0.0, 255.0)) as u32
        }
    };
    let a = (out_a * 255.0).clamp(0.0, 255.0) as u32;
    (a << 24) | (mix(sr, dr) << 16) | (mix(sg, dg) << 8) | mix(sb, db)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::display::DisplaySpec;

    fn small_compositor(w: u32, h: u32) -> Compositor {
        let mut d = DisplaySpec::reference_1080x2400();
        d.frame = RectI::new(0, 0, w as i32, h as i32);
        Compositor::new(d).unwrap()
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
}
