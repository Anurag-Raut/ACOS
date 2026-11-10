//! Typed `Parcel` codecs for the WMS parcelables.
//!
//! Every field order below was derived by disassembling the framework's own
//! `writeToParcel` out of the Android 14 boot DEX (`examples/derive_layouts.rs`
//! regenerates `results/wm/parcelable_layouts.txt`). The evidence lines quoted
//! next to each type are the exact `iget` → `Parcel.write*` sequence the DEX
//! contains.
//!
//! The reply/argument envelope helpers ([`write_cond`], [`write_typed_object`],
//! [`write_typed_array`]) mirror `Parcel.writeParcelable`/`writeTypedObject`/
//! `writeTypedArray`, which the AIDL stubs use for the `out` parameters.

use acos_parcel::binder::BinderId;
use acos_parcel::Parcel;

use crate::error::{Result, WmError};
use crate::geom::{InsetsI, PointI, RectI};

/// A value that knows its Android `Parcel` byte layout.
pub trait ParcelCodec: Sized + std::fmt::Debug {
    /// The framework class this codec reproduces (for diagnostics).
    const NAME: &'static str;

    /// Append `self` in the framework's field order.
    fn write_to_parcel(&self, p: &mut Parcel) -> Result<()>;

    /// Read a value the framework would have written.
    fn read_from_parcel(p: &mut Parcel) -> Result<Self>;
}

fn codec_err(what: &'static str, reason: impl Into<String>) -> WmError {
    WmError::Codec {
        what,
        reason: reason.into(),
    }
}

// ---------------------------------------------------------------------------
// envelope helpers (AIDL out-parameter / Parcelable containers)
// ---------------------------------------------------------------------------

/// `if (v != null) { writeInt(1); v.writeToParcel(); } else writeInt(0);`
pub fn write_cond<T: ParcelCodec>(p: &mut Parcel, v: Option<&T>) -> Result<()> {
    match v {
        Some(v) => {
            p.write_int32(1);
            v.write_to_parcel(p)
        }
        None => {
            p.write_int32(0);
            Ok(())
        }
    }
}

/// Read the marker written by [`write_cond`].
pub fn read_cond<T: ParcelCodec>(p: &mut Parcel) -> Result<Option<T>> {
    if p.read_int32()? != 0 {
        Ok(Some(T::read_from_parcel(p)?))
    } else {
        Ok(None)
    }
}

/// `Parcel.writeTypedObject`: `writeInt(1)` + body, or `writeInt(0)`.
pub fn write_typed_object<T: ParcelCodec>(p: &mut Parcel, v: Option<&T>) -> Result<()> {
    write_cond(p, v)
}

/// `Parcel.readTypedObject`.
pub fn read_typed_object<T: ParcelCodec>(p: &mut Parcel) -> Result<Option<T>> {
    read_cond(p)
}

/// `Parcel.writeTypedArray`: `-1` for null, else count + each typed element.
pub fn write_typed_array<T: ParcelCodec>(p: &mut Parcel, v: Option<&[T]>) -> Result<()> {
    match v {
        None => {
            p.write_int32(-1);
            Ok(())
        }
        Some(items) => {
            p.write_int32(items.len() as i32);
            for item in items {
                write_typed_object(p, Some(item))?;
            }
            Ok(())
        }
    }
}

/// `Parcel.readTypedArray`.
pub fn read_typed_array<T: ParcelCodec>(p: &mut Parcel) -> Result<Option<Vec<T>>> {
    let n = p.read_int32()?;
    if n < 0 {
        return Ok(None);
    }
    let mut out = Vec::with_capacity(n as usize);
    for _ in 0..n {
        let item = read_typed_object::<T>(p)?
            .ok_or_else(|| codec_err(T::NAME, "null element in non-null typed array"))?;
        out.push(item);
    }
    Ok(Some(out))
}

/// `TextUtils.writeToParcel` for a non-`Spanned` `CharSequence`.
fn write_char_sequence(p: &mut Parcel, v: Option<&str>) -> Result<()> {
    // DEX 0x0079: writeInt(1); if (cs != null) writeString8(toString()) else writeString8(null).
    p.write_int32(1);
    p.write_string8_opt(v)?;
    Ok(())
}

/// Read `TextUtils.writeToParcel`. `Spanned` (marker `0`) is a declared gap.
fn read_char_sequence(p: &mut Parcel) -> Result<Option<String>> {
    match p.read_int32()? {
        0 => Err(WmError::Unsupported(
            "TextUtils Spanned CharSequence in a Parcelable (spans not modelled)".into(),
        )),
        _ => p.read_string8().map_err(Into::into),
    }
}

// ---------------------------------------------------------------------------
// primitives
// ---------------------------------------------------------------------------

impl ParcelCodec for RectI {
    const NAME: &'static str = "android.graphics.Rect";

    fn write_to_parcel(&self, p: &mut Parcel) -> Result<()> {
        // DEX: iget left/top/right/bottom → writeInt ×4.
        p.write_int32(self.left);
        p.write_int32(self.top);
        p.write_int32(self.right);
        p.write_int32(self.bottom);
        Ok(())
    }

    fn read_from_parcel(p: &mut Parcel) -> Result<Self> {
        Ok(RectI {
            left: p.read_int32()?,
            top: p.read_int32()?,
            right: p.read_int32()?,
            bottom: p.read_int32()?,
        })
    }
}

impl ParcelCodec for PointI {
    const NAME: &'static str = "android.graphics.Point";

    fn write_to_parcel(&self, p: &mut Parcel) -> Result<()> {
        p.write_int32(self.x);
        p.write_int32(self.y);
        Ok(())
    }

    fn read_from_parcel(p: &mut Parcel) -> Result<Self> {
        Ok(PointI {
            x: p.read_int32()?,
            y: p.read_int32()?,
        })
    }
}

impl ParcelCodec for InsetsI {
    const NAME: &'static str = "android.graphics.Insets";

    fn write_to_parcel(&self, p: &mut Parcel) -> Result<()> {
        p.write_int32(self.left);
        p.write_int32(self.top);
        p.write_int32(self.right);
        p.write_int32(self.bottom);
        Ok(())
    }

    fn read_from_parcel(p: &mut Parcel) -> Result<Self> {
        Ok(InsetsI {
            left: p.read_int32()?,
            top: p.read_int32()?,
            right: p.read_int32()?,
            bottom: p.read_int32()?,
        })
    }
}

// ---------------------------------------------------------------------------
// Configuration (full framework field order)
// ---------------------------------------------------------------------------

