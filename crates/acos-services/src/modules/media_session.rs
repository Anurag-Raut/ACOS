//! `media.session` — app `MediaSession`s the agent can read and drive (`stub`,
//! `scripted`).
//!
//! An app that plays media creates a `MediaSession`, publishes what it is
//! playing (`setMetadata`) and how (`setPlaybackState`), and answers transport
//! commands through `MediaSession.Callback`. On a phone `MediaSessionService`
//! (system_server) sits between that app and the lock screen, the media
//! notification, a headset button or another app's `MediaController`. ACOS has
//! none of those consumers, so this module is the service half: it hands the app
//! real `ISession`/`ISessionController` binders, remembers what the app said,
//! lets the **agent** read it, and lets the agent send `play`, `pause`, `next`…
//! through the same `ISessionCallback` the platform uses, so they reach the app's
//! own `MediaSession.Callback`. **Off** is the tombstone (`media_session` →
//! structured `UNSUPPORTED_ANDROID_API`; a null binder would be an NPE out of
//! `getSystemService`).
//!
//! # What is served
//!
//! `ISessionManager` (`media_session`), image dex:
//!
//! | code | method | answer |
//! |---|---|---|
//! | 1 | `createSession(pkg, ISessionCallback, tag, Bundle, userId)` | a new `ISession` binder (+ its `ISessionController`); the callback is held with a host strong reference |
//! | 16 | `isGlobalPriorityActive()` | `false` |
//!
//! `ISession` (one node per session): `setActive`, `setFlags`, `setMetadata`
//! (decoded with the `MediaMetadata` codec; **kept as the framework's own bytes**
//! and replayed to a controller), `setPlaybackState` (the same), `setRatingType`,
//! `getController`, `destroySession`. `setMediaButtonReceiver`,
//! `setMediaButtonBroadcastReceiver`, `setLaunchPendingIntent`, `sendEvent`,
//! `setExtras`, `setQueueTitle`, `setPlaybackToLocal/Remote`, `setCurrentVolume`
//! and `resetQueue` are accepted and listed under `ignored` in `state` — ACOS has
//! no media-key router, launcher or volume UI to give them meaning.
//!
//! `ISessionController` (one node per session): `getPlaybackState`,
//! `getMetadata`, `getPackageName`, `getTag`, `getFlags`, `getRatingType`,
//! `registerCallback`/`unregisterCallback` (a registered
//! `ISessionControllerCallback` is told `onPlaybackStateChanged`,
//! `onMetadataChanged`, `onSessionDestroyed`), and the transport methods
//! (`play`, `pause`, `stop`, `next`, `previous`, `fastForward`, `rewind`,
//! `seekTo`, `skipToQueueItem`, `prepare`, `sendCustomAction`), which a
//! `MediaController` in any app calls and which are forwarded to the session's
//! `ISessionCallback`.
//!
//! Everything else (queue, volume, `getSessions`, `addSessionsListener`, media
//! key dispatch, remote sessions) answers a structured `UNSUPPORTED_ANDROID_API`
//! naming the method. `media_router` is served by this module only to **say so**:
//! every call is a structured `UNSUPPORTED_ANDROID_API` (the same answer the
//! unattached tombstone gives, kept so attaching the sessions module does not
//! turn `MediaRouter` into a null-binder NPE).
//!
//! # Limits that matter
//!
//! * A `MediaMetadata` bundle that holds a value the host cannot decode (album
//!   art `Bitmap`: an ashmem-backed parcelable) is accepted by `setMetadata` —
//!   the app must not crash — but is recorded as `unreadable` with the reason,
//!   and a controller's `getMetadata` then answers a structured error instead of
//!   an invented value.
//! * The command source is `dev.acos.agent` (pid 0, uid 2000): the caller
//!   `MediaSession.Callback` sees through `getCurrentControllerInfo()`.
//!
//! # Control ops (`scripted` drives; `stub` only reads)
//!
//! `sessions` (read: id, package, tag, active, playback, metadata, sent
//! commands), `play`, `pause`, `stop`, `next`, `previous`, `fast_forward`,
//! `rewind`, `seek {position}`, `skip_to_item {id}`, `custom_action {action}`,
//! `reset`. Every command takes an optional `session` (id or tag); the default
//! is the active session, else the newest.

use std::sync::{Arc, Mutex, MutexGuard};

use serde_json::{json, Value};

use crate::error::{Result, ServiceError};
use crate::exception::write_no_exception;
use crate::host::{NodeRegistrar, PendingDelivery, ServiceHost, ServiceObject};
use crate::module::{
    AttachMode, Budget, ControlOp, Module, ModuleConfig, ModuleManifest, ModuleReport, Profile,
    SecurityClass, ServiceProvide,
};
use crate::parcelable;
use crate::refs::GuestRefs;
use crate::value::{Parcelable, Value as PV};

/// The module id.
pub const MODULE_ID: &str = "media.session";
/// `ServiceManager` name of the session manager.
pub const MANAGER_NAME: &str = "media_session";
/// `ISessionManager`.
pub const MANAGER_DESCRIPTOR: &str = "android.media.session.ISessionManager";
/// `ServiceManager` name of the (unimplemented) router.
pub const ROUTER_NAME: &str = "media_router";
/// `IMediaRouterService`.
pub const ROUTER_DESCRIPTOR: &str = "android.media.IMediaRouterService";
/// `ISession`.
pub const SESSION_DESCRIPTOR: &str = "android.media.session.ISession";
/// `ISessionController`.
pub const CONTROLLER_DESCRIPTOR: &str = "android.media.session.ISessionController";
/// `ISessionCallback` (the app's `MediaSession.CallbackStub`).
pub const SESSION_CALLBACK_DESCRIPTOR: &str = "android.media.session.ISessionCallback";
/// `ISessionControllerCallback` (a `MediaController`'s listener).
pub const CONTROLLER_CALLBACK_DESCRIPTOR: &str = "android.media.session.ISessionControllerCallback";

