//! The small services: accessibility, input devices, text services, autofill,
//! audio, graphics stats, network management, game, virtual device, content
//! capture and voice interaction.
//!
//! Each reply is the value measured in `research/service-layer/services.tsv`
//! (with the trace row cited in the type docs). The rule everywhere is the same
//! as the rest of the layer: answer the measured call exactly, and return a
//! structured `UNSUPPORTED_ANDROID_API` for anything else — never a default
//! success.

use std::sync::{Arc, Mutex};

use acos_aidl::MethodInfo;
use acos_parcel::ParcelFd;

use crate::error::{Result, ServiceError};
use crate::host::{CallResult, PendingDelivery, ServiceObject};
use crate::value::{Bundle, Parcelable, RawFdValue, Value};

// ---------------------------------------------------------------------------
// accessibility
// ---------------------------------------------------------------------------

/// One recorded `IAccessibilityManager.sendAccessibilityEvent` call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct A11yEvent {
    /// `AccessibilityEvent.getEventType()` (`TYPE_VIEW_CLICKED` = 1, ...).
    pub event_type: i32,
    /// `AccessibilityEvent.getEventTime()` (0 when the event was not decoded).
    pub event_time: i64,
}

/// One registered `IAccessibilityInteractionConnection`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct A11yConnection {
    /// `IWindow` binder the app registered the connection for.
    pub window: u64,
    /// The `IAccessibilityInteractionConnection` binder.
    pub connection: u64,
    /// Package reported by `ViewRootImpl`.
    pub package: String,
}

/// `IAccessibilityManager` state.
///
/// ADR-0029's `accessibility` module owns the enabled/disabled state that the
/// stock service answered as a constant. The state is shared with the module
/// (which writes the secure settings at attach) and with the settings provider
/// (`Settings.Secure.getInt` must read exactly what the module wrote).
#[derive(Debug, Default)]
pub struct A11yState {
    /// Registered client binders.
    pub clients: Vec<u64>,
    /// Next client id (the reference returns 1).
    pub next_client_id: i64,
    /// The `Settings.Secure` table the module writes; `None` in a bare host
    /// built without the settings provider (the tests construct that shape).
    pub settings: Option<Arc<Mutex<crate::provider::SettingsState>>>,
    /// `Settings.Secure.ENABLED_ACCESSIBILITY_SERVICES` entries, in enable
    /// order (`pkg/.Class` as stored on the device).
    pub enabled_services: Vec<String>,
    /// `Settings.Secure.ACCESSIBILITY_ENABLED`.
    pub enabled: bool,
    /// The most recent events (bounded, newest last).
    pub events: Vec<A11yEvent>,
    /// Every interaction connection the app registered.
    pub connections: Vec<A11yConnection>,
    /// Set when a call arrived that this module cannot answer honestly.
    pub unsupported: Vec<String>,
}

/// Keep the event log bounded; an always-on UI sends thousands of events.
const MAX_EVENTS: usize = 256;

impl A11yState {
    /// New disabled state with no settings table.
    #[must_use]
    pub fn new() -> Self {
        Self {
            clients: Vec::new(),
            next_client_id: 1,
            settings: None,
            enabled_services: Vec::new(),
            enabled: false,
            events: Vec::new(),
            connections: Vec::new(),
            unsupported: Vec::new(),
        }
    }

    /// New disabled state that writes through to `settings`.
    #[must_use]
    pub fn with_settings(settings: Arc<Mutex<crate::provider::SettingsState>>) -> Self {
        let state = Self {
            settings: Some(settings),
            ..Self::new()
        };
        // Take the initial value from the settings table: the launch config is
        // the equivalent of switching the service on in Settings, and a host
        // that was seeded with the value must boot in the same state.
        let (enabled, services) = match &state.settings {
            Some(settings) => {
                let settings = settings
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                (
                    settings
                        .secure
                        .get("accessibility_enabled")
                        .is_some_and(|v| v == "1"),
                    settings
                        .secure
                        .get("enabled_accessibility_services")
                        .map(|raw| {
                            raw.split(':')
                                .filter(|s| !s.is_empty())
                                .map(str::to_string)
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default(),
                )
            }
            None => (false, Vec::new()),
        };
        Self {
            enabled,
            enabled_services: services,
            ..state
        }
    }

    /// Turn the service list on (the launch config's `enable: [...]`).
    ///
    /// Writes `Settings.Secure.ACCESSIBILITY_ENABLED` and
    /// `ENABLED_ACCESSIBILITY_SERVICES` the same way
    /// `AccessibilityManagerService.setAccessibilityEnabled` does, then records
    /// the list this session believes is enabled.
    pub fn enable_services(&mut self, services: &[String]) -> Result<()> {
        for service in services {
            validate_component(service)?;
        }
        let Some(settings) = &self.settings else {
            if services.is_empty() {
                self.enabled_services.clear();
                self.enabled = false;
                return Ok(());
            }
            return Err(ServiceError::ModuleRejected {
                module: "accessibility",
                detail: "host has no settings provider, so the secure settings cannot be written"
                    .into(),
            });
        };
        {
            let mut settings = settings
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let value = services.join(":");
            settings
                .secure
                .insert("enabled_accessibility_services".into(), value.clone());
            settings.secure.insert(
                "accessibility_enabled".into(),
                if services.is_empty() { "0" } else { "1" }.into(),
            );
            settings.changes.push((
                crate::provider::Namespace::Secure,
                "enabled_accessibility_services".into(),
            ));
            settings.changes.push((
                crate::provider::Namespace::Secure,
                "accessibility_enabled".into(),
            ));
        }
        self.enabled_services = services.to_vec();
        self.enabled = !services.is_empty();
        Ok(())
    }

    /// Turn accessibility off and clear the secure settings (detach).
    pub fn disable(&mut self) {
        if let Some(settings) = &self.settings {
            let mut settings = settings
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            settings
                .secure
                .insert("enabled_accessibility_services".into(), String::new());
            settings
                .secure
                .insert("accessibility_enabled".into(), "0".into());
        }
        self.enabled_services.clear();
        self.enabled = false;
        self.connections.clear();
    }

    /// The `setState` flag word: bit 0 enabled, bit 1 touch exploration.
    #[must_use]
    pub fn flags(&self) -> i32 {
        let mut bits = 0;
        if self.enabled {
            bits |= 1;
        }
        if self.settings.as_ref().is_some_and(|settings| {
            settings
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .secure
                .get("touch_exploration_enabled")
                .is_some_and(|v| v == "1")
        }) {
            bits |= 2;
        }
        bits
    }

    /// Record one event (bounded).
    pub fn record_event(&mut self, event_type: i32, event_time: i64) {
        if self.events.len() >= MAX_EVENTS {
            self.events.remove(0);
        }
        self.events.push(A11yEvent {
            event_type,
            event_time,
        });
    }

    /// Event counts by type, for the report.
    #[must_use]
    pub fn event_counts(&self) -> Vec<(i32, u32)> {
        let mut counts: Vec<(i32, u32)> = Vec::new();
        for event in &self.events {
            match counts.iter_mut().find(|(t, _)| *t == event.event_type) {
                Some((_, count)) => *count += 1,
                None => counts.push((event.event_type, 1)),
            }
        }
        counts
    }

    /// The honest state row the module report and `acos_services` carry.
    #[must_use]
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "enabled": self.enabled,
            "enabled_services": self.enabled_services,
            "clients": self.clients.len(),
            "connections": self.connections.iter().map(|c| serde_json::json!({
                "window": format!("{:#x}", c.window),
                "connection": format!("{:#x}", c.connection),
                "package": c.package,
            })).collect::<Vec<_>>(),
            "event_count": self.events.len(),
            "event_types": self.event_counts().into_iter().map(|(t, n)| {
                serde_json::json!({"type": t, "count": n})
            }).collect::<Vec<_>>(),
            "unsupported": self.unsupported,
        })
    }
}

