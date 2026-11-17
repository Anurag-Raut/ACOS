//! `system.notifications` — the notification layer as a module (MODNOTIF4).
//!
//! `core` answers `android.app.INotificationManager` with a *recorder*
//! (`declared::NotificationService`): it stores `(package, id)` and an empty
//! list for `getActiveNotifications`, because ACOS had no status bar. This
//! module is the real state: channels, posted notifications with the decoded
//! title/text/actions/channel and the content `PendingIntent` *sender*, the
//! `POST_NOTIFICATIONS` gate, foreground-service notifications, and control ops
//! an agent or scenario can drive (`list`, `tap`, `action`, `dismiss`).
//!
//! It takes the `notification` name over from `core` (same descriptor, so
//! `ServiceHost::end_scope` restores core's factory and binder on detach —
//! MODNOTIF2 proved that path).
//!
//! ## What Android does, and what this does
//!
//! * **`POST_NOTIFICATIONS`.** From API 33 a notification is dropped unless the
//!   app holds `android.permission.POST_NOTIFICATIONS`
//!   (`NotificationManagerService.enqueueNotificationInternal` →
//!   `checkDisqualifyingFeatures`). The gate reads the same
//!   [`crate::permissions::PermissionStore`] `IPermissionManager`,
//!   `PackageManager.checkPermission` and `ActivityManager.checkPermission` read
//!   (the store the `permissions` module writes), and `targetSdk < 33` is
//!   exempt exactly as AOSP is. A dropped notification is **recorded** — the app
//!   gets no exception, which is what a real device does.
//! * **`areNotificationsEnabled`** answers the same gate, so an app that checks
//!   it before posting sees the truth.
//! * **`tap`** fires the notification's content `PendingIntent` through the
//!   sender record (`AmsState::send_pending_intent`), i.e. the same delivery a
//!   real `IIntentSender.send` would perform: the target activity is started by
//!   the system. Whether the *guest* draws the second activity is the
//!   system→app `LaunchActivityItem` work (MPROC), so `tap` also reports the
//!   sender evidence (`start_result`) — the assertion an ART run can make today.
//! * **`action`** fires an action's `actionIntent` the same way (index-based,
//!   like `Notification.Action`).
//! * **`dismiss`/`cancelAll`** remove the record; a foreground-service
//!   notification cannot be dismissed (AOSP keeps it until the service stops —
//!   `NotificationManagerService.cancelNotificationWithTag` refuses to remove an
//!   FGS notification whose service is still running).
//!
//! Control ops: `list`, `tap {id|tag|title|text}`, `action {id, index}`,
//! `dismiss {id|tag}`. Launch config: `{"grant": [...]}` applies to the
//! permission store (the `permissions` module owns the store, so the config is
//! shared), `{"target_sdk": 34}` overrides the API level the gate uses.

use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use crate::error::{Result, ServiceError};
use crate::host::{CallResult, ServiceHost, ServiceObject};
use crate::module::{
    AttachMode, Budget, ControlOp, Module, ModuleConfig, ModuleManifest, ModuleReport, Profile,
    SecurityClass, ServiceProvide,
};
use crate::parcelable::parceled_list_of;
use crate::permissions::{PermissionStore, PERMISSION_GRANTED};
use crate::value::{Parcelable, Value as WireValue};

/// The `ServiceManager` name this module takes over from `core`.
pub const SERVICE_NAME: &str = "notification";
/// The AIDL descriptor of the interface it serves.
pub const DESCRIPTOR: &str = "android.app.INotificationManager";
/// The module id.
pub const MODULE_ID: &str = "system.notifications";
/// `android.permission.POST_NOTIFICATIONS` (`Manifest.permission`).
pub const POST_NOTIFICATIONS: &str = "android.permission.POST_NOTIFICATIONS";

/// SYSUI1: the guest system-UI app that draws the notification shade
/// (`com.android.systemui`, the package a real device's shade lives in).
pub const SHADE_PACKAGE: &str = "com.android.systemui";
/// SYSUI1: the shade activity (`package/class`, as a launch intent names it).
pub const SHADE_COMPONENT: &str = "com.android.systemui/.ShadeActivity";
/// SYSUI2: the shade activity's class (`ShadeActivity` in `com.android.systemui`).
pub const SHADE_ACTIVITY: &str = "com.android.systemui.ShadeActivity";
/// SYSUI1: the control op that opens the shade.
pub const OPEN_SHADE: &str = "open_shade";
/// SYSUI2: launch extra — one key per row, `"<id>:<tag>"` (`tag` may be empty).
pub const SHADE_EXTRA_KEYS: &str = "acos.shade.keys";
/// SYSUI2: launch extra — one `android.title` per row.
pub const SHADE_EXTRA_TITLES: &str = "acos.shade.titles";
/// SYSUI2: launch extra — one `android.text` per row.
pub const SHADE_EXTRA_TEXTS: &str = "acos.shade.texts";
/// SYSUI2: launch extra — one comma-joined action-title list per row.
pub const SHADE_EXTRA_ACTIONS: &str = "acos.shade.actions";
/// SYSUI2: result extra — the key of the row the user tapped.
pub const SHADE_EXTRA_TAPPED: &str = "acos.shade.tapped";
/// `config: {"shade_apk": …}` override for the shade system app.
pub const SHADE_APK_CONFIG: &str = "shade_apk";
/// Environment override for the shade system app.
pub const SHADE_APK_ENV: &str = "ACOS_SYSTEMUI_APK";
/// Workspace-relative default build output.
pub const SHADE_APK_REL: &str = "fixtures/build/apps/systemui.apk";

