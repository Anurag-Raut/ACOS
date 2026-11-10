//! Rasteriser frame-budget bench (W92-A).
//!
//! Renders a 1080x2400 frame of simple geometry through the software
//! rasteriser and asserts the ADR-0023 performance target: a full phone-sized
//! frame of flat geometry well under 100 ms in a release build.
//!
//! Run with `cargo bench -p acos-gles`. `harness = false`, so `main` runs the
//! measurement directly; the assertion is skipped for debug builds, where the
//! rasteriser is intentionally unoptimised.

use std::hint::black_box;
use std::time::Instant;

use acos_gles::raster::{project, triangle, Framebuffer, RasterState};

const WIDTH: u32 = 1080;
const HEIGHT: u32 = 2400;
/// Triangles to draw (a mix of a full-screen quad and a grid of small ones).
const TRIANGLES: usize = 130;

fn clip(pos: [i32; 2]) -> [f32; 4] {
    [
        pos[0] as f32 / WIDTH as f32 * 2.0 - 1.0,
        1.0 - pos[1] as f32 / HEIGHT as f32 * 2.0,
        0.0,
        1.0,
    ]
}

fn main() {
    let state = RasterState {
        viewport: (0, 0, WIDTH, HEIGHT),
        ..RasterState::default()
    };
    let mut fb = Framebuffer::new(WIDTH, HEIGHT);

    // Full-screen quad.
    let mut tris: Vec<[[f32; 4]; 3]> = vec![
        [
            [-1.0, -1.0, 0.0, 1.0],
            [1.0, -1.0, 0.0, 1.0],
            [-1.0, 1.0, 0.0, 1.0],
        ],
        [
            [-1.0, 1.0, 0.0, 1.0],
            [1.0, -1.0, 0.0, 1.0],
            [1.0, 1.0, 0.0, 1.0],
        ],
    ];
    // 8x8 grid of quads (~128 triangles) covering the screen.
    for gy in 0..8 {
        for gx in 0..8 {
            let x0 = gx * (WIDTH as i32 / 8);
            let y0 = gy * (HEIGHT as i32 / 8);
            let x1 = x0 + WIDTH as i32 / 8;
            let y1 = y0 + HEIGHT as i32 / 8;
            tris.push([clip([x0, y0]), clip([x1, y0]), clip([x0, y1])]);
            tris.push([clip([x0, y1]), clip([x1, y0]), clip([x1, y1])]);
        }
    }
    assert!(tris.len() >= TRIANGLES);

    // Warm up once.
    for t in &tris {
        let p = [
            project(t[0], vec![], &state),
            project(t[1], vec![], &state),
            project(t[2], vec![], &state),
        ];
        triangle(&mut fb, &state, [&p[0], &p[1], &p[2]], |_, c| {
            Some([c[0] / WIDTH as f32, 0.25, c[1] / HEIGHT as f32, 1.0])
        });
    }

    let rounds = 5;
    let start = Instant::now();
    for _ in 0..rounds {
        for t in &tris {
            let p = [
                project(t[0], vec![], &state),
                project(t[1], vec![], &state),
                project(t[2], vec![], &state),
            ];
            black_box(triangle(&mut fb, &state, [&p[0], &p[1], &p[2]], |_, c| {
                Some([c[0] / WIDTH as f32, 0.25, c[1] / HEIGHT as f32, 1.0])
            }));
        }
    }
    let elapsed = start.elapsed();
    let per_frame = elapsed.as_secs_f64() * 1000.0 / rounds as f64;
    println!(
        "acos-gles raster bench: {WIDTH}x{HEIGHT}, {} triangles, {per_frame:.2} ms/frame",
        tris.len()
    );
    let budget = 100.0;
    if cfg!(debug_assertions) {
        println!("(debug build: performance assertion skipped)");
    } else {
        assert!(
            per_frame < budget,
            "frame budget exceeded: {per_frame:.2} ms >= {budget} ms"
        );
    }
}
