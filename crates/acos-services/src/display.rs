//! `IDisplayManager` — the single 1080×2400@420dpi default display.
//!
//! Values are MEASURED from
//! `fixtures/reference/hello-views/art-api34-x86_64/display.txt`
//! (`init=1080x2400 420dpi app=1080x2209`, `ROTATION_0`,
//! `NavigationBar0` present) and `device-config.json`.

use std::cell::RefCell;
use std::sync::{Arc, Mutex, OnceLock};

/// A display configuration override applied before an app boots.
///
/// `acos launch --density 400` (and `--display 1080x2400`, `--reference`) set
/// this so ACOS is laid out on the phone's display instead of the API-34
/// emulator reference. The phone under test is `1080x2400 @ 400 dpi` with a
/// 108 px status bar and gesture navigation (zero navigation-bar inset).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DisplayOverride {
    /// Logical width in pixels.
    pub width: i32,
    /// Logical height in pixels.
    pub height: i32,
    /// Density in dpi (`400` on the moto g35).
    pub density_dpi: i32,
    /// Status-bar top inset in pixels (`108`).
    pub status_bar_top: i32,
    /// Navigation-bar bottom inset in pixels (`0` for gesture navigation).
    pub nav_bar_bottom: i32,
}

impl DisplayOverride {
    /// The moto g35 (ZA222WGJPP) under test: 1080x2400 @ 400 dpi, 108 px
    /// status bar, gesture navigation.
    #[must_use]
    pub const fn phone() -> Self {
        Self {
            width: 1080,
            height: 2400,
            density_dpi: 400,
            status_bar_top: 108,
            nav_bar_bottom: 0,
        }
    }

    /// Smallest screen width in dp, rounded to nearest.
    #[must_use]
    pub fn screen_width_dp(&self) -> i32 {
        (self.width * 160 + self.density_dpi / 2) / self.density_dpi.max(1)
    }

    /// Screen height in dp, rounded to nearest.
    #[must_use]
    pub fn screen_height_dp(&self) -> i32 {
        (self.height * 160 + self.density_dpi / 2) / self.density_dpi.max(1)
    }

    /// Application height in pixels (`height - status - nav`).
    #[must_use]
    pub fn app_height(&self) -> i32 {
        (self.height - self.status_bar_top - self.nav_bar_bottom).max(0)
    }

    /// `"1080x2400@400/108/0"`: the compact wire/env encoding.
    #[must_use]
    pub fn spec(&self) -> String {
        format!(
            "{}x{}@{}/{}/{}",
            self.width, self.height, self.density_dpi, self.status_bar_top, self.nav_bar_bottom
        )
    }

    /// Parse [`Self::spec`].
    #[must_use]
    pub fn parse(spec: &str) -> Option<Self> {
        let (size, density) = spec.split_once('@')?;
        let (w, h) = size.split_once('x')?;
        let parts: Vec<&str> = density.split('/').collect();
        let density_dpi = parts.first()?.parse().ok()?;
        let status_bar_top = parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
        let nav_bar_bottom = parts.get(2).and_then(|s| s.parse().ok()).unwrap_or(0);
        Some(Self {
            width: w.parse().ok()?,
            height: h.parse().ok()?,
            density_dpi,
            status_bar_top,
            nav_bar_bottom,
        })
    }
}

static OVERRIDE: OnceLock<Mutex<Option<DisplayOverride>>> = OnceLock::new();

fn override_slot() -> &'static Mutex<Option<DisplayOverride>> {
    OVERRIDE.get_or_init(|| Mutex::new(None))
}

/// Install (or clear) the process-wide display override.
///
/// The `acos serve` daemon replaces any prior session on launch, so one global
/// override is sufficient and matches the single-live-runtime transport seam.
pub fn set_display_override(override_: Option<DisplayOverride>) {
    *override_slot().lock().unwrap_or_else(|e| e.into_inner()) = override_;
}

/// The active display override: the in-process value, else `ACOS_DISPLAY_OVERRIDE`
/// (`"1080x2400@400/108/0"`), which the daemon inherits from `acos launch`.
#[must_use]
pub fn display_override() -> Option<DisplayOverride> {
    if let Some(o) = *override_slot().lock().unwrap_or_else(|e| e.into_inner()) {
        return Some(o);
    }
    std::env::var("ACOS_DISPLAY_OVERRIDE")
        .ok()
        .and_then(|s| DisplayOverride::parse(&s))
}

