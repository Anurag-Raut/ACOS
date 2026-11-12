//! Process-wide animation scale, the ACOS stand-in for Android's
//! `Settings.Global.animator_duration_scale`.
//!
//! Real Android reads the three global animation scales in the system process
//! and publishes the animator scale to app processes through
//! `IWindowManager.getCurrentAnimatorScale()` (at `WindowManagerGlobal`
//! initialisation) and `IWindowSessionCallback.onAnimatorScaleChanged()` (when
//! the setting changes). ACOS has no SettingsProvider and one WMS per session
//! process, so the launcher installs the per-session scale here and `WmsState`
//! answers exactly that value. The guest applies the same value with
//! `ValueAnimator.setDurationScale`, which is where a device's framework ends
//! up too.
//!
//! `on` (the default) is `1.0` and changes nothing. `off` is `0.0`, the standard
//! CI setting that makes `ValueAnimator`/`ObjectAnimator`/`StateListAnimator`
//! and the native `RenderNodeAnimator` (ripple) finish without running their
//! duration. Window/Activity transition chrome on a device is WMS-side; ACOS's
//! WMS runs no transition animations, so there is nothing further to scale.
//!
//! The scale is process-wide because one `acos-art` process serves one session
//! (the product route), and a session's animations flag is a launch-time
//! property. It is never edited by guest code.

use std::sync::atomic::{AtomicU32, Ordering};

/// Bits of the current scale; `1.0f32.to_bits()` is the default.
static SCALE_BITS: AtomicU32 = AtomicU32::new(0x3f80_0000);

/// Set the process-wide animator duration scale (`1.0` on, `0.0` off).
pub fn set_scale(scale: f32) {
    let scale = if scale.is_finite() {
        scale.clamp(0.0, 1.0)
    } else {
        1.0
    };
    SCALE_BITS.store(scale.to_bits(), Ordering::Release);
}

/// The current animator duration scale.
#[must_use]
pub fn scale() -> f32 {
    f32::from_bits(SCALE_BITS.load(Ordering::Acquire))
}

/// `true` when animations run with their real durations.
#[must_use]
pub fn enabled() -> bool {
    scale() > 0.0
}

/// Set from the session's `animations` choice (`on` = real behaviour).
pub fn set_enabled(on: bool) {
    set_scale(if on { 1.0 } else { 0.0 });
}