/// `android.content.res.Configuration`. Field order from `writeToParcel`.
#[derive(Debug, Clone, PartialEq)]
pub struct Configuration {
    pub font_scale: f32,
    pub mcc: i32,
    pub mnc: i32,
    pub locale_list: Option<String>,
    pub user_set_locale: bool,
    pub touchscreen: i32,
    pub keyboard: i32,
    pub keyboard_hidden: i32,
    pub hard_keyboard_hidden: i32,
    pub navigation: i32,
    pub navigation_hidden: i32,
    pub orientation: i32,
    pub screen_layout: i32,
    pub color_mode: i32,
    pub ui_mode: i32,
    pub screen_width_dp: i32,
    pub screen_height_dp: i32,
    pub smallest_screen_width_dp: i32,
    pub density_dpi: i32,
    pub compat_screen_width_dp: i32,
    pub compat_screen_height_dp: i32,
    pub compat_smallest_screen_width_dp: i32,
    pub window_configuration: WindowConfiguration,
    pub assets_seq: i32,
    pub seq: i32,
    pub font_weight_adjustment: i32,
    pub grammatical_gender: i32,
}

impl ParcelCodec for Configuration {
    const NAME: &'static str = "android.content.res.Configuration";

    fn write_to_parcel(&self, p: &mut Parcel) -> Result<()> {
        p.write_float(self.font_scale);
        p.write_int32(self.mcc);
        p.write_int32(self.mnc);
        // fixUpLocaleList() then writeTypedObject(LocaleList) — LocaleList writes String8.
        match &self.locale_list {
            Some(s) => {
                p.write_int32(1);
                p.write_string8(s)?;
            }
            None => p.write_int32(0),
        }
        p.write_int32(i32::from(self.user_set_locale));
        p.write_int32(self.touchscreen);
        p.write_int32(self.keyboard);
        p.write_int32(self.keyboard_hidden);
        p.write_int32(self.hard_keyboard_hidden);
        p.write_int32(self.navigation);
        p.write_int32(self.navigation_hidden);
        p.write_int32(self.orientation);
        p.write_int32(self.screen_layout);
        p.write_int32(self.color_mode);
        p.write_int32(self.ui_mode);
        p.write_int32(self.screen_width_dp);
        p.write_int32(self.screen_height_dp);
        p.write_int32(self.smallest_screen_width_dp);
        p.write_int32(self.density_dpi);
        p.write_int32(self.compat_screen_width_dp);
        p.write_int32(self.compat_screen_height_dp);
        p.write_int32(self.compat_smallest_screen_width_dp);
        self.window_configuration.write_to_parcel(p)?;
        p.write_int32(self.assets_seq);
        p.write_int32(self.seq);
        p.write_int32(self.font_weight_adjustment);
        p.write_int32(self.grammatical_gender);
        Ok(())
    }

    fn read_from_parcel(p: &mut Parcel) -> Result<Self> {
        let font_scale = p.read_float()?;
        let mcc = p.read_int32()?;
        let mnc = p.read_int32()?;
        let locale_list = if p.read_int32()? != 0 {
            p.read_string8()?
        } else {
            None
        };
        let value = Configuration {
            font_scale,
            mcc,
            mnc,
            locale_list,
            user_set_locale: p.read_int32()? != 0,
            touchscreen: p.read_int32()?,
            keyboard: p.read_int32()?,
            keyboard_hidden: p.read_int32()?,
            hard_keyboard_hidden: p.read_int32()?,
            navigation: p.read_int32()?,
            navigation_hidden: p.read_int32()?,
            orientation: p.read_int32()?,
            screen_layout: p.read_int32()?,
            color_mode: p.read_int32()?,
            ui_mode: p.read_int32()?,
            screen_width_dp: p.read_int32()?,
            screen_height_dp: p.read_int32()?,
            smallest_screen_width_dp: p.read_int32()?,
            density_dpi: p.read_int32()?,
            compat_screen_width_dp: p.read_int32()?,
            compat_screen_height_dp: p.read_int32()?,
            compat_smallest_screen_width_dp: p.read_int32()?,
            window_configuration: WindowConfiguration::read_from_parcel(p)?,
            assets_seq: p.read_int32()?,
            seq: p.read_int32()?,
            font_weight_adjustment: p.read_int32()?,
            grammatical_gender: p.read_int32()?,
        };
        Ok(value)
    }
}

/// `android.app.WindowConfiguration` (nested in `Configuration`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WindowConfiguration {
    pub bounds: RectI,
    pub app_bounds: Option<RectI>,
    pub max_bounds: RectI,
    pub windowing_mode: i32,
    pub activity_type: i32,
    pub always_on_top: i32,
    pub rotation: i32,
    pub display_windowing_mode: i32,
    pub display_rotation: i32,
}

impl ParcelCodec for WindowConfiguration {
    const NAME: &'static str = "android.app.WindowConfiguration";

    fn write_to_parcel(&self, p: &mut Parcel) -> Result<()> {
        self.bounds.write_to_parcel(p)?;
        write_typed_object(p, self.app_bounds.as_ref())?;
        self.max_bounds.write_to_parcel(p)?;
        p.write_int32(self.windowing_mode);
        p.write_int32(self.activity_type);
        p.write_int32(self.always_on_top);
        p.write_int32(self.rotation);
        p.write_int32(self.display_windowing_mode);
        p.write_int32(self.display_rotation);
        Ok(())
    }

    fn read_from_parcel(p: &mut Parcel) -> Result<Self> {
        let bounds = RectI::read_from_parcel(p)?;
        let app_bounds = read_typed_object::<RectI>(p)?;
        let max_bounds = RectI::read_from_parcel(p)?;
        Ok(WindowConfiguration {
            bounds,
            app_bounds,
            max_bounds,
            windowing_mode: p.read_int32()?,
            activity_type: p.read_int32()?,
            always_on_top: p.read_int32()?,
            rotation: p.read_int32()?,
            display_windowing_mode: p.read_int32()?,
            display_rotation: p.read_int32()?,
        })
    }
}

/// `android.util.MergedConfiguration`: global, override, merged.
#[derive(Debug, Clone, PartialEq)]
pub struct MergedConfiguration {
    pub global: Configuration,
    pub override_config: Configuration,
    pub merged: Configuration,
}

impl ParcelCodec for MergedConfiguration {
    const NAME: &'static str = "android.util.MergedConfiguration";

    fn write_to_parcel(&self, p: &mut Parcel) -> Result<()> {
        self.global.write_to_parcel(p)?;
        self.override_config.write_to_parcel(p)?;
        self.merged.write_to_parcel(p)?;
        Ok(())
    }