/// Validate the device's component spelling: `pkg/.Class`, `pkg/full.Class`
/// or `pkg/Class`. A bare package (no `/`) is refused: it would enable nothing
/// while looking enabled in the reply.
pub fn validate_component(value: &str) -> Result<()> {
    let Some((package, class)) = value.split_once('/') else {
        return Err(ServiceError::ModuleRejected {
            module: "accessibility",
            detail: format!(
                "enabled service {value:?} must be spelled `package/.Class` or `package/full.Class`"
            ),
        });
    };
    if package.is_empty() || class.is_empty() {
        return Err(ServiceError::ModuleRejected {
            module: "accessibility",
            detail: format!("enabled service {value:?} has an empty package or class"),
        });
    }
    Ok(())
}

/// The `AccessibilityServiceInfo` value the framework reads from
/// `getInstalledAccessibilityServiceList` / `getEnabledAccessibilityServiceList`.
///
/// Only the fields an app actually reads for an installed service are filled;
/// `resolveInfo` is null (the setting UI's label lookup is a later step) and the
/// scalar defaults are the ones a manifest without
/// `<accessibility-service>` metadata produces.
#[must_use]
pub fn service_info_value(component: &str) -> Option<Value> {
    let (package, class) = component.split_once('/')?;
    let class = if let Some(short) = class.strip_prefix('.') {
        format!("{package}.{short}")
    } else {
        class.to_string()
    };
    Some(Value::Parcelable(Parcelable::ordered(
        "android.accessibilityservice.AccessibilityServiceInfo",
        vec![
            Value::Int(-1),           // eventTypes: all
            Value::StringArray(None), // packageNames
            Value::Int(1),            // feedbackType: FEEDBACK_SPOKEN
            Value::Long(0),           // notificationTimeout
            Value::Int(0),            // nonInteractiveUiTimeout
            Value::Int(0),            // interactiveUiTimeout
            Value::Int(0),            // flags
            Value::Int(0),            // crashed
            Value::Parcelable(Parcelable::ordered(
                "android.content.ComponentName",
                vec![
                    Value::Str(Some(package.to_string())),
                    Value::Str(Some(class)),
                ],
            )),
            Value::Parcelable(Parcelable::Null), // resolveInfo
            Value::Str(None),                    // settingsActivityName
            Value::Int(0),                       // capabilities
            Value::Int(0),                       // summaryResId
            Value::Str(None),                    // nonLocalizedSummary
            Value::Int(0),                       // descriptionResId
            Value::Int(0),                       // animatedImageRes
            Value::Int(0),                       // htmlDescriptionRes
            Value::Str(None),                    // nonLocalizedDescription
            Value::Bool(false),                  // isAccessibilityTool
            Value::Str(None),                    // tileServiceName
            Value::Int(0),                       // introResId
            Value::Int(0),                       // motionEventSources
            Value::Int(0),                       // observedMotionEventSources
        ],
    )))
}

/// `IAccessibilityManager` handler.
pub struct A11yService {
    state: Arc<Mutex<A11yState>>,
}

impl A11yService {
    /// Wrap shared state.
    #[must_use]
    pub fn new(state: Arc<Mutex<A11yState>>) -> Self {
        Self { state }
    }

    /// The shared state (the `accessibility` module configures and reports it).
    #[must_use]
    pub fn state(&self) -> Arc<Mutex<A11yState>> {
        Arc::clone(&self.state)
    }
}