/// Who the platform says sent a command the agent scripted.
const AGENT_PACKAGE: &str = "dev.acos.agent";
const AGENT_PID: i32 = 0;
const AGENT_UID: i32 = 2000;

/// `ISessionCallback` transaction codes (image dex).
const CB_PREPARE: u32 = 4;
const CB_PLAY: u32 = 8;
const CB_SKIP_TO_TRACK: u32 = 12;
const CB_PAUSE: u32 = 13;
const CB_STOP: u32 = 14;
const CB_NEXT: u32 = 15;
const CB_PREVIOUS: u32 = 16;
const CB_FAST_FORWARD: u32 = 17;
const CB_REWIND: u32 = 18;
const CB_SEEK_TO: u32 = 19;
const CB_CUSTOM_ACTION: u32 = 22;

/// `PlaybackState.STATE_*` names.
#[must_use]
pub fn playback_state_name(state: i32) -> &'static str {
    match state {
        0 => "NONE",
        1 => "STOPPED",
        2 => "PAUSED",
        3 => "PLAYING",
        4 => "FAST_FORWARDING",
        5 => "REWINDING",
        6 => "BUFFERING",
        7 => "ERROR",
        8 => "CONNECTING",
        9 => "SKIPPING_TO_PREVIOUS",
        10 => "SKIPPING_TO_NEXT",
        11 => "SKIPPING_TO_QUEUE_ITEM",
        _ => "UNKNOWN",
    }
}

/// One `setMetadata`/`setPlaybackState` value: the framework's own bytes (replayed
/// to a controller) and what the host could read of them.
#[derive(Debug, Clone)]
struct Stored {
    /// The parcelable payload exactly as the app's `writeToParcel` produced it;
    /// `None` when the host could not decode it (see `reason`).
    raw: Option<Vec<u8>>,
    summary: Value,
    reason: Option<String>,
}

/// One live `MediaSession`.
#[derive(Debug)]
struct SessionRecord {
    id: u32,
    package: String,
    tag: String,
    user_id: i32,
    callback: u64,
    session_binder: u64,
    controller_binder: u64,
    active: bool,
    destroyed: bool,
    flags: i64,
    rating_type: i32,
    metadata: Option<Stored>,
    playback: Option<Stored>,
    controller_callbacks: Vec<u64>,
    /// Commands delivered to the app's callback (evidence).
    commands: Vec<String>,
    /// `ISession` calls accepted without meaning (see the module docs).
    ignored: Vec<String>,
}

/// The module's state, shared by the manager and every session node.
#[derive(Debug)]
pub struct MediaState {
    sessions: Vec<SessionRecord>,
    next_id: u32,
    /// The module detached: nodes created for sessions answer "module is off".
    detached: bool,
    refs: GuestRefs,
    unsupported: Vec<String>,
}

impl MediaState {
    fn new() -> Self {
        Self {
            sessions: Vec::new(),
            next_id: 1,
            detached: false,
            refs: GuestRefs::new(),
            unsupported: Vec::new(),
        }
    }

    fn to_json(&self) -> Value {
        json!({
            "sessions": self.sessions.iter().map(session_json).collect::<Vec<_>>(),
            "unsupported": self.unsupported,
        })
    }

    fn session_mut(&mut self, id: u32) -> Option<&mut SessionRecord> {
        self.sessions.iter_mut().find(|session| session.id == id)
    }
}

fn session_json(session: &SessionRecord) -> Value {
    json!({
        "id": session.id,
        "package": session.package,
        "tag": session.tag,
        "user_id": session.user_id,
        "active": session.active,
        "destroyed": session.destroyed,
        "flags": session.flags,
        "rating_type": session.rating_type,
        "playback": session.playback.as_ref().map(|stored| &stored.summary),
        "metadata": session.metadata.as_ref().map(|stored| &stored.summary),
        "controller_callbacks": session.controller_callbacks.len(),
        "commands": session.commands,
        "ignored": session.ignored,
    })
}

fn lock(state: &Arc<Mutex<MediaState>>) -> MutexGuard<'_, MediaState> {
    state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn rejected(detail: impl Into<String>) -> ServiceError {
    ServiceError::ModuleRejected {
        module: MODULE_ID,
        detail: detail.into(),
    }
}

fn unavailable(interface: &str) -> ServiceError {
    ServiceError::ModuleUnavailable {
        service: MANAGER_NAME.to_string(),
        interface: interface.to_string(),
        module: MODULE_ID,
        attach: "acos_launch {services: [{id: \"media.session\", profile: \"scripted\"}]}"
            .to_string(),
    }
}

// ---------------------------------------------------------------------------
// Decoding what the app published
// ---------------------------------------------------------------------------

fn ordered_values(value: &Parcelable) -> Option<&[PV]> {
    match value {
        Parcelable::Ordered { values, .. } => Some(values),
        _ => None,
    }
}

fn bundle_summary(bundle: &crate::value::Bundle) -> Value {
    let mut out = serde_json::Map::new();
    for (key, value) in &bundle.entries {
        let key = key
            .strip_prefix("android.media.metadata.")
            .unwrap_or(key)
            .to_string();
        let json = match value {
            PV::Str(Some(text)) | PV::Str8(Some(text)) => json!(text),
            PV::Int(number) => json!(number),
            PV::Long(number) => json!(number),
            PV::Bool(flag) => json!(flag),
            other => json!(format!("<{}>", value_kind(other))),
        };
        out.insert(key, json);
    }
    Value::Object(out)
}

