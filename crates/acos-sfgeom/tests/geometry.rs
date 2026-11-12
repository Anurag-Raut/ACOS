//! Geometry algebra tests against the vendored AOSP bodies. Each expected value names the
//! stock line it comes from (see third_party/aosp/PROVENANCE.md for the tag).

use acos_sfgeom::{compute, Display, LayerIn};

const ROT_90: u32 = 4;

#[test]
fn child_larger_than_parent_is_clipped_to_parent_bounds() {
    // Parent: 200x100 at (10,20). Parent geomLayerBounds = its buffer (LSB.cpp:982-983),
    // transformed by its local translate -> [10,20,210,120] (LSB.cpp:948, 988).
    let parent = LayerIn {
        id: 0,
        x: 10.0,
        y: 20.0,
        buffer_size: Some((200, 100)),
        ..Default::default()
    };
    // Child: 400x300 at (50,50) inside the parent. Its local transform is translate(50,50);
    // parent bounds are pulled back into child space with local.inverse() (LSB.cpp:980-981),
    // giving {0,0,400,300} intersect {-50,-50,150,50} = {0,0,150,50} (LSB.cpp:987).
    let child = LayerIn {
        id: 1,
        parent: Some(0),
        x: 50.0,
        y: 50.0,
        buffer_size: Some((400, 300)),
        ..Default::default()
    };

    let s = compute(&[parent, child], Display::new(1080, 1920));
    assert_eq!(s[0].geom_layer_bounds, [0.0, 0.0, 200.0, 100.0]);
    assert_eq!(s[0].transformed_bounds, [10.0, 20.0, 210.0, 120.0]);
    // The child keeps its own croppedBufferSize (RLS.cpp:447-479)...
    assert_eq!(s[1].cropped_buffer_size, [0, 0, 400, 300]);
    // ...but geomLayerBounds is clipped to the parent's frame in the child's space.
    assert_eq!(s[1].geom_layer_bounds, [0.0, 0.0, 150.0, 50.0]);
    // geom = parent translate(10,20) * child translate(50,50) = (60,70) (LSB.cpp:948).
    assert_eq!(s[1].transformed_bounds, [60.0, 70.0, 210.0, 120.0]);
}

#[test]
fn alpha_multiplies_down_the_tree_and_a_hidden_parent_hides_its_children() {
    let a = LayerIn {
        id: 0,
        alpha: 0.5,
        buffer_size: Some((100, 100)),
        ..Default::default()
    };
    let b = LayerIn {
        id: 1,
        parent: Some(0),
        alpha: 0.5,
        buffer_size: Some((100, 100)),
        ..Default::default()
    };
    let c = LayerIn {
        id: 2,
        parent: Some(1),
        alpha: 1.0,
        buffer_size: Some((100, 100)),
        ..Default::default()
    };

    let s = compute(&[a, b, c], Display::new(1080, 1920));
    // snapshot.color.a = parent.color.a * requested.color.a (LayerSnapshotBuilder.cpp:757).
    assert_eq!(s[0].alpha, 0.5);
    assert_eq!(s[1].alpha, 0.25);
    assert_eq!(s[2].alpha, 0.25);
    assert!(!s.iter().any(|s| s.hidden_by_policy));

    // Hidden-ness is inherited: parent.hidden || own hidden (LSB.cpp:717-719).
    let hidden_b = LayerIn { hidden: true, ..b };
    let s = compute(&[a, hidden_b, c], Display::new(1080, 1920));
    assert!(!s[0].hidden_by_policy);
    assert!(s[1].hidden_by_policy);
    assert!(
        s[2].hidden_by_policy,
        "a hidden parent must hide its children"
    );
    // Geometry is still computed for hidden layers (stock short-circuits at LSB.cpp:738-747;
    // B1a has no incremental update, so it always takes the full-recompute path).
    assert_eq!(s[2].alpha, 0.25);
}