impl ServiceObject for A11yService {
    fn call(&mut self, interface: &str, method: &MethodInfo, args: &[Value]) -> Result<CallResult> {
        if interface != "android.view.accessibility.IAccessibilityManager" {
            return Err(ServiceError::UnsupportedAndroidApi {
                interface: interface.to_string(),
                code: method.code,
                method: method.name.clone(),
            });
        }
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match method.name.as_str() {
            "addClient" => {
                // The image's `addClient` returns
                // `IntPair.of(stateFlags, relevantEventTypes)`: the high 32
                // bits are the state, the low bits the event mask
                // (`AccessibilityManager.tryConnectToServiceLocked` feeds
                // `IntPair.first` into `setStateLocked`). Returning a bare
                // client id leaves `isEnabled()` false, which is what the
                // pre-module service did.
                let state_flags = state.flags();
                if let Some(client) = args.first().and_then(Value::as_binder) {
                    state.clients.push(client.0);
                }
                let id = state.next_client_id;
                state.next_client_id += 1;
                let _ = id;
                Ok(CallResult {
                    reply: vec![Value::Long(
                        (i64::from(state_flags) << 32) | i64::from(0x7fff_ffff),
                    )],
                    // The state is in the return value on this image; a
                    // delivery would be a second, unsolicited `setState`.
                    deliveries: Vec::new(),
                })
            }
            "getInstalledAccessibilityServiceList" | "getEnabledAccessibilityServiceList" => {
                // `getEnabled...` takes a feedback-type filter; a configured
                // service declares `FEEDBACK_SPOKEN` and every answer includes
                // it. `getInstalled...` lists the same set because the launch
                // config is this session's installed-service source (the PMS
                // install happens after module attach; see the module docs).
                let enabled =
                    matches!(method.name.as_str(), "getInstalledAccessibilityServiceList")
                        || state.enabled;
                let items = if enabled {
                    state
                        .enabled_services
                        .iter()
                        .filter_map(|service| service_info_value(service))
                        .collect()
                } else {
                    Vec::new()
                };
                Ok(CallResult {
                    reply: vec![Value::List(Some(items))],
                    deliveries: Vec::new(),
                })
            }
            "sendAccessibilityEvent" => {
                // Decoded by the `read_args_override` in `codec.rs`: the event
                // parcel is not modelled, only its type and time.
                let event_type = args.first().and_then(Value::as_int).unwrap_or(0);
                let event_time = args.get(1).and_then(Value::as_long).unwrap_or(0);
                state.record_event(event_type, event_time);
                Ok(CallResult {
                    reply: Vec::new(),
                    deliveries: Vec::new(),
                })
            }
            "addAccessibilityInteractionConnection" => {
                let window = args
                    .first()
                    .and_then(Value::as_binder)
                    .map(|b| b.0)
                    .unwrap_or(0);
                let connection = args
                    .get(2)
                    .and_then(Value::as_binder)
                    .map(|b| b.0)
                    .unwrap_or(0);
                let package = args
                    .get(3)
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                state.connections.push(A11yConnection {
                    window,
                    connection,
                    package,
                });
                // `AccessibilityWindowId`: the image uses the interaction
                // connection id as the window id (`AccessibilityWindowInfo`).
                let id = i32::try_from(state.connections.len()).unwrap_or(i32::MAX);
                Ok(CallResult {
                    reply: vec![Value::Int(id)],
                    deliveries: Vec::new(),
                })
            }
            "removeAccessibilityInteractionConnection" => {
                let window = args.first().and_then(Value::as_binder).map(|b| b.0);
                if let Some(window) = window {
                    state.connections.retain(|c| c.window != window);
                }
                Ok(CallResult {
                    reply: Vec::new(),
                    deliveries: Vec::new(),
                })
            }
            "interrupt" => {
                state.record_event(-1, 0);
                Ok(CallResult {
                    reply: Vec::new(),
                    deliveries: Vec::new(),
                })
            }
            "getRecommendedTimeoutMillis" => Ok(CallResult {
                reply: vec![Value::Long(0)],
                deliveries: Vec::new(),
            }),
            "getFocusStrokeWidth" => Ok(CallResult {
                reply: vec![Value::Int(0)],
                deliveries: Vec::new(),
            }),
            "getFocusColor" => Ok(CallResult {
                reply: vec![Value::Int(0)],
                deliveries: Vec::new(),
            }),
            "isAudioDescriptionByDefaultEnabled" => Ok(CallResult {
                reply: vec![Value::Bool(false)],
                deliveries: Vec::new(),
            }),
            "removeClient" => Ok(CallResult {
                reply: vec![Value::Bool(true)],
                deliveries: Vec::new(),
            }),
            _ => {
                state.unsupported.push(method.name.clone());
                Err(ServiceError::UnsupportedAndroidApi {
                    interface: interface.to_string(),
                    code: method.code,
                    method: method.name.clone(),
                })
            }
        }
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }
}

// ---------------------------------------------------------------------------
// input devices
// ---------------------------------------------------------------------------

/// `IInputManager` state (one synthetic touchscreen).
#[derive(Debug)]
pub struct InputState {
    /// Registered `IInputDevicesChangedListener` binders.
    pub listeners: Vec<u64>,
}

impl InputState {
    /// New state.
    #[must_use]
    pub fn new() -> Self {
        Self {
            listeners: Vec::new(),
        }
    }
}

impl Default for InputState {
    fn default() -> Self {
        Self::new()
    }
}

/// `IInputManager` handler.
pub struct InputService {
    state: Arc<Mutex<InputState>>,
}

impl InputService {
    /// Wrap shared state.
    #[must_use]
    pub fn new(state: Arc<Mutex<InputState>>) -> Self {
        Self { state }
    }

    /// The `KeyCharacterMap` payload for a synthetic `InputDevice` (W16).
    ///
    /// The image's reader is `KeyCharacterMap.nativeReadFromParcel` ->
    /// `KeyCharacterMap::readFromParcel`, i.e. the *native* map parcel: a
    /// `deviceId`, a `hasMap` marker and then the map's keys. The host builds
    /// that payload from the stock `/system/usr/keychars/Virtual.kcm`
    /// (virtual keyboard, id `-1`) or `Generic.kcm` (touchscreen, id `0`) that
    /// `tools/art-sysroot.sh` ships. A device whose stock file is missing gets
    /// the honest `hasMap = false` payload (no characters), never a guessed
    /// layout.
    fn key_character_map(id: i32) -> Parcelable {
        let bytes = crate::keychars::stock_parcel(id < 0)
            .unwrap_or_else(|| crate::keychars::null_parcel(id));
        Parcelable::ordered(
            "android.view.KeyCharacterMap",
            vec![Value::ByteArray(Some(bytes))],
        )
    }

    /// One synthetic `InputDevice`.
    ///
    /// `id = -1` is `KeyCharacterMap.VIRTUAL_KEYBOARD` and `id = 0` is the
    /// built-in touchscreen. Both carry a real `KeyCharacterMap`, so
    /// `KeyCharacterMap.load(deviceId)` (and its `VIRTUAL_KEYBOARD` fallback)
    /// returns a usable map instead of throwing `UnavailableException`.
    fn input_device(id: i32) -> Parcelable {
        let virtual_keyboard = id < 0;
        let name = if virtual_keyboard {
            "ACOS Virtual Keyboard"
        } else {
            "ACOS Touch"
        };
        let descriptor = if virtual_keyboard {
            "acos-virtual-keyboard"
        } else {
            "acos-touch"
        };
        let sources = if virtual_keyboard {
            0x0000_0101 // SOURCE_KEYBOARD
        } else {
            0x0000_1002 // SOURCE_TOUCHSCREEN
        };
        let keyboard_type = if virtual_keyboard {
            2 // InputDevice.KEYBOARD_TYPE_ALPHABETIC
        } else {
            0 // InputDevice.KEYBOARD_TYPE_NONE
        };
        Parcelable::ordered(
            "android.view.InputDevice",
            vec![
                Value::Parcelable(Self::key_character_map(id)), // mKeyCharacterMap
                Value::Int(id),                                 // mId
                Value::Int(1),                                  // mGeneration
                Value::Int(0),                                  // mControllerNumber
                Value::Str(Some(name.into())),                  // mName (writeString)
                Value::Int(0),                                  // mVendorId
                Value::Int(0),                                  // mProductId
                Value::Str(Some(descriptor.into())),            // mDescriptor (writeString)
                Value::Int(0),                                  // mIsExternal
                Value::Int(sources),                            // mSources
                Value::Int(keyboard_type),                      // mKeyboardType
                Value::Str8(None),                              // mKeyboardLanguageTag
                Value::Str8(None),                              // mKeyboardLayoutType
                Value::Int(0),                                  // mHasVibrator
                Value::Int(0),                                  // mHasMicrophone
                Value::Int(0),                                  // mHasButtonUnderPad
                Value::Int(0),                                  // mHasSensor
                Value::Int(0),                                  // mHasBattery
                Value::Parcelable(Parcelable::ordered(
                    "android.hardware.input.HostUsiVersion",
                    vec![Value::Int(-1), Value::Int(-1)],
                )), // mHostUsiVersion
                Value::Int(-1),                                 // mAssociatedDisplayId
                Value::Int(0),                                  // numRanges
            ],
        )
    }
}