fn value_kind(value: &PV) -> &'static str {
    match value {
        PV::Parcelable(_) | PV::TypedObject(_) | PV::NamedParcelable(_) => "parcelable",
        PV::ByteArray(_) => "bytes",
        PV::Bundle(_) => "bundle",
        _ => "value",
    }
}

/// Read one typed-object `MediaMetadata`/`PlaybackState` argument: the presence
/// word, then the parcelable payload. Returns the stored form, or `None` for a
/// null object.
fn read_typed(data: &mut acos_parcel::Parcel, class: &str) -> Result<Option<Stored>> {
    if data.read_int32()? == 0 {
        return Ok(None);
    }
    let start = data.data_position();
    let parsed = parcelable::read_payload(class, data);
    let end = data.data_position();
    Ok(Some(match parsed {
        Ok(parcelable) => {
            let raw = data.marshall().get(start..end).map(<[u8]>::to_vec);
            let summary = summarize(class, &parcelable);
            Stored {
                raw,
                summary,
                reason: None,
            }
        }
        Err(error) => Stored {
            raw: None,
            summary: json!({"unreadable": error.to_string()}),
            reason: Some(error.to_string()),
        },
    }))
}

fn summarize(class: &str, parcelable: &Parcelable) -> Value {
    let Some(values) = ordered_values(parcelable) else {
        return Value::Null;
    };
    if class == "android.media.MediaMetadata" {
        return match values.first() {
            Some(PV::Bundle(Some(bundle))) => bundle_summary(bundle),
            _ => json!({}),
        };
    }
    let int = |index: usize| match values.get(index) {
        Some(PV::Int(value)) => *value,
        _ => 0,
    };
    let long = |index: usize| match values.get(index) {
        Some(PV::Long(value)) => *value,
        _ => 0,
    };
    let state = int(0);
    json!({
        "state": playback_state_name(state),
        "state_code": state,
        "position": long(1),
        "speed": match values.get(2) {
            Some(PV::Float(speed)) => f64::from(*speed),
            _ => 0.0,
        },
        "update_time": long(3),
        "buffered_position": long(4),
        "actions": long(5),
        "active_item_id": long(7),
        "error": match values.get(8) {
            Some(PV::Str(Some(text)) | PV::Str8(Some(text))) => json!(text),
            _ => Value::Null,
        },
        "custom_actions": match values.get(6) {
            Some(PV::List(Some(items))) => items.len(),
            _ => 0,
        },
    })
}

// ---------------------------------------------------------------------------
// Deliveries
// ---------------------------------------------------------------------------

/// Who a command is attributed to in `MediaSession.getCurrentControllerInfo()`.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Sender {
    /// The agent's control op (`dev.acos.agent`, uid 2000).
    Agent,
    /// A `MediaController` in an app: the package it named in the call.
    /// `MediaSessionService` reads the caller's real pid/uid from binder; the
    /// service host does not hand a `ServiceObject` the calling identity, so
    /// both are reported as unknown (`-1`) rather than invented.
    Controller(String),
}

fn callback_delivery(
    target: u64,
    code: u32,
    method: &str,
    sender: &Sender,
    extra: impl FnOnce(&mut acos_parcel::Parcel),
) -> Option<PendingDelivery> {
    let mut data = acos_parcel::Parcel::new();
    data.write_interface_token(SESSION_CALLBACK_DESCRIPTOR).ok()?;
    match sender {
        Sender::Agent => {
            data.write_string16(AGENT_PACKAGE).ok()?;
            data.write_int32(AGENT_PID);
            data.write_int32(AGENT_UID);
        }
        Sender::Controller(package) => {
            data.write_string16(package).ok()?;
            data.write_int32(-1);
            data.write_int32(-1);
        }
    }
    extra(&mut data);
    Some(PendingDelivery {
        target: acos_parcel::BinderId(target),
        interface: SESSION_CALLBACK_DESCRIPTOR.to_string(),
        code,
        method: method.to_string(),
        data,
    })
}

fn controller_callback_delivery(
    target: u64,
    code: u32,
    method: &str,
    body: impl FnOnce(&mut acos_parcel::Parcel),
) -> Option<PendingDelivery> {
    let mut data = acos_parcel::Parcel::new();
    data.write_interface_token(CONTROLLER_CALLBACK_DESCRIPTOR)
        .ok()?;
    body(&mut data);
    Some(PendingDelivery {
        target: acos_parcel::BinderId(target),
        interface: CONTROLLER_CALLBACK_DESCRIPTOR.to_string(),
        code,
        method: method.to_string(),
        data,
    })
}

/// The transport commands, shared by a `MediaController`'s `ISessionController`
/// call and the agent's control op: one table, one delivery.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Transport {
    Prepare,
    Play,
    Pause,
    Stop,
    Next,
    Previous,
    FastForward,
    Rewind,
    SeekTo(i64),
    SkipToItem(i64),
    CustomAction,
}

impl Transport {
    fn name(self) -> &'static str {
        match self {
            Self::Prepare => "prepare",
            Self::Play => "play",
            Self::Pause => "pause",
            Self::Stop => "stop",
            Self::Next => "next",
            Self::Previous => "previous",
            Self::FastForward => "fast_forward",
            Self::Rewind => "rewind",
            Self::SeekTo(_) => "seek",
            Self::SkipToItem(_) => "skip_to_item",
            Self::CustomAction => "custom_action",
        }
    }

    fn code(self) -> (u32, &'static str) {
        match self {
            Self::Prepare => (CB_PREPARE, "onPrepare"),
            Self::Play => (CB_PLAY, "onPlay"),
            Self::Pause => (CB_PAUSE, "onPause"),
            Self::Stop => (CB_STOP, "onStop"),
            Self::Next => (CB_NEXT, "onNext"),
            Self::Previous => (CB_PREVIOUS, "onPrevious"),
            Self::FastForward => (CB_FAST_FORWARD, "onFastForward"),
            Self::Rewind => (CB_REWIND, "onRewind"),
            Self::SeekTo(_) => (CB_SEEK_TO, "onSeekTo"),
            Self::SkipToItem(_) => (CB_SKIP_TO_TRACK, "onSkipToTrack"),
            Self::CustomAction => (CB_CUSTOM_ACTION, "onCustomAction"),
        }
    }
}

