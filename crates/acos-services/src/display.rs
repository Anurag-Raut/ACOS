//! `IDisplayManager` — the single 1080×2400@420dpi default display.
//!
//! Values are MEASURED from
//! `fixtures/reference/hello-views/art-api34-x86_64/display.txt`
//! (`init=1080x2400 420dpi app=1080x2209`, `ROTATION_0`,
//! `NavigationBar0` present) and `device-config.json`.

use std::sync::{Arc, Mutex};

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
    /// The reference hello-views display.
    #[must_use]
    pub fn hello_views() -> Self {
        Self {
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
        }
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
                    vec![Value::TypedObject(Parcelable::Null)]
                } else {
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
            "isUidPresentOnDisplay" => vec![Value::Bool(true)],
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