impl ServiceObject for InputService {
    fn call(&mut self, interface: &str, method: &MethodInfo, args: &[Value]) -> Result<CallResult> {
        if interface != "android.hardware.input.IInputManager" {
            return Err(ServiceError::UnsupportedAndroidApi {
                interface: interface.to_string(),
                code: method.code,
                method: method.name.clone(),
            });
        }
        let mut state = self.state.lock().unwrap();
        let reply = match method.name.as_str() {
            "getVelocityTrackerStrategy" => vec![Value::Str(Some("impulse".into()))],
            // `InputManagerGlobal.populateInputDevicesLocked` builds its id
            // set from this call, so the virtual keyboard (-1) must be listed
            // or `getInputDevice(-1)` never reaches the service.
            "getInputDeviceIds" => vec![Value::IntArray(Some(vec![0, -1]))],
            "getInputDevice" => {
                let id = args.first().and_then(Value::as_int).unwrap_or(0);
                if id == 0 || id == -1 {
                    let device = Self::input_device(id);
                    if std::env::var_os("ACOS_W16_TRACE").is_some() {
                        let kcm = crate::keychars::stock_parcel(id < 0)
                            .map(|b| b.len())
                            .unwrap_or(0);
                        let mut scratch = acos_parcel::Parcel::new();
                        let reply_bytes = crate::parcelable::write_payload(&mut scratch, &device)
                            .map(|()| scratch.data_size())
                            .unwrap_or(0);
                        eprintln!(
                            "ACOSW16 getInputDevice id={id} kcm_bytes={kcm} keys={} reply_bytes={reply_bytes}",
                            crate::keychars::stock()
                                .generic
                                .as_ref()
                                .map(|m| m.keys.len())
                                .unwrap_or(0)
                        );
                    }
                    vec![Value::TypedObject(device)]
                } else {
                    vec![Value::TypedObject(Parcelable::Null)]
                }
            }
            "registerInputDevicesChangedListener" => {
                if let Some(l) = args.first().and_then(Value::as_binder) {
                    state.listeners.push(l.0);
                }
                Vec::new()
            }
            "isInTabletMode" | "isMicMuted" => vec![Value::Int(0)],
            "cancelCurrentTouch" => Vec::new(),
            _ => {
                return Err(ServiceError::UnsupportedAndroidApi {
                    interface: interface.to_string(),
                    code: method.code,
                    method: method.name.clone(),
                })
            }
        };
        Ok(CallResult {
            reply,
            deliveries: Vec::new(),
        })
    }
}

// ---------------------------------------------------------------------------
// text services
// ---------------------------------------------------------------------------

/// `ITextServicesManager` state (no spell checker).
#[derive(Debug, Default)]
pub struct TextServicesState;