    fn read_from_parcel(p: &mut Parcel) -> Result<Self> {
        Ok(MergedConfiguration {
            global: Configuration::read_from_parcel(p)?,
            override_config: Configuration::read_from_parcel(p)?,
            merged: Configuration::read_from_parcel(p)?,
        })
    }
}

// ---------------------------------------------------------------------------
// ClientWindowFrames
// ---------------------------------------------------------------------------

/// `android.window.ClientWindowFrames`.
///
/// DEX order: `frame`, `displayFrame`, `parentFrame` (each a `Rect`),
/// `attachedFrame` (`writeTypedObject`), `isParentFrameClippedByDisplayCutout`
/// (`writeBoolean`), `compatScale` (`writeFloat`).
#[derive(Debug, Clone, PartialEq)]
pub struct ClientWindowFrames {
    pub frame: RectI,
    pub display_frame: RectI,
    pub parent_frame: RectI,
    pub attached_frame: Option<RectI>,
    pub is_parent_frame_clipped_by_display_cutout: bool,
    pub compat_scale: f32,
}

impl ParcelCodec for ClientWindowFrames {
    const NAME: &'static str = "android.window.ClientWindowFrames";

    fn write_to_parcel(&self, p: &mut Parcel) -> Result<()> {
        self.frame.write_to_parcel(p)?;
        self.display_frame.write_to_parcel(p)?;
        self.parent_frame.write_to_parcel(p)?;
        write_typed_object(p, self.attached_frame.as_ref())?;
        p.write_bool(self.is_parent_frame_clipped_by_display_cutout);
        p.write_float(self.compat_scale);
        Ok(())
    }

    fn read_from_parcel(p: &mut Parcel) -> Result<Self> {
        Ok(ClientWindowFrames {
            frame: RectI::read_from_parcel(p)?,
            display_frame: RectI::read_from_parcel(p)?,
            parent_frame: RectI::read_from_parcel(p)?,
            attached_frame: read_typed_object::<RectI>(p)?,
            is_parent_frame_clipped_by_display_cutout: p.read_bool()?,
            compat_scale: p.read_float()?,
        })
    }
}

// ---------------------------------------------------------------------------
// SurfaceControl
// ---------------------------------------------------------------------------

/// `android.view.SurfaceControl` as it crosses the Parcel.
///
/// Java writes `mName` (`String8`), `mWidth`/`mHeight` (`writeInt`), a non-null
/// marker, then `nativeWriteToParcel`. ACOS's native layer substitutes an
/// 8-byte layer handle for the native pointer (the real `nativeWriteToParcel`
/// bytes are a libgui object; see `results/wm/README.md`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SurfaceControlInfo {
    pub name: String,
    pub width: i32,
    pub height: i32,
    pub handle: Option<u64>,
}

impl ParcelCodec for SurfaceControlInfo {
    const NAME: &'static str = "android.view.SurfaceControl";

    fn write_to_parcel(&self, p: &mut Parcel) -> Result<()> {
        p.write_string8(&self.name)?;
        p.write_int32(self.width);
        p.write_int32(self.height);
        match self.handle {
            Some(_) => p.write_int32(1),
            None => p.write_int32(0),
        }
        p.write_int64(self.handle.unwrap_or(0) as i64);
        Ok(())
    }

    fn read_from_parcel(p: &mut Parcel) -> Result<Self> {
        let name = p.read_string8()?.unwrap_or_default();
        let width = p.read_int32()?;
        let height = p.read_int32()?;
        let present = p.read_int32()? != 0;
        let raw = p.read_int64()? as u64;
        Ok(SurfaceControlInfo {
            name,
            width,
            height,
            handle: if present && raw != 0 { Some(raw) } else { None },
        })
    }
}

// ---------------------------------------------------------------------------
// DisplayCutout
// ---------------------------------------------------------------------------

/// `DisplayCutout$CutoutPathParserInfo`.
#[derive(Debug, Clone, PartialEq)]
pub struct CutoutPathParserInfo {
    pub display_width: i32,
    pub display_height: i32,
    pub physical_display_width: i32,
    pub physical_display_height: i32,
    pub density: f32,
    pub cutout_spec: String,
    pub rotation: i32,
    pub scale: f32,
    pub physical_pixel_display_size_ratio: f32,
}

impl ParcelCodec for CutoutPathParserInfo {
    const NAME: &'static str = "android.view.DisplayCutout$CutoutPathParserInfo";

    fn write_to_parcel(&self, p: &mut Parcel) -> Result<()> {
        p.write_int32(self.display_width);
        p.write_int32(self.display_height);
        p.write_int32(self.physical_display_width);
        p.write_int32(self.physical_display_height);
        p.write_float(self.density);
        p.write_string16(&self.cutout_spec)?;
        p.write_int32(self.rotation);
        p.write_float(self.scale);
        p.write_float(self.physical_pixel_display_size_ratio);
        Ok(())
    }

    fn read_from_parcel(p: &mut Parcel) -> Result<Self> {
        Ok(CutoutPathParserInfo {
            display_width: p.read_int32()?,
            display_height: p.read_int32()?,
            physical_display_width: p.read_int32()?,
            physical_display_height: p.read_int32()?,
            density: p.read_float()?,
            cutout_spec: p.read_string16()?.unwrap_or_default(),
            rotation: p.read_int32()?,
            scale: p.read_float()?,
            physical_pixel_display_size_ratio: p.read_float()?,
        })
    }
}

/// `android.view.DisplayCutout`.
#[derive(Debug, Clone, PartialEq)]
pub struct DisplayCutout {
    pub safe_insets: Option<RectI>,
    pub bounds: Vec<RectI>,
    pub waterfall_insets: Option<InsetsI>,
    pub path_parser_info: CutoutPathParserInfo,
}

impl ParcelCodec for DisplayCutout {
    const NAME: &'static str = "android.view.DisplayCutout";

    fn write_to_parcel(&self, p: &mut Parcel) -> Result<()> {
        write_typed_object(p, self.safe_insets.as_ref())?;
        write_typed_array(p, Some(self.bounds.as_slice()))?;
        write_typed_object(p, self.waterfall_insets.as_ref())?;
        self.path_parser_info.write_to_parcel(p)
    }

    fn read_from_parcel(p: &mut Parcel) -> Result<Self> {
        Ok(DisplayCutout {
            safe_insets: read_typed_object::<RectI>(p)?,
            bounds: read_typed_array::<RectI>(p)?.unwrap_or_default(),
            waterfall_insets: read_typed_object::<InsetsI>(p)?,
            path_parser_info: CutoutPathParserInfo::read_from_parcel(p)?,
        })
    }
}

