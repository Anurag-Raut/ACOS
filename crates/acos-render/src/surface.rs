//! Offscreen surface + frame contract.
//!
//! A surface owns a software RGBA8888 buffer. Every time content is presented
//! it gets a monotonically increasing `frame_seq`, a production timestamp, a
//! caller-supplied `cause_id` and a stable 128-bit content hash. `capture()`
//! never re-renders; `capture_fresh(after_seq)` refuses to hand back a stale
//! frame (structured `NO_FRESH_FRAME`).

use std::sync::Arc;
use std::time::{Instant, SystemTime};

use crate::error::{RenderError, RenderResult};
use crate::geom::Rect;
use crate::hash;
use tiny_skia::Pixmap;

/// How "contentful" a frame is.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum FrameClass {
    /// Fully transparent.
    Empty,
    /// One (near-)uniform colour covering the frame — a placeholder.
    SingleColour,
    /// Real content.
    Content,
}

/// A presented frame.
#[derive(Clone, Debug)]
pub struct Frame {
    pub seq: u64,
    pub width: u32,
    pub height: u32,
    pub density: f32,
    pub produced_at: SystemTime,
    pub produced_at_instant: Instant,
    /// The id of the action/request that caused this frame.
    pub cause_id: i64,
    pub content_hash: u128,
    pub class: FrameClass,
    pub unsupported_ops: Vec<String>,
    pub pixels: Arc<Pixmap>,
}

impl Frame {
    /// Raw premultiplied RGBA8 bytes.
    pub fn rgba8(&self) -> &[u8] {
        self.pixels.data()
    }

    /// Hex content hash.
    pub fn hash_hex(&self) -> String {
        hash::hash_hex(self.content_hash)
    }

    /// Optional cryptographic digest of the pixels.
    pub fn sha256(&self) -> String {
        hash::sha256_hex(self.pixels.data())
    }

    /// Encode as PNG.
    pub fn png(&self, setting: PngSetting) -> Vec<u8> {
        encode_png(&self.pixels, setting)
    }

    /// Encode as QOI (cheap, agent-friendly, lossless).
    pub fn qoi(&self) -> Vec<u8> {
        encode_qoi(&self.pixels)
    }
}

/// PNG encoder tuning: the fast path matters for agent screenshot latency.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum PngSetting {
    /// No filtering, fastest.
    Fast,
    /// Sub filter, fast compression.
    FastFiltered,
    /// Adaptive filtering, default compression.
    Default,
    /// Best compression (slowest).
    Best,
}

impl PngSetting {
    pub fn all() -> [PngSetting; 4] {
        [
            PngSetting::Fast,
            PngSetting::FastFiltered,
            PngSetting::Default,
            PngSetting::Best,
        ]
    }
}

/// `Surface::new(width, height, density)` — the backing allocation is
/// lazy (a zeroed pixmap is allocated here; real Android defers until first
/// draw; the observable contract is identical).
pub struct Surface {
    pub width: u32,
    pub height: u32,
    pub density: f32,
    /// The working buffer the next present snapshots. It is **not** the buffer
    /// owned by the most recent [`Frame`]: presenting double-buffers (see
    /// [`Surface::present_damage`]).
    pub pixmap: Pixmap,
    /// The canvas that rasterises into this surface.
    pub canvas: Option<crate::handle::Handle>,
    pub(crate) frame: Option<Frame>,
    seq: u64,
    /// Version of the render tree at the last present (damage tracking).
    pub last_tree_version: Option<u64>,
    pub raster_ops: u64,
    pub strict: bool,
    /// Per-row xxh3-128 of the working buffer, used to maintain the full-buffer
    /// content hash across damage-limited presents. `row_hashes_valid` is false
    /// until the first present has hashed every row.
    row_hashes: Vec<u128>,
    row_hashes_valid: bool,
}

impl Surface {
    pub fn new(width: u32, height: u32, density: f32) -> RenderResult<Self> {
        if width == 0 || height == 0 {
            return Err(RenderError::size("surface dimensions must be non-zero"));
        }
        let pixmap = Pixmap::new(width, height)
            .ok_or_else(|| RenderError::size("surface allocation failed"))?;
        Ok(Surface {
            width,
            height,
            density,
            pixmap,
            canvas: None,
            frame: None,
            seq: 0,
            last_tree_version: None,
            raster_ops: 0,
            strict: false,
            row_hashes: Vec::new(),
            row_hashes_valid: false,
        })
    }

