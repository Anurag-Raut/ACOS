//! Property tests for the damage-aware present API.
//!
//! The frame contract says `content_hash` is over the **whole** buffer. These
//! tests pin the two properties that make `present_damage` safe:
//!
//! 1. a damage-limited present reports exactly the same `content_hash` as a full
//!    `present()` of the same pixels (>= 500 random damage sequences), and
//! 2. a [`Frame`] that was already handed out stays byte-for-byte immutable
//!    across later presents even though the surface reuses its backing buffers.

use acos_render::{Rect, Surface};
use proptest::prelude::*;

/// Write `px` into every pixel of `rect` (clamped) in both the surface working
/// buffer and the reference shadow buffer, mirroring what a caller that knows
/// its damage would do.
fn paint(surface: &mut Surface, shadow: &mut [u8], w: u32, h: u32, rect: Rect, px: [u8; 4]) {
    let l = rect.left.clamp(0, w as i32) as u32;
    let t = rect.top.clamp(0, h as i32) as u32;
    let r = rect.right.clamp(0, w as i32) as u32;
    let b = rect.bottom.clamp(0, h as i32) as u32;
    if l >= r || t >= b {
        return;
    }
    let data = surface.pixmap_mut().data_mut();
    for y in t..b {
        for x in l..r {
            let off = ((y * w + x) * 4) as usize;
            data[off..off + 4].copy_from_slice(&px);
            shadow[off..off + 4].copy_from_slice(&px);
        }
    }
}

/// The hash a full `present()` reports for `shadow`, via a throwaway surface.
fn full_hash(w: u32, h: u32, shadow: &[u8]) -> u128 {
    let mut reference = Surface::new(w, h, 1.0).unwrap();
    reference.pixmap_mut().data_mut().copy_from_slice(shadow);
    reference.present(0, Vec::new());
    reference.capture().unwrap().content_hash
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(600))]
    #[test]
    fn present_damage_hash_equals_full_present(
        w in 1u32..48,
        h in 1u32..48,
        ops in prop::collection::vec(
            (0i32..64, 0i32..64, 0i32..64, 0i32..64, any::<[u8; 4]>()),
            0..16,
        ),
    ) {
        let mut surface = Surface::new(w, h, 1.0).unwrap();
        let mut shadow = vec![0u8; (w * h * 4) as usize];
        for (i, (l, t, r, b, px)) in ops.iter().enumerate() {
            let damage = Rect::new(*l, *t, *r, *b);
            paint(&mut surface, &mut shadow, w, h, damage, *px);
            surface.present_damage(i as i64, Some(damage), Vec::new());
            let got = surface.capture().unwrap().content_hash;
            let want = full_hash(w, h, &shadow);
            prop_assert_eq!(got, want, "hash mismatch after damage {:?}", damage);
        }
    }

    /// A `None` damage present must also agree with a full present, and an empty
    /// or out-of-bounds rect must leave the hash untouched.
    #[test]
    fn none_damage_and_empty_damage_agree(
        w in 1u32..40,
        h in 1u32..40,
        seed in any::<u32>(),
        l in -8i32..60,
        t in -8i32..60,
    ) {
        let mut surface = Surface::new(w, h, 1.0).unwrap();
        let mut shadow = vec![0u8; (w * h * 4) as usize];
        let px = seed.to_le_bytes();
        let damage = Rect::new(0, 0, w as i32, h as i32);
        paint(&mut surface, &mut shadow, w, h, damage, px);
        surface.present_damage(1, Some(damage), Vec::new());
        let full = full_hash(w, h, &shadow);

        // Empty / out-of-bounds damage: nothing changed, hash must not move.
        let empty = Rect::new(l, t, l, t);
        surface.present_damage(2, Some(empty), Vec::new());
        prop_assert_eq!(surface.capture().unwrap().content_hash, full);

        // `None` (whole frame): same pixels, same hash.
        surface.present_damage(3, None, Vec::new());
        prop_assert_eq!(surface.capture().unwrap().content_hash, full);
    }
}

#[test]
fn captured_frame_stays_immutable_across_presents() {
    let mut surface = Surface::new(16, 12, 1.0).unwrap();
    for (i, b) in surface.pixmap_mut().data_mut().iter_mut().enumerate() {
        *b = (i as u8).wrapping_mul(7);
    }
    surface.present(1, Vec::new());
    let first = surface.capture().unwrap();
    let before = first.pixels.data().to_vec();

    // Overwrite the whole working buffer and present twice, so the surface
    // would reuse `first`'s backing buffer if it did not copy-on-write.
    for i in 0..4u8 {
        surface.pixmap_mut().data_mut().fill(i);
        surface.present(i as i64 + 2, Vec::new());
    }

    assert_eq!(
        first.pixels.data(),
        before.as_slice(),
        "a handed-out frame was mutated by a later present"
    );
}

#[test]
fn present_damage_increments_seq_and_propagates_cause() {
    let mut surface = Surface::new(8, 8, 1.0).unwrap();
    let seq1 = surface.present_damage(41, Some(Rect::new(0, 0, 4, 4)), vec!["x".into()]);
    assert_eq!(seq1, 1);
    let f1 = surface.capture().unwrap();
    assert_eq!(f1.cause_id, 41);
    assert_eq!(f1.unsupported_ops, vec!["x".to_string()]);
    assert!(surface.capture_fresh(0).is_ok());
    assert_eq!(
        surface.capture_fresh(1).unwrap_err().code(),
        "NO_FRESH_FRAME"
    );
    let seq2 = surface.present_damage(42, Some(Rect::new(1, 1, 2, 2)), Vec::new());
    assert_eq!(seq2, 2);
    assert_eq!(surface.capture_fresh(1).unwrap().cause_id, 42);
}