/// `DisplayCutout$ParcelableWrapper`: `-1` null, `0` NO_CUTOUT, `1` present.
#[derive(Debug, Clone, PartialEq)]
pub enum Cutout {
    /// `writeInt(-1)`.
    Null,
    /// `writeInt(0)`.
    NoCutout,
    /// `writeInt(1)` + body.
    Present(DisplayCutout),
}

impl ParcelCodec for Cutout {
    const NAME: &'static str = "android.view.DisplayCutout$ParcelableWrapper";

    fn write_to_parcel(&self, p: &mut Parcel) -> Result<()> {
        match self {
            Cutout::Null => {
                p.write_int32(-1);
                Ok(())
            }
            Cutout::NoCutout => {
                p.write_int32(0);
                Ok(())
            }
            Cutout::Present(c) => {
                p.write_int32(1);
                c.write_to_parcel(p)
            }
        }
    }

    fn read_from_parcel(p: &mut Parcel) -> Result<Self> {
        Ok(match p.read_int32()? {
            -1 => Cutout::Null,
            0 => Cutout::NoCutout,
            _ => Cutout::Present(DisplayCutout::read_from_parcel(p)?),
        })
    }
}

// ---------------------------------------------------------------------------
// RoundedCorners / PrivacyIndicatorBounds / DisplayShape
// ---------------------------------------------------------------------------

/// `android.view.RoundedCorner`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RoundedCorner {
    pub position: i32,
    pub radius: i32,
    pub center: PointI,
}

impl ParcelCodec for RoundedCorner {
    const NAME: &'static str = "android.view.RoundedCorner";

    fn write_to_parcel(&self, p: &mut Parcel) -> Result<()> {
        p.write_int32(self.position);
        p.write_int32(self.radius);
        p.write_int32(self.center.x);
        p.write_int32(self.center.y);
        Ok(())
    }

    fn read_from_parcel(p: &mut Parcel) -> Result<Self> {
        Ok(RoundedCorner {
            position: p.read_int32()?,
            radius: p.read_int32()?,
            center: PointI::new(p.read_int32()?, p.read_int32()?),
        })
    }
}

/// `android.view.RoundedCorners`: `0` = NO_ROUNDED_CORNERS, else typed array.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoundedCorners {
    /// The sentinel (`writeInt(0)`).
    None,
    /// A real corner set (`writeInt(1)` + typed array).
    Corners(Vec<RoundedCorner>),
}

impl ParcelCodec for RoundedCorners {
    const NAME: &'static str = "android.view.RoundedCorners";

    fn write_to_parcel(&self, p: &mut Parcel) -> Result<()> {
        match self {
            RoundedCorners::None => {
                p.write_int32(0);
                Ok(())
            }
            RoundedCorners::Corners(c) => {
                p.write_int32(1);
                write_typed_array(p, Some(c.as_slice()))
            }
        }
    }

    fn read_from_parcel(p: &mut Parcel) -> Result<Self> {
        if p.read_int32()? == 0 {
            Ok(RoundedCorners::None)
        } else {
            Ok(RoundedCorners::Corners(
                read_typed_array::<RoundedCorner>(p)?.unwrap_or_default(),
            ))
        }
    }
}

/// `android.view.PrivacyIndicatorBounds`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrivacyIndicatorBounds {
    pub static_bounds: Vec<RectI>,
    pub rotation: i32,
}

impl ParcelCodec for PrivacyIndicatorBounds {
    const NAME: &'static str = "android.view.PrivacyIndicatorBounds";

    fn write_to_parcel(&self, p: &mut Parcel) -> Result<()> {
        write_typed_array(p, Some(self.static_bounds.as_slice()))?;
        p.write_int32(self.rotation);
        Ok(())
    }

    fn read_from_parcel(p: &mut Parcel) -> Result<Self> {
        Ok(PrivacyIndicatorBounds {
            static_bounds: read_typed_array::<RectI>(p)?.unwrap_or_default(),
            rotation: p.read_int32()?,
        })
    }
}

/// `android.view.DisplayShape`.
#[derive(Debug, Clone, PartialEq)]
pub struct DisplayShape {
    pub spec: String,
    pub display_width: i32,
    pub display_height: i32,
    pub physical_pixel_display_size_ratio: f32,
    pub rotation: i32,
    pub offset_x: i32,
    pub offset_y: i32,
    pub scale: f32,
}

impl DisplayShape {
    /// `DisplayShape.NONE`: the framework's rectangular-display singleton.
    ///
    /// `InsetsState.readFromParcel` assigns `mDisplayShape` from the parcel and
    /// `calculateRelativeDisplayShape` dereferences it unconditionally, so the
    /// field can never be a null parcel object.
    #[must_use]
    pub fn none() -> Self {
        DisplayShape {
            spec: String::new(),
            display_width: 0,
            display_height: 0,
            physical_pixel_display_size_ratio: 0.0,
            rotation: 0,
            offset_x: 0,
            offset_y: 0,
            scale: 1.0,
        }
    }
}

impl ParcelCodec for DisplayShape {
    const NAME: &'static str = "android.view.DisplayShape";

    fn write_to_parcel(&self, p: &mut Parcel) -> Result<()> {
        p.write_string8(&self.spec)?;
        p.write_int32(self.display_width);
        p.write_int32(self.display_height);
        p.write_float(self.physical_pixel_display_size_ratio);
        p.write_int32(self.rotation);
        p.write_int32(self.offset_x);
        p.write_int32(self.offset_y);
        p.write_float(self.scale);
        Ok(())
    }

    fn read_from_parcel(p: &mut Parcel) -> Result<Self> {
        Ok(DisplayShape {
            spec: p.read_string8()?.unwrap_or_default(),
            display_width: p.read_int32()?,
            display_height: p.read_int32()?,
            physical_pixel_display_size_ratio: p.read_float()?,
            rotation: p.read_int32()?,
            offset_x: p.read_int32()?,
            offset_y: p.read_int32()?,
            scale: p.read_float()?,
        })
    }
}

// ---------------------------------------------------------------------------
// InsetsState / InsetsSource / InsetsSourceControl
// ---------------------------------------------------------------------------