    pub fn ensure_canvas(&mut self, canvas: crate::handle::Handle) {
        if self.canvas.is_none() {
            self.canvas = Some(canvas);
        }
    }

    /// Borrow the working buffer that the next present snapshots.
    pub fn pixmap(&self) -> &Pixmap {
        &self.pixmap
    }

    /// Mutably borrow the working buffer.
    ///
    /// The caller owns the damage contract: writes made here are only reflected
    /// in the next frame's content hash for the region passed to
    /// [`Surface::present_damage`]. Writing through the public `pixmap` field is
    /// equivalent; `pixmap_mut` exists to make that contract explicit at the
    /// call site.
    pub fn pixmap_mut(&mut self) -> &mut Pixmap {
        &mut self.pixmap
    }

    /// Present the working buffer as a new frame, hashing every pixel.
    ///
    /// This is the full-frame path: it is equivalent to
    /// `present_damage(cause_id, None, unsupported_ops)` and costs a full-buffer
    /// hash plus a full-buffer copy. Callers that know which region changed
    /// should use [`Surface::present_damage`] instead.
    pub fn present(&mut self, cause_id: i64, unsupported_ops: Vec<String>) -> u64 {
        self.present_damage(cause_id, None, unsupported_ops)
    }

    /// Present after the caller has written a subset of the working buffer.
    ///
    /// * `damage == None` means "unknown / whole frame" and hashes/copies the
    ///   whole buffer (the honest full-frame cost).
    /// * `damage == Some(rect)` means only `rect` (clamped to the surface) may
    ///   have changed. Only the rows intersecting `rect` are re-hashed; the
    ///   full-buffer content hash is then re-folded from the per-row hashes, so
    ///   it is identical to the hash a full [`Surface::present`] would report
    ///   for the same pixels.
    ///
    /// ## Frame contract
    ///
    /// `seq`, `cause_id`, `content_hash` (over the whole buffer),
    /// `class`, timestamps, `unsupported_ops` and the `NO_FRESH_FRAME`
    /// semantics of `capture_fresh` are unchanged. `class` still classifies the
    /// full working buffer.
    ///
    /// ## Double buffering / immutability
    ///
    /// The just-presented pixels are moved into an `Arc<Pixmap>` owned by the
    /// new [`Frame`]; the surface keeps working in the *other* buffer, which is
    /// first synced over `damage` so the next damage-only composite has the
    /// right base. The other buffer is the previous frame's buffer, reclaimed
    /// with `Arc::try_unwrap`: if a previously handed-out frame still references
    /// it, the buffer is copied first (Arc copy-on-write), so previous frames
    /// stay immutable. No full clone is paid on the steady-state damage path
    /// where the caller does not retain frames across presents.
    pub fn present_damage(
        &mut self,
        cause_id: i64,
        damage: Option<Rect>,
        unsupported_ops: Vec<String>,
    ) -> u64 {
        self.seq += 1;
        let w = self.width as usize;
        let h = self.height as usize;

        // Clamp the caller's damage to the buffer; `None` means "whole frame".
        let full = damage.is_none();
        let rect = clamp_damage(damage, self.width, self.height);

        // Maintain the per-row hashes for the changed rows only. A partial
        // present before every row has been hashed falls back to a full hash.
        if full || !self.row_hashes_valid {
            if self.row_hashes.len() != h {
                self.row_hashes = vec![0u128; h];
            }
            let data = self.pixmap.data();
            for y in 0..h {
                self.row_hashes[y] = hash::hash_bytes(&data[y * w * 4..(y + 1) * w * 4]);
            }
            self.row_hashes_valid = true;
        } else if !rect.is_empty() {
            let data = self.pixmap.data();
            for y in rect.top as usize..rect.bottom as usize {
                self.row_hashes[y] = hash::hash_bytes(&data[y * w * 4..(y + 1) * w * 4]);
            }
        }
        let content_hash = hash::hash_rows(&self.row_hashes);
        let class = classify(&self.pixmap);

        // Reclaim the previous frame's buffer as the next working buffer. If an
        // externally held frame still owns it, clone it (copy-on-write) so that
        // frame stays immutable.
        let mut next = match self.frame.take() {
            Some(prev) => {
                let Frame { pixels, .. } = prev;
                match Arc::try_unwrap(pixels) {
                    Ok(pixmap) => pixmap,
                    Err(shared) => (*shared).clone(),
                }
            }
            None => Pixmap::new(self.width, self.height)
                .expect("surface dimensions validated at construction"),
        };
        copy_damage(&self.pixmap, &mut next, full, rect, w);
        let presented = Arc::new(std::mem::replace(&mut self.pixmap, next));

        let frame = Frame {
            seq: self.seq,
            width: self.width,
            height: self.height,
            density: self.density,
            produced_at: SystemTime::now(),
            produced_at_instant: Instant::now(),
            cause_id,
            content_hash,
            class,
            unsupported_ops,
            pixels: presented,
        };
        self.frame = Some(frame);
        self.seq
    }

