//! Pure-Rust image decoding for `BitmapFactory`-style ingest.
//!
//! Supports PNG, JPEG and WebP. All decoders return premultiplied RGBA8 words
//! (the renderer's canonical bitmap format) plus dimensions.

use crate::error::{RenderError, RenderResult};

/// A decoded image: premultiplied RGBA8, `[r,g,b,a]` byte order.
pub struct DecodedImage {
    pub width: u32,
    pub height: u32,
    /// One word per pixel, premultiplied RGBA in memory order `[r,g,b,a]`.
    pub pixels: Vec<u32>,
}

impl DecodedImage {
    fn from_rgba8(width: u32, height: u32, data: &[u8]) -> Self {
        let mut pixels = Vec::with_capacity((width as usize) * (height as usize));
        for px in data.chunks_exact(4) {
            pixels.push(crate::color::pack_premul(px[0], px[1], px[2], px[3]));
        }
        DecodedImage {
            width,
            height,
            pixels,
        }
    }

    fn from_rgb8(width: u32, height: u32, data: &[u8]) -> Self {
        let mut pixels = Vec::with_capacity((width as usize) * (height as usize));
        for px in data.chunks_exact(3) {
            pixels.push(crate::color::pack_premul(px[0], px[1], px[2], 255));
        }
        DecodedImage {
            width,
            height,
            pixels,
        }
    }

    fn from_l8(width: u32, height: u32, data: &[u8]) -> Self {
        let mut pixels = Vec::with_capacity((width as usize) * (height as usize));
        for &v in data {
            pixels.push(crate::color::pack_premul(v, v, v, 255));
        }
        DecodedImage {
            width,
            height,
            pixels,
        }
    }

    fn from_la8(width: u32, height: u32, data: &[u8]) -> Self {
        let mut pixels = Vec::with_capacity((width as usize) * (height as usize));
        for px in data.chunks_exact(2) {
            pixels.push(crate::color::pack_premul(px[0], px[0], px[0], px[1]));
        }
        DecodedImage {
            width,
            height,
            pixels,
        }
    }
}

/// Sniff the container format from the leading bytes.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum ImageFormat {
    Png,
    Jpeg,
    WebP,
    Unknown,
}

pub fn sniff(data: &[u8]) -> ImageFormat {
    if data.starts_with(&[0x89, b'P', b'N', b'G']) {
        ImageFormat::Png
    } else if data.starts_with(&[0xFF, 0xD8]) {
        ImageFormat::Jpeg
    } else if data.len() >= 12 && &data[0..4] == b"RIFF" && &data[8..12] == b"WEBP" {
        ImageFormat::WebP
    } else {
        ImageFormat::Unknown
    }
}

/// Decode any supported format.
pub fn decode(data: &[u8]) -> RenderResult<DecodedImage> {
    match sniff(data) {
        ImageFormat::Png => decode_png(data),
        ImageFormat::Jpeg => decode_jpeg(data),
        ImageFormat::WebP => decode_webp(data),
        ImageFormat::Unknown => Err(RenderError::Decode {
            msg: "unrecognised image format".to_string(),
        }),
    }
}

pub fn decode_png(data: &[u8]) -> RenderResult<DecodedImage> {
    let mut decoder = png::Decoder::new(data);
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder
        .read_info()
        .map_err(|e| RenderError::Decode {
            msg: format!("png header: {e}"),
        })?;
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let info = reader
        .next_frame(&mut buf)
        .map_err(|e| RenderError::Decode {
            msg: format!("png decode: {e}"),
        })?;
    let bytes = &buf[..info.buffer_size()];
    let (w, h) = (info.width, info.height);
    Ok(match info.color_type {
        png::ColorType::Rgba => DecodedImage::from_rgba8(w, h, bytes),
        png::ColorType::Rgb => DecodedImage::from_rgb8(w, h, bytes),
        png::ColorType::Grayscale => DecodedImage::from_l8(w, h, bytes),
        png::ColorType::GrayscaleAlpha => DecodedImage::from_la8(w, h, bytes),
        png::ColorType::Indexed => {
            return Err(RenderError::Decode {
                msg: "indexed PNG was not expanded".to_string(),
            })
        }
    })
}