/// The `WindowInsets.Type` public bit values an `InsetsSource` carries.
///
/// `InsetsSource.mType` is an `@InsetsType` value: the framework compares it
/// against `WindowInsets.Type.*` (`InsetsState.processSource` /
/// `WindowInsets$Type.indexOf`) and `InsetsSource.createId` rejects anything
/// that is not a single `Type` bit. The DEX-derived values are
/// `STATUS_BARS=1, NAVIGATION_BARS=2, CAPTION_BAR=4, IME=8, SYSTEM_GESTURES=16`,
/// `MANDATORY_SYSTEM_GESTURES=32, TAPPABLE_ELEMENT=64, DISPLAY_CUTOUT=128`,
/// `WINDOW_DECOR=256, SYSTEM_OVERLAYS=512`. (These are *not* the legacy
/// `ITYPE_*` indices a `dumpsys` prints: the wire format stores the public
/// type, which is what `WindowInsets$Type.indexOf` accepts.)
pub mod inset_type {
    /// `WindowInsets.Type.statusBars()`.
    pub const STATUS_BAR: i32 = 1;
    /// `WindowInsets.Type.navigationBars()`.
    pub const NAVIGATION_BAR: i32 = 2;
    /// `WindowInsets.Type.captionBar()`.
    pub const CAPTION_BAR: i32 = 4;
    /// `WindowInsets.Type.systemGestures()` (top strip).
    pub const TOP_GESTURES: i32 = 16;
    /// `WindowInsets.Type.systemGestures()` (bottom strip).
    pub const BOTTOM_GESTURES: i32 = 16;
    /// `WindowInsets.Type.systemGestures()` (left strip).
    pub const LEFT_GESTURES: i32 = 16;
    /// `WindowInsets.Type.systemGestures()` (right strip).
    pub const RIGHT_GESTURES: i32 = 16;
    /// `WindowInsets.Type.tappableElement()` (status-bar strip).
    pub const TOP_TAPPABLE_ELEMENT: i32 = 64;
    /// `WindowInsets.Type.tappableElement()` (nav-bar strip).
    pub const BOTTOM_TAPPABLE_ELEMENT: i32 = 64;
    /// `WindowInsets.Type.ime()`.
    pub const IME: i32 = 8;
    /// `WindowInsets.Type.displayCutout()`.
    pub const DISPLAY_CUTOUT: i32 = 128;
    /// `WindowInsets.Type.systemGestures()`.
    pub const SYSTEM_GESTURES: i32 = 16;
    /// `WindowInsets.Type.mandatorySystemGestures()`.
    pub const MANDATORY_SYSTEM_GESTURES: i32 = 32;
}

/// `android.view.InsetsSource`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InsetsSource {
    pub id: i32,
    pub type_: i32,
    pub frame: RectI,
    pub visible_frame: Option<RectI>,
    pub visible: bool,
    pub flags: i32,
    pub insets_rounded_corner_frame: bool,
}

impl InsetsSource {
    /// A source with `type_` matching the reference dumpsys.
    #[must_use]
    pub fn new(id: i32, type_: i32, frame: RectI, visible: bool) -> Self {
        Self {
            id,
            type_,
            frame,
            visible_frame: None,
            visible,
            flags: 0,
            insets_rounded_corner_frame: false,
        }
    }
}

impl ParcelCodec for InsetsSource {
    const NAME: &'static str = "android.view.InsetsSource";

    fn write_to_parcel(&self, p: &mut Parcel) -> Result<()> {
        p.write_int32(self.id);
        p.write_int32(self.type_);
        self.frame.write_to_parcel(p)?;
        write_typed_object(p, self.visible_frame.as_ref())?;
        p.write_bool(self.visible);
        p.write_int32(self.flags);
        p.write_bool(self.insets_rounded_corner_frame);
        Ok(())
    }

    fn read_from_parcel(p: &mut Parcel) -> Result<Self> {
        Ok(InsetsSource {
            id: p.read_int32()?,
            type_: p.read_int32()?,
            frame: RectI::read_from_parcel(p)?,
            visible_frame: read_typed_object::<RectI>(p)?,
            visible: p.read_bool()?,
            flags: p.read_int32()?,
            insets_rounded_corner_frame: p.read_bool()?,
        })
    }
}

/// `android.view.InsetsSourceControl`.
#[derive(Debug, Clone, PartialEq)]
pub struct InsetsSourceControl {
    pub id: i32,
    pub type_: i32,
    pub leash: Option<SurfaceControlInfo>,
    pub initially_visible: bool,
    pub surface_position: Option<PointI>,
    pub insets_hint: Option<InsetsI>,
    pub skip_animation_once: bool,
}

impl ParcelCodec for InsetsSourceControl {
    const NAME: &'static str = "android.view.InsetsSourceControl";

    fn write_to_parcel(&self, p: &mut Parcel) -> Result<()> {
        p.write_int32(self.id);
        p.write_int32(self.type_);
        write_typed_object(p, self.leash.as_ref())?;
        p.write_bool(self.initially_visible);
        write_typed_object(p, self.surface_position.as_ref())?;
        write_typed_object(p, self.insets_hint.as_ref())?;
        p.write_bool(self.skip_animation_once);
        Ok(())
    }

    fn read_from_parcel(p: &mut Parcel) -> Result<Self> {
        Ok(InsetsSourceControl {
            id: p.read_int32()?,
            type_: p.read_int32()?,
            leash: read_typed_object::<SurfaceControlInfo>(p)?,
            initially_visible: p.read_bool()?,
            surface_position: read_typed_object::<PointI>(p)?,
            insets_hint: read_typed_object::<InsetsI>(p)?,
            skip_animation_once: p.read_bool()?,
        })
    }
}

/// `android.view.InsetsSourceControl$Array`: a typed array of controls.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct InsetsSourceControlArray(pub Vec<InsetsSourceControl>);

impl ParcelCodec for InsetsSourceControlArray {
    const NAME: &'static str = "android.view.InsetsSourceControl$Array";

    fn write_to_parcel(&self, p: &mut Parcel) -> Result<()> {
        write_typed_array(p, Some(self.0.as_slice()))
    }

    fn read_from_parcel(p: &mut Parcel) -> Result<Self> {
        Ok(InsetsSourceControlArray(
            read_typed_array::<InsetsSourceControl>(p)?.unwrap_or_default(),
        ))
    }
}

/// `android.view.InsetsState`.
#[derive(Debug, Clone, PartialEq)]
pub struct InsetsState {
    pub display_frame: RectI,
    pub display_cutout: Cutout,
    pub rounded_corners: Option<RoundedCorners>,
    pub rounded_corner_frame: RectI,
    pub privacy_indicator_bounds: Option<PrivacyIndicatorBounds>,
    pub display_shape: Option<DisplayShape>,
    pub sources: Vec<InsetsSource>,
}

