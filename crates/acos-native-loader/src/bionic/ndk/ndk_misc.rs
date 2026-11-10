//! `libandroid` entries that are not asset/bitmap/window: `AChoreographer`,
//! `AConfiguration`, `ASensor*` and `AInputQueue`.
//!
//! * **`AChoreographer`** is real, but the clock is ACOS's: the loader arms the
//!   callback through the host, and the host invokes it from the VM's
//!   `fwk_vsync` tick. `AChoreographer_getInstance` is per-thread, matching the
//!   NDK contract that frame callbacks run on the calling thread's looper.
//! * **`AConfiguration`** mirrors the real `android.content.res.Configuration`
//!   ACOS holds (`acos-vm` `fwk_res`), including `getScreenSize` derived from
//!   the density and the `*Dp` fields. Setters mutate the local AConfiguration
//!   object exactly like the NDK does; they never leak into the system config.
//! * **`ASensor*`** has no ACOS substrate (no sensor HAL), so every entry
//!   reports `UNSUPPORTED_NATIVE_SYMBOL` and returns the documented failure.
//! * **`AInputQueue`** is not modelled: input reaches apps through the WMS
//!   `InputChannel`/`MotionEvent` path, not the NDK queue. Every entry reports
//!   unsupported and aborts the specific call; it never claims an event.

use std::cell::RefCell;
use std::collections::HashMap;
use std::os::raw::{c_char, c_int, c_void};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

use super::{
    handle, handle_id, host_failed, unsupported, with_host, FrameCallback, NdkConfiguration,
};

const LIB: &str = "libandroid";

// ---------------------------------------------------------------------------
// AChoreographer
// ---------------------------------------------------------------------------

thread_local! {
    static CHOREOGRAPHER: RefCell<Option<u64>> = const { RefCell::new(None) };
}

fn next_id() -> u64 {
    static N: AtomicUsize = AtomicUsize::new(0x4348_4f52_0000);
    N.fetch_add(1, Ordering::SeqCst) as u64
}

/// `AChoreographer_getInstance()`.
///
/// # Safety
/// Always safe.
pub unsafe extern "C" fn achoreographer_getinstance() -> *mut c_void {
    if let Some(id) = CHOREOGRAPHER.with(|c| *c.borrow()) {
        return handle(id);
    }
    let id = next_id();
    CHOREOGRAPHER.with(|c| *c.borrow_mut() = Some(id));
    handle(id)
}

fn post(
    frame_time_nanos: i64,
    cb: FrameCallback,
    data: *mut c_void,
    symbol: &'static str,
) -> u64 {
    static TRACED: AtomicUsize = AtomicUsize::new(0);
    if TRACED.fetch_add(1, Ordering::Relaxed) < 16 {
        eprintln!(
            "ACOS W108A {symbol} delay={frame_time_nanos} cb={:p} data={:p} tid={:?}",
            cb.map(|f| f as *const c_void).unwrap_or(std::ptr::null()),
            data,
            std::thread::current().id()
        );
    }
    match with_host(|h| h.choreographer_post(frame_time_nanos, cb, data)) {
        Ok(token) => token,
        Err(e) => host_failed(LIB, symbol, e, 0),
    }
}

/// `AChoreographer_postFrameCallback(AChoreographer*, frameCallback, void*)`.
///
/// # Safety
/// `callback` must be a valid C function pointer for the duration of the frame.
pub unsafe extern "C" fn achoreographer_postframecallback(
    _choreographer: *mut c_void,
    callback: FrameCallback,
    data: *mut c_void,
) {
    let _ = post(0, callback, data, "AChoreographer_postFrameCallback");
}

/// `AChoreographer_postFrameCallback64(...)` — the 64-bit variant.
///
/// # Safety
/// `callback` must be a valid C function pointer for the duration of the frame.
pub unsafe extern "C" fn achoreographer_postframecallback64(
    choreographer: *mut c_void,
    callback: FrameCallback,
    data: *mut c_void,
) {
    achoreographer_postframecallback(choreographer, callback, data);
}