    pub fn last_seq(&self) -> u64 {
        self.seq
    }

    /// Present a new frame that reuses the previous pixel buffer (zero raster
    /// work). Used by damage tracking when the tree version is unchanged.
    pub fn present_cached(&mut self, cause_id: i64) -> u64 {
        self.seq += 1;
        match self.frame.clone() {
            Some(prev) => {
                let frame = Frame {
                    seq: self.seq,
                    produced_at: SystemTime::now(),
                    produced_at_instant: Instant::now(),
                    cause_id,
                    ..prev
                };
                self.frame = Some(frame);
                self.seq
            }
            None => self.present(cause_id, Vec::new()),
        }
    }

    /// Latest presented frame without re-rendering.
    pub fn capture(&self) -> Option<Frame> {
        self.frame.clone()
    }

    /// The frame if it is strictly newer than `after_seq`.
    pub fn capture_fresh(&self, after_seq: u64) -> RenderResult<Frame> {
        match &self.frame {
            Some(f) if f.seq > after_seq => Ok(f.clone()),
            Some(f) => Err(RenderError::NoFreshFrame {
                after: after_seq,
                latest: f.seq,
            }),
            None => Err(RenderError::NoFreshFrame {
                after: after_seq,
                latest: 0,
            }),
        }
    }
}

/// Clamp a caller-reported damage rect to `[0,width] x [0,height]`.
///
/// `None` ("unknown / whole frame") is clamped to the whole buffer; an empty or
/// fully out-of-bounds rect yields an empty `Rect`, which the caller treats as
/// "nothing changed".
fn clamp_damage(damage: Option<Rect>, width: u32, height: u32) -> Rect {
    let (w, h) = (width as i32, height as i32);
    match damage {
        None => Rect::new(0, 0, w, h),
        Some(r) => Rect::new(
            r.left.clamp(0, w),
            r.top.clamp(0, h),
            r.right.clamp(0, w),
            r.bottom.clamp(0, h),
        ),
    }
}

/// Copy the presented region from the just-composited working buffer into the
/// buffer that will become the next working buffer. `full` selects the whole
/// buffer (the honest full-frame path); otherwise only `rect` is copied.
fn copy_damage(src: &Pixmap, dst: &mut Pixmap, full: bool, rect: Rect, width: usize) {
    let (l, t, r, b) = if full {
        (0, 0, src.width() as i32, src.height() as i32)
    } else {
        (rect.left, rect.top, rect.right, rect.bottom)
    };
    if l >= r || t >= b {
        return;
    }
    let row_bytes = (r - l) as usize * 4;
    let src_data = src.data();
    let dst_data = dst.data_mut();
    for y in t..b {
        let off = (y as usize * width + l as usize) * 4;
        dst_data[off..off + row_bytes].copy_from_slice(&src_data[off..off + row_bytes]);
    }
}

/// Classify a frame as empty / single-colour / content.
pub fn classify(pixmap: &Pixmap) -> FrameClass {
    let mut seen: Option<u32> = None;
    let mut distinct = 0u32;
    let mut all_transparent = true;
    for p in pixmap.pixels() {
        let word = (p.red() as u32)
            | ((p.green() as u32) << 8)
            | ((p.blue() as u32) << 16)
            | ((p.alpha() as u32) << 24);
        let q = quantize(word);
        if q != 0 {
            all_transparent = false;
        }
        match seen {
            None => {
                seen = Some(q);
                distinct = 1;
            }
            Some(s) if s == q => {}
            Some(_) => {
                distinct += 1;
                // More than a handful of quantised colours => real content.
                if distinct > 8 {
                    return FrameClass::Content;
                }
            }
        }
    }
    if all_transparent {
        FrameClass::Empty
    } else if distinct <= 1 {
        FrameClass::SingleColour
    } else {
        FrameClass::Content
    }
}