impl InsetsState {
    /// The edge thickness contributed by the visible sources of `type_`.
    ///
    /// This is the reduced form of `InsetsState.calculateInsets` for a source
    /// that occupies one display edge: a source touching the top contributes its
    /// height to `top`, one touching the bottom contributes its height to
    /// `bottom`, and so on. A full-span gesture source (e.g. the left/right
    /// gesture strips) contributes to its own edge only.
    #[must_use]
    pub fn visible_insets_for(&self, type_: i32) -> InsetsI {
        let d = self.display_frame;
        let mut out = InsetsI::default();
        for s in self.sources.iter().filter(|s| s.type_ == type_ && s.visible) {
            let f = s.frame;
            if f.top <= d.top && f.bottom < d.bottom {
                out.top = out.top.max(f.bottom - d.top);
            }
            if f.bottom >= d.bottom && f.top > d.top {
                out.bottom = out.bottom.max(d.bottom - f.top);
            }
            if f.left <= d.left && f.right < d.right {
                out.left = out.left.max(f.right - d.left);
            }
            if f.right >= d.right && f.left > d.left {
                out.right = out.right.max(d.right - f.left);
            }
        }
        out
    }
}

impl ParcelCodec for InsetsState {
    const NAME: &'static str = "android.view.InsetsState";

    fn write_to_parcel(&self, p: &mut Parcel) -> Result<()> {
        self.display_frame.write_to_parcel(p)?;
        self.display_cutout.write_to_parcel(p)?;
        write_typed_object(p, self.rounded_corners.as_ref())?;
        self.rounded_corner_frame.write_to_parcel(p)?;
        write_typed_object(p, self.privacy_indicator_bounds.as_ref())?;
        write_typed_object(p, self.display_shape.as_ref())?;
        // mSources: SparseArray.size() written as an int, then valueAt(i) typed.
        p.write_int32(self.sources.len() as i32);
        for s in &self.sources {
            write_typed_object(p, Some(s))?;
        }
        Ok(())
    }

    fn read_from_parcel(p: &mut Parcel) -> Result<Self> {
        let display_frame = RectI::read_from_parcel(p)?;
        let display_cutout = Cutout::read_from_parcel(p)?;
        let rounded_corners = read_typed_object::<RoundedCorners>(p)?;
        let rounded_corner_frame = RectI::read_from_parcel(p)?;
        let privacy_indicator_bounds = read_typed_object::<PrivacyIndicatorBounds>(p)?;
        let display_shape = read_typed_object::<DisplayShape>(p)?;
        let n = p.read_int32()?;
        let mut sources = Vec::with_capacity(n.max(0) as usize);
        for _ in 0..n {
            sources.push(
                read_typed_object::<InsetsSource>(p)?
                    .ok_or_else(|| codec_err(InsetsState::NAME, "null InsetsSource"))?,
            );
        }
        Ok(InsetsState {
            display_frame,
            display_cutout,
            rounded_corners,
            rounded_corner_frame,
            privacy_indicator_bounds,
            display_shape,
            sources,
        })
    }
}

// ---------------------------------------------------------------------------
// InputChannel
// ---------------------------------------------------------------------------

/// `android.view.InputChannel` as it crosses the Parcel.
///
/// The real `nativeWriteToParcel` writes a libinput `InputChannel` (a client fd
/// and a token binder). ACOS owns both sides of this ABI, so the codec carries
/// the *handle* of a live channel in [`crate::transport::InputChannelRegistry`]
/// rather than a raw fd; the native layer's `nativeWriteToParcel` resolves the
/// handle to the real socketpair descriptor. Layout: `present`, `String8 name`,
/// `int32 channel_handle`, token binder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputChannelParcel {
    pub name: String,
    pub present: bool,
    pub channel_handle: i32,
    pub token: Option<BinderId>,
}

impl InputChannelParcel {
    /// An absent channel (`writeInt(0)`).
    #[must_use]
    pub const fn absent() -> Self {
        Self {
            name: String::new(),
            present: false,
            channel_handle: -1,
            token: None,
        }
    }
}

impl ParcelCodec for InputChannelParcel {
    const NAME: &'static str = "android.view.InputChannel";

    fn write_to_parcel(&self, p: &mut Parcel) -> Result<()> {
        if !self.present {
            p.write_int32(0);
            return Ok(());
        }
        p.write_int32(1);
        p.write_string8(&self.name)?;
        p.write_int32(self.channel_handle);
        p.write_strong_binder_opt(self.token);
        Ok(())
    }

    fn read_from_parcel(p: &mut Parcel) -> Result<Self> {
        if p.read_int32()? == 0 {
            return Ok(InputChannelParcel::absent());
        }
        let name = p.read_string8()?.unwrap_or_default();
        let channel_handle = p.read_int32()?;
        let token = p.read_strong_binder()?;
        Ok(InputChannelParcel {
            name,
            present: true,
            channel_handle,
            token,
        })
    }
}

// ---------------------------------------------------------------------------
// WindowManager.LayoutParams
// ---------------------------------------------------------------------------

/// `android.view.InsetsFrameProvider` (nested in `LayoutParams.providedInsets`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InsetsFrameProvider {
    pub id: i32,
    pub source: i32,
    pub flags: i32,
    pub insets_size: Option<InsetsI>,
    /// `InsetsSizeOverride[]`; ACOS never populates it for app windows.
    pub insets_size_overrides: Option<Vec<InsetsSizeOverride>>,
    pub arbitrary_rectangle: Option<RectI>,
    pub minimal_insets_size_in_display_cutout_safe: Option<InsetsI>,
}

/// `InsetsFrameProvider$InsetsSizeOverride`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InsetsSizeOverride {
    pub inset_type: i32,
    pub insets_size: InsetsI,
}

impl ParcelCodec for InsetsSizeOverride {
    const NAME: &'static str = "android.view.InsetsFrameProvider$InsetsSizeOverride";

    fn write_to_parcel(&self, p: &mut Parcel) -> Result<()> {
        p.write_int32(self.inset_type);
        self.insets_size.write_to_parcel(p)
    }

    fn read_from_parcel(p: &mut Parcel) -> Result<Self> {
        Ok(InsetsSizeOverride {
            inset_type: p.read_int32()?,
            insets_size: InsetsI::read_from_parcel(p)?,
        })
    }
}

impl ParcelCodec for InsetsFrameProvider {
    const NAME: &'static str = "android.view.InsetsFrameProvider";

