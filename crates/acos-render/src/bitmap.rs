//! `android.graphics.Bitmap` — canonical premultiplied RGBA8 storage with
//! `ALPHA_8`/`RGB_565` conversions at the API edges.

use crate::color::{
    argb_to_rgba, pack_premul, premultiply_channel, unpack_unpremul,
};
use crate::error::{RenderError, RenderResult};
use crate::image::DecodedImage;
use tiny_skia::Pixmap;

/// Android `Bitmap.Config` ordinals (API 34).
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum BitmapConfig {
    Alpha8 = 1,
    Rgb565 = 2,
    Argb4444 = 3,
    RgbaF16 = 4,
    Rgba8888 = 5,
    Rgba1010102 = 6,
    Rgbx8888 = 7,
    Gray8 = 8,
}

impl BitmapConfig {
    pub fn from_ordinal(o: i32) -> Option<Self> {
        Some(match o {
            1 => BitmapConfig::Alpha8,
            2 => BitmapConfig::Rgb565,
            3 => BitmapConfig::Argb4444,
            4 => BitmapConfig::RgbaF16,
            5 => BitmapConfig::Rgba8888,
            6 => BitmapConfig::Rgba1010102,
            7 => BitmapConfig::Rgbx8888,
            8 => BitmapConfig::Gray8,
            _ => return None,
        })
    }

    /// Bytes per pixel in Android's native allocation.
    pub fn bytes_per_pixel(self) -> usize {
        match self {
            BitmapConfig::Alpha8 | BitmapConfig::Gray8 => 1,
            BitmapConfig::Rgb565 | BitmapConfig::Argb4444 => 2,
            BitmapConfig::Rgba8888 | BitmapConfig::Rgbx8888 | BitmapConfig::Rgba1010102 => 4,
            BitmapConfig::RgbaF16 => 8,
        }
    }

    /// Whether the config carries an alpha channel.
    pub fn has_alpha_channel(self) -> bool {
        matches!(
            self,
            BitmapConfig::Alpha8
                | BitmapConfig::Argb4444
                | BitmapConfig::RgbaF16
                | BitmapConfig::Rgba8888
                | BitmapConfig::Rgba1010102
        )
    }
}

#[inline]
fn quantize_565(r: u8, g: u8, b: u8) -> (u8, u8, u8) {
    let r5 = (r >> 3) << 3;
    let g6 = (g >> 2) << 2;
    let b5 = (b >> 3) << 3;
    (r5 | (r5 >> 5), g6 | (g6 >> 6), b5 | (b5 >> 5))
}

#[inline]
fn quantize_4444(c: u8) -> u8 {
    let v = (c >> 4) << 4;
    v | (v >> 4)
}

/// An offscreen bitmap.
#[derive(Clone)]
pub struct Bitmap {
    width: u32,
    height: u32,
    config: BitmapConfig,
    /// Canonical premultiplied RGBA8, memory order `[r,g,b,a]`.
    pixels: Vec<u32>,
    has_alpha: bool,
    premultiplied: bool,
    immutable: bool,
    generation_id: i32,
}

impl Bitmap {
    pub fn new(width: u32, height: u32, config: BitmapConfig) -> RenderResult<Self> {
        if width == 0 || height == 0 {
            return Err(RenderError::size("bitmap dimensions must be non-zero"));
        }
        let area = (width as usize) * (height as usize);
        if area > 256 * 1024 * 1024 {
            return Err(RenderError::size(format!("bitmap {width}x{height} too large")));
        }
        Ok(Bitmap {
            width,
            height,
            config,
            pixels: vec![0u32; area],
            has_alpha: config.has_alpha_channel(),
            premultiplied: true,
            immutable: false,
            generation_id: 1,
        })
    }

    pub fn from_decoded(img: DecodedImage) -> RenderResult<Self> {
        Ok(Bitmap {
            width: img.width,
            height: img.height,
            config: BitmapConfig::Rgba8888,
            pixels: img.pixels,
            has_alpha: true,
            premultiplied: true,
            immutable: false,
            generation_id: 1,
        })
    }