/// Record a command on `session` and build its `ISessionCallback` delivery.
fn deliver(
    session: &mut SessionRecord,
    command: Transport,
    action: &str,
    sender: &Sender,
) -> Option<PendingDelivery> {
    if session.destroyed {
        return None;
    }
    let (code, method) = command.code();
    let name = match command {
        Transport::SeekTo(position) => format!("seek {position}"),
        Transport::SkipToItem(id) => format!("skip_to_item {id}"),
        Transport::CustomAction => format!("custom_action {action}"),
        other => other.name().to_string(),
    };
    session.commands.push(match sender {
        Sender::Agent => format!("{name} (agent)"),
        Sender::Controller(package) => format!("{name} ({package})"),
    });
    let action = action.to_string();
    callback_delivery(session.callback, code, method, sender, move |data| match command {
        Transport::SeekTo(position) | Transport::SkipToItem(position) => {
            data.write_int64(position);
        }
        Transport::CustomAction => {
            let _ = data.write_string16(&action);
            data.write_int32(0); // `Bundle` extras: a null typed object
        }
        _ => {}
    })
}

// ---------------------------------------------------------------------------
// Manifest
// ---------------------------------------------------------------------------

const CONTROL_OPS: [ControlOp; 12] = [
    ControlOp {
        name: "sessions",
        args: "none — every session: package, tag, active, playback state, metadata, delivered commands",
    },
    ControlOp {
        name: "play",
        args: "{session?} (scripted) — ISessionCallback.onPlay reaches MediaSession.Callback.onPlay",
    },
    ControlOp {
        name: "pause",
        args: "{session?} (scripted) — onPause",
    },
    ControlOp {
        name: "stop",
        args: "{session?} (scripted) — onStop",
    },
    ControlOp {
        name: "next",
        args: "{session?} (scripted) — onSkipToNext",
    },
    ControlOp {
        name: "previous",
        args: "{session?} (scripted) — onSkipToPrevious",
    },
    ControlOp {
        name: "fast_forward",
        args: "{session?} (scripted) — onFastForward",
    },
    ControlOp {
        name: "rewind",
        args: "{session?} (scripted) — onRewind",
    },
    ControlOp {
        name: "seek",
        args: "{session?, position} (scripted) — onSeekTo(position ms)",
    },
    ControlOp {
        name: "skip_to_item",
        args: "{session?, id} (scripted) — onSkipToQueueItem(id)",
    },
    ControlOp {
        name: "custom_action",
        args: "{session?, action} (scripted) — onCustomAction(action, null)",
    },
    ControlOp {
        name: "reset",
        args: "none — clear the recorded `unsupported` calls",
    },
];

fn manifest_ref() -> &'static ModuleManifest {
    static MANIFEST: ModuleManifest = ModuleManifest {
        id: MODULE_ID,
        version: "1.0.0",
        summary: "media sessions: stub (record what apps publish) / scripted (agent play, pause, next, seek reach MediaSession.Callback); no media-key routing, queue, volume, remote sessions or router",
        services: &[
            ServiceProvide {
                name: MANAGER_NAME,
                descriptor: MANAGER_DESCRIPTOR,
            },
            ServiceProvide {
                name: ROUTER_NAME,
                descriptor: ROUTER_DESCRIPTOR,
            },
        ],
        replaces: &[],
        process_initializers: &[],
        features: &[],
        providers: &[],
        broadcasts: &[],
        windows: &[],
        system_apps: &[],
        requires: &[],
        requires_host_tools: &[],
        profiles: &[Profile::Stub, Profile::Scripted],
        budget: Budget {
            ram_mib: 1,
            startup_ms: 5,
        },
        attach: AttachMode::Hot,
        controls: &CONTROL_OPS,
        security: SecurityClass::InProcess,
    };
    &MANIFEST
}

/// The module.
pub struct MediaSessionModule {
    profile: Profile,
    state: Option<Arc<Mutex<MediaState>>>,
}

impl MediaSessionModule {
    /// A detached module.
    #[must_use]
    pub fn new() -> Self {
        Self {
            profile: Profile::Off,
            state: None,
        }
    }

    /// The manifest (tests and the registry).
    #[must_use]
    pub fn manifest_ref() -> &'static ModuleManifest {
        manifest_ref()
    }
}

impl Default for MediaSessionModule {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// The nodes
// ---------------------------------------------------------------------------

/// `IMediaRouterService`: every call says it is not modelled.
struct RouterService;

impl ServiceObject for RouterService {
    fn call(
        &mut self,
        interface: &str,
        method: &acos_aidl::MethodInfo,
        _args: &[crate::value::Value],
    ) -> Result<crate::host::CallResult> {
        Err(ServiceError::UnsupportedAndroidApi {
            interface: interface.to_string(),
            code: method.code,
            method: format!(
                "{MODULE_ID}: {} (media_router is not modelled; the module serves sessions only)",
                method.name
            ),
        })
    }