use acos_aidl::MethodInfo;

use crate::error::{ServiceError, Result};
use crate::host::{CallResult, ServiceObject};
use crate::parcelable;
use crate::value::{Parcelable, Value};

/// The typed `DisplayInfo` fields ACOS serves.
#[derive(Debug, Clone, PartialEq)]
pub struct DisplayInfo {
    /// Display id (0).
    pub display_id: i32,
    /// Logical width in physical pixels.
    pub logical_width: i32,
    /// Logical height in physical pixels.
    pub logical_height: i32,
    /// Application width (excludes system decor).
    pub app_width: i32,
    /// Application height.
    pub app_height: i32,
    /// Display name.
    pub name: String,
    /// Display flags.
    pub flags: i32,
    /// `TYPE_INTERNAL` = 1.
    pub display_type: i32,
    /// Installation orientation.
    pub install_orientation: i32,
    /// Rotation (`ROTATION_0` = 0).
    pub rotation: i32,
    /// Active mode id.
    pub mode_id: i32,
    /// Logical density DPI (420).
    pub density_dpi: i32,
    /// Display state (`STATE_ON` = 2).
    pub state: i32,
    /// Refresh rate in Hz (60.000004 on the reference).
    pub refresh_rate: f32,
    /// Color mode (`COLOR_MODE_SRGB` = 0).
    pub color_mode: i32,
}

impl DisplayInfo {
    /// The reference hello-views display, with any active [`DisplayOverride`]
    /// applied so `getDisplayInfo` / `DisplayMetrics` follow `acos launch`.
    #[must_use]
    pub fn hello_views() -> Self {
        let mut d = Self {
            display_id: 0,
            logical_width: 1080,
            logical_height: 2400,
            app_width: 1080,
            app_height: 2209,
            name: "Built-in Screen".to_string(),
            flags: 0,
            display_type: 1,
            install_orientation: 0,
            rotation: 0,
            mode_id: 1,
            density_dpi: 420,
            state: 2,
            refresh_rate: 60.000004,
            color_mode: 0,
        };
        if let Some(o) = display_override() {
            d.logical_width = o.width;
            d.logical_height = o.height;
            d.app_width = o.width;
            d.app_height = o.app_height();
            d.density_dpi = o.density_dpi;
        }
        d
    }

    /// Encode as a typed parcelable in `DisplayInfo.writeToParcel` order.
    #[must_use]
    pub fn to_parcelable(&self) -> Parcelable {
        Parcelable::ordered(
            "android.view.DisplayInfo",
            vec![
                Value::Int(self.display_id),
                Value::Int(self.logical_width),
                Value::Int(self.logical_height),
                Value::Int(self.app_width),
                Value::Int(self.app_height),
                Value::NamedParcelable(Parcelable::Null), // displayCutout
                Value::NamedParcelable(Parcelable::Null), // displayShape
                Value::Str8(Some(self.name.clone())),
                Value::Int(self.flags),
                Value::Int(self.display_type),
                Value::Int(0), // address
                Value::Int(0), // deviceProductInfo
                Value::Int(self.rotation),
                Value::Int(self.mode_id),
                Value::Int(60), // renderFrameRate (int)
                Value::Int(1),  // supportedModes length
                Value::Int(self.density_dpi),
                Value::Int(self.state),
                Value::Float(self.refresh_rate),
                Value::Int(self.color_mode),
                Value::Int(self.install_orientation),
            ],
        )
    }

    /// Decode a typed parcelable produced by [`DisplayInfo::to_parcelable`].
    #[must_use]
    pub fn from_parcelable(p: &Parcelable) -> Option<Self> {
        let f = p.fields()?;
        Some(Self {
            display_id: f.first()?.as_int()?,
            logical_width: f.get(1)?.as_int()?,
            logical_height: f.get(2)?.as_int()?,
            app_width: f.get(3)?.as_int()?,
            app_height: f.get(4)?.as_int()?,
            name: f.get(7)?.as_str()?.to_string(),
            flags: f.get(8)?.as_int()?,
            display_type: f.get(9)?.as_int()?,
            rotation: f.get(12)?.as_int()?,
            mode_id: f.get(13)?.as_int()?,
            density_dpi: f.get(16)?.as_int()?,
            state: f.get(17)?.as_int()?,
            refresh_rate: f.get(18)?.as_float()?,
            color_mode: f.get(19)?.as_int()?,
            install_orientation: f.get(20)?.as_int()?,
        })
    }
}

