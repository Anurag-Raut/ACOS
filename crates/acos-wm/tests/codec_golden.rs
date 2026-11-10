//! Golden Parcel bytes and round-trip stability for every WMS codec.
//!
//! The explicit byte vectors are computed from the DEX-derived field order (see
//! `results/wm/parcelable_layouts.txt`); the round-trip assertions pin that
//! decoding and re-encoding is byte-identical.

use acos_parcel::Parcel;
use acos_wm::codec::{
    ClientWindowFrames, Configuration, Cutout, InsetsSource, LayoutParams,
    MergedConfiguration, ParcelCodec, PrivacyIndicatorBounds, RoundedCorners, WindowConfiguration,
};
use acos_wm::display::DisplaySpec;
use acos_wm::geom::{InsetsI, PointI, RectI};
use acos_wm::wms::{empty_insets_state, reference_insets_state};

fn encode<T: ParcelCodec>(v: &T) -> Vec<u8> {
    let mut p = Parcel::new();
    v.write_to_parcel(&mut p).unwrap();
    p.marshall()
}

fn roundtrip<T: ParcelCodec + PartialEq + std::fmt::Debug>(v: T) {
    let bytes = encode(&v);
    let mut p = Parcel::new();
    p.unmarshall(&bytes);
    let back = T::read_from_parcel(&mut p).unwrap();
    assert_eq!(back, v, "{} round trip", T::NAME);
    p.enforce_no_data_avail().unwrap();
    assert_eq!(encode(&back), bytes, "{} re-encode", T::NAME);
}

#[test]
fn rect_golden_bytes() {
    let r = RectI::new(0, 128, 1080, 2337);
    let bytes = encode(&r);
    assert_eq!(
        bytes,
        vec![
            0x00, 0x00, 0x00, 0x00, // left 0
            0x80, 0x00, 0x00, 0x00, // top 128
            0x38, 0x04, 0x00, 0x00, // right 1080
            0x21, 0x09, 0x00, 0x00, // bottom 2337
        ]
    );
    roundtrip(r);
}

#[test]
fn point_and_insets_golden_bytes() {
    assert_eq!(
        encode(&PointI::new(0, 2274)),
        vec![0, 0, 0, 0, 0xe2, 0x08, 0, 0]
    );
    assert_eq!(
        encode(&InsetsI::new(0, 0, 0, 63)),
        vec![0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x3f, 0, 0, 0]
    );
    roundtrip(PointI::new(-3, 9));
    roundtrip(InsetsI::new(1, 2, 3, 4));
}

#[test]
fn client_window_frames_golden_layout() {
    let f = ClientWindowFrames {
        frame: RectI::new(0, 128, 1080, 2337),
        display_frame: RectI::new(0, 0, 1080, 2400),
        parent_frame: RectI::new(0, 128, 1080, 2337),
        attached_frame: None,
        is_parent_frame_clipped_by_display_cutout: false,
        compat_scale: 1.0,
    };
    let bytes = encode(&f);
    // 3 rects (48) + attached marker (4) + bool (4) + float (4).
    assert_eq!(bytes.len(), 60);
    assert_eq!(&bytes[48..52], &0i32.to_le_bytes());
    assert_eq!(&bytes[52..56], &0i32.to_le_bytes());
    assert_eq!(&bytes[56..60], &1.0f32.to_le_bytes());
    roundtrip(f);
    roundtrip(ClientWindowFrames {
        attached_frame: Some(RectI::new(1, 2, 3, 4)),
        ..ClientWindowFrames {
            frame: RectI::default(),
            display_frame: RectI::default(),
            parent_frame: RectI::default(),
            attached_frame: None,
            is_parent_frame_clipped_by_display_cutout: true,
            compat_scale: 0.5,
        }
    });
}

