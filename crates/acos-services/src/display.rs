//! `IDisplayManager` — the single 1080×2400@420dpi default display.
//
// Values are MEASURED from
// `fixtures/reference/hello-views/art-api34-x86_64/display.txt`
// (`init=1080x2400 420dpi app=1080x2209`, `ROTATION_0`,
// `NavigationBar0` present) and `device-config.json`.

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

// One `android.view.Display$Mode` — `Display.Mode.writeToParcel`
// (`Display.java:2375-2382`):
//
// ```text
// writeInt(mModeId) writeInt(mWidth) writeInt(mHeight)
// writeFloat(mPeakRefreshRate) writeFloat(mVsyncRate)
// writeFloatArray(mAlternativeRefreshRates) writeIntArray(mSupportedHdrTypes)
// ```
#[derive(Debug, Clone, PartialEq)]
pub struct DisplayMode {
    /// `mModeId`.
    pub mode_id: i32,
    /// `mWidth`.
    pub width: i32,
    /// `mHeight`.
    pub height: i32,
    /// `mPeakRefreshRate`.
    pub refresh_rate: f32,
    /// `mAlternativeRefreshRates` (`writeFloatArray`).
    pub alternative_refresh_rates: Vec<f32>,
    /// `mSupportedHdrTypes` (`writeIntArray`).
    pub supported_hdr_types: Vec<i32>,
}

impl DisplayMode {
    /// A mode with no alternative refresh rates and no HDR types (the
    /// `Display.Mode(int, int, int, float)` constructor).
    #[must_use]
    pub fn new(mode_id: i32, width: i32, height: i32, refresh_rate: f32) -> Self {
        Self {
            mode_id,
            width,
            height,
            refresh_rate,
            alternative_refresh_rates: Vec::new(),
            supported_hdr_types: Vec::new(),
        }
    }

    fn to_parcelable(&self) -> Parcelable {
        Parcelable::ordered(
            "android.view.Display$Mode",
            vec![
                Value::Int(self.mode_id),
                Value::Int(self.width),
                Value::Int(self.height),
                Value::Float(self.refresh_rate),
                Value::FloatArray(Some(self.alternative_refresh_rates.clone())),
                Value::IntArray(Some(self.supported_hdr_types.clone())),
            ],
        )
    }

    fn from_parcelable(p: &Parcelable) -> Option<Self> {
        let f = p.fields()?;
        let float_array = |v: &Value| match v {
            Value::FloatArray(a) => Some(a.clone().unwrap_or_default()),
            _ => None,
        };
        let int_array = |v: &Value| match v {
            Value::IntArray(a) => Some(a.clone().unwrap_or_default()),
            _ => None,
        };
        Some(Self {
            mode_id: f.first()?.as_int()?,
            width: f.get(1)?.as_int()?,
            height: f.get(2)?.as_int()?,
            refresh_rate: f.get(3)?.as_float()?,
            alternative_refresh_rates: float_array(f.get(4)?)?,
            supported_hdr_types: int_array(f.get(5)?)?,
        })
    }
}

/// A rectangle as `Rect.writeToParcel` writes it (`left, top, right, bottom`).
pub type RectI = (i32, i32, i32, i32);

/// A real display cutout — the variant-`1` branch of
/// `DisplayCutout$ParcelableWrapper.writeCutoutToParcel`
/// (`DisplayCutout.java:1467-1482`).
#[derive(Debug, Clone, PartialEq)]
pub struct CutoutGeometry {
    /// `mSafeInsets` (`left, top, right, bottom`).
    pub safe_insets: RectI,
    /// `mBounds.getRects()` — exactly four rects
    /// (`LEFT, TOP, RIGHT, BOTTOM`, `BOUNDS_POSITION_LENGTH = 4`).
    pub bounds: [RectI; 4],
    /// `mWaterfallInsets`.
    pub waterfall_insets: RectI,
    /// `mCutoutPathParserInfo.displayWidth`.
    pub display_width: i32,
    /// `mCutoutPathParserInfo.displayHeight`.
    pub display_height: i32,
    /// `mCutoutPathParserInfo.physicalDisplayWidth`.
    pub physical_display_width: i32,
    /// `mCutoutPathParserInfo.physicalDisplayHeight`.
    pub physical_display_height: i32,
    /// `mCutoutPathParserInfo.density`.
    pub density: f32,
    /// `mCutoutPathParserInfo.cutoutSpec` (`writeString`, i.e. UTF-16).
    pub cutout_spec: Option<String>,
    /// `mCutoutPathParserInfo.rotation`.
    pub rotation: i32,
    /// `mCutoutPathParserInfo.scale`.
    pub scale: f32,
    /// `mCutoutPathParserInfo.physicalPixelDisplaySizeRatio`.
    pub physical_pixel_display_size_ratio: f32,
}

