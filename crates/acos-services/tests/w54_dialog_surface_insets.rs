//! W54: a window's **surface** is `frame + surfaceInsets` and its layer sits at
//! `frame − surfaceInsets`; C1 clips the composite to the display viewport.
//!
//! Root cause this pins: the guest's own `ViewRootImpl` sizes its hardware
//! renderer to `width + insets` and translates the canvas by the insets
//! (`ThreadedRenderer.setup`, AOSP `ThreadedRenderer.java:515-535`, `:741`), so
//! the window manager must grow the `SurfaceControl` by the same insets and
//! place the layer that much up/left. ACOS sized the surface to the bare frame
//! and placed the layer at the frame origin, so an elevation-shadowed
//! `AlertDialog` (surfaceInsets 84 px per side from `setSurfaceInsets`'s
//! `ceil(view.getZ() * 2)`) was drawn 84 px into a buffer that was 168 px too
//! small: the card appeared at `surfaceInset + layoutInset = 126` px inside the
//! window frame and the right/bottom rounded corners were cut by the buffer
//! edge. The tree was correct (`[28,976][1052,1489]`) and the pixels were not.
//!
//! Teeth: the pre-fix window manager returns a 1024x513 surface and the pre-fix
//! composite has no display clip, so every assertion below is false on it.

use acos_services::compositor_client::DisplayDescriptor;
use acos_services::surface_client::{placed_rect, LayerPlacement};
use acos_wm::codec::LayoutParams;
use acos_wm::display::DisplaySpec;
use acos_wm::geom::RectI;
use acos_wm::wms::WindowManager;

/// The measured dialog: frame `[28,976][1052,1489]` (1024x513) and the
/// `surfaceInsets` the guest sent for it (`ceil(DecorView.getZ() * 2)` = 84).
const DIALOG_FRAME: RectI = RectI {
    left: 28,
    top: 976,
    right: 1052,
    bottom: 1489,
};
const SURFACE_INSETS: i32 = 84;

fn dialog_params() -> LayoutParams {
    LayoutParams {
        width: -2, // WRAP_CONTENT
        height: -2,
        type_: 2, // TYPE_APPLICATION
        surface_insets: RectI::new(
            SURFACE_INSETS,
            SURFACE_INSETS,
            SURFACE_INSETS,
            SURFACE_INSETS,
        ),
        ..LayoutParams::default()
    }
}

#[test]
fn window_surface_grows_by_surface_insets_and_the_layer_moves_by_them() {
    let mut wm = WindowManager::new(DisplaySpec::reference_1080x2400());
    let (sess, _binder) = wm.open_session(None);
    const WINDOW: u64 = 0x2222;
    wm.add_to_display_as_user(
        sess,
        acos_parcel::BinderId(WINDOW),
        dialog_params(),
        0,
    )
    .expect("dialog window is added");

    let r = wm
        .relayout_with_requested(sess, WINDOW, dialog_params(), 1024, 513, -1)
        .expect("relayout");

    // The frame the client lays out in is unchanged (the tree's bounds).
    assert_eq!(r.frames.frame, DIALOG_FRAME);

    // Stock `WindowState.getSurfaceWidth/Height`: frame + insets on both sides.
    assert_eq!(
        (r.surface.width, r.surface.height),
        (1024 + 2 * SURFACE_INSETS, 513 + 2 * SURFACE_INSETS),
        "the window surface must be the frame grown by the client's surface insets"
    );

    // The layer origin is the frame origin minus the insets; the client's canvas
    // is translated by +insets, so the window content lands on the frame.
    let placement = LayerPlacement {
        x: r.frames.frame.left - SURFACE_INSETS,
        y: r.frames.frame.top - SURFACE_INSETS,
        z: 2,
        alpha: 1.0,
        dim_amount: 0.6,
    };
    assert_eq!(
        (placement.x + SURFACE_INSETS, placement.y + SURFACE_INSETS),
        (DIALOG_FRAME.left, DIALOG_FRAME.top),
        "surface origin + insets is the window frame (SurfaceView.java:1180's algebra)"
    );

    // And the composite's display-space rectangle: the 1192x681 buffer at
    // (-56, 892) is clipped to the 1080-wide display, not to the frame.
    let display = DisplayDescriptor::reference();
    assert_eq!(
        placed_rect((0, 0, 1192, 681), &placement, display),
        Some((0, 892, 1080, 892 + 681)),
        "a surface-inset layer overhangs the display and is clipped to it"
    );
}

#[test]
fn composite_clips_a_layer_to_the_display_viewport() {
    let display = DisplayDescriptor::reference();
    let placement = LayerPlacement {
        x: -56,
        y: 892,
        z: 2,
        alpha: 1.0,
        dim_amount: 0.0,
    };
    // Entirely inside: unchanged (the pre-W54 behaviour, and the pass-through
    // case for a normal window).
    assert_eq!(
        placed_rect((0, 0, 1024, 513), &placement, display),
        Some((0, 892, 968, 1405))
    );
    // Entirely off the display: no rectangle, so it cannot grow the canvas.
    assert_eq!(
        placed_rect((0, 0, 1024, 513), &LayerPlacement { x: 2000, ..placement }, display),
        None
    );
    // The dialog card inside the surface: buffer (126,126) is what the client
    // drew — its position plus the insets is the window content origin.
    let card = (
        placement.x + 126,
        placement.y + 126,
        placement.x + 126 + 940,
        placement.y + 126 + 429,
    );
    assert_eq!(
        (card.0, card.1),
        (70, 1018),
        "card origin is the window frame + the dialog's 42 px layout inset"
    );
}