/// `AChoreographer_postFrameCallbackDelayed(..., long delayMillis, void*)`.
///
/// # Safety
/// `callback` must be a valid C function pointer.
pub unsafe extern "C" fn achoreographer_postframecallbackdelayed(
    _choreographer: *mut c_void,
    callback: FrameCallback,
    delay_millis: i64,
    data: *mut c_void,
) {
    let _ = post(
        delay_millis.max(0) * 1_000_000,
        callback,
        data,
        "AChoreographer_postFrameCallbackDelayed",
    );
}

/// `AChoreographer_postVsyncCallback(AChoreographer*, vsyncCallback, void*)`.
///
/// # Safety
/// `callback` must be a valid C function pointer for the duration of the frame.
pub unsafe extern "C" fn achoreographer_postvsynccallback(
    _choreographer: *mut c_void,
    callback: FrameCallback,
    data: *mut c_void,
) {
    let _ = post(0, callback, data, "AChoreographer_postVsyncCallback");
}

// ---------------------------------------------------------------------------
// AConfiguration
// ---------------------------------------------------------------------------

fn configs() -> &'static Mutex<HashMap<u64, NdkConfiguration>> {
    static C: OnceLock<Mutex<HashMap<u64, NdkConfiguration>>> = OnceLock::new();
    C.get_or_init(|| Mutex::new(HashMap::new()))
}

fn with_config<T>(p: *mut c_void, f: impl FnOnce(&mut NdkConfiguration) -> T, fail: T) -> T {
    let Some(id) = handle_id(p) else { return fail };
    let mut g = configs().lock().unwrap_or_else(|e| e.into_inner());
    match g.get_mut(&id) {
        Some(c) => f(c),
        None => fail,
    }
}

fn system_config() -> NdkConfiguration {
    with_host(|h| h.configuration()).unwrap_or_default()
}

/// `AConfiguration_new()`.
///
/// # Safety
/// Always safe.
pub unsafe extern "C" fn aconfiguration_new() -> *mut c_void {
    let id = next_id();
    let c = system_config();
    configs()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(id, c);
    handle(id)
}

/// `AConfiguration_delete(AConfiguration*)`.
///
/// # Safety
/// `config` must be a live `AConfiguration`.
pub unsafe extern "C" fn aconfiguration_delete(config: *mut c_void) {
    if let Some(id) = handle_id(config) {
        configs()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&id);
    }
}

/// `AConfiguration_fromAssetManager(AConfiguration*, AAssetManager*)`.
///
/// # Safety
/// `config` must be live; `manager` must come from `AAssetManager_fromJava`.
pub unsafe extern "C" fn aconfiguration_fromassetmanager(
    config: *mut c_void,
    _manager: *mut c_void,
) {
    let c = system_config();
    with_config(config, |slot| *slot = c, ());
}

/// `AConfiguration_copy(AConfiguration* dest, AConfiguration* src)`.
///
/// # Safety
/// Both must be live `AConfiguration` objects.
pub unsafe extern "C" fn aconfiguration_copy(dest: *mut c_void, src: *mut c_void) {
    let copy = with_config(src, |c| Some(c.clone()), None);
    if let Some(c) = copy {
        with_config(dest, |slot| *slot = c, ());
    }
}

// The macro above cannot both define the item and a const of the same name;
// use plain functions plus a table instead.