/// The three wire variants of `writeCutoutToParcel`.
#[derive(Debug, Clone, PartialEq)]
pub enum DisplayCutout {
    /// `writeToParcel(null)` — variant `-1`.
    Null,
    /// `DisplayCutout.NO_CUTOUT` — variant `0`.
    None,
    /// A real cutout — variant `1`.
    Some(CutoutGeometry),
}

impl DisplayCutout {
    fn to_parcelable(&self) -> Parcelable {
        let mut values = Vec::new();
        match self {
            Self::Null => values.push(Value::Int(-1)),
            Self::None => values.push(Value::Int(0)),
            Self::Some(g) => {
                values.push(Value::Int(1));
                values.push(Value::Parcelable(rect(&g.safe_insets))); // writeTypedObject
                values.push(Value::List(Some(
                    g.bounds
                        .iter()
                        .map(|r| Value::Parcelable(rect(r)))
                        .collect(),
                ))); // writeTypedArray
                values.push(Value::Parcelable(rect(&g.waterfall_insets))); // writeTypedObject
                values.push(Value::Int(g.display_width));
                values.push(Value::Int(g.display_height));
                values.push(Value::Int(g.physical_display_width));
                values.push(Value::Int(g.physical_display_height));
                values.push(Value::Float(g.density));
                values.push(Value::Str(g.cutout_spec.clone())); // writeString
                values.push(Value::Int(g.rotation));
                values.push(Value::Float(g.scale));
                values.push(Value::Float(g.physical_pixel_display_size_ratio));
            }
        }
        Parcelable::ordered(parcelable::DISPLAY_CUTOUT_CLASS, values)
    }

    fn from_parcelable(p: &Parcelable) -> Option<Self> {
        let f = p.fields()?;
        match f.first()?.as_int()? {
            -1 => Some(Self::Null),
            0 => Some(Self::None),
            1 => {
                if f.len() != 13 {
                    return None;
                }
                let safe_insets = rect_fields(f.get(1)?.as_parcelable()?)?;
                let Value::List(Some(items)) = f.get(2)? else {
                    return None;
                };
                let mut bounds = [(0, 0, 0, 0); 4];
                if items.len() != 4 {
                    return None;
                }
                for (slot, item) in bounds.iter_mut().zip(items) {
                    *slot = rect_fields(item.as_parcelable()?)?;
                }
                Some(Self::Some(CutoutGeometry {
                    safe_insets,
                    bounds,
                    waterfall_insets: rect_fields(f.get(3)?.as_parcelable()?)?,
                    display_width: f.get(4)?.as_int()?,
                    display_height: f.get(5)?.as_int()?,
                    physical_display_width: f.get(6)?.as_int()?,
                    physical_display_height: f.get(7)?.as_int()?,
                    density: f.get(8)?.as_float()?,
                    cutout_spec: match f.get(9)? {
                        Value::Str(s) => s.clone(),
                        _ => return None,
                    },
                    rotation: f.get(10)?.as_int()?,
                    scale: f.get(11)?.as_float()?,
                    physical_pixel_display_size_ratio: f.get(12)?.as_float()?,
                }))
            }
            _ => None,
        }
    }
}

/// `android.graphics.Rect` as a typed parcelable (no marker).
fn rect(r: &RectI) -> Parcelable {
    Parcelable::ordered(
        "android.graphics.Rect",
        vec![
            Value::Int(r.0),
            Value::Int(r.1),
            Value::Int(r.2),
            Value::Int(r.3),
        ],
    )
}

/// Decode a `Rect`/`Insets` payload (`left, top, right, bottom`).
fn rect_fields(p: &Parcelable) -> Option<RectI> {
    let f = p.fields()?;
    Some((
        f.first()?.as_int()?,
        f.get(1)?.as_int()?,
        f.get(2)?.as_int()?,
        f.get(3)?.as_int()?,
    ))
}