pub fn decode_jpeg(data: &[u8]) -> RenderResult<DecodedImage> {
    let mut decoder = jpeg_decoder::Decoder::new(std::io::Cursor::new(data));
    let pixels = decoder.decode().map_err(|e| RenderError::Decode {
        msg: format!("jpeg decode: {e}"),
    })?;
    let info = decoder.info().ok_or_else(|| RenderError::Decode {
        msg: "jpeg has no frame info".to_string(),
    })?;
    let (w, h) = (info.width as u32, info.height as u32);
    Ok(match info.pixel_format {
        jpeg_decoder::PixelFormat::RGB24 => DecodedImage::from_rgb8(w, h, &pixels),
        jpeg_decoder::PixelFormat::L8 => DecodedImage::from_l8(w, h, &pixels),
        jpeg_decoder::PixelFormat::L16 => {
            // Down-sample 16-bit grayscale to 8-bit.
            let g: Vec<u8> = pixels.chunks_exact(2).map(|c| c[0]).collect();
            DecodedImage::from_l8(w, h, &g)
        }
        jpeg_decoder::PixelFormat::CMYK32 => {
            let mut rgba = Vec::with_capacity((w as usize) * (h as usize) * 4);
            for px in pixels.chunks_exact(4) {
                let c = px[0] as f32 / 255.0;
                let m = px[1] as f32 / 255.0;
                let y = px[2] as f32 / 255.0;
                let k = px[3] as f32 / 255.0;
                let r = (255.0 * (1.0 - c) * (1.0 - k)) as u8;
                let g = (255.0 * (1.0 - m) * (1.0 - k)) as u8;
                let b = (255.0 * (1.0 - y) * (1.0 - k)) as u8;
                rgba.extend_from_slice(&[r, g, b, 255]);
            }
            DecodedImage::from_rgba8(w, h, &rgba)
        }
    })
}

pub fn decode_webp(data: &[u8]) -> RenderResult<DecodedImage> {
    let mut decoder = image_webp::WebPDecoder::new(std::io::Cursor::new(data)).map_err(|e| {
        RenderError::Decode {
            msg: format!("webp header: {e}"),
        }
    })?;
    let (w, h) = decoder.dimensions();
    let size = decoder.output_buffer_size().ok_or_else(|| RenderError::Size {
        msg: "webp image too large".to_string(),
    })?;
    let mut buf = vec![0u8; size];
    decoder
        .read_image(&mut buf)
        .map_err(|e| RenderError::Decode {
            msg: format!("webp decode: {e}"),
        })?;
    let pixel_count = (w as usize) * (h as usize);
    if buf.len() >= pixel_count * 4 {
        Ok(DecodedImage::from_rgba8(w, h, &buf))
    } else if buf.len() >= pixel_count * 3 {
        Ok(DecodedImage::from_rgb8(w, h, &buf))
    } else {
        Err(RenderError::Decode {
            msg: format!("webp produced {} bytes for {w}x{h}", buf.len()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sniff_formats() {
        assert_eq!(sniff(&[0x89, b'P', b'N', b'G', 0, 0]), ImageFormat::Png);
        assert_eq!(sniff(&[0xFF, 0xD8, 0xFF]), ImageFormat::Jpeg);
        assert_eq!(sniff(b"RIFF\0\0\0\0WEBPVP8 "), ImageFormat::WebP);
        assert_eq!(sniff(b"nope"), ImageFormat::Unknown);
    }

    #[test]
    fn decode_a_tiny_png_round_trips() {
        // Build a 2x2 RGBA PNG in memory using the png encoder.
        let mut buf = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut buf, 2, 2);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            let mut writer = enc.write_header().unwrap();
            writer
                .write_image_data(&[
                    255, 0, 0, 255, // red
                    0, 255, 0, 255, // green
                    0, 0, 255, 255, // blue
                    0, 0, 0, 0, // transparent
                ])
                .unwrap();
        }
        let img = decode_png(&buf).unwrap();
        assert_eq!((img.width, img.height), (2, 2));
        assert_eq!(crate::color::unpack_unpremul(img.pixels[0]), (255, 0, 0, 255));
        assert_eq!(crate::color::unpack_unpremul(img.pixels[1]), (0, 255, 0, 255));
        assert_eq!(crate::color::unpack_unpremul(img.pixels[2]), (0, 0, 255, 255));
        assert_eq!(crate::color::unpack_unpremul(img.pixels[3]).3, 0);
    }
}
