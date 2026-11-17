//! Nine-patch stretch-region rendering from a real `npTc` chunk (W78-A).
//!
//! The source is a 5x5 bitmap whose nine regions have distinct colours. With
//! two divs per axis, the fixed columns/rows keep their 2px size and only the
//! middle band stretches — the golden expectation below is what Android's
//! `Canvas.drawPatch` produces for this chunk.

use acos_render::{BitmapConfig, NinePatchChunk, Rect, RenderContext};

const RED: u32 = 0xffff_0000;
const GREEN: u32 = 0xff00_ff00;
const BLUE: u32 = 0xff00_00ff;
const YELLOW: u32 = 0xffff_ff00;
const WHITE: u32 = 0xffff_ffff;
const CYAN: u32 = 0xff00_ffff;
const MAGENTA: u32 = 0xffff_00ff;
const ORANGE: u32 = 0xffff_8800;
const BLACK: u32 = 0xff00_0000;

fn build_bitmap(ctx: &mut RenderContext) -> acos_render::Handle {
    let b = ctx.bitmap_create(5, 5, BitmapConfig::Rgba8888).unwrap();
    // Each of the nine regions is uniform: columns [0,2) [2,3) [3,5),
    // rows [0,2) [2,3) [3,5).
    for y in 0..5i32 {
        for x in 0..5i32 {
            let cx = if x < 2 {
                0
            } else if x == 2 {
                1
            } else {
                2
            };
            let cy = if y < 2 {
                0
            } else if y == 2 {
                1
            } else {
                2
            };
            let c = match (cx, cy) {
                (0, 0) => RED,
                (1, 0) => GREEN,
                (2, 0) => BLUE,
                (0, 1) => YELLOW,
                (1, 1) => WHITE,
                (2, 1) => CYAN,
                (0, 2) => MAGENTA,
                (1, 2) => ORANGE,
                _ => BLACK,
            };
            ctx.bitmap_set_pixel(b, x, y, c).unwrap();
        }
    }
    b
}

fn render(chunk: NinePatchChunk) -> acos_render::Frame {
    let mut ctx = RenderContext::new();
    let bmp = build_bitmap(&mut ctx);
    let surface = ctx.surface_create(16, 16, 1.0).unwrap();
    ctx.canvas_for_surface(surface).unwrap();
    ctx.canvas_draw_nine_patch(surface, bmp, chunk, Rect::new(2, 2, 13, 13), None, false)
        .unwrap();
    ctx.surface_present(surface, 1).unwrap();
    ctx.surface_capture(surface).unwrap().unwrap()
}

fn px(frame: &acos_render::Frame, x: u32, y: u32) -> (u8, u8, u8) {
    let p = frame.pixels.pixel(x, y).unwrap();
    (p.red(), p.green(), p.blue())
}

#[test]
fn nine_patch_stretches_only_the_middle_bands() {
    let chunk = NinePatchChunk {
        x_divs: vec![2, 3],
        y_divs: vec![2, 3],
        colors: Vec::new(),
        padding: [0; 4],
    };
    let frame = render(chunk);
    assert!(
        frame.unsupported_ops.is_empty(),
        "{:?}",
        frame.unsupported_ops
    );
    // Destination 2..13 => cuts [2,4,11,13] (fixed 2px, stretch 7px).
    // Corners keep their pixel colour.
    assert_eq!(px(&frame, 3, 3), (255, 0, 0)); // top-left
    assert_eq!(px(&frame, 12, 3), (0, 0, 255)); // top-right
    assert_eq!(px(&frame, 3, 12), (255, 0, 255)); // bottom-left
    assert_eq!(px(&frame, 12, 12), (0, 0, 0)); // bottom-right
                                               // Edges stretch.
    assert_eq!(px(&frame, 7, 3), (0, 255, 0)); // top middle
    assert_eq!(px(&frame, 3, 7), (255, 255, 0)); // left middle
    assert_eq!(px(&frame, 12, 7), (0, 255, 255)); // right middle
    assert_eq!(px(&frame, 7, 12), (255, 136, 0)); // bottom middle
                                                  // Centre stretches both ways.
    assert_eq!(px(&frame, 7, 7), (255, 255, 255));
    // Outside the destination is untouched (transparent black).
    assert_eq!(px(&frame, 1, 1), (0, 0, 0));
}

#[test]
fn nine_patch_chunk_round_trips_through_nptc_bytes() {
    let chunk = NinePatchChunk {
        x_divs: vec![2, 3],
        y_divs: vec![1, 4],
        colors: vec![0x9933_b5e5, 0xffff_ffff],
        padding: [0; 4],
    };
    let bytes = chunk.to_bytes();
    let parsed = NinePatchChunk::parse(&bytes).expect("parse npTc");
    assert_eq!(parsed, chunk);
    // A truncated chunk is rejected, never silently mis-stretched.
    assert!(NinePatchChunk::parse(&bytes[..16]).is_none());
}