/// The typed `DisplayInfo` fields ACOS serves, in the API-34
/// `writeToParcel` order.
///
/// The field *order* is the whole point: API 34 rewrote
/// `DisplayInfo.writeToParcel` (`DisplayInfo.java:587-647`, 51 `Parcel.write*`
/// sites) and the pre-API-34 ACOS table desynchronised the framework's own
/// reader (`DisplayCutout.readCutoutFromParcel → bad array lengths`). Every
/// field below is written in the framework's order by
/// [`DisplayInfo::to_parcelable`]; the fields ACOS does not model are written
/// with the values the framework's own `DisplayInfo()` constructor has
/// (`null` object fields, `NaN` `hdrSdrRatio`, an *empty* (not null)
/// `thermalRefreshRateThrottling` `SparseArray`).
#[derive(Debug, Clone, PartialEq)]
pub struct DisplayInfo {
    /// Display id (0).
    pub display_id: i32,
    /// `layerStack` (0 for the default display).
    pub layer_stack: i32,
    /// `displayGroupId` (0: the default display group).
    pub display_group_id: i32,
    /// Logical width in physical pixels.
    pub logical_width: i32,
    /// Logical height in physical pixels.
    pub logical_height: i32,
    /// Application width (excludes system decor).
    pub app_width: i32,
    /// Application height.
    pub app_height: i32,
    /// `smallestNominalAppWidth`.
    pub smallest_nominal_app_width: i32,
    /// `smallestNominalAppHeight`.
    pub smallest_nominal_app_height: i32,
    /// `largestNominalAppWidth`.
    pub largest_nominal_app_width: i32,
    /// `largestNominalAppHeight`.
    pub largest_nominal_app_height: i32,
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
    /// Default mode id.
    pub default_mode_id: i32,
    /// User-preferred mode id (`0` = no preference).
    pub user_preferred_mode_id: i32,
    /// `supportedModes` (`writeInt(length)` + each `Mode.writeToParcel`).
    pub supported_modes: Vec<DisplayMode>,
    /// Logical density DPI (420).
    pub density_dpi: i32,
    /// `physicalXDpi`.
    pub physical_x_dpi: f32,
    /// `physicalYDpi`.
    pub physical_y_dpi: f32,
    /// `appVsyncOffsetNanos`.
    pub app_vsync_offset_nanos: i64,
    /// `presentationDeadlineNanos`.
    pub presentation_deadline_nanos: i64,
    /// Display state (`STATE_ON` = 2).
    pub state: i32,
    /// `committedState`.
    pub committed_state: i32,
    /// `ownerUid` (the fixture app uid, `10192`).
    pub owner_uid: i32,
    /// `removeMode`.
    pub remove_mode: i32,
    /// `refreshRateOverride` (`0` = none).
    pub refresh_rate_override: f32,
    /// `brightnessMinimum`.
    pub brightness_minimum: f32,
    /// `brightnessMaximum`.
    pub brightness_maximum: f32,
    /// `brightnessDefault`.
    pub brightness_default: f32,
    /// `userDisabledHdrTypes` (`writeInt(length)` + ints).
    pub user_disabled_hdr_types: Vec<i32>,
    /// Refresh rate in Hz (60.000004 on the reference).
    pub refresh_rate: f32,
    /// Color mode (`COLOR_MODE_SRGB` = 0).
    pub color_mode: i32,
    /// `supportedColorModes` (`writeInt(length)` + ints).
    pub supported_color_modes: Vec<i32>,
    /// `minimalPostProcessingSupported`.
    pub minimal_post_processing_supported: bool,
    /// The display cutout (`writeCutoutToParcel`).
    pub cutout: DisplayCutout,
}

