//! Microbenchmark from the B1a spec: 3-layer snapshot geometry per transaction.
//! Run with: cargo test -p acos-sfgeom --release --test bench -- --ignored --nocapture

use std::time::Instant;

use acos_sfgeom::{compute, Display, LayerIn};

#[test]
#[ignore = "microbenchmark; run explicitly in release"]
fn three_layer_transaction_ns() {
    let display = Display::new(1080, 1920);
    let layers = [
        LayerIn {
            id: 0,
            x: 400.0,
            y: 200.0,
            buffer_size: Some((1080, 1920)),
            ..Default::default()
        },
        LayerIn {
            id: 1,
            parent: Some(0),
            x: 57.0,
            y: 640.0,
            buffer_size: Some((466, 400)),
            ..Default::default()
        },
        LayerIn {
            id: 2,
            parent: Some(1),
            destination_frame: Some([0, 0, 466, 400]),
            buffer_size: Some((1024, 400)),
            ..Default::default()
        },
    ];

    let iterations: u64 = 300_000;
    let mut sink = 0.0f32;
    for _ in 0..10_000 {
        sink += compute(&layers, display)[2].transformed_bounds[2];
    }

    let start = Instant::now();
    for _ in 0..iterations {
        let snapshots = compute(&layers, display);
        sink += snapshots[2].transformed_bounds[2] + snapshots[0].geom_transform[6];
    }
    let elapsed = start.elapsed();

    let ns_per_transaction = elapsed.as_nanos() as f64 / iterations as f64;
    println!(
        "3-layer compute: {ns_per_transaction:.1} ns/transaction ({:.1} ns/layer), sink={sink}",
        ns_per_transaction / 3.0
    );
    assert!(ns_per_transaction < 100_000.0);
}