    fn raw_transact(
        &mut self,
        interface: &str,
        code: u32,
        _data: &mut acos_parcel::Parcel,
        _reply: &mut acos_parcel::Parcel,
    ) -> Result<bool> {
        Err(ServiceError::UnsupportedAndroidApi {
            interface: interface.to_string(),
            code,
            method: format!(
                "{MODULE_ID}: media_router is not modelled; the module serves sessions only"
            ),
        })
    }
}

struct ManagerService {
    state: Arc<Mutex<MediaState>>,
    nodes: NodeRegistrar,
}

impl ServiceObject for ManagerService {
    fn drain_retained(&mut self) -> Vec<acos_parcel::BinderId> {
        lock(&self.state).refs.drain_retained()
    }

    fn drain_released(&mut self) -> Vec<acos_parcel::BinderId> {
        lock(&self.state).refs.drain_released()
    }

    fn release_all_refs(&mut self) -> Vec<acos_parcel::BinderId> {
        lock(&self.state).refs.release_all()
    }

    fn call(
        &mut self,
        interface: &str,
        method: &acos_aidl::MethodInfo,
        _args: &[crate::value::Value],
    ) -> Result<crate::host::CallResult> {
        lock(&self.state)
            .unsupported
            .push(format!("{interface}::{}", method.name));
        Err(ServiceError::UnsupportedAndroidApi {
            interface: interface.to_string(),
            code: method.code,
            method: format!("{MODULE_ID}: {} has no stub answer", method.name),
        })
    }

    fn raw_transact(
        &mut self,
        interface: &str,
        code: u32,
        data: &mut acos_parcel::Parcel,
        reply: &mut acos_parcel::Parcel,
    ) -> Result<bool> {
        if interface != MANAGER_DESCRIPTOR {
            return Ok(false);
        }
        if lock(&self.state).detached {
            return Err(unavailable(interface));
        }
        match code {
            // ISession createSession(String packageName, ISessionCallback cb,
            //                        String tag, in Bundle sessionInfo, int userId)
            1 => {
                let package = data.read_string16()?.unwrap_or_default();
                let callback = data.read_strong_binder()?.ok_or_else(|| ServiceError::BadRequest {
                    interface: interface.to_string(),
                    method: "createSession".to_string(),
                    detail: "the ISessionCallback binder is null".to_string(),
                })?;
                let tag = data.read_string16()?.unwrap_or_default();
                // `sessionInfo` is a typed Bundle; a malformed one is not worth
                // refusing the session for.
                if data.read_int32()? != 0 {
                    let _ = parcelable::read_bundle(data);
                }
                let user_id = data.read_int32().unwrap_or(0);

                let session_id = lock(&self.state).next_id;
                let session_binder = self.nodes.register(
                    SESSION_DESCRIPTOR,
                    Box::new(SessionNode {
                        state: Arc::clone(&self.state),
                        id: session_id,
                        queued: Vec::new(),
                    }),
                );
                let controller_binder = self.nodes.register(
                    CONTROLLER_DESCRIPTOR,
                    Box::new(ControllerNode {
                        state: Arc::clone(&self.state),
                        id: session_id,
                        queued: Vec::new(),
                    }),
                );
                {
                    let mut state = lock(&self.state);
                    state.next_id += 1;
                    state.refs.hold(callback);
                    state.sessions.push(SessionRecord {
                        id: session_id,
                        package,
                        tag,
                        user_id,
                        callback: callback.0,
                        session_binder: session_binder.0,
                        controller_binder: controller_binder.0,
                        active: false,
                        destroyed: false,
                        flags: 0,
                        rating_type: 0,
                        metadata: None,
                        playback: None,
                        controller_callbacks: Vec::new(),
                        commands: Vec::new(),
                        ignored: Vec::new(),
                    });
                }
                write_no_exception(reply);
                reply.write_strong_binder(session_binder);
                Ok(true)
            }
            // boolean isGlobalPriorityActive()
            16 => {
                write_no_exception(reply);
                reply.write_int32(0);
                Ok(true)
            }
            _ => {
                let name = acos_aidl::lookup(MANAGER_DESCRIPTOR, code)
                    .map_or_else(|| format!("code {code}"), |method| method.name.clone());
                lock(&self.state).unsupported.push(name);
                Ok(false)
            }
        }
    }
}

/// Run `f` on the node's session while the module is attached and the session is
/// live enough to answer.
fn with_session<R>(
    state: &Arc<Mutex<MediaState>>,
    id: u32,
    interface: &str,
    f: impl FnOnce(&mut SessionRecord, &mut GuestRefs) -> R,
) -> Result<R> {
    let mut guard = lock(state);
    if guard.detached {
        return Err(unavailable(interface));
    }
    let state = &mut *guard;
    let refs = &mut state.refs;
    let session = state
        .sessions
        .iter_mut()
        .find(|session| session.id == id)
        .ok_or_else(|| unavailable(interface))?;
    Ok(f(session, refs))
}

fn controller_updates(
    session: &SessionRecord,
    code: u32,
    method: &str,
    stored: Option<&Stored>,
) -> Vec<PendingDelivery> {
    let Some(raw) = stored.and_then(|stored| stored.raw.clone()) else {
        return Vec::new();
    };
    session
        .controller_callbacks
        .iter()
        .filter_map(|callback| {
            let raw = raw.clone();
            controller_callback_delivery(*callback, code, method, move |data| {
                data.write_int32(1);
                data.write_raw_bytes(&raw);
            })
        })
        .collect()
}

struct SessionNode {
    state: Arc<Mutex<MediaState>>,
    id: u32,
    queued: Vec<PendingDelivery>,
}

impl ServiceObject for SessionNode {
    fn call(
        &mut self,
        interface: &str,
        method: &acos_aidl::MethodInfo,
        _args: &[crate::value::Value],
    ) -> Result<crate::host::CallResult> {
        Err(ServiceError::UnsupportedAndroidApi {
            interface: interface.to_string(),
            code: method.code,
            method: format!("{MODULE_ID}: {} has no stub answer", method.name),
        })
    }