fn quantize(word: u32) -> u32 {
    // Quantise each channel to 5 bits so dithering/AA noise does not defeat
    // the "single colour" test.
    let r = (word & 0xff) >> 3;
    let g = ((word >> 8) & 0xff) >> 3;
    let b = ((word >> 16) & 0xff) >> 3;
    let a = ((word >> 24) & 0xff) >> 3;
    r | (g << 5) | (b << 10) | (a << 15)
}

/// Encode a pixmap as PNG with the chosen tradeoff.
pub fn encode_png(pixmap: &Pixmap, setting: PngSetting) -> Vec<u8> {
    let mut out = Vec::with_capacity((pixmap.width() * pixmap.height()) as usize / 4 + 1024);
    // PNG expects non-premultiplied RGBA. tiny-skia stores premultiplied.
    let mut rgba = vec![0u8; (pixmap.width() * pixmap.height() * 4) as usize];
    for (dst, p) in rgba.chunks_exact_mut(4).zip(pixmap.pixels()) {
        let a = p.alpha();
        dst[0] = crate::color::unpremultiply_channel(p.red(), a);
        dst[1] = crate::color::unpremultiply_channel(p.green(), a);
        dst[2] = crate::color::unpremultiply_channel(p.blue(), a);
        dst[3] = a;
    }
    {
        let mut enc = png::Encoder::new(&mut out, pixmap.width(), pixmap.height());
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        match setting {
            PngSetting::Fast => {
                enc.set_compression(png::Compression::Fast);
                enc.set_filter(png::FilterType::NoFilter);
                enc.set_adaptive_filter(png::AdaptiveFilterType::NonAdaptive);
            }
            PngSetting::FastFiltered => {
                enc.set_compression(png::Compression::Fast);
                enc.set_filter(png::FilterType::Sub);
                enc.set_adaptive_filter(png::AdaptiveFilterType::NonAdaptive);
            }
            PngSetting::Default => {
                enc.set_compression(png::Compression::Default);
                enc.set_adaptive_filter(png::AdaptiveFilterType::Adaptive);
            }
            PngSetting::Best => {
                enc.set_compression(png::Compression::Best);
                enc.set_adaptive_filter(png::AdaptiveFilterType::Adaptive);
            }
        }
        if let Ok(mut writer) = enc.write_header() {
            let _ = writer.write_image_data(&rgba);
        }
    }
    out
}

/// Minimal QOI (Quite OK Image) encoder: lossless, cheap, decoded by many
/// agent tools.
pub fn encode_qoi(pixmap: &Pixmap) -> Vec<u8> {
    let w = pixmap.width();
    let h = pixmap.height();
    let mut out = Vec::with_capacity((w * h * 4) as usize + 14);
    out.extend_from_slice(b"qoif");
    out.extend_from_slice(&w.to_be_bytes());
    out.extend_from_slice(&h.to_be_bytes());
    out.push(4); // channels: RGBA
    out.push(0); // colorspace: sRGB
    let mut index = [[0u8; 4]; 64];
    let mut prev = [0u8, 0, 0, 255];
    let mut run = 0u8;
    for p in pixmap.pixels() {
        let a = p.alpha();
        let px = [
            crate::color::unpremultiply_channel(p.red(), a),
            crate::color::unpremultiply_channel(p.green(), a),
            crate::color::unpremultiply_channel(p.blue(), a),
            a,
        ];
        if px == prev {
            run += 1;
            if run == 62 {
                out.push(0xc0 | (run - 1));
                run = 0;
            }
            continue;
        }
        if run > 0 {
            out.push(0xc0 | (run - 1));
            run = 0;
        }
        let idx = qoi_hash(px);
        if index[idx] == px {
            out.push(idx as u8);
        } else {
            index[idx] = px;
            if px[3] == prev[3] {
                let dr = px[0] as i8 - prev[0] as i8;
                let dg = px[1] as i8 - prev[1] as i8;
                let db = px[2] as i8 - prev[2] as i8;
                if (-2..=1).contains(&dr) && (-2..=1).contains(&dg) && (-2..=1).contains(&db) {
                    out.push(0x40 | ((dr + 2) as u8) << 4 | ((dg + 2) as u8) << 2 | (db + 2) as u8);
                } else if (-32..=31).contains(&dg)
                    && (-8..=7).contains(&(dr - dg))
                    && (-8..=7).contains(&(db - dg))
                {
                    out.push(0x80 | (dg + 32) as u8);
                    out.push((((dr - dg) + 8) as u8) << 4 | ((db - dg) + 8) as u8);
                } else {
                    out.push(0xfe);
                    out.extend_from_slice(&px[0..3]);
                }
            } else {
                out.push(0xff);
                out.extend_from_slice(&px);
            }
        }
        prev = px;
    }
    if run > 0 {
        out.push(0xc0 | (run - 1));
    }
    out.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 1]); // end marker
    out
}

