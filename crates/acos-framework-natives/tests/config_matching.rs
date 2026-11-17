#![allow(clippy::field_reassign_with_default)]
//! Configuration-matching unit tests, one per qualifier.
//!
//! These encode the `ResTable_config::match` / `isBetterThan` semantics. The
//! density expectations were calibrated against `aapt2 optimize
//! --target-densities` on real resource tables (see `results/res/README.md`).

use acos_framework_natives::res::config::*;
use acos_framework_natives::res::ResConfig;

fn request() -> ResConfig {
    let mut c = ResConfig::default();
    c.language = *b"en";
    c.country = *b"US";
    c.density = 420;
    c.smallest_screen_width_dp = 411;
    c.screen_width_dp = 411;
    c.screen_height_dp = 842;
    c.orientation = ORIENTATION_PORT;
    c.ui_mode = MASK_UI_MODE_TYPE & 0x01 | UI_MODE_NIGHT_NO; // normal + notnight
    c.screen_layout = SCREENSIZE_NORMAL | LAYOUTDIR_LTR;
    c.sdk_version = 34;
    c.touchscreen = TOUCHSCREEN_FINGER;
    c.keyboard = KEYBOARD_QWERTY;
    c.navigation = NAVIGATION_DPAD;
    c
}

fn pick(candidates: &[ResConfig], req: &ResConfig) -> Option<ResConfig> {
    let mut best: Option<ResConfig> = None;
    for c in candidates {
        if !c.matches(req) {
            continue;
        }
        match &best {
            None => best = Some(c.clone()),
            Some(cur) => {
                if c.is_better_than(cur, req) {
                    best = Some(c.clone());
                }
            }
        }
    }
    best
}

fn with<F: FnOnce(&mut ResConfig)>(f: F) -> ResConfig {
    let mut c = ResConfig::default();
    f(&mut c);
    c
}

#[test]
fn locale_exact_beats_language_beats_default() {
    let req = request();
    let us = with(|c| {
        c.language = *b"en";
        c.country = *b"US";
    });
    let en = with(|c| c.language = *b"en");
    let fr = with(|c| c.language = *b"fr");
    let def = ResConfig::default();
    assert!(us.matches(&req) && en.matches(&req) && def.matches(&req));
    assert!(!fr.matches(&req));
    let winner = pick(&[def.clone(), en.clone(), us.clone()], &req).unwrap();
    assert_eq!(winner.language, *b"en");
    assert_eq!(winner.country, *b"US");
    // Without the country, `en` beats the default.
    let winner = pick(&[def.clone(), en.clone()], &req).unwrap();
    assert_eq!(winner.language, *b"en");
    assert_eq!(winner.country, [0, 0]);
}

#[test]
fn density_nearest_rule_calibrated_against_aapt2() {
    // (request, lower, higher, expected) from `aapt2 optimize --target-densities`.
    let cases = [
        (420u16, DENSITY_XHIGH, DENSITY_XXHIGH, DENSITY_XXHIGH),
        (420, DENSITY_XXHIGH, DENSITY_XXXHIGH, DENSITY_XXHIGH),
        (190, DENSITY_MEDIUM, DENSITY_HIGH, DENSITY_HIGH),
        (160, DENSITY_MEDIUM, DENSITY_HIGH, DENSITY_MEDIUM),
        (161, DENSITY_MEDIUM, DENSITY_HIGH, DENSITY_HIGH),
        (161, DENSITY_MEDIUM, DENSITY_XXXHIGH, DENSITY_XXXHIGH),
        (245, DENSITY_HIGH, DENSITY_XHIGH, DENSITY_XHIGH),
        (240, DENSITY_HIGH, DENSITY_XHIGH, DENSITY_HIGH),
        (300, DENSITY_HIGH, DENSITY_XHIGH, DENSITY_XHIGH),
        (640, DENSITY_XXXHIGH, DENSITY_XXHIGH, DENSITY_XXXHIGH),
    ];
    for (req_density, a, b, expected) in cases {
        let ca = with(|c| c.density = a);
        let cb = with(|c| c.density = b);
        let winner = pick(
            &[ca.clone(), cb.clone()],
            &with(|r| r.density = req_density),
        )
        .unwrap();
        assert_eq!(
            winner.density, expected,
            "request {req_density} of ({a},{b}) should pick {expected}"
        );
        // Symmetry: order must not matter.
        let winner2 = pick(&[cb, ca], &with(|r| r.density = req_density)).unwrap();
        assert_eq!(winner2.density, expected);
    }
}