#[test]
fn buffer_transform_rot_90_swaps_the_unrotated_buffer_size() {
    // getUnrotatedBufferSize swaps on ROT_90 (RLS.cpp:352-354), getBufferSize too
    // (RLS.cpp:455-459), while getBufferCrop returns the raw buffer bounds (RLS.cpp:481-494).
    let rotated = LayerIn {
        id: 0,
        buffer_size: Some((100, 50)),
        buffer_transform: ROT_90,
        ..Default::default()
    };
    let s = compute(&[rotated], Display::new(1080, 1920))[0];
    assert_eq!(s.cropped_buffer_size, [0, 0, 50, 100]);
    assert_eq!(s.buffer_crop, [0, 0, 100, 50]);
    assert_eq!(s.geom_layer_bounds, [0.0, 0.0, 50.0, 100.0]);

    // destinationFrame 100x100 over the 50x100 unrotated buffer -> sx = 2, sy = 1
    // (RLS.cpp:388-396).
    let scaled = LayerIn {
        destination_frame: Some([0, 0, 100, 100]),
        ..rotated
    };
    let s = compute(&[scaled], Display::new(1080, 1920))[0];
    assert_eq!(s.geom_transform[0], 2.0);
    assert_eq!(s.geom_transform[4], 1.0);
    assert_eq!(s.transformed_bounds, [0.0, 0.0, 100.0, 100.0]);

    // Without bufferTransform the unrotated size is the raw 100x50.
    let unrotated = LayerIn {
        buffer_transform: 0,
        ..rotated
    };
    let s = compute(&[unrotated], Display::new(1080, 1920))[0];
    assert_eq!(s.cropped_buffer_size, [0, 0, 100, 50]);
}

#[test]
fn transform_to_display_inverse_uses_the_display_rotation() {
    // transformToDisplayInverse swaps when the *display* has a 90-degree rotation
    // (RLS.cpp:356-359 and 461-466).
    let layer = LayerIn {
        id: 0,
        buffer_size: Some((100, 50)),
        transform_to_display_inverse: true,
        ..Default::default()
    };
    let rotated_display = compute(&[layer], Display::with_rotation(1920, 1080, ROT_90))[0];
    assert_eq!(rotated_display.cropped_buffer_size, [0, 0, 50, 100]);
    assert_eq!(rotated_display.geom_layer_bounds, [0.0, 0.0, 50.0, 100.0]);

    let upright_display = compute(&[layer], Display::new(1080, 1920))[0];
    assert_eq!(upright_display.cropped_buffer_size, [0, 0, 100, 50]);
}

#[test]
fn crop_is_intersected_with_buffer_size_and_buffer_crop_stays_distinct() {
    let layer = LayerIn {
        id: 0,
        buffer_size: Some((100, 80)),
        crop: Some([10, 10, 60, 50]),
        buffer_crop: Some([0, 0, 30, 20]),
        ..Default::default()
    };
    let s = compute(&[layer], Display::new(1080, 1920))[0];
    // getCroppedBufferSize intersects bufferSize with crop (RLS.cpp:471-479).
    assert_eq!(s.cropped_buffer_size, [10, 10, 60, 50]);
    // getBufferCrop intersects the raw buffer bounds with bufferCrop (RLS.cpp:481-494).
    assert_eq!(s.buffer_crop, [0, 0, 30, 20]);
    // updateLayerBounds clips the buffer rect by crop, not by bufferCrop (LSB.cpp:982-987).
    assert_eq!(s.geom_layer_bounds, [10.0, 10.0, 60.0, 50.0]);
    assert_eq!(s.transformed_bounds, [10.0, 10.0, 60.0, 50.0]);

    // Oversized crops never exceed the buffer: both get clipped by the buffer bounds.
    let oversized = LayerIn {
        crop: Some([0, 0, 200, 200]),
        buffer_crop: Some([0, 0, 200, 200]),
        ..layer
    };
    let s = compute(&[oversized], Display::new(1080, 1920))[0];
    assert_eq!(s.cropped_buffer_size, [0, 0, 100, 80]);
    assert_eq!(s.buffer_crop, [0, 0, 100, 80]);
    assert_eq!(s.geom_layer_bounds, [0.0, 0.0, 100.0, 80.0]);

    // With no buffer: the crop path falls back to the crop itself (RLS.cpp:474-476) and
    // bufferCrop to bufferCrop (RLS.cpp:490-491).
    let no_buffer = LayerIn {
        buffer_size: None,
        ..layer
    };
    let s = compute(&[no_buffer], Display::new(1080, 1920))[0];
    assert_eq!(s.cropped_buffer_size, [10, 10, 60, 50]);
    assert_eq!(s.buffer_crop, [0, 0, 30, 20]);
}