/// # Safety
/// `config` must be a live `AConfiguration`.
pub unsafe extern "C" fn aconfiguration_getmcc(c: *mut c_void) -> c_int {
    with_config(c, |c| c.mcc, 0)
}
/// # Safety
/// `config` must be a live `AConfiguration`.
pub unsafe extern "C" fn aconfiguration_getmnc(c: *mut c_void) -> c_int {
    with_config(c, |c| c.mnc, 0)
}
/// # Safety
/// `config` must be a live `AConfiguration`.
pub unsafe extern "C" fn aconfiguration_getorientation(c: *mut c_void) -> c_int {
    with_config(c, |c| c.orientation, 0)
}
/// # Safety
/// `config` must be a live `AConfiguration`.
pub unsafe extern "C" fn aconfiguration_gettouchscreen(c: *mut c_void) -> c_int {
    with_config(c, |c| c.touchscreen, 0)
}
/// # Safety
/// `config` must be a live `AConfiguration`.
pub unsafe extern "C" fn aconfiguration_getdensity(c: *mut c_void) -> c_int {
    with_config(c, |c| c.density, 0)
}
/// # Safety
/// `config` must be a live `AConfiguration`.
pub unsafe extern "C" fn aconfiguration_getkeyboard(c: *mut c_void) -> c_int {
    with_config(c, |c| c.keyboard, 0)
}
/// # Safety
/// `config` must be a live `AConfiguration`.
pub unsafe extern "C" fn aconfiguration_getkeyboardhidden(c: *mut c_void) -> c_int {
    with_config(c, |c| c.keyboard_hidden, 0)
}
/// # Safety
/// `config` must be a live `AConfiguration`.
pub unsafe extern "C" fn aconfiguration_getnavigation(c: *mut c_void) -> c_int {
    with_config(c, |c| c.navigation, 0)
}
/// `AConfiguration_getScreenSize` — packed `(width << 16) | height` in pixels,
/// exactly the NDK's `int32_t` encoding (Configuration stores dp, the NDK
/// reports px).
///
/// # Safety
/// `config` must be a live `AConfiguration`.
pub unsafe extern "C" fn aconfiguration_getscreensize(c: *mut c_void) -> i32 {
    let (width, height) = with_config(
        c,
        |c| {
            let density = if c.density > 0 { c.density } else { 160 };
            let px = |dp: i32| dp.saturating_mul(density) / 160;
            (px(c.screen_width_dp), px(c.screen_height_dp))
        },
        (0, 0),
    );
    ((width.clamp(0, 0x7fff)) << 16) | height.clamp(0, 0xffff)
}
/// # Safety
/// `config` must be a live `AConfiguration`.
pub unsafe extern "C" fn aconfiguration_getscreenwidthdp(c: *mut c_void) -> c_int {
    with_config(c, |c| c.screen_width_dp, 0)
}
/// # Safety
/// `config` must be a live `AConfiguration`.
pub unsafe extern "C" fn aconfiguration_getscreenheightdp(c: *mut c_void) -> c_int {
    with_config(c, |c| c.screen_height_dp, 0)
}
/// # Safety
/// `config` must be a live `AConfiguration`.
pub unsafe extern "C" fn aconfiguration_getsmallestscreenwidthdp(c: *mut c_void) -> c_int {
    with_config(c, |c| c.smallest_screen_width_dp, 0)
}
/// # Safety
/// `config` must be a live `AConfiguration`.
pub unsafe extern "C" fn aconfiguration_getscreenlayout(c: *mut c_void) -> c_int {
    with_config(c, |c| c.screen_layout, 0)
}
/// # Safety
/// `config` must be a live `AConfiguration`.
pub unsafe extern "C" fn aconfiguration_getuimode(c: *mut c_void) -> c_int {
    with_config(c, |c| c.ui_mode, 0)
}
/// # Safety
/// `config` must be a live `AConfiguration`.
pub unsafe extern "C" fn aconfiguration_getcolormode(c: *mut c_void) -> c_int {
    with_config(c, |c| c.color_mode, 0)
}