/// The `ServiceManager` name -> descriptor pairs this module serves.
pub const SERVICES: &[ServiceProvide] = &[ServiceProvide {
    name: SERVICE_NAME,
    descriptor: DESCRIPTOR,
}];

/// `Notification.FLAG_ONGOING_EVENT`.
pub const FLAG_ONGOING_EVENT: i32 = 0x2;
/// `Notification.FLAG_FOREGROUND_SERVICE`.
pub const FLAG_FOREGROUND_SERVICE: i32 = 0x40;

const CONTROL_OPS: [ControlOp; 6] = [
    ControlOp {
        name: "list",
        args: "none — every posted notification with id/tag/title/text/channel/actions/content sender",
    },
    ControlOp {
        name: "tap",
        args: "{id} or {tag} or {title}/{text} — fire the content PendingIntent (starts the target activity)",
    },
    ControlOp {
        name: "action",
        args: "{id, index} — fire action `index`'s PendingIntent",
    },
    ControlOp {
        name: "dismiss",
        args: "{id} or {tag} — remove the notification, like a user swipe",
    },
    ControlOp {
        name: "state",
        args: "none — channels, posted, dropped (POST_NOTIFICATIONS), taps and the gate",
    },
    ControlOp {
        name: OPEN_SHADE,
        args: "none — open the notification shade: start the com.android.systemui system app's ShadeActivity with one row per posted notification (starts a process lazily, like requestPermissions -> the dialog)",
    },
];

fn manifest_ref() -> &'static ModuleManifest {
    static MANIFEST: ModuleManifest = ModuleManifest {
        id: MODULE_ID,
        version: "1.0.0",
        summary: "notification channels, posting with the POST_NOTIFICATIONS gate, taps/actions fired through the real PendingIntent sender, dismiss (no status-bar pixels: the shade is the system-UI track)",
        services: SERVICES,
        replaces: &[SERVICE_NAME],
        process_initializers: &[],
        features: &[],
        providers: &[],
        broadcasts: &[],
        windows: &[],
        // SYSUI2: the shade is this module's system app (ADR-0029 amendment:
        // system UI comes from system apps in the guest).
        system_apps: &[SHADE_PACKAGE],
        requires: &[],
        requires_host_tools: &[],
        profiles: &[Profile::Real],
        budget: Budget {
            ram_mib: 1,
            startup_ms: 2,
        },
        attach: AttachMode::Hot,
        controls: &CONTROL_OPS,
        security: SecurityClass::InProcess,
    };
    &MANIFEST
}

/// One action of a posted notification.
#[derive(Debug, Clone, PartialEq)]
pub struct PostedAction {
    /// The action's index in `Notification.actions`.
    pub index: usize,
    /// `Notification.Action.title` (`TextUtils` written).
    pub title: String,
    /// The `PendingIntent` sender binder, when the action has one.
    pub sender: Option<u64>,
}

/// One posted notification.
#[derive(Debug, Clone, PartialEq)]
pub struct PostedNotification {
    /// The posting package.
    pub package: String,
    /// `NotificationManager.notify(id, …)`.
    pub id: i32,
    /// `notify(tag, id, …)`.
    pub tag: Option<String>,
    /// `Notification.getChannelId()`.
    pub channel: Option<String>,
    /// `EXTRA_TITLE` (`android.title`) from the extras bundle.
    pub title: Option<String>,
    /// `EXTRA_TEXT` (`android.text`).
    pub text: Option<String>,
    /// `Notification.flags`.
    pub flags: i32,
    /// The actions, in order.
    pub actions: Vec<PostedAction>,
    /// The content `PendingIntent` sender binder.
    pub content_sender: Option<u64>,
    /// `true` when the notification is a foreground-service notification and the
    /// service is still running.
    pub foreground_service: bool,
}

impl PostedNotification {
    /// A JSON view (the control ops and the module report).
    #[must_use]
    pub fn to_json(&self) -> Value {
        json!({
            "package": self.package,
            "id": self.id,
            "tag": self.tag,
            "channel": self.channel,
            "title": self.title,
            "text": self.text,
            "flags": self.flags,
            "foreground_service": self.foreground_service,
            "ongoing": self.flags & FLAG_ONGOING_EVENT != 0,
            "actions": self.actions.iter().map(|a| json!({
                "index": a.index,
                "title": a.title,
                "sender": a.sender,
            })).collect::<Vec<_>>(),
            "content_intent_sender": self.content_sender,
        })
    }

    fn matches(&self, query: &TapQuery) -> bool {
        if let Some(id) = query.id {
            if self.id != id {
                return false;
            }
        }
        if let Some(tag) = &query.tag {
            if self.tag.as_deref() != Some(tag.as_str()) {
                return false;
            }
        }
        if let Some(title) = &query.title {
            if self.title.as_deref() != Some(title.as_str()) {
                return false;
            }
        }
        if let Some(text) = &query.text {
            if self.text.as_deref() != Some(text.as_str()) {
                return false;
            }
        }
        query.id.is_some() || query.tag.is_some() || query.title.is_some() || query.text.is_some()
    }
}

