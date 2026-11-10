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

    fn input_device() -> Parcelable {
        Parcelable::ordered(
            "android.view.InputDevice",
            vec![
                Value::Int(0),                 // id
                Value::Int(1),                 // generation
                Value::Int(0),                 // controllerNumber
                Value::Str8(Some("ACOS Touch".into())),
                Value::Int(0x1002 | 0x0001),   // SOURCE_TOUCHSCREEN | SOURCE_KEYBOARD
                Value::Int(0),                 // vendorId
                Value::Int(0),                 // productId
                Value::Int(0),                 // descriptor
                Value::Int(0),                 // keyboardType
                Value::Int(1),                 // hasMotionRangeX
                Value::Int(1),                 // hasMotionRangeY
                Value::Float(-1.0),
                Value::Float(1080.0),
                Value::Float(-1.0),
                Value::Float(2400.0),
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
            "getInputDeviceIds" => vec![Value::IntArray(Some(vec![0]))],
            "getInputDevice" => {
                let id = args.first().and_then(Value::as_int).unwrap_or(0);
                if id == 0 {
                    vec![Value::TypedObject(Self::input_device())]
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

/// A convenience `Bundle` for evidence.
#[must_use]
pub fn empty_bundle() -> Bundle {
    Bundle::new()
}