/// `AConfiguration_getLocale(AConfiguration*, char* out, size_t len)`.
///
/// # Safety
/// `out` must be writable for `len` bytes.
pub unsafe extern "C" fn aconfiguration_getlocale(c: *mut c_void, out: *mut c_char, len: usize) {
    if out.is_null() || len == 0 {
        return;
    }
    let locale = with_config(c, |c| c.locale.clone(), String::new());
    let bytes = locale.as_bytes();
    let n = bytes.len().min(len - 1);
    std::ptr::copy_nonoverlapping(bytes.as_ptr() as *const c_char, out, n);
    out.add(n).write(0);
}

macro_rules! setter {
    ($name:ident, $field:ident) => {
        /// # Safety
        /// `config` must be a live `AConfiguration`.
        pub unsafe extern "C" fn $name(config: *mut c_void, value: c_int) {
            with_config(config, |c| c.$field = value, ());
        }
    };
}
setter!(aconfiguration_setmcc, mcc);
setter!(aconfiguration_setmnc, mnc);
setter!(aconfiguration_setorientation, orientation);
setter!(aconfiguration_settouchscreen, touchscreen);
setter!(aconfiguration_setdensity, density);
setter!(aconfiguration_setkeyboard, keyboard);
setter!(aconfiguration_setkeyboardhidden, keyboard_hidden);
setter!(aconfiguration_setnavigation, navigation);
setter!(aconfiguration_setscreenwidthdp, screen_width_dp);
setter!(aconfiguration_setscreenheightdp, screen_height_dp);
setter!(
    aconfiguration_setsmallestscreenwidthdp,
    smallest_screen_width_dp
);
setter!(aconfiguration_setscreenlayout, screen_layout);
setter!(aconfiguration_setuimode, ui_mode);
setter!(aconfiguration_setcolormode, color_mode);

/// Numeric diff mask used by `AConfiguration_diff`.
pub const ACONFIGURATION_DIFF_MCC: c_int = 0x0000_0001;
/// See [`ACONFIGURATION_DIFF_MCC`].
pub const ACONFIGURATION_DIFF_LOCALE: c_int = 0x0000_0002;
/// See [`ACONFIGURATION_DIFF_MCC`].
pub const ACONFIGURATION_DIFF_ORIENTATION: c_int = 0x0000_0080;
/// See [`ACONFIGURATION_DIFF_MCC`].
pub const ACONFIGURATION_DIFF_DENSITY: c_int = 0x0000_0400;
/// See [`ACONFIGURATION_DIFF_MCC`].
pub const ACONFIGURATION_DIFF_SCREEN_SIZE: c_int = 0x0000_0800;
/// See [`ACONFIGURATION_DIFF_MCC`].
pub const ACONFIGURATION_DIFF_SCREEN_LAYOUT: c_int = 0x0000_1000;
/// See [`ACONFIGURATION_DIFF_MCC`].
pub const ACONFIGURATION_DIFF_UI_MODE: c_int = 0x0000_2000;
/// See [`ACONFIGURATION_DIFF_MCC`].
pub const ACONFIGURATION_DIFF_COLOR_MODE: c_int = 0x0020_0000;

/// `AConfiguration_diff(base, requested)`.
///
/// # Safety
/// Both must be live `AConfiguration` objects.
pub unsafe extern "C" fn aconfiguration_diff(base: *mut c_void, requested: *mut c_void) -> c_int {
    let a = with_config(base, |c| Some(c.clone()), None);
    let b = with_config(requested, |c| Some(c.clone()), None);
    let (Some(a), Some(b)) = (a, b) else { return 0 };
    let mut d = 0;
    if a.mcc != b.mcc || a.mnc != b.mnc {
        d |= ACONFIGURATION_DIFF_MCC;
    }
    if a.locale != b.locale {
        d |= ACONFIGURATION_DIFF_LOCALE;
    }
    if a.orientation != b.orientation {
        d |= ACONFIGURATION_DIFF_ORIENTATION;
    }
    if a.density != b.density {
        d |= ACONFIGURATION_DIFF_DENSITY;
    }
    if a.screen_width_dp != b.screen_width_dp || a.screen_height_dp != b.screen_height_dp {
        d |= ACONFIGURATION_DIFF_SCREEN_SIZE;
    }
    if a.screen_layout != b.screen_layout {
        d |= ACONFIGURATION_DIFF_SCREEN_LAYOUT;
    }
    if a.ui_mode != b.ui_mode {
        d |= ACONFIGURATION_DIFF_UI_MODE;
    }
    if a.color_mode != b.color_mode {
        d |= ACONFIGURATION_DIFF_COLOR_MODE;
    }
    d
}