thread_local! {
    static LAST_DISPLAY_INFO: RefCell<Option<DisplayInfo>> = const { RefCell::new(None) };
}

/// Record the [`DisplayInfo`] a `getDisplayInfo` handler just served (or `None`
/// for an unknown display id).
///
/// The runtime transport re-encodes the reply through the real Java
/// `DisplayInfo.writeToParcel` (see `acos-runtime::display_java`); it cannot see
/// this crate's state, so the handler leaves the served value here the same way
/// the PMS caches its typed description.
pub fn cache_display_info(info: Option<DisplayInfo>) {
    LAST_DISPLAY_INFO.with(|slot| *slot.borrow_mut() = info);
}

/// The [`DisplayInfo`] recorded by the last `getDisplayInfo` handler call.
#[must_use]
pub fn cached_display_info() -> Option<DisplayInfo> {
    LAST_DISPLAY_INFO.with(|slot| slot.borrow().clone())
}

/// Mutable display state.
#[derive(Debug)]
pub struct DisplayState {
    /// The default display.
    pub default_display: DisplayInfo,
    /// Registered `IDisplayManagerCallback` binders.
    pub callbacks: Vec<u64>,
}

impl DisplayState {
    /// New state with the default display.
    #[must_use]
    pub fn new(default_display: DisplayInfo) -> Self {
        Self {
            default_display,
            callbacks: Vec::new(),
        }
    }
}

/// `IDisplayManager` handler.
pub struct DisplayService {
    state: Arc<Mutex<DisplayState>>,
}

impl DisplayService {
    /// Wrap shared display state.
    #[must_use]
    pub fn new(state: Arc<Mutex<DisplayState>>) -> Self {
        Self { state }
    }
}

impl ServiceObject for DisplayService {
    fn call(
        &mut self,
        interface: &str,
        method: &MethodInfo,
        args: &[Value],
    ) -> Result<CallResult> {
        if interface != "android.hardware.display.IDisplayManager" {
            return Err(ServiceError::UnsupportedAndroidApi {
                interface: interface.to_string(),
                code: method.code,
                method: method.name.clone(),
            });
        }
        let mut state = self.state.lock().unwrap();
        let reply = match method.name.as_str() {
            "getPreferredWideGamutColorSpaceId" => vec![Value::Int(1)], // ColorSpace SRGB
            "getOverlaySupport" => vec![Value::TypedObject(Parcelable::ordered(
                "android.hardware.OverlayProperties",
                vec![Value::Bool(false), Value::Bool(false)],
            ))],
            "getDisplayInfo" => {
                let id = args.first().and_then(Value::as_int).unwrap_or(0);
                if id != 0 {
                    cache_display_info(None);
                    vec![Value::TypedObject(Parcelable::Null)]
                } else {
                    cache_display_info(Some(state.default_display.clone()));
                    vec![Value::TypedObject(state.default_display.to_parcelable())]
                }
            }
            "getDisplayIds" => vec![Value::IntArray(Some(vec![0]))],
            "getStableDisplaySize" => vec![Value::Parcelable(Parcelable::ordered(
                "android.graphics.Point",
                vec![Value::Int(1080), Value::Int(2400)],
            ))],
            "registerCallbackWithEventMask" => {
                if let Some(id) = args.first().and_then(Value::as_binder) {
                    state.callbacks.push(id.0);
                }
                Vec::new()
            }
            "registerCallback" => {
                if let Some(id) = args.first().and_then(Value::as_binder) {
                    state.callbacks.push(id.0);
                }
                Vec::new()
            }
            // Only display 0 exists; an unknown display id must not report
            // every uid as present (HONESTY_AUDIT #22).
            "isUidPresentOnDisplay" => {
                let display = args.get(1).and_then(Value::as_int).unwrap_or(-1);
                vec![Value::Bool(display == 0)]
            }
            _ => {
                return Err(ServiceError::UnsupportedAndroidApi {
                    interface: interface.to_string(),
                    code: method.code,
                    method: method.name.clone(),
                })
            }
        };
        let _ = parcelable::has_codec("android.view.DisplayInfo");
        Ok(CallResult {
            reply,
            deliveries: Vec::new(),
        })
    }

    fn drain_events(&mut self) -> Vec<acos_protocol::Event> {
        Vec::new()
    }
}