    fn write_to_parcel(&self, p: &mut Parcel) -> Result<()> {
        p.write_int32(self.id);
        p.write_int32(self.source);
        p.write_int32(self.flags);
        write_typed_object(p, self.insets_size.as_ref())?;
        write_typed_array(p, self.insets_size_overrides.as_deref())?;
        write_typed_object(p, self.arbitrary_rectangle.as_ref())?;
        write_typed_object(p, self.minimal_insets_size_in_display_cutout_safe.as_ref())
    }

    fn read_from_parcel(p: &mut Parcel) -> Result<Self> {
        Ok(InsetsFrameProvider {
            id: p.read_int32()?,
            source: p.read_int32()?,
            flags: p.read_int32()?,
            insets_size: read_typed_object::<InsetsI>(p)?,
            insets_size_overrides: read_typed_array::<InsetsSizeOverride>(p)?,
            arbitrary_rectangle: read_typed_object::<RectI>(p)?,
            minimal_insets_size_in_display_cutout_safe: read_typed_object::<InsetsI>(p)?,
        })
    }
}

/// `android.view.WindowManager$LayoutParams` (read side for requests, write side
/// for `paramsForRotation`). The full 56-step field order is transcribed from
/// `LayoutParams.writeToParcel`; see `results/wm/parcelable_layouts.txt`.
#[derive(Debug, Clone, PartialEq)]
pub struct LayoutParams {
    pub width: i32,
    pub height: i32,
    pub x: i32,
    pub y: i32,
    pub type_: i32,
    pub flags: i32,
    pub private_flags: i32,
    pub soft_input_mode: i32,
    pub layout_in_display_cutout_mode: i32,
    pub gravity: i32,
    pub horizontal_margin: f32,
    pub vertical_margin: f32,
    pub format: i32,
    pub window_animations: i32,
    pub alpha: f32,
    pub dim_amount: f32,
    pub screen_brightness: f32,
    pub button_brightness: f32,
    pub rotation_animation: i32,
    pub token: Option<BinderId>,
    pub window_context_token: Option<BinderId>,
    pub package_name: Option<String>,
    pub title: Option<String>,
    pub screen_orientation: i32,
    pub preferred_refresh_rate: f32,
    pub preferred_display_mode_id: i32,
    pub preferred_min_display_refresh_rate: f32,
    pub preferred_max_display_refresh_rate: f32,
    pub system_ui_visibility: i32,
    pub subtree_system_ui_visibility: i32,
    pub has_system_ui_listeners: bool,
    pub input_features: i32,
    pub user_activity_timeout: i64,
    pub surface_insets: RectI,
    pub has_manual_surface_insets: bool,
    pub receive_insets_ignoring_z_order: bool,
    pub preserve_previous_surface_insets: bool,
    pub accessibility_id_of_anchor: i64,
    pub accessibility_title: Option<String>,
    pub color_mode: i32,
    pub hide_timeout_milliseconds: i64,
    pub insets_appearance: i32,
    pub insets_behavior: i32,
    pub fit_insets_types: i32,
    pub fit_insets_sides: i32,
    pub fit_insets_ignoring_visibility: bool,
    pub prefer_minimal_post_processing: bool,
    pub blur_behind_radius: i32,
    pub insets_rounded_corner_frame: bool,
    pub wallpaper_touch_events_enabled: bool,
    pub provided_insets: Option<Vec<InsetsFrameProvider>>,
    pub params_for_rotation: Option<Vec<LayoutParams>>,
    pub display_flags: i32,
}

impl Default for LayoutParams {
    fn default() -> Self {
        Self {
            width: -1,
            height: -1,
            x: 0,
            y: 0,
            type_: 2, // TYPE_APPLICATION
            flags: 0,
            private_flags: 0,
            soft_input_mode: 0,
            layout_in_display_cutout_mode: 0,
            gravity: 0,
            horizontal_margin: 0.0,
            vertical_margin: 0.0,
            format: -1,
            window_animations: 0,
            alpha: 1.0,
            dim_amount: 0.0,
            screen_brightness: -1.0,
            button_brightness: -1.0,
            rotation_animation: -1,
            token: None,
            window_context_token: None,
            package_name: None,
            title: None,
            screen_orientation: -1,
            preferred_refresh_rate: 0.0,
            preferred_display_mode_id: 0,
            preferred_min_display_refresh_rate: 0.0,
            preferred_max_display_refresh_rate: 0.0,
            system_ui_visibility: 0,
            subtree_system_ui_visibility: 0,
            has_system_ui_listeners: false,
            input_features: 0,
            user_activity_timeout: -1,
            surface_insets: RectI::new(0, 0, 0, 0),
            has_manual_surface_insets: false,
            receive_insets_ignoring_z_order: false,
            preserve_previous_surface_insets: false,
            accessibility_id_of_anchor: 0,
            accessibility_title: None,
            color_mode: 0,
            hide_timeout_milliseconds: -1,
            insets_appearance: 0,
            insets_behavior: 0,
            fit_insets_types: 0,
            fit_insets_sides: 0,
            fit_insets_ignoring_visibility: false,
            prefer_minimal_post_processing: false,
            blur_behind_radius: 0,
            insets_rounded_corner_frame: false,
            wallpaper_touch_events_enabled: false,
            provided_insets: None,
            params_for_rotation: None,
            display_flags: 0,
        }
    }
}

impl LayoutParams {
    /// A normal full-screen application window for the reference device.
    #[must_use]
    pub fn application(package: &str, title: &str) -> Self {
        Self {
            package_name: Some(package.to_string()),
            title: Some(title.to_string()),
            format: 1, // PixelFormat.OPAQUE (RGBA_8888 variants)
            ..Self::default()
        }
    }

    /// `true` when the window is a system window (`FIRST_SYSTEM_WINDOW..=LAST`).
    #[must_use]
    pub const fn is_system_window(&self) -> bool {
        self.type_ >= 2000 && self.type_ <= 2999
    }

    /// `true` when the layout requests fixed dimensions.
    #[must_use]
    pub const fn has_fixed_size(&self) -> bool {
        self.width >= 0 && self.height >= 0
    }
}

impl ParcelCodec for LayoutParams {
    const NAME: &'static str = "android.view.WindowManager$LayoutParams";