/// `AConfiguration_match(base, requested)`.
///
/// # Safety
/// Both must be live `AConfiguration` objects.
pub unsafe extern "C" fn aconfiguration_match(base: *mut c_void, requested: *mut c_void) -> bool {
    aconfiguration_diff(base, requested) == 0
}

// ---------------------------------------------------------------------------
// ASensor / AInputQueue: no ACOS substrate — structured unsupported
// ---------------------------------------------------------------------------

macro_rules! unsupported_fn {
    ($name:ident, $symbol:literal, $ret:ty, $why:literal, $fail:expr) => {
        /// # Safety
        /// Reports `UNSUPPORTED_NATIVE_SYMBOL`; no ACOS substrate exists today.
        pub unsafe extern "C" fn $name() -> $ret {
            unsupported(LIB, $symbol, $why, $fail)
        }
    };
}

unsupported_fn!(
    asensor_geteventqueue,
    "ASensorManager_getInstance",
    *mut c_void,
    "no sensor HAL in ACOS",
    std::ptr::null_mut()
);
unsupported_fn!(
    asensormanager_getinstance,
    "ASensorManager_getInstance",
    *mut c_void,
    "no sensor HAL in ACOS",
    std::ptr::null_mut()
);
unsupported_fn!(
    asensor_getdefaultsensor,
    "ASensorManager_getDefaultSensor",
    *mut c_void,
    "no sensor HAL in ACOS",
    std::ptr::null_mut()
);
unsupported_fn!(
    asensor_getname,
    "ASensor_getName",
    *const c_char,
    "no sensor HAL in ACOS",
    std::ptr::null()
);
unsupported_fn!(
    asensor_gettype,
    "ASensor_getType",
    c_int,
    "no sensor HAL in ACOS",
    -1
);
unsupported_fn!(
    asensor_enable,
    "ASensorEventQueue_enableSensor",
    c_int,
    "no sensor HAL in ACOS",
    -1
);

unsupported_fn!(
    ainputqueue_attachlooper,
    "AInputQueue_attachLooper",
    (),
    "input is delivered through the WMS InputChannel, not AInputQueue",
    ()
);
unsupported_fn!(
    ainputqueue_detachlooper,
    "AInputQueue_detachLooper",
    (),
    "input is delivered through the WMS InputChannel, not AInputQueue",
    ()
);
unsupported_fn!(
    ainputqueue_hasevents,
    "AInputQueue_hasEvents",
    c_int,
    "input is delivered through the WMS InputChannel, not AInputQueue",
    0
);
unsupported_fn!(
    ainputqueue_getevent,
    "AInputQueue_getEvent",
    c_int,
    "input is delivered through the WMS InputChannel, not AInputQueue",
    -1
);