#[test]
fn draw_order_is_ascending_z_with_a_stable_id_tie_break() {
    // LayerHierarchy.cpp:28-38: z ascending, then id ascending (stock also compares
    // layerStack first; B1a has a single stack, so that term is constant).
    let layers = [
        LayerIn {
            id: 5,
            z: 1,
            ..Default::default()
        },
        LayerIn {
            id: 3,
            z: 1,
            ..Default::default()
        },
        LayerIn {
            id: 7,
            z: 0,
            ..Default::default()
        },
        LayerIn {
            id: 9,
            z: 2,
            ..Default::default()
        },
        LayerIn {
            id: 4,
            z: 1,
            ..Default::default()
        },
    ];
    let s = compute(&layers, Display::new(1080, 1920));
    // Draw order: z=0 id7, then z=1 by id 3,4,5, then z=2 id9.
    assert_eq!(
        s.iter().map(|s| (s.id, s.draw_order)).collect::<Vec<_>>(),
        vec![(5, 3), (3, 1), (7, 0), (9, 4), (4, 2)]
    );
    let mut sorted = s.clone();
    sorted.sort_by_key(|s| s.draw_order);
    assert_eq!(
        sorted.iter().map(|s| s.id).collect::<Vec<_>>(),
        vec![7, 3, 4, 5, 9]
    );
}

#[test]
fn zero_scale_matrix_is_flagged_invalid_and_reset_to_identity() {
    // LayerSnapshot::isTransformValid rejects det == 0 (LayerSnapshot.cpp:261-264);
    // updateLayerBounds resets the transform (LSB.cpp:950-963).
    let invalid = LayerIn {
        id: 0,
        x: 10.0,
        y: 10.0,
        matrix: [0.0, 0.0, 0.0, 0.0],
        ..Default::default()
    };
    let child = LayerIn {
        id: 1,
        parent: Some(0),
        buffer_size: Some((50, 50)),
        ..Default::default()
    };
    let s = compute(&[invalid, child], Display::new(1080, 1920));
    assert!(s[0].invalid_transform);
    // Reset to identity, including the translation (LSB.cpp:961).
    assert_eq!(
        s[0].geom_transform,
        [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0]
    );
    // Bounds are NaN because the local inverse has det 0 (LSB.cpp:980-981); stock has the
    // same order of operations. Only the flag is contractual.
    assert!(s[0].geom_layer_bounds.iter().all(|v| v.is_nan()));
    // A child of an invalid-transform parent is hidden by policy (LSB.cpp:717-719).
    assert!(s[1].hidden_by_policy);
    assert!(!s[1].invalid_transform);
}

#[test]
fn empty_input_is_an_empty_snapshot() {
    assert!(compute(&[], Display::new(1080, 1920)).is_empty());
}

#[test]
fn input_order_does_not_matter_and_a_parent_cycle_does_not_hang() {
    let parent = LayerIn {
        id: 0,
        x: 10.0,
        y: 20.0,
        buffer_size: Some((200, 100)),
        ..Default::default()
    };
    let child = LayerIn {
        id: 1,
        parent: Some(0),
        x: 50.0,
        y: 50.0,
        buffer_size: Some((400, 300)),
        ..Default::default()
    };
    let forward = compute(&[parent, child], Display::new(1080, 1920));
    let reversed = compute(&[child, parent], Display::new(1080, 1920));
    assert_eq!(reversed[0], forward[1]);
    assert_eq!(reversed[1], forward[0]);

    // Invalid hierarchy: 0 <-> 1. Stock cannot build it; B1a must stay deterministic and
    // finite rather than chasing the cycle.
    let a = LayerIn {
        id: 0,
        parent: Some(1),
        buffer_size: Some((10, 10)),
        ..Default::default()
    };
    let b = LayerIn {
        id: 1,
        parent: Some(0),
        buffer_size: Some((10, 10)),
        ..Default::default()
    };
    let s = compute(&[a, b], Display::new(1080, 1920));
    assert_eq!(s.len(), 2);
    assert!(s.iter().all(|s| s.hidden_by_policy));
    assert!(s
        .iter()
        .all(|s| s.transformed_bounds.iter().all(|v| v.is_finite())));
}