    fn write_to_parcel(&self, p: &mut Parcel) -> Result<()> {
        p.write_int32(self.width);
        p.write_int32(self.height);
        p.write_int32(self.x);
        p.write_int32(self.y);
        p.write_int32(self.type_);
        p.write_int32(self.flags);
        p.write_int32(self.private_flags);
        p.write_int32(self.soft_input_mode);
        p.write_int32(self.layout_in_display_cutout_mode);
        p.write_int32(self.gravity);
        p.write_float(self.horizontal_margin);
        p.write_float(self.vertical_margin);
        p.write_int32(self.format);
        p.write_int32(self.window_animations);
        p.write_float(self.alpha);
        p.write_float(self.dim_amount);
        p.write_float(self.screen_brightness);
        p.write_float(self.button_brightness);
        p.write_int32(self.rotation_animation);
        p.write_strong_binder_opt(self.token);
        p.write_strong_binder_opt(self.window_context_token);
        p.write_string16_opt(self.package_name.as_deref())?;
        write_char_sequence(p, self.title.as_deref())?;
        p.write_int32(self.screen_orientation);
        p.write_float(self.preferred_refresh_rate);
        p.write_int32(self.preferred_display_mode_id);
        p.write_float(self.preferred_min_display_refresh_rate);
        p.write_float(self.preferred_max_display_refresh_rate);
        p.write_int32(self.system_ui_visibility);
        p.write_int32(self.subtree_system_ui_visibility);
        p.write_bool(self.has_system_ui_listeners);
        p.write_int32(self.input_features);
        p.write_int64(self.user_activity_timeout);
        p.write_int32(self.surface_insets.left);
        p.write_int32(self.surface_insets.top);
        p.write_int32(self.surface_insets.right);
        p.write_int32(self.surface_insets.bottom);
        p.write_bool(self.has_manual_surface_insets);
        p.write_bool(self.receive_insets_ignoring_z_order);
        p.write_bool(self.preserve_previous_surface_insets);
        p.write_int64(self.accessibility_id_of_anchor);
        write_char_sequence(p, self.accessibility_title.as_deref())?;
        p.write_int32(self.color_mode);
        p.write_int64(self.hide_timeout_milliseconds);
        p.write_int32(self.insets_appearance);
        p.write_int32(self.insets_behavior);
        p.write_int32(self.fit_insets_types);
        p.write_int32(self.fit_insets_sides);
        p.write_bool(self.fit_insets_ignoring_visibility);
        p.write_bool(self.prefer_minimal_post_processing);
        p.write_int32(self.blur_behind_radius);
        p.write_bool(self.insets_rounded_corner_frame);
        p.write_bool(self.wallpaper_touch_events_enabled);
        write_typed_array(p, self.provided_insets.as_deref())?;
        write_typed_array(p, self.params_for_rotation.as_deref())?;
        p.write_int32(self.display_flags);
        Ok(())
    }

    fn read_from_parcel(p: &mut Parcel) -> Result<Self> {
        let width = p.read_int32()?;
        let height = p.read_int32()?;
        let x = p.read_int32()?;
        let y = p.read_int32()?;
        let type_ = p.read_int32()?;
        let flags = p.read_int32()?;
        let private_flags = p.read_int32()?;
        let soft_input_mode = p.read_int32()?;
        let layout_in_display_cutout_mode = p.read_int32()?;
        let gravity = p.read_int32()?;
        let horizontal_margin = p.read_float()?;
        let vertical_margin = p.read_float()?;
        let format = p.read_int32()?;
        let window_animations = p.read_int32()?;
        let alpha = p.read_float()?;
        let dim_amount = p.read_float()?;
        let screen_brightness = p.read_float()?;
        let button_brightness = p.read_float()?;
        let rotation_animation = p.read_int32()?;
        let token = p.read_strong_binder()?;
        let window_context_token = p.read_strong_binder()?;
        let package_name = p.read_string16()?;
        let title = read_char_sequence(p)?;
        let screen_orientation = p.read_int32()?;
        let preferred_refresh_rate = p.read_float()?;
        let preferred_display_mode_id = p.read_int32()?;
        let preferred_min_display_refresh_rate = p.read_float()?;
        let preferred_max_display_refresh_rate = p.read_float()?;
        let system_ui_visibility = p.read_int32()?;
        let subtree_system_ui_visibility = p.read_int32()?;
        let has_system_ui_listeners = p.read_bool()?;
        let input_features = p.read_int32()?;
        let user_activity_timeout = p.read_int64()?;
        let surface_insets = RectI::new(
            p.read_int32()?,
            p.read_int32()?,
            p.read_int32()?,
            p.read_int32()?,
        );
        let has_manual_surface_insets = p.read_bool()?;
        let receive_insets_ignoring_z_order = p.read_bool()?;
        let preserve_previous_surface_insets = p.read_bool()?;
        let accessibility_id_of_anchor = p.read_int64()?;
        let accessibility_title = read_char_sequence(p)?;
        let color_mode = p.read_int32()?;
        let hide_timeout_milliseconds = p.read_int64()?;
        let insets_appearance = p.read_int32()?;
        let insets_behavior = p.read_int32()?;
        let fit_insets_types = p.read_int32()?;
        let fit_insets_sides = p.read_int32()?;
        let fit_insets_ignoring_visibility = p.read_bool()?;
        let prefer_minimal_post_processing = p.read_bool()?;
        let blur_behind_radius = p.read_int32()?;
        let insets_rounded_corner_frame = p.read_bool()?;
        let wallpaper_touch_events_enabled = p.read_bool()?;
        let provided_insets = read_typed_array::<InsetsFrameProvider>(p)?;
        let params_for_rotation = read_typed_array::<LayoutParams>(p)?;
        let display_flags = p.read_int32()?;
        Ok(LayoutParams {
            width,
            height,
            x,
            y,
            type_,
            flags,
            private_flags,
            soft_input_mode,
            layout_in_display_cutout_mode,
            gravity,
            horizontal_margin,
            vertical_margin,
            format,
            window_animations,
            alpha,
            dim_amount,
            screen_brightness,
            button_brightness,
            rotation_animation,
            token,
            window_context_token,
            package_name,
            title,
            screen_orientation,
            preferred_refresh_rate,
            preferred_display_mode_id,
            preferred_min_display_refresh_rate,
            preferred_max_display_refresh_rate,
            system_ui_visibility,
            subtree_system_ui_visibility,
            has_system_ui_listeners,
            input_features,
            user_activity_timeout,
            surface_insets,
            has_manual_surface_insets,
            receive_insets_ignoring_z_order,
            preserve_previous_surface_insets,
            accessibility_id_of_anchor,
            accessibility_title,
            color_mode,
            hide_timeout_milliseconds,
            insets_appearance,
            insets_behavior,
            fit_insets_types,
            fit_insets_sides,
            fit_insets_ignoring_visibility,
            prefer_minimal_post_processing,
            blur_behind_radius,
            insets_rounded_corner_frame,
            wallpaper_touch_events_enabled,
            provided_insets,
            params_for_rotation,
            display_flags,
        })
    }
}