/// How a control op names the notification it acts on.
#[derive(Debug, Default, Clone)]
pub struct TapQuery {
    /// `NotificationManager.notify(id, …)`.
    pub id: Option<i32>,
    /// `notify(tag, id, …)`.
    pub tag: Option<String>,
    /// The `android.title` extra.
    pub title: Option<String>,
    /// The `android.text` extra.
    pub text: Option<String>,
}

/// One channel the app created.
#[derive(Debug, Clone, PartialEq)]
pub struct ChannelRecord {
    /// Posting package.
    pub package: String,
    /// `NotificationChannel.getId()`.
    pub id: String,
    /// `getName()`.
    pub name: Option<String>,
    /// `getImportance()`.
    pub importance: i32,
}

/// One notification the `POST_NOTIFICATIONS` gate dropped.
#[derive(Debug, Clone, PartialEq)]
pub struct DroppedNotification {
    /// Posting package.
    pub package: String,
    /// The id it would have had.
    pub id: i32,
    /// Why it was dropped.
    pub reason: String,
}

/// One `tap`/`action` that fired a sender.
#[derive(Debug, Clone, PartialEq)]
pub struct FiredNotification {
    /// The notification id.
    pub id: i32,
    /// The action index, or `None` for a content tap.
    pub action: Option<usize>,
    /// The sender binder that was fired.
    pub sender: u64,
    /// `Ok(status)` from the delivery path, or the structured error.
    pub result: std::result::Result<i32, String>,
}

/// The notification state a session's module owns.
#[derive(Debug, Default)]
pub struct NotificationStore {
    /// `mChannels` per package.
    pub channels: Vec<ChannelRecord>,
    /// Posted notifications, newest last.
    pub posted: Vec<PostedNotification>,
    /// Notifications the `POST_NOTIFICATIONS` gate dropped.
    pub dropped: Vec<DroppedNotification>,
    /// Every sender a `tap`/`action` fired.
    pub fired: Vec<FiredNotification>,
    /// The permission table the gate reads (`None` = no table, which is the
    /// conformance kit's bare host).
    pub permissions: Option<Arc<Mutex<PermissionStore>>>,
    /// The package the store speaks for (the session's app).
    pub package: String,
    /// The target SDK the gate uses (default: the image's API level, 34).
    pub target_sdk: i32,
}

impl NotificationStore {
    /// A store for `package`.
    #[must_use]
    pub fn new(package: impl Into<String>) -> Self {
        Self {
            package: package.into(),
            target_sdk: 34,
            ..Self::default()
        }
    }

    /// `areNotificationsEnabled`: the `POST_NOTIFICATIONS` gate.
    ///
    /// AOSP (`NotificationManagerService.areNotificationsEnabledForPackageImpl`)
    /// exempts an app whose `targetSdk` is below 33 (the permission did not
    /// exist when it was written), and every app when no runtime table exists.
    #[must_use]
    pub fn notifications_enabled(&self) -> bool {
        let Some(store) = &self.permissions else {
            return true;
        };
        if self.target_sdk < 33 {
            return true;
        }
        let mut store = store
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        store.check(POST_NOTIFICATIONS) == PERMISSION_GRANTED
    }

    /// The gate plus the record. Returns whether it was posted.
    pub fn post(&mut self, notification: PostedNotification) -> bool {
        if !self.notifications_enabled() {
            self.dropped.push(DroppedNotification {
                package: notification.package.clone(),
                id: notification.id,
                reason: format!(
                    "{POST_NOTIFICATIONS} is not granted (targetSdk {}); \
                     NotificationManagerService drops the notification",
                    self.target_sdk
                ),
            });
            return false;
        }
        // `notify(tag, id, …)` replaces the previous notification with the same
        // (package, tag, id) — `NotificationManagerService.enqueueNotification`.
        self.posted.retain(|posted| {
            !(posted.package == notification.package
                && posted.id == notification.id
                && posted.tag == notification.tag)
        });
        self.posted.push(notification);
        true
    }

    /// Remove one notification (`cancelNotificationWithTag`).
    pub fn cancel(&mut self, package: &str, tag: Option<&str>, id: i32) -> bool {
        let before = self.posted.len();
        self.posted.retain(|posted| {
            !(posted.package == package
                && posted.id == id
                && (tag.is_none() || posted.tag.as_deref() == tag))
        });
        self.posted.len() != before
    }

    /// Remove every notification of `package`.
    pub fn cancel_all(&mut self, package: &str) -> usize {
        let before = self.posted.len();
        self.posted.retain(|posted| posted.package != package);
        before - self.posted.len()
    }