    pub fn from_pixmap(pixmap: &Pixmap) -> Self {
        let mut pixels: Vec<u32> = Vec::with_capacity((pixmap.width() * pixmap.height()) as usize);
        for p in pixmap.pixels() {
            pixels.push(
                (p.red() as u32)
                    | ((p.green() as u32) << 8)
                    | ((p.blue() as u32) << 16)
                    | ((p.alpha() as u32) << 24),
            );
        }
        Bitmap {
            width: pixmap.width(),
            height: pixmap.height(),
            config: BitmapConfig::Rgba8888,
            pixels,
            has_alpha: true,
            premultiplied: true,
            immutable: false,
            generation_id: 1,
        }
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn config(&self) -> BitmapConfig {
        self.config
    }

    pub fn row_bytes(&self) -> usize {
        self.width as usize * 4
    }

    pub fn allocation_byte_count(&self) -> usize {
        self.width as usize * self.height as usize * self.config.bytes_per_pixel()
    }

    pub fn has_alpha(&self) -> bool {
        self.has_alpha
    }

    pub fn set_has_alpha(&mut self, v: bool) {
        self.has_alpha = v;
    }

    pub fn is_premultiplied(&self) -> bool {
        self.premultiplied
    }

    pub fn set_premultiplied(&mut self, v: bool) {
        self.premultiplied = v;
    }

    pub fn is_immutable(&self) -> bool {
        self.immutable
    }

    pub fn set_immutable(&mut self) {
        self.immutable = true;
    }

    pub fn generation_id(&self) -> i32 {
        self.generation_id
    }

    fn bump(&mut self) {
        self.generation_id = self.generation_id.wrapping_add(1).max(1);
    }

    pub fn pixels(&self) -> &[u32] {
        &self.pixels
    }

    pub fn pixels_mut(&mut self) -> &mut [u32] {
        self.bump();
        &mut self.pixels
    }

    /// Android `Bitmap.eraseColor(argb)`.
    pub fn erase(&mut self, argb: u32) {
        let (r, g, b, a) = argb_to_rgba(argb);
        let word = self.encode(r, g, b, a);
        self.pixels.fill(word);
        self.bump();
    }

    /// Android `Bitmap.getPixel(x, y)` — un-premultiplied ARGB.
    pub fn get_pixel(&self, x: i32, y: i32) -> u32 {
        if x < 0 || y < 0 || x >= self.width as i32 || y >= self.height as i32 {
            return 0;
        }
        let word = self.pixels[y as usize * self.width as usize + x as usize];
        match self.config {
            BitmapConfig::Alpha8 => {
                let a = (word >> 24) as u8;
                (a as u32) << 24
            }
            _ => {
                let (r, g, b, a) = unpack_unpremul(word);
                ((a as u32) << 24) | ((r as u32) << 16) | ((g as u32) << 8) | b as u32
            }
        }
    }

    /// Android `Bitmap.setPixel(x, y, argb)`.
    pub fn set_pixel(&mut self, x: i32, y: i32, argb: u32) {
        if x < 0 || y < 0 || x >= self.width as i32 || y >= self.height as i32 {
            return;
        }
        let (r, g, b, a) = argb_to_rgba(argb);
        let word = self.encode(r, g, b, a);
        let idx = y as usize * self.width as usize + x as usize;
        self.pixels[idx] = word;
        self.bump();
    }

    fn encode(&self, r: u8, g: u8, b: u8, a: u8) -> u32 {
        match self.config {
            BitmapConfig::Alpha8 | BitmapConfig::Gray8 => {
                // Alpha-only: RGB is undefined; store premultiplied white.
                let a = if self.config == BitmapConfig::Alpha8 { a } else { r };
                pack_premul(a, a, a, a)
            }
            BitmapConfig::Rgb565 => {
                let (r, g, b) = quantize_565(r, g, b);
                pack_premul(r, g, b, 255)
            }
            BitmapConfig::Argb4444 => {
                let (r, g, b, a) = (quantize_4444(r), quantize_4444(g), quantize_4444(b), quantize_4444(a));
                pack_premul(r, g, b, a)
            }
            BitmapConfig::Rgbx8888 => pack_premul(r, g, b, 255),
            BitmapConfig::RgbaF16 | BitmapConfig::Rgba1010102 | BitmapConfig::Rgba8888 => {
                pack_premul(r, g, b, a)
            }
        }
    }

    /// Copy with a possibly different config.
    pub fn copy(&self, config: Option<BitmapConfig>, _mutable: bool) -> RenderResult<Bitmap> {
        let cfg = config.unwrap_or(self.config);
        let mut out = Bitmap::new(self.width, self.height, cfg)?;
        out.has_alpha = self.has_alpha;
        out.premultiplied = self.premultiplied;
        for y in 0..self.height as i32 {
            for x in 0..self.width as i32 {
                out.set_pixel(x, y, self.get_pixel(x, y));
            }
        }
        Ok(out)
    }

    /// Android `Bitmap.extractAlpha` (luminance → alpha).
    pub fn extract_alpha(&self) -> RenderResult<Bitmap> {
        let mut out = Bitmap::new(self.width, self.height, BitmapConfig::Alpha8)?;
        for (dst, &src) in out.pixels.iter_mut().zip(self.pixels.iter()) {
            let (r, g, b, a) = unpack_unpremul(src);
            let l = (0.299 * r as f32 + 0.587 * g as f32 + 0.114 * b as f32) as u8;
            let alpha = premultiply_channel(l, a);
            *dst = pack_premul(alpha, alpha, alpha, alpha);
        }
        Ok(out)
    }

    /// Convert to a tiny-skia pixmap (premultiplied RGBA8) for rasterisation.
    pub fn to_pixmap(&self) -> RenderResult<Pixmap> {
        let mut pixmap = Pixmap::new(self.width, self.height)
            .ok_or_else(|| RenderError::size("could not allocate pixmap"))?;
        {
            let dst = pixmap.pixels_mut();
            for (d, &s) in dst.iter_mut().zip(self.pixels.iter()) {
                *d = tiny_skia::PremultipliedColorU8::from_rgba(
                    (s & 0xff) as u8,
                    ((s >> 8) & 0xff) as u8,
                    ((s >> 16) & 0xff) as u8,
                    ((s >> 24) & 0xff) as u8,
                )
                .unwrap_or(tiny_skia::PremultipliedColorU8::TRANSPARENT);
            }
        }
        Ok(pixmap)
    }

    /// Replace contents from a tiny-skia pixmap.
    pub fn from_pixmap_mut(&mut self, pixmap: &Pixmap) {
        let n = (self.width * self.height) as usize;
        self.pixels.clear();
        self.pixels.reserve(n);
        for p in pixmap.pixels() {
            self.pixels.push(
                (p.red() as u32)
                    | ((p.green() as u32) << 8)
                    | ((p.blue() as u32) << 16)
                    | ((p.alpha() as u32) << 24),
            );
        }
        self.bump();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn erase_and_get_pixel_round_trip() {
        let mut b = Bitmap::new(4, 4, BitmapConfig::Rgba8888).unwrap();
        b.erase(0xff123456);
        assert_eq!(b.get_pixel(0, 0), 0xff123456);
        assert_eq!(b.get_pixel(3, 3), 0xff123456);
        assert_eq!(b.get_pixel(4, 4), 0);
    }

    #[test]
    fn set_pixel_bounds_are_safe() {
        let mut b = Bitmap::new(2, 2, BitmapConfig::Rgba8888).unwrap();
        b.set_pixel(-1, 0, 0xffffffff);
        b.set_pixel(100, 100, 0xffffffff);
        assert_eq!(b.get_pixel(0, 0), 0);
    }

    #[test]
    fn rgb565_quantises_channels() {
        let mut b = Bitmap::new(1, 1, BitmapConfig::Rgb565).unwrap();
        b.set_pixel(0, 0, 0xff123456);
        let got = b.get_pixel(0, 0);
        let (r, g, bl, _) = argb_to_rgba(got);
        // 5/6/5 bit precision: within 8/4/8 of the original, and idempotent.
        assert!((r as i32 - 0x12).abs() <= 8, "r={r}");
        assert!((g as i32 - 0x34).abs() <= 4, "g={g}");
        assert!((bl as i32 - 0x56).abs() <= 8, "b={bl}");
        let mut c = Bitmap::new(1, 1, BitmapConfig::Rgb565).unwrap();
        c.set_pixel(0, 0, got);
        assert_eq!(c.get_pixel(0, 0), got);
    }

    #[test]
    fn alpha8_stores_alpha() {
        let mut b = Bitmap::new(1, 1, BitmapConfig::Alpha8).unwrap();
        b.set_pixel(0, 0, 0x80402010);
        assert_eq!(b.get_pixel(0, 0), 0x80000000);
    }

    #[test]
    fn copy_preserves_pixels() {
        let mut b = Bitmap::new(3, 2, BitmapConfig::Rgba8888).unwrap();
        b.set_pixel(1, 1, 0xffabcdef);
        let c = b.copy(None, true).unwrap();
        assert_eq!(c.get_pixel(1, 1), 0xffabcdef);
    }

    #[test]
    fn pixmap_round_trip() {
        let mut b = Bitmap::new(3, 3, BitmapConfig::Rgba8888).unwrap();
        b.erase(0xff204060);
        let pm = b.to_pixmap().unwrap();
        let c = Bitmap::from_pixmap(&pm);
        assert_eq!(c.get_pixel(0, 0), 0xff204060);
    }
}
