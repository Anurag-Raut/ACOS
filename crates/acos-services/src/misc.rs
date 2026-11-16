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

use crate::error::{ServiceError, Result};
use crate::host::{CallResult, PendingDelivery, ServiceObject};
use crate::value::{Bundle, Parcelable, RawFdValue, Value};

// ---------------------------------------------------------------------------
// accessibility
// ---------------------------------------------------------------------------

/// `IAccessibilityManager` state (disabled, matching the reference).
#[derive(Debug, Default)]
pub struct A11yState {
    /// Registered client binders.
    pub clients: Vec<u64>,
    /// Next client id (the reference returns 1).
    pub next_client_id: i64,
}

impl A11yState {
    /// New disabled state.
    #[must_use]
    pub fn new() -> Self {
        Self {
            clients: Vec::new(),
            next_client_id: 1,
        }
    }
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
        let mut state = self.state.lock().unwrap();
        let reply = match method.name.as_str() {
            "addClient" => {
                if let Some(client) = args.first().and_then(Value::as_binder) {
                    state.clients.push(client.0);
                }
                let id = state.next_client_id;
                state.next_client_id += 1;
                vec![Value::Long(id)]
            }
            "getRecommendedTimeoutMillis" => vec![Value::Long(0)],
            "getFocusStrokeWidth" => vec![Value::Int(0)],
            "getFocusColor" => vec![Value::Int(0)],
            "isAudioDescriptionByDefaultEnabled" => vec![Value::Bool(false)],
            "removeClient" => vec![Value::Bool(true)],
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
        let bytes =
            crate::keychars::stock_parcel(id < 0).unwrap_or_else(|| crate::keychars::null_parcel(id));
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
                Value::Int(id),                                // mId
                Value::Int(1),                                 // mGeneration
                Value::Int(0),                                 // mControllerNumber
                Value::Str(Some(name.into())),                 // mName (writeString)
                Value::Int(0),                                 // mVendorId
                Value::Int(0),                                 // mProductId
                Value::Str(Some(descriptor.into())),           // mDescriptor (writeString)
                Value::Int(0),                                 // mIsExternal
                Value::Int(sources),                           // mSources
                Value::Int(keyboard_type),                     // mKeyboardType
                Value::Str8(None),                             // mKeyboardLanguageTag
                Value::Str8(None),                             // mKeyboardLayoutType
                Value::Int(0),                                 // mHasVibrator
                Value::Int(0),                                 // mHasMicrophone
                Value::Int(0),                                 // mHasButtonUnderPad
                Value::Int(0),                                 // mHasSensor
                Value::Int(0),                                 // mHasBattery
                Value::Parcelable(Parcelable::ordered(
                    "android.hardware.input.HostUsiVersion",
                    vec![Value::Int(-1), Value::Int(-1)],
                )),                                            // mHostUsiVersion
                Value::Int(-1),                                // mAssociatedDisplayId
                Value::Int(0),                                 // numRanges
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
    fn call(&mut self, interface: &str, method: &MethodInfo, _args: &[Value]) -> Result<CallResult> {
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
                self.state.lock().unwrap().requests.push(process.to_string());
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
                self.state.lock().unwrap().cleartext_policies.push((uid, policy));
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
    fn call(&mut self, interface: &str, method: &MethodInfo, _args: &[Value]) -> Result<CallResult> {
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
    fn call(&mut self, interface: &str, method: &MethodInfo, _args: &[Value]) -> Result<CallResult> {
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
                let token = args.first().and_then(Value::as_binder).map(|b| b.0).unwrap_or(0);
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
// sensors (W26 -> MODSENS): the native `sensorservice`, now in `crate::sensors`
// ---------------------------------------------------------------------------
//
// `SystemSensorManager.nativeCreate` (libandroid_runtime) constructs
// `android::SensorManager`, which calls
// `defaultServiceManager()->getService(String16("sensorservice"))`; a **null**
// answer costs libbinder's `ServiceManagerShim::getService` retry loop
// (`native/libs/binder/IServiceManager.cpp:309`) 5000 ms per lookup and the
// framework makes two, which spent ~12 s of the launcher's resume budget (W25
// §3c). The node therefore always exists.
//
// The reply shapes, the sensor table and the event channel live in
// [`crate::sensors`] since the `sensors` service module fills the node with a
// scripted feed (ADR-0029). This module re-exports the names W26 pinned so the
// existing evidence tests keep compiling against one source of truth.
pub use crate::sensors::{
    SENSOR_CREATE_DIRECT_CONNECTION, SENSOR_CREATE_EVENT_CONNECTION,
    SENSOR_GET_DYNAMIC_SENSOR_LIST, SENSOR_GET_SENSOR_LIST, SENSOR_NOARG_INT,
    SENSOR_SERVER_DESCRIPTOR,
};


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
        let mut p = Reader { bytes: &bytes, pos: 0 };

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
        for field in ["vibrator", "microphone", "buttonUnderPad", "sensor", "battery"] {
            assert_eq!(p.i32(), 0, "mHas{field}");
        }
        assert_eq!(p.i32(), -1, "HostUsiVersion major");
        assert_eq!(p.i32(), -1, "HostUsiVersion minor");
        assert_eq!(p.i32(), -1, "mAssociatedDisplayId");
        assert_eq!(p.i32(), 0, "numRanges");
        assert!(p.pos == bytes.len(), "consumed {} of {}", p.pos, bytes.len());
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
                        self.bytes[start + i * 2..start + i * 2 + 2].try_into().unwrap(),
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