impl DisplayInfo {
    /// The reference hello-views display, with any active [`DisplayOverride`]
    /// applied so `getDisplayInfo` / `DisplayMetrics` follow `acos launch`.
    #[must_use]
    pub fn hello_views() -> Self {
        let mut d = Self {
            display_id: 0,
            layer_stack: 0,
            display_group_id: 0,
            logical_width: 1080,
            logical_height: 2400,
            app_width: 1080,
            app_height: 2209,
            smallest_nominal_app_width: 1080,
            smallest_nominal_app_height: 2209,
            largest_nominal_app_width: 1080,
            largest_nominal_app_height: 2209,
            name: "Built-in Screen".to_string(),
            flags: 0,
            display_type: 1, // TYPE_INTERNAL
            install_orientation: 0,
            rotation: 0,
            mode_id: 1,
            default_mode_id: 1,
            user_preferred_mode_id: 0,
            supported_modes: Vec::new(),
            density_dpi: 420,
            physical_x_dpi: 420.0,
            physical_y_dpi: 420.0,
            app_vsync_offset_nanos: 0,
            presentation_deadline_nanos: 0,
            state: 2, // STATE_ON
            committed_state: 2,
            owner_uid: crate::pms::FIXTURE_UID,
            remove_mode: 0,
            refresh_rate_override: 0.0,
            brightness_minimum: 0.0,
            brightness_maximum: 1.0,
            brightness_default: 0.5,
            user_disabled_hdr_types: Vec::new(),
            refresh_rate: 60.000004_f32,
            color_mode: 0,                  // COLOR_MODE_SRGB
            supported_color_modes: vec![0], // {COLOR_MODE_DEFAULT}
            minimal_post_processing_supported: false,
            // The API-34 emulator has no cutout on this display; ART's own
            // reply (the seam this replaces) served `DisplayCutout.NO_CUTOUT`.
            cutout: DisplayCutout::None,
        };
        if let Some(o) = display_override() {
            d.logical_width = o.width;
            d.logical_height = o.height;
            d.app_width = o.width;
            d.app_height = o.app_height();
            d.smallest_nominal_app_width = o.width;
            d.smallest_nominal_app_height = d.app_height;
            d.largest_nominal_app_width = o.width;
            d.largest_nominal_app_height = d.app_height;
            d.density_dpi = o.density_dpi;
            d.physical_x_dpi = o.density_dpi as f32;
            d.physical_y_dpi = o.density_dpi as f32;
        }
        d.supported_modes = vec![DisplayMode::new(
            d.mode_id,
            d.logical_width,
            d.logical_height,
            d.refresh_rate,
        )];
        d
    }

    /// The wire value list, in `DisplayInfo.writeToParcel` order.
    ///
    /// The comments are the framework lines: `DisplayInfo.java:587-647`.
    #[must_use]
    pub fn to_parcelable(&self) -> Parcelable {
        Parcelable::ordered(
            parcelable::DISPLAY_INFO_CLASS,
            vec![
                Value::Int(self.layer_stack),      // writeInt(layerStack)
                Value::Int(self.flags),            // writeInt(flags)
                Value::Int(self.display_type),     // writeInt(type)
                Value::Int(self.display_id),       // writeInt(displayId)
                Value::Int(self.display_group_id), // writeInt(displayGroupId)
                // writeParcelable(address) — ACOS has no physical display id.
                Value::NamedParcelable(Parcelable::Null),
                // writeParcelable(deviceProductInfo)
                Value::NamedParcelable(Parcelable::Null),
                Value::Str8(Some(self.name.clone())), // writeString8(name)
                Value::Int(self.app_width),           // writeInt(appWidth)
                Value::Int(self.app_height),          // writeInt(appHeight)
                Value::Int(self.smallest_nominal_app_width),
                Value::Int(self.smallest_nominal_app_height),
                Value::Int(self.largest_nominal_app_width),
                Value::Int(self.largest_nominal_app_height),
                Value::Int(self.logical_width),  // writeInt(logicalWidth)
                Value::Int(self.logical_height), // writeInt(logicalHeight)
                // DisplayCutout$ParcelableWrapper.writeCutoutToParcel
                Value::Parcelable(self.cutout.to_parcelable()),
                Value::Int(self.rotation),          // writeInt(rotation)
                Value::Int(self.mode_id),           // writeInt(modeId)
                Value::Float(self.refresh_rate),    // writeFloat(renderFrameRate)
                Value::Int(self.default_mode_id),   // writeInt(defaultModeId)
                Value::Int(self.user_preferred_mode_id),
                // writeInt(supportedModes.length) + each Mode.writeToParcel
                Value::List(Some(
                    self.supported_modes
                        .iter()
                        .map(|m| Value::Parcelable(m.to_parcelable()))
                        .collect(),
                )),
                Value::Int(self.color_mode), // writeInt(colorMode)
                // writeInt(supportedColorModes.length) + each int
                Value::IntArray(Some(self.supported_color_modes.clone())),
                // writeParcelable(hdrCapabilities) — ACOS reports no HDR.
                Value::NamedParcelable(Parcelable::Null),
                Value::Bool(self.minimal_post_processing_supported),
                Value::Int(self.density_dpi),      // writeInt(logicalDensityDpi)
                Value::Float(self.physical_x_dpi), // writeFloat(physicalXDpi)
                Value::Float(self.physical_y_dpi), // writeFloat(physicalYDpi)
                Value::Long(self.app_vsync_offset_nanos),
                Value::Long(self.presentation_deadline_nanos),
                Value::Int(self.state),           // writeInt(state)
                Value::Int(self.committed_state), // writeInt(committedState)
                Value::Int(self.owner_uid),       // writeInt(ownerUid)
                Value::Str8(None),                // writeString8(ownerPackageName)
                Value::Str8(None),                // writeString8(uniqueId)
                Value::Int(self.remove_mode),     // writeInt(removeMode)
                Value::Float(self.refresh_rate_override),
                Value::Float(self.brightness_minimum),
                Value::Float(self.brightness_maximum),
                Value::Float(self.brightness_default),
                // writeTypedObject(roundedCorners) — no rounded corners served.
                Value::TypedObject(Parcelable::Null),
                // writeInt(userDisabledHdrTypes.length) + each int
                Value::IntArray(Some(self.user_disabled_hdr_types.clone())),
                Value::Int(self.install_orientation), // writeInt(installOrientation)
                // writeTypedObject(displayShape)
                Value::TypedObject(Parcelable::Null),
                // writeTypedObject(layoutLimitedRefreshRate)
                Value::TypedObject(Parcelable::Null),
                // writeFloat(hdrSdrRatio) — `Float.NaN` is the framework default.
                Value::Float(f32::NAN),
                // writeSparseArray(thermalRefreshRateThrottling): the empty
                // array the framework's constructor installs (count 0).
                Value::List(Some(Vec::new())),
                Value::Str8(None), // writeString8(thermalBrightnessThrottlingDataId)
            ],
        )
    }