    /// The JSON the control op and the report use.
    #[must_use]
    pub fn to_json(&self) -> Value {
        json!({
            "package": self.package,
            "target_sdk": self.target_sdk,
            "notifications_enabled": self.notifications_enabled(),
            "channels": self.channels.iter().map(|c| json!({
                "package": c.package,
                "id": c.id,
                "name": c.name,
                "importance": c.importance,
            })).collect::<Vec<_>>(),
            "posted": self.posted.iter().map(PostedNotification::to_json).collect::<Vec<_>>(),
            "dropped": self.dropped.iter().map(|d| json!({
                "package": d.package,
                "id": d.id,
                "reason": d.reason,
            })).collect::<Vec<_>>(),
            "fired": self.fired.iter().map(|f| json!({
                "id": f.id,
                "action": f.action,
                "sender": f.sender,
                "result": match &f.result {
                    Ok(status) => json!({"start_result": status}),
                    Err(error) => json!({"error": error}),
                },
            })).collect::<Vec<_>>(),
        })
    }
}

/// Decoded pieces of an `android.app.Notification` payload.
///
/// Field indices are the MODNOTIF3 codec's (`parcelable.rs`,
/// `android.app.Notification`), which is byte-verified against the real
/// `writeToParcelImpl`.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct NotificationFacts {
    /// `Notification.flags`.
    pub flags: i32,
    /// `Notification.number`.
    pub number: i32,
    /// `mChannelId`.
    pub channel: Option<String>,
    /// `android.title` from the extras bundle.
    pub title: Option<String>,
    /// `android.text` from the extras bundle.
    pub text: Option<String>,
    /// The content `PendingIntent` sender binder.
    pub content_sender: Option<u64>,
    /// The actions.
    pub actions: Vec<PostedAction>,
}

/// `Cond` field -> the first nested value.
fn cond_first<'a>(payload: &'a Parcelable, index: usize) -> Option<&'a WireValue> {
    match payload.field(index) {
        Some(WireValue::Cond {
            present: true,
            values,
        }) => values.first(),
        _ => None,
    }
}

/// `Cond[Str8]` field -> the string.
fn cond_string(payload: &Parcelable, index: usize) -> Option<String> {
    cond_first(payload, index)
        .and_then(WireValue::as_str)
        .map(str::to_string)
}

/// A `PendingIntent` field -> the sender binder id it wraps.
fn cond_sender(payload: &Parcelable, index: usize) -> Option<u64> {
    cond_first(payload, index)
        .and_then(WireValue::as_parcelable)
        .and_then(|intent| intent.field(0))
        .and_then(WireValue::as_binder)
        .map(|binder| binder.0)
}

/// Decode the notification payload the framework serialised.
///
/// Returns an error for a payload that is not a notification; every field the
/// app did not set is `None`, never a guess.
pub fn decode_notification(payload: &Parcelable) -> Result<NotificationFacts> {
    if payload.class() != Some("android.app.Notification") {
        return Err(ServiceError::BadRequest {
            interface: DESCRIPTOR.into(),
            method: "enqueueNotificationWithTag".into(),
            detail: format!(
                "expected an android.app.Notification payload, got {}",
                payload.class().unwrap_or("<none>")
            ),
        });
    }
    let extras = match payload.field(27) {
        Some(WireValue::Bundle(Some(bundle))) => bundle.clone(),
        _ => crate::value::Bundle::new(),
    };
    let string_extra = |key: &str| {
        extras
            .get(key)
            .and_then(WireValue::as_str)
            .map(str::to_string)
    };
    let actions = match payload.field(28) {
        Some(WireValue::List(Some(items))) => items
            .iter()
            .enumerate()
            .filter_map(|(index, item)| {
                item.as_parcelable().map(|action| PostedAction {
                    index,
                    title: action
                        .field(1)
                        .and_then(WireValue::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    sender: cond_sender(&action, 2),
                })
            })
            .collect(),
        _ => Vec::new(),
    };
    Ok(NotificationFacts {
        flags: payload.field(13).and_then(WireValue::as_int).unwrap_or(0),
        number: payload.field(5).and_then(WireValue::as_int).unwrap_or(0),
        channel: cond_string(payload, 34),
        title: string_extra("android.title"),
        text: string_extra("android.text"),
        content_sender: cond_sender(payload, 6),
        actions,
    })
}

/// The module.
pub struct NotificationsModule {
    profile: Profile,
    store: Option<Arc<Mutex<NotificationStore>>>,
    fire: Option<crate::host::FireSender>,
    configured: Option<Value>,
}

impl NotificationsModule {
    /// A detached module.
    #[must_use]
    pub fn new() -> Self {
        Self {
            profile: Profile::Off,
            store: None,
            fire: None,
            configured: None,
        }
    }

    /// The manifest.
    #[must_use]
    pub fn manifest_ref() -> &'static ModuleManifest {
        manifest_ref()
    }
}

impl Default for NotificationsModule {
    fn default() -> Self {
        Self::new()
    }
}

/// The `INotificationManager` face, over the shared store.
struct NotificationsService {
    store: Arc<Mutex<NotificationStore>>,
}