    fn finished(&self) -> bool {
        let state = lock(&self.state);
        state.detached
            || state
                .sessions
                .iter()
                .find(|session| session.id == self.id)
                .is_none_or(|session| session.destroyed)
    }

    fn drain_raw_deliveries(&mut self) -> Vec<PendingDelivery> {
        std::mem::take(&mut self.queued)
    }

    fn raw_transact(
        &mut self,
        interface: &str,
        code: u32,
        data: &mut acos_parcel::Parcel,
        reply: &mut acos_parcel::Parcel,
    ) -> Result<bool> {
        if interface != SESSION_DESCRIPTOR {
            return Ok(false);
        }
        let ignored = |name: &'static str| {
            move |session: &mut SessionRecord, _refs: &mut GuestRefs| {
                session.ignored.push(name.to_string());
            }
        };
        match code {
            // void sendEvent(String, in Bundle)
            1 => with_session(&self.state, self.id, interface, ignored("sendEvent"))?,
            // IBinder getController()
            2 => {
                let controller =
                    with_session(&self.state, self.id, interface, |session, _| {
                        session.controller_binder
                    })?;
                write_no_exception(reply);
                reply.write_strong_binder(acos_parcel::BinderId(controller));
                return Ok(true);
            }
            // void setFlags(int)
            3 => {
                let flags = data.read_int32()?;
                with_session(&self.state, self.id, interface, |session, _| {
                    session.flags = i64::from(flags);
                })?;
            }
            // void setActive(boolean)
            4 => {
                let active = data.read_bool()?;
                with_session(&self.state, self.id, interface, |session, _| {
                    session.active = active;
                })?;
            }
            5 => with_session(
                &self.state,
                self.id,
                interface,
                ignored("setMediaButtonReceiver"),
            )?,
            6 => with_session(
                &self.state,
                self.id,
                interface,
                ignored("setMediaButtonBroadcastReceiver"),
            )?,
            7 => with_session(
                &self.state,
                self.id,
                interface,
                ignored("setLaunchPendingIntent"),
            )?,
            // void destroySession()
            8 => {
                let deliveries = with_session(&self.state, self.id, interface, |session, refs| {
                    session.destroyed = true;
                    session.active = false;
                    refs.release(acos_parcel::BinderId(session.callback));
                    session
                        .controller_callbacks
                        .iter()
                        .filter_map(|callback| {
                            controller_callback_delivery(
                                *callback,
                                2,
                                "onSessionDestroyed",
                                |_| {},
                            )
                        })
                        .collect::<Vec<_>>()
                })?;
                self.queued.extend(deliveries);
            }
            // void setMetadata(in MediaMetadata, long duration, String description)
            9 => {
                let stored = read_typed(data, "android.media.MediaMetadata")?;
                let deliveries = with_session(&self.state, self.id, interface, |session, _| {
                    session.metadata = stored.clone();
                    controller_updates(session, 4, "onMetadataChanged", stored.as_ref())
                })?;
                self.queued.extend(deliveries);
            }
            // void setPlaybackState(in PlaybackState)
            10 => {
                let stored = read_typed(data, "android.media.session.PlaybackState")?;
                let deliveries = with_session(&self.state, self.id, interface, |session, _| {
                    session.playback = stored.clone();
                    controller_updates(session, 3, "onPlaybackStateChanged", stored.as_ref())
                })?;
                self.queued.extend(deliveries);
            }
            11 => with_session(&self.state, self.id, interface, ignored("resetQueue"))?,
            13 => with_session(&self.state, self.id, interface, ignored("setQueueTitle"))?,
            14 => with_session(&self.state, self.id, interface, ignored("setExtras"))?,
            // void setRatingType(int)
            15 => {
                let rating = data.read_int32()?;
                with_session(&self.state, self.id, interface, |session, _| {
                    session.rating_type = rating;
                })?;
            }
            16 => with_session(
                &self.state,
                self.id,
                interface,
                ignored("setPlaybackToLocal"),
            )?,
            17 => with_session(
                &self.state,
                self.id,
                interface,
                ignored("setPlaybackToRemote"),
            )?,
            18 => with_session(
                &self.state,
                self.id,
                interface,
                ignored("setCurrentVolume"),
            )?,
            _ => {
                let name = acos_aidl::lookup(SESSION_DESCRIPTOR, code)
                    .map_or_else(|| format!("code {code}"), |method| method.name.clone());
                lock(&self.state).unsupported.push(format!("ISession::{name}"));
                return Ok(false);
            }
        }
        write_no_exception(reply);
        Ok(true)
    }
}

struct ControllerNode {
    state: Arc<Mutex<MediaState>>,
    id: u32,
    queued: Vec<PendingDelivery>,
}

impl ServiceObject for ControllerNode {
    fn call(
        &mut self,
        interface: &str,
        method: &acos_aidl::MethodInfo,
        _args: &[crate::value::Value],
    ) -> Result<crate::host::CallResult> {
        Err(ServiceError::UnsupportedAndroidApi {
            interface: interface.to_string(),
            code: method.code,
            method: format!("{MODULE_ID}: {} has no stub answer", method.name),
        })
    }

    fn finished(&self) -> bool {
        let state = lock(&self.state);
        state.detached
            || state
                .sessions
                .iter()
                .find(|session| session.id == self.id)
                .is_none_or(|session| session.destroyed)
    }

    fn drain_raw_deliveries(&mut self) -> Vec<PendingDelivery> {
        std::mem::take(&mut self.queued)
    }

    fn drain_retained(&mut self) -> Vec<acos_parcel::BinderId> {
        lock(&self.state).refs.drain_retained()
    }