    /// Decode a typed parcelable produced by [`DisplayInfo::to_parcelable`]
    /// (indices are the `writeToParcel` positions above).
    #[must_use]
    pub fn from_parcelable(p: &Parcelable) -> Option<Self> {
        let f = p.fields()?;
        let int = |i: usize| f.get(i).and_then(Value::as_int);
        let float = |i: usize| f.get(i).and_then(Value::as_float);
        let long = |i: usize| f.get(i).and_then(Value::as_long);
        let modes = match f.get(22)? {
            Value::List(Some(items)) => items
                .iter()
                .map(|v| DisplayMode::from_parcelable(v.as_parcelable()?))
                .collect::<Option<Vec<_>>>()?,
            _ => return None,
        };
        Some(Self {
            layer_stack: int(0)?,
            flags: int(1)?,
            display_type: int(2)?,
            display_id: int(3)?,
            display_group_id: int(4)?,
            name: f.get(7)?.as_str()?.to_string(),
            app_width: int(8)?,
            app_height: int(9)?,
            smallest_nominal_app_width: int(10)?,
            smallest_nominal_app_height: int(11)?,
            largest_nominal_app_width: int(12)?,
            largest_nominal_app_height: int(13)?,
            logical_width: int(14)?,
            logical_height: int(15)?,
            cutout: DisplayCutout::from_parcelable(f.get(16)?.as_parcelable()?)?,
            rotation: int(17)?,
            mode_id: int(18)?,
            refresh_rate: float(19)?,
            default_mode_id: int(20)?,
            user_preferred_mode_id: int(21)?,
            supported_modes: modes,
            color_mode: int(23)?,
            density_dpi: int(27)?,
            physical_x_dpi: float(28)?,
            physical_y_dpi: float(29)?,
            app_vsync_offset_nanos: long(30)?,
            presentation_deadline_nanos: long(31)?,
            state: int(32)?,
            committed_state: int(33)?,
            owner_uid: int(34)?,
            remove_mode: int(37)?,
            refresh_rate_override: float(38)?,
            brightness_minimum: float(39)?,
            brightness_maximum: float(40)?,
            brightness_default: float(41)?,
            user_disabled_hdr_types: match f.get(43)? {
                Value::IntArray(a) => a.clone().unwrap_or_default(),
                _ => return None,
            },
            install_orientation: int(44)?,
            minimal_post_processing_supported: f.get(26)?.as_bool()?,
            supported_color_modes: match f.get(24)? {
                Value::IntArray(a) => a.clone().unwrap_or_default(),
                _ => return None,
            },
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