impl TextServicesState {
    /// New state.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

/// `ITextServicesManager` handler.
pub struct TextServicesService {
    _state: Arc<Mutex<TextServicesState>>,
}

impl TextServicesService {
    /// Wrap shared state.
    #[must_use]
    pub fn new(state: Arc<Mutex<TextServicesState>>) -> Self {
        Self { _state: state }
    }
}

impl ServiceObject for TextServicesService {
    fn call(
        &mut self,
        interface: &str,
        method: &MethodInfo,
        _args: &[Value],
    ) -> Result<CallResult> {
        if interface != "com.android.internal.textservice.ITextServicesManager" {
            return Err(ServiceError::UnsupportedAndroidApi {
                interface: interface.to_string(),
                code: method.code,
                method: method.name.clone(),
            });
        }
        let reply = match method.name.as_str() {
            "getCurrentSpellCheckerSubtype" | "getCurrentSpellChecker" => {
                vec![Value::TypedObject(Parcelable::Null)]
            }
            "isSpellCheckerEnabled" => vec![Value::Bool(false)],
            "getSpellCheckerService" | "finishSpellCheckerService" => Vec::new(),
            "getEnabledSpellCheckers" => vec![Value::List(Some(Vec::new()))],
            _ => {
                return Err(ServiceError::UnsupportedAndroidApi {
                    interface: interface.to_string(),
                    code: method.code,
                    method: method.name.clone(),
                })
            }
        };
        Ok(CallResult {
            reply,
            deliveries: Vec::new(),
        })
    }
}

// ---------------------------------------------------------------------------
// autofill
// ---------------------------------------------------------------------------

/// `IAutoFillManager` state (disabled).
#[derive(Debug, Default)]
pub struct AutofillState {
    /// Registered clients.
    pub clients: Vec<u64>,
    /// Sessions started (and immediately finished).
    pub sessions: Vec<i32>,
}

impl AutofillState {
    /// New state.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

/// `IAutoFillManager` handler.
pub struct AutofillService {
    state: Arc<Mutex<AutofillState>>,
}

impl AutofillService {
    /// Wrap shared state.
    #[must_use]
    pub fn new(state: Arc<Mutex<AutofillState>>) -> Self {
        Self { state }
    }
}

impl ServiceObject for AutofillService {
    fn call(&mut self, interface: &str, method: &MethodInfo, args: &[Value]) -> Result<CallResult> {
        if interface != "android.view.autofill.IAutoFillManager" {
            return Err(ServiceError::UnsupportedAndroidApi {
                interface: interface.to_string(),
                code: method.code,
                method: method.name.clone(),
            });
        }
        let mut state = self.state.lock().unwrap();
        let mut deliveries = Vec::new();
        let reply = match method.name.as_str() {
            "addClient" => {
                if let Some(client) = args.first().and_then(Value::as_binder) {
                    state.clients.push(client.0);
                }
                // The IResultReceiver is the last argument; answer it so the
                // app never waits (services.tsv: "report disabled").
                if let Some(receiver) = args.last().and_then(Value::as_binder) {
                    deliveries.push(result_receiver_delivery(receiver.0, 0));
                }
                Vec::new()
            }
            "startSession" => {
                state.sessions.push(0);
                if let Some(receiver) = args.last().and_then(Value::as_binder) {
                    deliveries.push(result_receiver_delivery(receiver.0, 0));
                }
                Vec::new()
            }
            "removeClient" | "finishSession" | "cancelSession" => Vec::new(),
            "isServiceEnabled" => vec![Value::Bool(false)],
            "isServiceSupported" => vec![Value::Bool(false)],
            "isFieldClassificationEnabled" => vec![Value::Bool(false)],
            _ => {
                return Err(ServiceError::UnsupportedAndroidApi {
                    interface: interface.to_string(),
                    code: method.code,
                    method: method.name.clone(),
                })
            }
        };
        Ok(CallResult { reply, deliveries })
    }
}

fn result_receiver_delivery(target: u64, result_code: i32) -> PendingDelivery {
    let info = acos_aidl::lookup_by_name("com.android.internal.os.IResultReceiver", "send")
        .expect("IResultReceiver.send is in the AIDL table");
    let args = vec![Value::Int(result_code), Value::Bundle(None)];
    let data = crate::codec::build_request("com.android.internal.os.IResultReceiver", info, &args)
        .unwrap_or_default();
    PendingDelivery {
        target: acos_parcel::BinderId(target),
        interface: "com.android.internal.os.IResultReceiver".into(),
        code: info.code,
        method: info.name.clone(),
        data,
    }
}

// ---------------------------------------------------------------------------
// audio
// ---------------------------------------------------------------------------

/// `IAudioService` state (silent, no audio device).
#[derive(Debug, Default)]
pub struct AudioState {
    /// Sound effects requested for evidence.
    pub effects: Vec<(i32, i32)>,
}

impl AudioState {
    /// New state.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

/// `IAudioService` handler.
pub struct AudioService {
    state: Arc<Mutex<AudioState>>,
}

impl AudioService {
    /// Wrap shared state.
    #[must_use]
    pub fn new(state: Arc<Mutex<AudioState>>) -> Self {
        Self { state }
    }
}

impl ServiceObject for AudioService {
    fn call(&mut self, interface: &str, method: &MethodInfo, args: &[Value]) -> Result<CallResult> {
        if interface != "android.media.IAudioService" {
            return Err(ServiceError::UnsupportedAndroidApi {
                interface: interface.to_string(),
                code: method.code,
                method: method.name.clone(),
            });
        }
        let mut state = self.state.lock().unwrap();
        let reply = match method.name.as_str() {
            "areNavigationRepeatSoundEffectsEnabled" => vec![Value::Bool(false)],
            "playSoundEffect" => {
                let effect = args.first().and_then(Value::as_int).unwrap_or(0);
                let user = args.get(1).and_then(Value::as_int).unwrap_or(0);
                state.effects.push((effect, user));
                Vec::new()
            }
            "playSoundEffectVolume" => Vec::new(),
            "isMasterMute" | "isStreamMute" => vec![Value::Bool(false)],
            _ => {
                return Err(ServiceError::UnsupportedAndroidApi {
                    interface: interface.to_string(),
                    code: method.code,
                    method: method.name.clone(),
                })
            }
        };
        Ok(CallResult {
            reply,
            deliveries: Vec::new(),
        })
    }
}

// ---------------------------------------------------------------------------
// graphics stats
// ---------------------------------------------------------------------------

/// `IGraphicsStats` state.
#[derive(Debug, Default)]
pub struct GraphicsStatsState {
    /// Processes that requested a stats buffer.
    pub requests: Vec<String>,
}

impl GraphicsStatsState {
    /// New state.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

/// `IGraphicsStats` handler.
pub struct GraphicsStatsService {
    state: Arc<Mutex<GraphicsStatsState>>,
}

impl GraphicsStatsService {
    /// Wrap shared state.
    #[must_use]
    pub fn new(state: Arc<Mutex<GraphicsStatsState>>) -> Self {
        Self { state }
    }
}

impl ServiceObject for GraphicsStatsService {
    fn call(&mut self, interface: &str, method: &MethodInfo, args: &[Value]) -> Result<CallResult> {
        if interface != "android.view.IGraphicsStats" {
            return Err(ServiceError::UnsupportedAndroidApi {
                interface: interface.to_string(),
                code: method.code,
                method: method.name.clone(),
            });
        }
        let reply = match method.name.as_str() {
            "requestBufferForProcess" => {
                let process = args.first().and_then(Value::as_str).unwrap_or_default();
                self.state
                    .lock()
                    .unwrap()
                    .requests
                    .push(process.to_string());
                let fd = ParcelFd::memfd("acos-gfxstats").ok();
                vec![Value::TypedObject(Parcelable::ordered(
                    "android.os.ParcelFileDescriptor",
                    vec![Value::Int(0), Value::RawFd(fd.map(RawFdValue))],
                ))]
            }
            _ => {
                return Err(ServiceError::UnsupportedAndroidApi {
                    interface: interface.to_string(),
                    code: method.code,
                    method: method.name.clone(),
                })
            }
        };
        Ok(CallResult {
            reply,
            deliveries: Vec::new(),
        })
    }
}

// ---------------------------------------------------------------------------
// network management
// ---------------------------------------------------------------------------

/// `INetworkManagementService` state.
#[derive(Debug, Default)]
pub struct NetState {
    /// Recorded (uid, policy) pairs.
    pub cleartext_policies: Vec<(i32, i32)>,
}

impl NetState {
    /// New state.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

/// `INetworkManagementService` handler.
pub struct NetService {
    state: Arc<Mutex<NetState>>,
}

impl NetService {
    /// Wrap shared state.
    #[must_use]
    pub fn new(state: Arc<Mutex<NetState>>) -> Self {
        Self { state }
    }
}

impl ServiceObject for NetService {
    fn call(&mut self, interface: &str, method: &MethodInfo, args: &[Value]) -> Result<CallResult> {
        if interface != "android.os.INetworkManagementService" {
            return Err(ServiceError::UnsupportedAndroidApi {
                interface: interface.to_string(),
                code: method.code,
                method: method.name.clone(),
            });
        }
        let reply = match method.name.as_str() {
            "setUidCleartextNetworkPolicy" => {
                let uid = args.first().and_then(Value::as_int).unwrap_or(0);
                let policy = args.get(1).and_then(Value::as_int).unwrap_or(0);
                self.state
                    .lock()
                    .unwrap()
                    .cleartext_policies
                    .push((uid, policy));
                Vec::new()
            }
            "isBandwidthControlEnabled" => vec![Value::Bool(false)],
            _ => {
                return Err(ServiceError::UnsupportedAndroidApi {
                    interface: interface.to_string(),
                    code: method.code,
                    method: method.name.clone(),
                })
            }
        };
        Ok(CallResult {
            reply,
            deliveries: Vec::new(),
        })
    }
}

// ---------------------------------------------------------------------------
// game / virtual device / content capture / voice
// ---------------------------------------------------------------------------

/// `IGameManagerService` state.
#[derive(Debug, Default)]
pub struct GameState;

impl GameState {
    /// New state.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

/// `IGameManagerService` handler.
pub struct GameService {
    _state: Arc<Mutex<GameState>>,
}

impl GameService {
    /// Wrap shared state.
    #[must_use]
    pub fn new(state: Arc<Mutex<GameState>>) -> Self {
        Self { _state: state }
    }
}

impl ServiceObject for GameService {
    fn call(
        &mut self,
        interface: &str,
        method: &MethodInfo,
        _args: &[Value],
    ) -> Result<CallResult> {
        if interface != "android.app.IGameManagerService" {
            return Err(ServiceError::UnsupportedAndroidApi {
                interface: interface.to_string(),
                code: method.code,
                method: method.name.clone(),
            });
        }
        let reply = match method.name.as_str() {
            "isAngleEnabled" => vec![Value::Bool(false)],
            // One-way notification from `ActivityThread.setupGraphicsSupport`:
            // the process finished its graphics bootstrap. ACOS has no game
            // mode / ANGLE policy to update, so recording the notification and
            // returning nothing is the service's real answer (the framework
            // discards the reply).
            "notifyGraphicsEnvironmentSetup" => Vec::new(),
            _ => {
                return Err(ServiceError::UnsupportedAndroidApi {
                    interface: interface.to_string(),
                    code: method.code,
                    method: method.name.clone(),
                })
            }
        };
        Ok(CallResult {
            reply,
            deliveries: Vec::new(),
        })
    }
}

/// `IVirtualDeviceManager` state.
#[derive(Debug, Default)]
pub struct VirtualDeviceState;

impl VirtualDeviceState {
    /// New state.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

/// `IVirtualDeviceManager` handler.
pub struct VirtualDeviceService {
    _state: Arc<Mutex<VirtualDeviceState>>,
}

impl VirtualDeviceService {
    /// Wrap shared state.
    #[must_use]
    pub fn new(state: Arc<Mutex<VirtualDeviceState>>) -> Self {
        Self { _state: state }
    }
}

impl ServiceObject for VirtualDeviceService {
    fn call(
        &mut self,
        interface: &str,
        method: &MethodInfo,
        _args: &[Value],
    ) -> Result<CallResult> {
        if interface != "android.companion.virtual.IVirtualDeviceManager" {
            return Err(ServiceError::UnsupportedAndroidApi {
                interface: interface.to_string(),
                code: method.code,
                method: method.name.clone(),
            });
        }
        let reply = match method.name.as_str() {
            "getDeviceIdForDisplayId" => vec![Value::Int(0)],
            _ => {
                return Err(ServiceError::UnsupportedAndroidApi {
                    interface: interface.to_string(),
                    code: method.code,
                    method: method.name.clone(),
                })
            }
        };
        Ok(CallResult {
            reply,
            deliveries: Vec::new(),
        })
    }
}

/// `IContentCaptureManager` state.
#[derive(Debug, Default)]
pub struct ContentCaptureState {
    /// Registered callbacks.
    pub callbacks: Vec<u64>,
}

impl ContentCaptureState {
    /// New state.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

/// `IContentCaptureManager` handler.
pub struct ContentCaptureService {
    state: Arc<Mutex<ContentCaptureState>>,
}

impl ContentCaptureService {
    /// Wrap shared state.
    #[must_use]
    pub fn new(state: Arc<Mutex<ContentCaptureState>>) -> Self {
        Self { state }
    }
}

impl ServiceObject for ContentCaptureService {
    fn call(&mut self, interface: &str, method: &MethodInfo, args: &[Value]) -> Result<CallResult> {
        if interface != "android.view.contentcapture.IContentCaptureManager" {
            return Err(ServiceError::UnsupportedAndroidApi {
                interface: interface.to_string(),
                code: method.code,
                method: method.name.clone(),
            });
        }
        let reply = match method.name.as_str() {
            "registerContentCaptureOptionsCallback" => {
                if let Some(cb) = args.get(1).and_then(Value::as_binder) {
                    self.state.lock().unwrap().callbacks.push(cb.0);
                }
                Vec::new()
            }
            "isContentCaptureFeatureEnabled" => vec![Value::Bool(false)],
            _ => {
                return Err(ServiceError::UnsupportedAndroidApi {
                    interface: interface.to_string(),
                    code: method.code,
                    method: method.name.clone(),
                })
            }
        };
        Ok(CallResult {
            reply,
            deliveries: Vec::new(),
        })
    }
}

/// `IVoiceInteractionManagerService` state.
#[derive(Debug, Default)]
pub struct VoiceState {
    /// Recorded activity events.
    pub events: Vec<(u64, i32)>,
}

impl VoiceState {
    /// New state.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

/// `IVoiceInteractionManagerService` handler.
pub struct VoiceService {
    state: Arc<Mutex<VoiceState>>,
}

impl VoiceService {
    /// Wrap shared state.
    #[must_use]
    pub fn new(state: Arc<Mutex<VoiceState>>) -> Self {
        Self { state }
    }
}

impl ServiceObject for VoiceService {
    fn call(&mut self, interface: &str, method: &MethodInfo, args: &[Value]) -> Result<CallResult> {
        if interface != "com.android.internal.app.IVoiceInteractionManagerService" {
            return Err(ServiceError::UnsupportedAndroidApi {
                interface: interface.to_string(),
                code: method.code,
                method: method.name.clone(),
            });
        }
        let reply = match method.name.as_str() {
            "notifyActivityEventChanged" => {
                let token = args
                    .first()
                    .and_then(Value::as_binder)
                    .map(|b| b.0)
                    .unwrap_or(0);
                let event = args.get(1).and_then(Value::as_int).unwrap_or(0);
                self.state.lock().unwrap().events.push((token, event));
                Vec::new()
            }
            "isSessionRunning" => vec![Value::Bool(false)],
            _ => {
                return Err(ServiceError::UnsupportedAndroidApi {
                    interface: interface.to_string(),
                    code: method.code,
                    method: method.name.clone(),
                })
            }
        };
        Ok(CallResult {
            reply,
            deliveries: Vec::new(),
        })
    }
}

/// `IAndroidFuture` state (ACOS never blocks on a future).
#[derive(Debug, Default)]
pub struct FutureState {
    /// Completed futures.
    pub completed: Vec<u64>,
}

impl FutureState {
    /// New state.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

/// `IAndroidFuture` handler.
pub struct FutureService {
    state: Arc<Mutex<FutureState>>,
}

impl FutureService {
    /// Wrap shared state.
    #[must_use]
    pub fn new(state: Arc<Mutex<FutureState>>) -> Self {
        Self { state }
    }
}

impl ServiceObject for FutureService {
    fn call(&mut self, interface: &str, method: &MethodInfo, args: &[Value]) -> Result<CallResult> {
        if interface != "com.android.internal.infra.IAndroidFuture" {
            return Err(ServiceError::UnsupportedAndroidApi {
                interface: interface.to_string(),
                code: method.code,
                method: method.name.clone(),
            });
        }
        let reply = match method.name.as_str() {
            "complete" => {
                let key = args
                    .first()
                    .and_then(Value::as_parcelable)
                    .and_then(|p| p.field(0))
                    .and_then(Value::as_int)
                    .map(|v| v as u64)
                    .unwrap_or(0);
                self.state.lock().unwrap().completed.push(key);
                Vec::new()
            }
            _ => {
                return Err(ServiceError::UnsupportedAndroidApi {
                    interface: interface.to_string(),
                    code: method.code,
                    method: method.name.clone(),
                })
            }
        };
        Ok(CallResult {
            reply,
            deliveries: Vec::new(),
        })
    }
}

/// Placeholder so the registry can keep the `IContentService` import stable.
pub type ContentServiceState = ();

// ---------------------------------------------------------------------------
// sensors (W26): a stock-shaped `ISensorServer` with an empty sensor list
// ---------------------------------------------------------------------------
//
// `SystemSensorManager.nativeCreate` (libandroid_runtime) constructs
// `android::SensorManager`, which calls
// `defaultServiceManager()->getService(String16("sensorservice"))` and then
// `ISensorServer::getSensorList(package, &mSensorList)`. On stock Android the
// service always exists; in ACOS it did not, and `ServiceManagerShim::getService`
// (`.cache/aosp/native/libs/binder/IServiceManager.cpp:309`) retries
// `checkService` for **5000 ms per lookup** before returning null. Two lookups
// therefore burned ~10 s of the launcher's 12 s resume budget (W25 §3c).
//
// The wire below is read from the **image**, not the tag:
// `nm -D .cache/art/sysroot/system/lib64/libsensor.so` locates
// `android::BnSensorServer::onTransact` at `0xe8e0`; its jump table at
// `0x953c` maps codes 1..5, and each case's `Parcel` calls give the argument
// order. The interface descriptors are UTF-16 literals in `.rodata`:
// `android.gui.SensorServer` (0x16754) and
// `android.gui.SensorEventConnection`.

/// Descriptor `ISensorServer::descriptor` (image `.rodata`, UTF-16).
pub const SENSOR_SERVER_DESCRIPTOR: &str = "android.gui.SensorServer";
/// Descriptor `ISensorEventConnection::descriptor` (image `.rodata`, UTF-16).
pub const SENSOR_EVENT_CONNECTION_DESCRIPTOR: &str = "android.gui.SensorEventConnection";
/// `getSensorList(String16 package) -> Vector<Sensor>` (case `0xE927`).
pub const SENSOR_GET_SENSOR_LIST: u32 = 1;
/// `createSensorEventConnection(String8 opPackage, int mode, String16 package,
/// String16 attributionTag) -> ISensorEventConnection` (case `0xECDB`).
pub const SENSOR_CREATE_EVENT_CONNECTION: u32 = 2;
/// A no-argument call returning `int32` (case `0xECA8`). The image's name for
/// it is not recoverable from the stripped table, so it is answered
/// structurally only when its reply shape is proven; today it is a named gap.
pub const SENSOR_NOARG_INT: u32 = 3;
/// `getDynamicSensorList(String16 package) -> Vector<Sensor>` (case `0xEE0B`).
pub const SENSOR_GET_DYNAMIC_SENSOR_LIST: u32 = 4;
/// `createSensorDirectConnection(...)` — takes a `NativeHandle` (case `0xEB68`).
pub const SENSOR_CREATE_DIRECT_CONNECTION: u32 = 5;

/// What the sensor server was asked for during a session (evidence).
#[derive(Debug, Default)]
pub struct SensorState {
    /// `getSensorList` calls, in request order, with the package argument.
    pub sensor_list_calls: Vec<String>,
    /// `getDynamicSensorList` calls.
    pub dynamic_list_calls: Vec<String>,
    /// `createSensorEventConnection` calls (packages).
    pub event_connection_calls: Vec<String>,
}

/// `android.gui.SensorServer`: an `ISensorServer` with zero sensors, which is
/// exactly what an emulator image without sensor hardware serves.
pub struct SensorServer {
    state: Arc<Mutex<SensorState>>,
}

impl SensorServer {
    /// A server with an empty sensor list.
    #[must_use]
    pub fn new(state: Arc<Mutex<SensorState>>) -> Self {
        Self { state }
    }
}

fn sensor_bad(method: &str, detail: impl std::fmt::Display) -> ServiceError {
    ServiceError::BadRequest {
        interface: SENSOR_SERVER_DESCRIPTOR.to_string(),
        method: method.to_string(),
        detail: detail.to_string(),
    }
}

impl ServiceObject for SensorServer {
    fn call(
        &mut self,
        interface: &str,
        method: &MethodInfo,
        _args: &[Value],
    ) -> Result<CallResult> {
        // `android.gui.*` has no decoded `acos-aidl` table: `raw_transact` is
        // the only path (the same rule as the compositor and `IDisplayEventConnection`).
        Err(ServiceError::UnsupportedAndroidApi {
            interface: interface.to_string(),
            code: method.code,
            method: method.name.clone(),
        })
    }