/// Table entries for the miscellaneous `libandroid` API.
pub fn table() -> Vec<(&'static str, *const c_void)> {
    vec![
        (
            "AChoreographer_getInstance",
            achoreographer_getinstance as *const c_void,
        ),
        (
            "AChoreographer_postFrameCallback",
            achoreographer_postframecallback as *const c_void,
        ),
        (
            "AChoreographer_postFrameCallback64",
            achoreographer_postframecallback64 as *const c_void,
        ),
        (
            "AChoreographer_postFrameCallbackDelayed",
            achoreographer_postframecallbackdelayed as *const c_void,
        ),
        (
            "AChoreographer_postVsyncCallback",
            achoreographer_postvsynccallback as *const c_void,
        ),
        ("AConfiguration_new", aconfiguration_new as *const c_void),
        ("AConfiguration_delete", aconfiguration_delete as *const c_void),
        (
            "AConfiguration_fromAssetManager",
            aconfiguration_fromassetmanager as *const c_void,
        ),
        ("AConfiguration_copy", aconfiguration_copy as *const c_void),
        ("AConfiguration_getMcc", aconfiguration_getmcc as *const c_void),
        ("AConfiguration_getMnc", aconfiguration_getmnc as *const c_void),
        (
            "AConfiguration_getOrientation",
            aconfiguration_getorientation as *const c_void,
        ),
        (
            "AConfiguration_getTouchscreen",
            aconfiguration_gettouchscreen as *const c_void,
        ),
        (
            "AConfiguration_getDensity",
            aconfiguration_getdensity as *const c_void,
        ),
        (
            "AConfiguration_getKeyboard",
            aconfiguration_getkeyboard as *const c_void,
        ),
        (
            "AConfiguration_getKeyboardHidden",
            aconfiguration_getkeyboardhidden as *const c_void,
        ),
        (
            "AConfiguration_getNavigation",
            aconfiguration_getnavigation as *const c_void,
        ),
        (
            "AConfiguration_getScreenSize",
            aconfiguration_getscreensize as *const c_void,
        ),
        (
            "AConfiguration_getScreenWidthDp",
            aconfiguration_getscreenwidthdp as *const c_void,
        ),
        (
            "AConfiguration_getScreenHeightDp",
            aconfiguration_getscreenheightdp as *const c_void,
        ),
        (
            "AConfiguration_getSmallestScreenWidthDp",
            aconfiguration_getsmallestscreenwidthdp as *const c_void,
        ),
        (
            "AConfiguration_getScreenLayout",
            aconfiguration_getscreenlayout as *const c_void,
        ),
        (
            "AConfiguration_getUiMode",
            aconfiguration_getuimode as *const c_void,
        ),
        (
            "AConfiguration_getColorMode",
            aconfiguration_getcolormode as *const c_void,
        ),
        (
            "AConfiguration_getLocale",
            aconfiguration_getlocale as *const c_void,
        ),
        (
            "AConfiguration_setMcc",
            aconfiguration_setmcc as *const c_void,
        ),
        (
            "AConfiguration_setMnc",
            aconfiguration_setmnc as *const c_void,
        ),
        (
            "AConfiguration_setOrientation",
            aconfiguration_setorientation as *const c_void,
        ),
        (
            "AConfiguration_setDensity",
            aconfiguration_setdensity as *const c_void,
        ),
        (
            "AConfiguration_setScreenSize",
            aconfiguration_setscreenwidthdp as *const c_void,
        ),
        ("AConfiguration_diff", aconfiguration_diff as *const c_void),
        (
            "AConfiguration_match",
            aconfiguration_match as *const c_void,
        ),
        (
            "ASensorManager_getInstance",
            asensormanager_getinstance as *const c_void,
        ),
        (
            "ASensorManager_getDefaultSensor",
            asensor_getdefaultsensor as *const c_void,
        ),
        ("ASensor_getName", asensor_getname as *const c_void),
        ("ASensor_getType", asensor_gettype as *const c_void),
        (
            "ASensorEventQueue_enableSensor",
            asensor_enable as *const c_void,
        ),
        (
            "AInputQueue_attachLooper",
            ainputqueue_attachlooper as *const c_void,
        ),
        (
            "AInputQueue_detachLooper",
            ainputqueue_detachlooper as *const c_void,
        ),
        (
            "AInputQueue_hasEvents",
            ainputqueue_hasevents as *const c_void,
        ),
        (
            "AInputQueue_getEvent",
            ainputqueue_getevent as *const c_void,
        ),
    ]
}