#[test]
fn dens_any_wins() {
    let req = with(|r| r.density = 420);
    let any = with(|c| c.density = DENSITY_ANY);
    let xx = with(|c| c.density = DENSITY_XXHIGH);
    assert!(pick(&[xx, any.clone()], &req).unwrap().density == DENSITY_ANY);
}

#[test]
fn smallest_width_must_fit() {
    let req = with(|r| r.smallest_screen_width_dp = 411);
    let sw600 = with(|c| c.smallest_screen_width_dp = 600);
    let sw411 = with(|c| c.smallest_screen_width_dp = 411);
    let sw320 = with(|c| c.smallest_screen_width_dp = 320);
    assert!(!sw600.matches(&req));
    assert!(sw411.matches(&req) && sw320.matches(&req));
    let winner = pick(&[sw320, sw411], &req).unwrap();
    assert_eq!(winner.smallest_screen_width_dp, 411);
}

#[test]
fn orientation_and_night_and_uimode() {
    let req = request();
    let port = with(|c| c.orientation = ORIENTATION_PORT);
    let land = with(|c| c.orientation = ORIENTATION_LAND);
    assert!(port.matches(&req) && !land.matches(&req));
    assert_eq!(
        pick(&[land, port], &req).unwrap().orientation,
        ORIENTATION_PORT
    );

    let night_req = with(|r| r.ui_mode = UI_MODE_NIGHT_YES);
    let night = with(|c| c.ui_mode = UI_MODE_NIGHT_YES);
    let day = with(|c| c.ui_mode = UI_MODE_NIGHT_NO);
    assert!(night.matches(&night_req) && !day.matches(&night_req));
    assert_eq!(
        pick(&[day, night], &night_req).unwrap().ui_mode,
        UI_MODE_NIGHT_YES
    );

    let tv_req = with(|r| r.ui_mode = UI_MODE_TYPE_TELEVISION);
    let tv = with(|c| c.ui_mode = UI_MODE_TYPE_TELEVISION);
    let watch = with(|c| c.ui_mode = UI_MODE_TYPE_WATCH);
    assert!(tv.matches(&tv_req) && !watch.matches(&tv_req));
    assert_eq!(
        pick(&[watch, tv], &tv_req).unwrap().ui_mode,
        UI_MODE_TYPE_TELEVISION
    );
}

#[test]
fn sdk_version_and_layout_direction() {
    let req = with(|r| r.sdk_version = 34);
    let v26 = with(|c| c.sdk_version = 26);
    let v40 = with(|c| c.sdk_version = 40);
    let def = ResConfig::default();
    assert!(v26.matches(&req) && !v40.matches(&req));
    assert_eq!(pick(&[def, v26], &req).unwrap().sdk_version, 26);

    let rtl_req = with(|r| r.screen_layout = LAYOUTDIR_RTL);
    let rtl = with(|c| c.screen_layout = LAYOUTDIR_RTL);
    let ltr = with(|c| c.screen_layout = LAYOUTDIR_LTR);
    assert!(rtl.matches(&rtl_req) && !ltr.matches(&rtl_req));
    assert_eq!(
        pick(&[ltr, rtl], &rtl_req).unwrap().screen_layout,
        LAYOUTDIR_RTL
    );
}

#[test]
fn screen_size_larger_than_request_does_not_match() {
    let req = with(|r| r.screen_layout = SCREENSIZE_NORMAL);
    let large = with(|c| c.screen_layout = SCREENSIZE_LARGE);
    let small = with(|c| c.screen_layout = SCREENSIZE_SMALL);
    let normal = with(|c| c.screen_layout = SCREENSIZE_NORMAL);
    assert!(!large.matches(&req) && small.matches(&req) && normal.matches(&req));
    // Among <= normal, normal wins.
    assert_eq!(
        pick(&[small, normal], &req).unwrap().screen_layout,
        SCREENSIZE_NORMAL
    );
}