    fn drain_released(&mut self) -> Vec<acos_parcel::BinderId> {
        lock(&self.state).refs.drain_released()
    }

    fn raw_transact(
        &mut self,
        interface: &str,
        code: u32,
        data: &mut acos_parcel::Parcel,
        reply: &mut acos_parcel::Parcel,
    ) -> Result<bool> {
        if interface != CONTROLLER_DESCRIPTOR {
            return Ok(false);
        }
        let state = Arc::clone(&self.state);
        let id = self.id;
        // The transport methods a `MediaController` calls: the platform forwards
        // each to the session's `ISessionCallback`, with the caller's identity.
        let transport = match code {
            13 => Some(Transport::Prepare),
            17 => Some(Transport::Play),
            22 => Some(Transport::Pause),
            23 => Some(Transport::Stop),
            24 => Some(Transport::Next),
            25 => Some(Transport::Previous),
            26 => Some(Transport::FastForward),
            27 => Some(Transport::Rewind),
            _ => None,
        };
        if let Some(command) = transport {
            let sender = Sender::Controller(data.read_string16()?.unwrap_or_default());
            let delivery = with_session(&state, id, interface, |session, _| {
                deliver(session, command, "", &sender)
            })?;
            self.queued.extend(delivery);
            write_no_exception(reply);
            return Ok(true);
        }
        match code {
            // void seekTo(String packageName, long pos)
            28 => {
                let sender = Sender::Controller(data.read_string16()?.unwrap_or_default());
                let position = data.read_int64()?;
                let delivery = with_session(&state, id, interface, |session, _| {
                    deliver(session, Transport::SeekTo(position), "", &sender)
                })?;
                self.queued.extend(delivery);
            }
            // void skipToQueueItem(String packageName, long id)
            21 => {
                let sender = Sender::Controller(data.read_string16()?.unwrap_or_default());
                let item = data.read_int64()?;
                let delivery = with_session(&state, id, interface, |session, _| {
                    deliver(session, Transport::SkipToItem(item), "", &sender)
                })?;
                self.queued.extend(delivery);
            }
            // void sendCustomAction(String packageName, String action, in Bundle args)
            31 => {
                let sender = Sender::Controller(data.read_string16()?.unwrap_or_default());
                let action = data.read_string16()?.unwrap_or_default();
                let delivery = with_session(&state, id, interface, |session, _| {
                    deliver(session, Transport::CustomAction, &action, &sender)
                })?;
                self.queued.extend(delivery);
            }
            // void registerCallback(String packageName, ISessionControllerCallback cb)
            3 => {
                let _ = data.read_string16()?;
                if let Some(callback) = data.read_strong_binder()? {
                    with_session(&state, id, interface, |session, refs| {
                        if !session.controller_callbacks.contains(&callback.0) {
                            session.controller_callbacks.push(callback.0);
                        }
                        refs.hold(callback);
                    })?;
                }
            }
            // void unregisterCallback(ISessionControllerCallback cb)
            4 => {
                if let Some(callback) = data.read_strong_binder()? {
                    with_session(&state, id, interface, |session, refs| {
                        session.controller_callbacks.retain(|c| *c != callback.0);
                        refs.release(callback);
                    })?;
                }
            }
            // String getPackageName() / String getTag()
            5 | 6 => {
                let text = with_session(&state, id, interface, |session, _| {
                    if code == 5 {
                        session.package.clone()
                    } else {
                        session.tag.clone()
                    }
                })?;
                write_no_exception(reply);
                reply.write_string16_opt(Some(&text))?;
                return Ok(true);
            }
            // Bundle getSessionInfo() / Bundle getExtras(): null
            7 | 36 => {
                write_no_exception(reply);
                reply.write_int32(0);
                return Ok(true);
            }
            // long getFlags()
            9 => {
                let flags = with_session(&state, id, interface, |session, _| session.flags)?;
                write_no_exception(reply);
                reply.write_int64(flags);
                return Ok(true);
            }
            // MediaMetadata getMetadata() / PlaybackState getPlaybackState()
            32 | 33 => {
                let stored = with_session(&state, id, interface, |session, _| {
                    if code == 32 {
                        session.metadata.clone()
                    } else {
                        session.playback.clone()
                    }
                })?;
                match stored {
                    None => {
                        write_no_exception(reply);
                        reply.write_int32(0);
                    }
                    Some(Stored { raw: Some(raw), .. }) => {
                        write_no_exception(reply);
                        reply.write_int32(1);
                        reply.write_raw_bytes(&raw);
                    }
                    Some(Stored { reason, .. }) => {
                        return Err(ServiceError::UnsupportedAndroidApi {
                            interface: interface.to_string(),
                            code,
                            method: format!(
                                "{MODULE_ID}: the session published a value the host could not decode ({}); \
                                 it will not invent one",
                                reason.unwrap_or_default()
                            ),
                        })
                    }
                }
                return Ok(true);
            }
            // int getRatingType()
            37 => {
                let rating = with_session(&state, id, interface, |session, _| session.rating_type)?;
                write_no_exception(reply);
                reply.write_int32(rating);
                return Ok(true);
            }
            _ => {
                let name = acos_aidl::lookup(CONTROLLER_DESCRIPTOR, code)
                    .map_or_else(|| format!("code {code}"), |method| method.name.clone());
                lock(&self.state)
                    .unsupported
                    .push(format!("ISessionController::{name}"));
                return Ok(false);
            }
        }
        write_no_exception(reply);
        Ok(true)
    }
}

// ---------------------------------------------------------------------------
// The module
// ---------------------------------------------------------------------------