impl NotificationsService {
    fn store(&self) -> std::sync::MutexGuard<'_, NotificationStore> {
        self.store
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// The channel payload's id/name/importance (`parcelable.rs`,
/// `android.app.NotificationChannel`: byte-conditional id, name).
fn channel_record(package: &str, payload: &Parcelable) -> Option<ChannelRecord> {
    let id = cond_string(payload, 0)?;
    Some(ChannelRecord {
        package: package.to_string(),
        id,
        name: cond_string(payload, 1),
        importance: payload.field(3).and_then(WireValue::as_int).unwrap_or(0),
    })
}

impl ServiceObject for NotificationsService {
    fn call(
        &mut self,
        interface: &str,
        method: &acos_aidl::MethodInfo,
        args: &[WireValue],
    ) -> Result<CallResult> {
        if interface != DESCRIPTOR {
            return Err(ServiceError::UnsupportedAndroidApi {
                interface: interface.to_string(),
                code: method.code,
                method: method.name.clone(),
            });
        }
        let string = |index: usize| -> String {
            args.get(index)
                .and_then(WireValue::as_str)
                .unwrap_or_default()
                .to_string()
        };
        let int =
            |index: usize| -> i32 { args.get(index).and_then(WireValue::as_int).unwrap_or(0) };
        let reply = match method.name.as_str() {
            // `enqueueNotificationWithTag(String pkg, String opPkg, String tag,
            //  int id, Notification notification, int userId)`
            "enqueueNotificationWithTag" => {
                let package = string(0);
                let tag = args.get(2).and_then(WireValue::as_str).map(str::to_string);
                let id = int(3);
                let payload = args
                    .get(4)
                    .and_then(WireValue::as_parcelable)
                    .ok_or_else(|| ServiceError::BadRequest {
                        interface: DESCRIPTOR.into(),
                        method: method.name.clone(),
                        detail: "arg 4 is the android.app.Notification payload".into(),
                    })?;
                let facts = decode_notification(payload)?;
                let mut store = self.store();
                // The gate is per package: a notification posted for a package
                // other than the session's app is judged by that app's state,
                // which ACOS does not model — refuse instead of guessing.
                if !store.package.is_empty() && package != store.package {
                    return Err(ServiceError::BadRequest {
                        interface: DESCRIPTOR.into(),
                        method: method.name.clone(),
                        detail: format!(
                            "the session posts for {:?}; {package:?} has no runtime state",
                            store.package
                        ),
                    });
                }
                store.post(PostedNotification {
                    package,
                    id,
                    tag,
                    channel: facts.channel,
                    title: facts.title,
                    text: facts.text,
                    flags: facts.flags,
                    actions: facts.actions,
                    content_sender: facts.content_sender,
                    foreground_service: facts.flags & FLAG_FOREGROUND_SERVICE != 0,
                });
                Vec::new()
            }
            // `cancelNotificationWithTag(String pkg, String opPkg, String tag,
            //  int id, int userId)`
            "cancelNotificationWithTag" => {
                let package = string(0);
                let tag = args.get(2).and_then(WireValue::as_str).map(str::to_string);
                let id = int(3);
                let mut store = self.store();
                let blocked = store
                    .posted
                    .iter()
                    .find(|posted| {
                        posted.package == package
                            && posted.id == id
                            && (tag.is_none() || posted.tag == tag)
                    })
                    .map(|posted| posted.foreground_service)
                    .unwrap_or(false);
                if blocked {
                    // `NotificationManagerService.cancelNotificationWithTag`
                    // keeps a foreground-service notification while its service
                    // runs; the app must stop the service.
                    store.dropped.push(DroppedNotification {
                        package,
                        id,
                        reason: "the notification belongs to a running foreground service".into(),
                    });
                } else {
                    store.cancel(&package, tag.as_deref(), id);
                }
                Vec::new()
            }
            // `cancelAllNotifications(String pkg, int userId)`
            "cancelAllNotifications" => {
                let package = string(0);
                let mut store = self.store();
                let keep: Vec<PostedNotification> = store
                    .posted
                    .iter()
                    .filter(|posted| posted.package == package && posted.foreground_service)
                    .cloned()
                    .collect();
                store.cancel_all(&package);
                for notification in keep {
                    store.posted.push(notification);
                }
                Vec::new()
            }
            "areNotificationsEnabled" | "areNotificationsEnabledForPackage" => {
                vec![WireValue::Bool(self.store().notifications_enabled())]
            }
            "getActiveNotifications" | "getActiveNotificationsWithAttribution" => {
                // `getActiveNotifications` returns a `StatusBarNotification[]`
                // (`tables.rs:6915`). ACOS has no status-bar service to build
                // one, so the reply is the empty array the framework can read;
                // the control op `list` is the API an agent uses.
                vec![WireValue::List(Some(Vec::new()))]
            }
            "getNotificationChannels" | "getNotificationChannelsForPackage" => {
                let package = string(0);
                let store = self.store();
                let items: Vec<WireValue> = store
                    .channels
                    .iter()
                    .filter(|channel| channel.package == package)
                    .map(|channel| {
                        // `NotificationChannel(Parcel)` reads `writeByte(0|1)` +
                        // `writeString` for the id and the name first; a channel
                        // ACOS synthesized carries those two and nothing else.
                        let values = vec![
                            WireValue::Cond {
                                present: true,
                                values: vec![WireValue::Str(Some(channel.id.clone()))],
                            },
                            match &channel.name {
                                Some(name) => WireValue::Cond {
                                    present: true,
                                    values: vec![WireValue::Str(Some(name.clone()))],
                                },
                                None => WireValue::Cond {
                                    present: false,
                                    values: Vec::new(),
                                },
                            },
                        ];
                        WireValue::Parcelable(Parcelable::ordered(
                            "android.app.NotificationChannel",
                            values,
                        ))
                    })
                    .collect();
                vec![WireValue::TypedObject(parceled_list_of(items))]
            }
            "createNotificationChannels"
            | "createNotificationChannelsForPackage"
            | "createNotificationChannelGroups" => {
                let package = string(0);
                let slice_index = if method.name == "createNotificationChannels" {
                    1
                } else {
                    2
                };
                let items: Vec<Parcelable> = args
                    .get(slice_index)
                    .and_then(WireValue::as_parcelable)
                    .and_then(crate::parcelable::parceled_list_items)
                    .map(|items| {
                        items
                            .iter()
                            .filter_map(WireValue::as_parcelable)
                            .cloned()
                            .collect()
                    })
                    .unwrap_or_default();
                let mut store = self.store();
                for payload in items {
                    let Some(record) = channel_record(&package, &payload) else {
                        continue;
                    };
                    store
                        .channels
                        .retain(|channel| channel.package != package || channel.id != record.id);
                    store.channels.push(record);
                }
                Vec::new()
            }
            "getPackageImportance" => {
                let enabled = self.store().notifications_enabled();
                // `IMPORTANCE_NONE` (0) when the app cannot post; the default
                // `IMPORTANCE_DEFAULT` (3) otherwise.
                vec![WireValue::Int(if enabled { 3 } else { 0 })]
            }
            "canShowBadge" => vec![WireValue::Bool(self.store().notifications_enabled())],
            "getZenMode" => vec![WireValue::Int(0)],
            "getHintsFromListenerNoToken" => vec![WireValue::Int(0)],
            "isNotificationListenerAccessGranted" => vec![WireValue::Bool(false)],
            "getEnabledNotificationListeners" => vec![WireValue::List(Some(Vec::new()))],
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

impl Module for NotificationsModule {
    fn manifest(&self) -> &'static ModuleManifest {
        manifest_ref()
    }

    fn attach(
        &mut self,
        host: &mut ServiceHost,
        profile: Profile,
        config: &ModuleConfig,
    ) -> Result<()> {
        if profile != Profile::Real {
            return Err(ServiceError::ModuleRejected {
                module: MODULE_ID,
                detail: format!(
                    "{profile} is not implemented: the notification records and the sender-backed tap are real; use `real`"
                ),
            });
        }
        let store = Arc::new(Mutex::new(NotificationStore::new(
            config.string("package").unwrap_or("").to_string(),
        )));
        {
            let mut store = store
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            // The gate reads the session's one permission table. A bare host
            // (the conformance kit) has none, and then the gate is open — the
            // same answer AOSP gives an app with no runtime-permission state.
            store.permissions = host.permission_store();
            if let Some(sdk) = config.get("target_sdk").and_then(Value::as_i64) {
                store.target_sdk =
                    i32::try_from(sdk).map_err(|_| ServiceError::ModuleRejected {
                        module: MODULE_ID,
                        detail: format!("target_sdk {sdk} is out of range"),
                    })?;
            }
        }
        self.fire = host.pending_intent_fire();
        // SYSUI2: the shade's row tap. The shade system app answers with the
        // tapped key; this module owns the records (which notification the key
        // names, and that the row leaves the list) while the sender table and
        // the activity start stay in the AMS, so the seam is one closure that
        // answers the senders to fire.
        if let Some(atms) = host.atms() {
            let sink_store = Arc::clone(&store);
            atms.lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .system_ui_result_sink
                .set(Arc::new(move |facts: &crate::ams::IntentFacts| {
                    shade_tap_senders(&sink_store, facts)
                }));
        }
        self.configured = Some(json!({
            "package": config.string("package"),
            "target_sdk": config.get("target_sdk"),
            "has_permission_store": host.permission_store().is_some(),
            "has_sender_delivery": self.fire.is_some(),
        }));
        // Registered through the factory path so the decoded argument list
        // reaches `call`; the descriptor is the one `core` registered, which is
        // what makes the takeover an overwrite `end_scope` restores.
        let service_store = Arc::clone(&store);
        host.register(SERVICE_NAME, DESCRIPTOR, move |_wiring| {
            Box::new(NotificationsService {
                store: Arc::clone(&service_store),
            }) as Box<dyn ServiceObject>
        });
        self.store = Some(store);
        self.profile = profile;
        Ok(())
    }

    fn detach(&mut self, host: &mut ServiceHost) -> Result<()> {
        // `end_scope` reinstates core's `INotificationManager` factory and
        // binder. The records are dropped with the module: a notification is
        // the module's state, and core's recorder answers again. The shade's
        // result seam goes with it (ADR-0029: no registration left behind).
        if let Some(atms) = host.atms() {
            atms.lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .system_ui_result_sink
                .clear();
        }
        self.store = None;
        self.fire = None;
        self.configured = None;
        self.profile = Profile::Off;
        Ok(())
    }

    fn control(&mut self, host: &mut ServiceHost, op: &str, args: &Value) -> Result<Value> {
        let Some(store) = self.store.as_ref() else {
            return Err(ServiceError::ModuleRejected {
                module: MODULE_ID,
                detail: format!("module is off, so {op:?} was not applied"),
            });
        };
        let query = TapQuery {
            id: args.get("id").and_then(Value::as_i64).map(|v| v as i32),
            tag: args.get("tag").and_then(Value::as_str).map(str::to_string),
            title: args
                .get("title")
                .and_then(Value::as_str)
                .map(str::to_string),
            text: args.get("text").and_then(Value::as_str).map(str::to_string),
        };
        match op {
            "list" | "state" => Ok(store
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .to_json()),
            "tap" | "action" => {
                let action_index = if op == "action" {
                    Some(
                        args.get("index")
                            .and_then(Value::as_i64)
                            .map(|v| v as usize)
                            .ok_or_else(|| ServiceError::ModuleRejected {
                                module: MODULE_ID,
                                detail: "action needs an integer `index`".into(),
                            })?,
                    )
                } else {
                    None
                };
                let target = {
                    let store = store
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    let matches: Vec<&PostedNotification> = store
                        .posted
                        .iter()
                        .filter(|posted| posted.matches(&query))
                        .collect();
                    match matches.len() {
                        0 => {
                            return Err(ServiceError::ModuleRejected {
                                module: MODULE_ID,
                                detail: format!(
                                    "no posted notification matches {op} {query:?}; `list` shows what is posted"
                                ),
                            })
                        }
                        1 => matches[0].clone(),
                        n => {
                            return Err(ServiceError::ModuleRejected {
                                module: MODULE_ID,
                                detail: format!(
                                    "{n} posted notifications match {query:?}; name one with `id`"
                                ),
                            })
                        }
                    }
                };
                let sender = match action_index {
                    None => target.content_sender,
                    Some(index) => target
                        .actions
                        .iter()
                        .find(|action| action.index == index)
                        .and_then(|action| action.sender),
                }
                .ok_or_else(|| ServiceError::ModuleRejected {
                    module: MODULE_ID,
                    detail: format!(
                        "notification {} has no {} PendingIntent",
                        target.id,
                        action_index
                            .map_or_else(|| "content".to_string(), |i| format!("action {i}"))
                    ),
                })?;
                let Some(fire) = self.fire.clone() else {
                    return Err(ServiceError::ModuleRejected {
                        module: MODULE_ID,
                        detail: "no sender delivery is published on this host (no AMS)".into(),
                    });
                };
                // A notification action/content intent carries no fill-in:
                // `NotificationManagerService` fires it with `send(code)`.
                let evidence = fire(sender, 0, None)?;
                let fired = FiredNotification {
                    id: target.id,
                    action: action_index,
                    sender,
                    result: evidence.outcome.clone(),
                };
                store
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .fired
                    .push(fired.clone());
                Ok(json!({
                    "notification": target.to_json(),
                    "fired": {
                        "id": fired.id,
                        "action": fired.action,
                        "sender": fired.sender,
                        "intent": {
                            "action": evidence.intent.action,
                            "data": evidence.intent.data,
                            "component": evidence.intent.component.as_ref()
                                .map(|(p, c)| format!("{p}/{c}")),
                        },
                        "result": match &fired.result {
                            Ok(status) => json!({"start_result": status}),
                            Err(error) => json!({"error": error}),
                        },
                    },
                }))
            }
            "dismiss" => {
                let mut store = store
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let target = store
                    .posted
                    .iter()
                    .find(|posted| posted.matches(&query))
                    .cloned()
                    .ok_or_else(|| ServiceError::ModuleRejected {
                        module: MODULE_ID,
                        detail: format!("no posted notification matches dismiss {query:?}"),
                    })?;
                if target.foreground_service {
                    return Err(ServiceError::ModuleRejected {
                        module: MODULE_ID,
                        detail: format!(
                            "notification {} belongs to a running foreground service; stop the service instead",
                            target.id
                        ),
                    });
                }
                store.cancel(&target.package, target.tag.as_deref(), target.id);
                Ok(json!({"dismissed": target.to_json(), "remaining": store.posted.len()}))
            }
            OPEN_SHADE => open_shade(host, store),
            other => Err(ServiceError::ModuleRejected {
                module: MODULE_ID,
                detail: format!(
                    "unknown control op {other:?}; this module implements {}",
                    CONTROL_OPS
                        .iter()
                        .map(|op| op.name)
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            }),
        }
    }

    fn report(&self) -> ModuleReport {
        let mut report = ModuleReport::of(manifest_ref(), self.profile);
        if self.profile.is_attached() {
            if let Some(store) = &self.store {
                let store = store
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                report.reason = Some(format!(
                    "real notification records: {} posted, {} dropped by the POST_NOTIFICATIONS gate, {} sender(s) fired; no status-bar pixels (the shade is the system-UI track)",
                    store.posted.len(),
                    store.dropped.len(),
                    store.fired.len(),
                ));
            }
        }
        report
    }
}

/// The shade row key of a posted notification: `"<id>:<tag>"` (`tag` may be
/// empty; a notification posted without one still gets a stable key).
fn shade_key(posted: &PostedNotification) -> String {
    format!("{}:{}", posted.id, posted.tag.clone().unwrap_or_default())
}

/// Parse a shade row key back into `(id, tag)`.
fn shade_key_parts(key: &str) -> Option<(i32, Option<String>)> {
    let (id, tag) = key.split_once(':')?;
    let id = id.trim().parse::<i32>().ok()?;
    Some((id, if tag.is_empty() { None } else { Some(tag.to_string()) }))
}

/// SYSUI2: open the shade — start the `com.android.systemui` system app's
/// `ShadeActivity` with one row per posted notification.
///
/// This is the system's own start (`AmsState::start_activity`, the same call a
/// guest `startActivity` makes): the launcher spawns the system app's process
/// lazily on the queued need and the host delivers the launch, exactly like the
/// permission dialog. No row is invented: the extras are the posted records'
/// own id/tag, `android.title`, `android.text` and action titles.
fn open_shade(host: &mut ServiceHost, store: &Arc<Mutex<NotificationStore>>) -> Result<Value> {
    let Some(atms) = host.atms() else {
        return Err(ServiceError::ModuleRejected {
            module: MODULE_ID,
            detail: "no activity manager is published on this host, so the shade system app \
                     cannot be started"
                .into(),
        });
    };
    let (keys, titles, texts, actions) = {
        let store = store
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut keys = Vec::with_capacity(store.posted.len());
        let mut titles = Vec::with_capacity(store.posted.len());
        let mut texts = Vec::with_capacity(store.posted.len());
        let mut actions = Vec::with_capacity(store.posted.len());
        for posted in &store.posted {
            keys.push(shade_key(posted));
            titles.push(posted.title.clone().unwrap_or_default());
            texts.push(posted.text.clone().unwrap_or_default());
            actions.push(
                posted
                    .actions
                    .iter()
                    .map(|action| action.title.clone())
                    .collect::<Vec<_>>()
                    .join(", "),
            );
        }
        (keys, titles, texts, actions)
    };
    let rows = keys.len();
    let mut intent = crate::ams::IntentFacts::default();
    intent.component = Some((SHADE_PACKAGE.to_string(), SHADE_ACTIVITY.to_string()));
    intent.package = Some(SHADE_PACKAGE.to_string());
    intent.flags = FLAG_ACTIVITY_NEW_TASK;
    intent
        .extras
        .insert(SHADE_EXTRA_KEYS.to_string(), crate::ams::Extra::Strings(keys.clone()));
    intent.extras.insert(
        SHADE_EXTRA_TITLES.to_string(),
        crate::ams::Extra::Strings(titles),
    );
    intent.extras.insert(
        SHADE_EXTRA_TEXTS.to_string(),
        crate::ams::Extra::Strings(texts),
    );
    intent.extras.insert(
        SHADE_EXTRA_ACTIONS.to_string(),
        crate::ams::Extra::Strings(actions),
    );
    let start_result = atms
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .ams
        .start_activity(&intent, FLAG_ACTIVITY_NEW_TASK, 0, -1)?;
    Ok(json!({
        "shade": {
            "component": SHADE_COMPONENT,
            "rows": rows,
            "keys": keys,
            "start_result": start_result,
        }
    }))
}

/// `Intent.FLAG_ACTIVITY_NEW_TASK`: the shade is a top-level window.
const FLAG_ACTIVITY_NEW_TASK: i32 = 0x1000_0000;

/// SYSUI2: the shade's row tap, as the AMS sees it.
///
/// The shade answers `acos.shade.tapped`; this finds the record the key names,
/// takes the row out of the list (`cancelNotificationWithTag`, except for a
/// foreground-service notification, which keeps its row like a device does) and
/// answers the content `PendingIntent` sender the AMS must fire. The started
/// activity then follows the normal launch/spawn route.
fn shade_tap_senders(
    store: &Arc<Mutex<NotificationStore>>,
    facts: &crate::ams::IntentFacts,
) -> Vec<u64> {
    let Some(crate::ams::Extra::String(key)) = facts.extras.get(SHADE_EXTRA_TAPPED) else {
        return Vec::new();
    };
    let Some((id, tag)) = shade_key_parts(key) else {
        return Vec::new();
    };
    let mut store = store
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(posted) = store
        .posted
        .iter()
        .find(|posted| posted.id == id && posted.tag.as_deref() == tag.as_deref())
        .cloned()
    else {
        return Vec::new();
    };
    if !posted.foreground_service {
        store.cancel(&posted.package, posted.tag.as_deref(), posted.id);
    }
    posted.content_sender.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_manifest_is_valid_and_declares_the_takeover() {
        manifest_ref().validate().expect("valid manifest");
        assert_eq!(manifest_ref().replaces, &["notification"]);
    }
}

/// The host APK a module's declared **system app** installs
/// (`crate::module::system_apks`). The shade app is SYSUI1's system app.
#[must_use]
pub fn system_app_apk(module: &str, app: &str, config: &ModuleConfig) -> Option<std::path::PathBuf> {
    if module != MODULE_ID || app != SHADE_PACKAGE {
        return None;
    }
    if let Some(path) = config.string(SHADE_APK_CONFIG).filter(|path| !path.is_empty()) {
        return Some(std::path::PathBuf::from(path));
    }
    if let Some(path) = std::env::var_os(SHADE_APK_ENV) {
        if !path.is_empty() {
            return Some(std::path::PathBuf::from(path));
        }
    }
    Some(crate::workspace_path(SHADE_APK_REL).unwrap_or_else(|| std::path::PathBuf::from(SHADE_APK_REL)))
}