#[test]
fn configuration_round_trips() {
    let dc = DisplaySpec::reference_1080x2400();
    let cfg = Configuration {
        font_scale: 1.0,
        mcc: 310,
        mnc: 260,
        locale_list: Some("en-US".into()),
        user_set_locale: false,
        touchscreen: 3,
        keyboard: 2,
        keyboard_hidden: 1,
        hard_keyboard_hidden: 1,
        navigation: 1,
        navigation_hidden: 1,
        orientation: 1,
        screen_layout: 0x1a,
        color_mode: 0,
        ui_mode: 0x11,
        screen_width_dp: 411,
        screen_height_dp: 842,
        smallest_screen_width_dp: 411,
        density_dpi: 420,
        compat_screen_width_dp: 411,
        compat_screen_height_dp: 842,
        compat_smallest_screen_width_dp: 411,
        window_configuration: WindowConfiguration {
            bounds: dc.frame,
            app_bounds: Some(dc.app_bounds),
            max_bounds: dc.frame,
            windowing_mode: 1,
            activity_type: 0,
            always_on_top: 0,
            rotation: 0,
            display_windowing_mode: 1,
            display_rotation: 0,
        },
        assets_seq: 0,
        seq: 1,
        font_weight_adjustment: 0,
        grammatical_gender: 0,
    };
    roundtrip(cfg.clone());
    roundtrip(MergedConfiguration {
        global: cfg.clone(),
        override_config: cfg.clone(),
        merged: cfg,
    });
}

#[test]
fn insets_state_round_trips_with_reference_sources() {
    let dc = DisplaySpec::reference_1080x2400();
    let state = reference_insets_state(&dc, false);
    assert_eq!(state.sources.len(), 10);
    assert!(matches!(state.display_cutout, Cutout::Present(_)));
    roundtrip(state.clone());
    // The IME source is hidden but carries its visible frame.
    let ime = state.sources.iter().find(|s| s.id == 3).unwrap();
    assert!(!ime.visible);
    assert_eq!(ime.visible_frame, Some(RectI::new(0, 2274, 1080, 2400)));
    // Status/nav insets are the measured 128/63.
    assert_eq!(state.visible_insets_for(0).top, 128);
    assert_eq!(state.visible_insets_for(1).bottom, 63);
    roundtrip(empty_insets_state(&dc));
}

#[test]
fn insets_state_ime_visible_round_trips() {
    let dc = DisplaySpec::reference_1080x2400();
    let state = reference_insets_state(&dc, true);
    roundtrip(state.clone());
    assert_eq!(state.visible_insets_for(13).bottom, 126);
}

#[test]
fn optional_codecs_round_trip() {
    roundtrip(RoundedCorners::None);
    roundtrip(PrivacyIndicatorBounds {
        static_bounds: vec![RectI::new(827, 0, 1043, 128)],
        rotation: 0,
    });
    roundtrip(InsetsSource {
        id: 3,
        type_: 13,
        frame: RectI::new(0, 0, 0, 0),
        visible_frame: Some(RectI::new(0, 2274, 1080, 2400)),
        visible: false,
        flags: 0,
        insets_rounded_corner_frame: false,
    });
}

#[test]
fn layout_params_full_round_trip() {
    let lp = LayoutParams::application("dev.acos.fixtures.helloviews", "MainActivity");
    let bytes = encode(&lp);
    assert!(bytes.len() > 200, "LayoutParams is large: {}", bytes.len());
    roundtrip(lp);
}

#[test]
fn display_cutout_marker_is_present_zero_or_minus_one() {
    let dc = DisplaySpec::reference_1080x2400();
    let present = encode(&reference_insets_state(&dc, false).display_cutout);
    assert_eq!(&present[0..4], &1i32.to_le_bytes());
    assert_eq!(&encode(&Cutout::NoCutout)[0..4], &0i32.to_le_bytes());
    assert_eq!(&encode(&Cutout::Null)[0..4], &(-1i32).to_le_bytes());
}

#[test]
fn derived_layout_file_exists_and_covers_every_codec() {
    let path = acos_wm::replay::workspace_root().join("results/wm/parcelable_layouts.txt");
    let text = std::fs::read_to_string(&path).expect("parcelable_layouts.txt");
    for class in [
        "Landroid/graphics/Rect;",
        "Landroid/window/ClientWindowFrames;",
        "Landroid/view/InsetsState;",
        "Landroid/view/SurfaceControl;",
        "Landroid/view/WindowManager$LayoutParams;",
        "Lcom/android/internal/inputmethod/InputBindResult;",
    ] {
        assert!(text.contains(class), "layout file missing {class}");
    }
}