fn qoi_hash(px: [u8; 4]) -> usize {
    (px[0] as usize * 3 + px[1] as usize * 5 + px[2] as usize * 7 + px[3] as usize * 11) % 64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn surface_with_content() -> Surface {
        let mut s = Surface::new(4, 4, 1.0).unwrap();
        s.pixmap.fill(tiny_skia::Color::from_rgba8(255, 0, 0, 255));
        s
    }

    #[test]
    fn frame_sequence_and_freshness() {
        let mut s = surface_with_content();
        let seq1 = s.present(1, vec![]);
        assert_eq!(seq1, 1);
        assert_eq!(s.capture().unwrap().seq, 1);
        assert!(s.capture_fresh(0).is_ok());
        let err = s.capture_fresh(1).unwrap_err();
        assert_eq!(err.code(), "NO_FRESH_FRAME");
        let seq2 = s.present(2, vec![]);
        assert_eq!(seq2, 2);
        assert_eq!(s.capture_fresh(1).unwrap().seq, 2);
    }

    #[test]
    fn blank_vs_content_classification() {
        let mut s = Surface::new(8, 8, 1.0).unwrap();
        s.present(1, vec![]);
        assert_eq!(s.capture().unwrap().class, FrameClass::Empty);
        s.pixmap.fill(tiny_skia::Color::from_rgba8(0, 0, 0, 255));
        s.present(2, vec![]);
        assert_eq!(s.capture().unwrap().class, FrameClass::SingleColour);
        for y in 0..8 {
            for x in 0..8 {
                let c = tiny_skia::Color::from_rgba8((x * 20) as u8, (y * 20) as u8, 128, 255);
                let mut paint = tiny_skia::Paint::default();
                paint.set_color(c);
                s.pixmap.fill_rect(
                    tiny_skia::Rect::from_xywh(x as f32, y as f32, 1.0, 1.0).unwrap(),
                    &paint,
                    tiny_skia::Transform::identity(),
                    None,
                );
            }
        }
        s.present(3, vec![]);
        assert_eq!(s.capture().unwrap().class, FrameClass::Content);
    }

    #[test]
    fn content_hash_changes_with_pixels() {
        let mut s = surface_with_content();
        s.present(1, vec![]);
        let h1 = s.capture().unwrap().content_hash;
        s.pixmap.fill(tiny_skia::Color::from_rgba8(0, 255, 0, 255));
        s.present(2, vec![]);
        let h2 = s.capture().unwrap().content_hash;
        assert_ne!(h1, h2);
    }

    #[test]
    fn png_and_qoi_headers() {
        let mut s = surface_with_content();
        s.present(1, vec![]);
        let f = s.capture().unwrap();
        let png = f.png(PngSetting::Fast);
        assert_eq!(&png[0..8], &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]);
        let qoi = f.qoi();
        assert_eq!(&qoi[0..4], b"qoif");
        // Decode the PNG back to verify.
        let decoded = tiny_skia::Pixmap::decode_png(&png).unwrap();
        assert_eq!(decoded.pixel(0, 0).unwrap().red(), 255);
    }

    #[test]
    fn png_settings_all_produce_valid_images() {
        let mut s = surface_with_content();
        s.present(1, vec![]);
        let f = s.capture().unwrap();
        let mut sizes = Vec::new();
        for setting in PngSetting::all() {
            let bytes = f.png(setting);
            assert!(bytes.len() > 8);
            let decoded = tiny_skia::Pixmap::decode_png(&bytes).unwrap();
            assert_eq!((decoded.width(), decoded.height()), (4, 4));
            sizes.push(bytes.len());
        }
    }
}