impl Module for MediaSessionModule {
    fn manifest(&self) -> &'static ModuleManifest {
        manifest_ref()
    }

    fn attach(
        &mut self,
        host: &mut ServiceHost,
        profile: Profile,
        config: &ModuleConfig,
    ) -> Result<()> {
        if !matches!(profile, Profile::Stub | Profile::Scripted) {
            return Err(rejected(format!(
                "{profile} is not implemented: ACOS has no media-key router or system UI, so use `stub` or `scripted`"
            )));
        }
        if !config.is_empty() {
            return Err(rejected(format!(
                "this module takes no config, got {}",
                config.to_json()
            )));
        }
        let state = Arc::new(Mutex::new(MediaState::new()));
        host.register_dynamic_named(
            MANAGER_NAME,
            MANAGER_DESCRIPTOR,
            Box::new(ManagerService {
                state: Arc::clone(&state),
                nodes: host.node_registrar(),
            }),
        );
        host.register_dynamic_named(ROUTER_NAME, ROUTER_DESCRIPTOR, Box::new(RouterService));
        self.state = Some(state);
        self.profile = profile;
        Ok(())
    }

    fn detach(&mut self, _host: &mut ServiceHost) -> Result<()> {
        if let Some(state) = self.state.take() {
            lock(&state).detached = true;
        }
        self.profile = Profile::Off;
        Ok(())
    }

    fn control(&mut self, host: &mut ServiceHost, op: &str, args: &Value) -> Result<Value> {
        let Some(shared) = self.state.as_ref().cloned() else {
            return Err(rejected(format!(
                "module is off, so {op:?} was not applied"
            )));
        };
        let command = match op {
            "sessions" => return Ok(lock(&shared).to_json()),
            "reset" => {
                lock(&shared).unsupported.clear();
                return Ok(json!({"reset": true}));
            }
            "play" => Transport::Play,
            "pause" => Transport::Pause,
            "stop" => Transport::Stop,
            "next" => Transport::Next,
            "previous" => Transport::Previous,
            "fast_forward" => Transport::FastForward,
            "rewind" => Transport::Rewind,
            "seek" => Transport::SeekTo(
                args.get("position")
                    .and_then(Value::as_i64)
                    .ok_or_else(|| rejected("seek needs {position: <ms>}"))?,
            ),
            "skip_to_item" => Transport::SkipToItem(
                args.get("id")
                    .and_then(Value::as_i64)
                    .ok_or_else(|| rejected("skip_to_item needs {id: <queue item id>}"))?,
            ),
            "custom_action" => Transport::CustomAction,
            other => {
                return Err(rejected(format!(
                    "unknown control op {other:?}; this module implements {}",
                    CONTROL_OPS
                        .iter()
                        .map(|op| op.name)
                        .collect::<Vec<_>>()
                        .join(", ")
                )))
            }
        };
        if self.profile != Profile::Scripted {
            return Err(rejected(format!(
                "{op:?} sends a command to the app, and the `stub` profile only records; \
                 attach `scripted` ({{\"id\": \"{MODULE_ID}\", \"profile\": \"scripted\"}}) to drive it"
            )));
        }
        let action = match (command, args.get("action")) {
            (Transport::CustomAction, Some(Value::String(action))) if !action.is_empty() => {
                action.clone()
            }
            (Transport::CustomAction, _) => {
                return Err(rejected("custom_action needs {action: <string>}"))
            }
            _ => String::new(),
        };
        let selector = args.get("session");
        let (delivery, summary) = {
            let mut state = lock(&shared);
            let session = pick_session(&mut state, selector)?;
            let delivery = deliver(session, command, &action, &Sender::Agent);
            (delivery, session_json(session))
        };
        let Some(delivery) = delivery else {
            return Err(rejected(
                "the selected session is destroyed; there is no callback to reach",
            ));
        };
        host.queue_delivery(delivery);
        Ok(json!({
            "sent": command.name(),
            "callback": command.code().1,
            "session": summary,
        }))
    }

    fn report(&self) -> ModuleReport {
        let mut report = ModuleReport::of(manifest_ref(), self.profile);
        if self.profile.is_attached() {
            let count = self
                .state
                .as_ref()
                .map_or(0, |state| lock(state).sessions.len());
            report.reason = Some(format!(
                "{} profile: sessions are recorded and commands are delivered to the app's MediaSession.Callback; {count} session(s) so far; no media-key routing, queue, volume, remote sessions or media router (UNSUPPORTED_ANDROID_API); metadata with an undecodable value (album art) is accepted but reported unreadable",
                self.profile
            ));
        }
        report
    }
}

/// The session a command targets: `session` (id or tag), else the active one,
/// else the newest live one.
fn pick_session<'a>(
    state: &'a mut MediaState,
    selector: Option<&Value>,
) -> Result<&'a mut SessionRecord> {
    let live = |session: &&mut SessionRecord| !session.destroyed;
    let index = match selector {
        None | Some(Value::Null) => {
            let sessions = &state.sessions;
            sessions
                .iter()
                .rposition(|session| !session.destroyed && session.active)
                .or_else(|| sessions.iter().rposition(|session| !session.destroyed))
        }
        Some(Value::Number(number)) => {
            let id = number.as_u64().and_then(|id| u32::try_from(id).ok());
            state
                .sessions
                .iter()
                .position(|session| Some(session.id) == id && !session.destroyed)
        }
        Some(Value::String(tag)) => state
            .sessions
            .iter()
            .rposition(|session| session.tag == *tag && !session.destroyed),
        Some(other) => {
            return Err(rejected(format!(
                "`session` must be a session id or a tag, got {other}"
            )))
        }
    };
    let _ = live;
    match index {
        Some(index) => Ok(&mut state.sessions[index]),
        None => Err(rejected(
            "no such live session: the app has not created a MediaSession (or it was released); \
             `sessions` lists what exists",
        )),
    }
}