    fn raw_transact(
        &mut self,
        interface: &str,
        code: u32,
        data: &mut acos_parcel::Parcel,
        reply: &mut acos_parcel::Parcel,
    ) -> Result<bool> {
        if interface != SENSOR_SERVER_DESCRIPTOR {
            return Ok(false);
        }
        match code {
            SENSOR_GET_SENSOR_LIST | SENSOR_GET_DYNAMIC_SENSOR_LIST => {
                // `BnSensorServer::onTransact` reads the package name first
                // (`checkInterface`, `readString16`), then the reply is
                // `writeUint32(count)` followed by, per sensor,
                // `writeInt32(flattenedSize)` + the flattened `Sensor`. Zero
                // sensors is one count word and nothing else, so this is
                // byte-exact for the empty case and needs no `Sensor` layout.
                let package = data
                    .read_string16()
                    .map_err(|e| sensor_bad("getSensorList", e))?
                    .unwrap_or_default();
                {
                    let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                    if code == SENSOR_GET_SENSOR_LIST {
                        state.sensor_list_calls.push(package);
                    } else {
                        state.dynamic_list_calls.push(package);
                    }
                }
                crate::exception::write_no_exception(reply);
                reply.write_int32(0);
                Ok(true)
            }
            SENSOR_CREATE_EVENT_CONNECTION => {
                // Decode the whole request so a malformed parcel is a
                // structured error instead of a half-consumed transaction,
                // then refuse explicitly: a real device answers with an
                // `ISensorEventConnection` carrying a `BitTube` of event fds,
                // and with an empty sensor list no guest can ever need one.
                let op_package = data
                    .read_string8()
                    .map_err(|e| sensor_bad("createSensorEventConnection", e))?;
                let _mode = data
                    .read_int32()
                    .map_err(|e| sensor_bad("createSensorEventConnection", e))?;
                let package = data
                    .read_string16()
                    .map_err(|e| sensor_bad("createSensorEventConnection", e))?
                    .unwrap_or_default();
                let _attribution_tag = data
                    .read_string16()
                    .map_err(|e| sensor_bad("createSensorEventConnection", e))?;
                let _ = op_package;
                self.state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .event_connection_calls
                    .push(package);
                Err(ServiceError::UnsupportedAndroidApi {
                    interface: SENSOR_SERVER_DESCRIPTOR.to_string(),
                    code,
                    method: "createSensorEventConnection".to_string(),
                })
            }
            _ => {
                // Codes 3 (`() -> int`) and 5 (a `NativeHandle` argument) never
                // occur on the empty-list path. Answering them would mean
                // guessing a layout, so they stay a named, structured gap.
                Ok(false)
            }
        }
    }
}

/// A convenience `Bundle` for evidence.
#[must_use]
pub fn empty_bundle() -> Bundle {
    Bundle::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// W16: the `IInputManager.getInputDevice` reply carries the image's native
    /// `KeyCharacterMap` payload (from the stock `Virtual.kcm`) and the field
    /// order matches the image's `InputDevice(Parcel)` (no `mDeviceBus` and no
    /// view-behaviour boolean; see the spec comment). A reply
    /// that is one word off makes the Java reader shift every later field, which
    /// is how W13's synthetic device ended up with `mSources == 2`.
    #[test]
    fn the_virtual_keyboard_input_device_matches_the_image_field_order() {
        let device = InputService::input_device(-1);
        let mut parcel = acos_parcel::Parcel::new();
        crate::parcelable::write_payload(&mut parcel, &device).expect("write InputDevice");
        let bytes = parcel.marshall();
        assert_eq!(bytes.len() % 4, 0);
        let mut p = Reader {
            bytes: &bytes,
            pos: 0,
        };

        // mKeyCharacterMap: FieldOp::Raw -> nativeReadFromParcel payload.
        assert_eq!(p.i32(), -1, "KCM deviceId is the virtual keyboard");
        assert_eq!(p.i32(), 1, "KCM hasMap");
        let load_file = p.cstring();
        assert!(
            load_file == "/system/usr/keychars/Virtual.kcm"
                || load_file == "/system/usr/keychars/Generic.kcm",
            "stock KCM file, got {load_file}"
        );
        assert_eq!(p.i32(), 4, "type FULL");
        assert_eq!(p.i32(), 0, "no overlay");
        let num_keys = p.i32();
        assert!(num_keys > 60, "parsed {num_keys} keys");
        for _ in 0..num_keys {
            let _key_code = p.i32();
            let _label = p.i32();
            let _number = p.i32();
            while p.i32() == 1 {
                let _ = (p.i32(), p.i32(), p.i32(), p.i32());
            }
        }
        assert_eq!((p.i32(), p.i32(), p.i32()), (0, 0, 0), "empty remap tables");

        // The InputDevice scalar fields, in Java `readFromParcel` order.
        assert_eq!(p.i32(), -1, "mId");
        assert_eq!(p.i32(), 1, "mGeneration");
        assert_eq!(p.i32(), 0, "mControllerNumber");
        assert_eq!(p.string16(), "ACOS Virtual Keyboard");
        assert_eq!(p.i32(), 0, "mVendorId");
        assert_eq!(p.i32(), 0, "mProductId");
        assert_eq!(p.string16(), "acos-virtual-keyboard");
        assert_eq!(p.i32(), 0, "mIsExternal");
        assert_eq!(p.i32(), 0x101, "mSources = SOURCE_KEYBOARD");
        assert_eq!(p.i32(), 2, "mKeyboardType = ALPHABETIC");
        assert_eq!(p.i32(), -1, "mKeyboardLanguageTag null");
        assert_eq!(p.i32(), -1, "mKeyboardLayoutType null");
        for field in [
            "vibrator",
            "microphone",
            "buttonUnderPad",
            "sensor",
            "battery",
        ] {
            assert_eq!(p.i32(), 0, "mHas{field}");
        }
        assert_eq!(p.i32(), -1, "HostUsiVersion major");
        assert_eq!(p.i32(), -1, "HostUsiVersion minor");
        assert_eq!(p.i32(), -1, "mAssociatedDisplayId");
        assert_eq!(p.i32(), 0, "numRanges");
        assert!(
            p.pos == bytes.len(),
            "consumed {} of {}",
            p.pos,
            bytes.len()
        );
    }

    /// The native KCM payload has no wire length; the codec's `Raw` reader must
    /// walk it (deviceId, cstring, key records, remap tables) so a captured
    /// `InputDevice` reply decodes back to the same value (the replay oracle).
    #[test]
    fn the_input_device_reply_round_trips_through_the_codec() {
        let device = InputService::input_device(-1);
        let mut parcel = acos_parcel::Parcel::new();
        crate::parcelable::write_payload(&mut parcel, &device).expect("write");
        parcel.set_data_position(0);
        let decoded = crate::parcelable::read_payload("android.view.InputDevice", &mut parcel)
            .expect("decode");
        assert_eq!(decoded, device, "InputDevice must round-trip byte for byte");
        parcel.enforce_no_data_avail().expect("no trailing bytes");
    }

    struct Reader<'a> {
        bytes: &'a [u8],
        pos: usize,
    }

    impl Reader<'_> {
        fn i32(&mut self) -> i32 {
            let v = i32::from_le_bytes(self.bytes[self.pos..self.pos + 4].try_into().unwrap());
            self.pos += 4;
            v
        }
        fn string16(&mut self) -> String {
            let len = self.i32();
            assert!(len >= 0, "non-null string");
            let start = self.pos;
            let utf16: Vec<u16> = (0..len as usize)
                .map(|i| {
                    u16::from_le_bytes(
                        self.bytes[start + i * 2..start + i * 2 + 2]
                            .try_into()
                            .unwrap(),
                    )
                })
                .collect();
            self.pos += (len as usize + 1) * 2;
            while self.pos % 4 != 0 {
                self.pos += 1;
            }
            String::from_utf16(&utf16).unwrap()
        }
        fn cstring(&mut self) -> &str {
            let start = self.pos;
            while self.bytes[self.pos] != 0 {
                self.pos += 1;
            }
            let s = std::str::from_utf8(&self.bytes[start..self.pos]).unwrap();
            self.pos += 1;
            while self.pos % 4 != 0 {
                self.pos += 1;
            }
            s
        }
    }
}
