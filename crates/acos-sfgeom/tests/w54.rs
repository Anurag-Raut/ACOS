//! W54 regression: the BLAST child's buffer width must not change where it lands.
//!
//! Rebuilt from SPIKE-B §5 / evidence/SPIKE-B/07-w54-arithmetic.txt. The expected
//! numbers come from the AOSP bodies now compiled into `cpp/sfgeom_kernel.cpp`:
//!
//!   destination-frame scale   RequestedLayerState.cpp:388-396
//!   parent*local composition  LayerSnapshotBuilder.cpp:948
//!   parent-bounds inverse     LayerSnapshotBuilder.cpp:980-981
//!   bufferSize / crop clip    LayerSnapshotBuilder.cpp:982-987
//!   transformedBounds         LayerSnapshotBuilder.cpp:988
//!   croppedBufferSize         RequestedLayerState.cpp:447-468, 471-479

use acos_sfgeom::{compute, Display, LayerIn};

fn w54_layers(buffer_width: u32) -> Vec<LayerIn> {
    let root = LayerIn {
        id: 0,
        x: 400.0,
        y: 200.0,
        buffer_size: Some((1080, 1920)),
        ..Default::default()
    };
    let window = LayerIn {
        id: 1,
        parent: Some(0),
        x: 57.0,
        y: 640.0,
        buffer_size: Some((466, 400)),
        ..Default::default()
    };
    let blast = LayerIn {
        id: 2,
        parent: Some(1),
        destination_frame: Some([0, 0, 466, 400]),
        buffer_size: Some((buffer_width, 400)),
        ..Default::default()
    };
    vec![root, window, blast]
}

#[test]
fn destination_frame_rescales_the_buffer_and_bounds_stay_put() {
    for (buffer_width, expected_sx) in [(776u32, 466.0f32 / 776.0f32), (1024, 466.0f32 / 1024.0f32)]
    {
        let snapshots = compute(&w54_layers(buffer_width), Display::new(1080, 1920));
        assert_eq!(snapshots.len(), 3);

        // Root window layer: local translate only (RequestedLayerState.cpp:364).
        assert_eq!(
            snapshots[0].geom_transform,
            [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 400.0, 200.0, 1.0]
        );
        assert_eq!(
            snapshots[0].transformed_bounds,
            [400.0, 200.0, 1480.0, 2120.0]
        );

        // Dialog window at (57,640): composed origin 400+57, 200+640 (LSB.cpp:948).
        assert_eq!(
            snapshots[1].geom_transform,
            [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 457.0, 840.0, 1.0]
        );
        assert_eq!(
            snapshots[1].transformed_bounds,
            [457.0, 840.0, 923.0, 1240.0]
        );
        assert_eq!(snapshots[1].geom_layer_bounds, [0.0, 0.0, 466.0, 400.0]);

        // BLAST child with destinationFrame 466x400: sx = 466/bufferWidth (RLS.cpp:392-396),
        // sy = 400/400, no translation of its own. geom[] is column-major,
        // so index 0 is dsdx and 6/7 are tx/ty (Transform.cpp:103-108, 229-236).
        assert_eq!(snapshots[2].geom_transform[0], expected_sx);
        assert_eq!(snapshots[2].geom_transform[4], 1.0);
        assert_eq!(snapshots[2].geom_transform[6], 457.0);
        assert_eq!(snapshots[2].geom_transform[7], 840.0);

        // The whole point: the unrotated buffer size is the clip space (LSB.cpp:982-983),
        // and the destination frame maps it back onto 466x400 (LSB.cpp:988).
        assert_eq!(
            snapshots[2].geom_layer_bounds,
            [0.0, 0.0, buffer_width as f32, 400.0]
        );
        assert_eq!(
            snapshots[2].transformed_bounds,
            [457.0, 840.0, 923.0, 1240.0]
        );
        // Note: 923.0 is exact in f32 for both widths because 776 * (466/776) and
        // 1024 * (466/1024) round to exactly 466 on x86-64 SSE.
        assert_eq!(
            snapshots[2].cropped_buffer_size,
            [0, 0, buffer_width as i32, 400]
        );
        assert_eq!(snapshots[2].transformed_bounds[2], 923.0);
        assert_eq!(snapshots[2].transformed_bounds[3], 1240.0);

        assert_eq!(
            snapshots.iter().map(|s| s.draw_order).collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
    }
}

#[test]
fn the_dialog_rectangle_is_invariant_to_buffer_width() {
    let narrow = compute(&w54_layers(776), Display::new(1080, 1920));
    let wide = compute(&w54_layers(1024), Display::new(1080, 1920));
    assert_ne!(narrow[2].geom_transform[0], wide[2].geom_transform[0]);
    assert_eq!(narrow[2].transformed_bounds, wide[2].transformed_bounds);
}
