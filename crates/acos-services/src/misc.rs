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

/// One accessibility window (MODA11Y3 step 1).
///
/// The facts come from the LAYER1 window stack (`crate::wms::WmsState`), which
/// is the one description of the stack composition, the UI tree and input read;
/// the accessibility window id is this module's own (assigned when the app
/// registers its interaction connection or when the window is first listed),
/// because that is the id the app's `ViewRootImpl` is handed back by
/// `addAccessibilityInteractionConnection` and the id the service passes to
/// `findAccessibilityNodeInfoByAccessibilityId`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct A11yWindow {
    /// `AccessibilityWindowInfo.getId()`.
    pub id: i32,
    /// The `IWindow` binder id the window was added with.
    pub window: u64,
    /// The registered `IAccessibilityInteractionConnection`, if any.
    pub connection: Option<u64>,
    /// The package that owns the window (`addAccessibilityInteractionConnection`).
    pub package: String,
    /// `AccessibilityWindowInfo` type (`TYPE_APPLICATION` = 1, ...).
    pub type_: i32,
    /// Layer: larger is higher (`getWindows` order is topmost first).
    pub layer: i32,
    /// Window frame in display coordinates.
    pub frame: (i32, i32, i32, i32),
    /// The window is the focused one.
    pub focused: bool,
    /// The window is the active one (AOSP: the focused one while no touch
    /// interaction is in progress).
    pub active: bool,
}

/// One interaction request relayed to an app's
/// `IAccessibilityInteractionConnection` (MODA11Y3 step 1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct A11yInteraction {
    /// `IAccessibilityServiceConnection` transaction code (3 = by id, 4 = by
    /// text, 5 = by view id, 8 = perform action).
    pub code: u32,
    /// The accessibility window id the request named (resolved).
    pub window: i32,
    /// The `interactionId` the app must echo in the callback.
    pub interaction_id: i32,
    /// The `IWindow` binder whose connection the request went to.
    pub window_binder: u64,
}

/// The bounded interaction log (an always-on service polls).
const MAX_INTERACTIONS: usize = 128;

/// One system-initiated bind waiting for the app's `publishService` reply.
#[derive(Debug, Clone, PartialEq)]
pub struct A11yPendingBind {
    /// `package/.Class` as enabled in the secure settings.
    pub component: String,
    /// Package of the enabled service.
    pub package: String,
    /// Fully-qualified service class.
    pub class: String,
    /// The bind token that must come back in `publishService`.
    pub token: u64,
    /// `bindSeq` reported to the app (AOSP's monotonic per-bind sequence).
    pub seq: i64,
    /// The `ServiceInfo` parcel the bind was created with.
    pub info: crate::value::Parcelable,
}

/// One bound `AccessibilityService` (`onServiceConnected` ran).
#[derive(Debug, Clone, PartialEq)]
pub struct A11yBoundService {
    /// `package/.Class`.
    pub component: String,
    /// Package of the bound service.
    pub package: String,
    /// Fully-qualified class.
    pub class: String,
    /// The app-side `IAccessibilityServiceClient` binder (`onBind`'s reply).
    pub binder: u64,
    /// `IAccessibilityServiceConnection` connection id handed to the service.
    pub connection_id: i32,
    /// The system-side connection node the service received.
    pub connection: u64,
    /// The window token passed to `init`.
    pub window_token: u64,
    /// The info the service last set through `setServiceInfo`, if any.
    pub service_info: Option<crate::value::Value>,
    /// The `AccessibilityServiceInfo` the bind was made with.
    pub bind_info: crate::value::Parcelable,
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
    /// The package manager, so the installed service list is read from the
    /// **installed** APK (after attach) and not from the launch config.
    pub pms: Option<Arc<Mutex<crate::pms::PmsState>>>,
    /// The app process's `IApplicationThread`, captured from
    /// `attachApplication`; the bind has no target before that.
    pub app_thread: Option<u64>,
    /// The bind that is in flight (deliveries queued, `publishService` not yet
    /// received).
    pub pending_bind: Option<A11yPendingBind>,
    /// The bound service (`onServiceConnected` ran on the app side).
    pub bound_service: Option<A11yBoundService>,
    /// `bindSeq` counter for system-initiated binds.
    pub next_bind_seq: i64,
    /// `disableSelf()` was called by the service.
    pub self_disabled: bool,
    /// The LAYER1 window-manager state `getWindows` reads. `None` in a bare
    /// host (unit tests construct that shape); the module then reports no
    /// windows instead of inventing them.
    pub windows: Option<Arc<Mutex<crate::wms::WmsState>>>,
    /// The session's input channels.
    ///
    /// `performGlobalAction(GLOBAL_ACTION_BACK)` is routed through the same
    /// path a scenario `back`/`acos key back` takes (a real `KEYCODE_BACK` on
    /// the window's `InputChannel`), so the app's own `Window` decides — a
    /// dialog dismisses first, then the activity finishes. `None` in a bare
    /// host: the action is then a structured refusal, never a fake `true`.
    pub input: Option<crate::window_session::input_dispatch::InputHost>,
    /// accessibility window id assigned to each `IWindow` binder.
    pub window_ids: Vec<(u64, i32)>,
    /// Next accessibility window id (1-based, never reused).
    pub next_window_id: i32,
    /// Interaction requests relayed to an app connection (bounded).
    pub interactions: Vec<A11yInteraction>,
    /// The bound service's `<accessibility-service>` meta-data
    /// (`AccessibilityServiceInfo.parseServiceInfo`): the capabilities and
    /// flags `AccessibilitySecurityPolicy` gates the interaction surface on.
    /// `None` while nothing is bound, and for a service that declares no
    /// meta-data — which is the AOSP answer an unprivileged service gets.
    pub service_config: Option<crate::a11y_config::A11yServiceConfig>,
    /// Window-stack changes the bound service has not been told about yet.
    ///
    /// AOSP's `AccessibilityWindowManager` emits one
    /// `AccessibilityEvent.TYPE_WINDOWS_CHANGED` per added/removed/changed
    /// window, and that event is what clears a service's
    /// `AccessibilityCache` (`AccessibilityCache.onAccessibilityEvent`).
    /// Without it a bound service that already asked `getWindows()` answers
    /// every later query from the cache and never sees a window added after
    /// its first query. Bounded like `events`.
    pub windows_changed: Vec<A11yWindowChange>,
    /// The compositor-client (C1) state: the newest composited frame and the
    /// `FLAG_SECURE` answer `takeScreenshot` needs.
    ///
    /// AOSP's `takeScreenshot` captures the display
    /// (`DisplayManagerInternal.userScreenshot`) and fails with
    /// `ERROR_TAKE_SCREENSHOT_SECURE_WINDOW` when a secure window is showing.
    /// ACOS's composited frame is C1's, so the module reads exactly that.
    /// `None` in a bare host (unit tests): a screenshot then answers
    /// `ERROR_TAKE_SCREENSHOT_INVALID_DISPLAY`, never pixels it does not have.
    pub compositor: Option<Arc<Mutex<crate::surface_client::SurfaceState>>>,
    /// `mRequestTakeScreenshotTimestampMs`: the last `takeScreenshot` request,
    /// for `ACCESSIBILITY_TAKE_SCREENSHOT_REQUEST_INTERVAL_TIMES_MS`.
    pub last_screenshot_ms: Option<i64>,
    /// Gesture sequences the module published, and their outcome (bounded).
    pub gestures: Vec<A11yGestureRecord>,
    /// Screenshot requests and their status (bounded).
    pub screenshots: Vec<A11yScreenshotRecord>,
    /// Guest callbacks this service holds strong references on: the
    /// `IAccessibilityManagerClient` of every connected app, the
    /// `IAccessibilityInteractionConnection` of every window, and the bound
    /// accessibility service's `IAccessibilityServiceClient` (`crate::refs`).
    /// The host's reference is what keeps a client's node rooted between
    /// `addClient` and `removeClient`; a detach releases whatever is left.
    pub refs: crate::refs::GuestRefs,
}

/// One `dispatchGesture` request, as the module served it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct A11yGestureRecord {
    /// `AccessibilityService.mSequence`.
    pub sequence: i32,
    /// How many `MotionEvent`s the `ParceledListSlice` carried.
    pub events: usize,
    /// `true` when every event was published on the session input channel.
    pub delivered: bool,
    /// Why delivery failed (only when `delivered` is false).
    pub reason: Option<String>,
}

/// One `takeScreenshot` request, as the module served it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct A11yScreenshotRecord {
    /// The requested display id.
    pub display_id: i32,
    /// `AccessibilityService.TAKE_SCREENSHOT_SUCCESS` or one `ERROR_*` code
    /// (`-1` for the capability refusal, which AOSP throws instead of sending
    /// a status).
    pub status: i32,
}

/// One pending `TYPE_WINDOWS_CHANGED` event, as the window manager produced it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct A11yWindowChange {
    /// The changed window's `IWindow` binder id (the accessibility window id
    /// is assigned when the event is built).
    pub window: u64,
    /// `AccessibilityEvent.WINDOWS_CHANGE_*` mask.
    pub changes: i32,
}

/// Keep the event log bounded; an always-on UI sends thousands of events.
const MAX_EVENTS: usize = 256;

impl A11yState {
    /// New disabled state with no settings table.
    #[must_use]
    pub fn new() -> Self {
        Self {
            clients: Vec::new(),
            refs: crate::refs::GuestRefs::new(),
            next_client_id: 1,
            settings: None,
            enabled_services: Vec::new(),
            enabled: false,
            events: Vec::new(),
            connections: Vec::new(),
            unsupported: Vec::new(),
            pms: None,
            app_thread: None,
            pending_bind: None,
            bound_service: None,
            next_bind_seq: 0,
            self_disabled: false,
            windows: None,
            input: None,
            window_ids: Vec::new(),
            next_window_id: 1,
            interactions: Vec::new(),
            service_config: None,
            windows_changed: Vec::new(),
            compositor: None,
            last_screenshot_ms: None,
            gestures: Vec::new(),
            screenshots: Vec::new(),
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
        // A bound service keeps no overlay authority once the module is turned
        // off: AOSP removes the accessibility window token with the service.
        if let Some(token) = self.bound_service.as_ref().map(|bound| bound.window_token) {
            self.release_overlay_token(token);
        }
        if let Some(bound) = self.bound_service.take() {
            self.refs.release(acos_parcel::BinderId(bound.binder));
        }
    }

    /// `IWindowManager.addWindowToken(token, TYPE_ACCESSIBILITY_OVERLAY)`:
    /// record the overlay token the a11y manager created for a bound service,
    /// so the WMS accepts only that service's `TYPE_ACCESSIBILITY_OVERLAY`
    /// windows (`WindowManagerService.java:1630-1640, 9914-9928`).
    pub fn register_overlay_token(&mut self, token: u64) {
        if let Some(windows) = &self.windows {
            windows
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .wm
                .add_overlay_token(acos_parcel::binder::BinderId(token));
        }
    }

    /// `IWindowManager.removeWindowToken`: the service is gone.
    pub fn release_overlay_token(&mut self, token: u64) {
        if let Some(windows) = &self.windows {
            windows
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .wm
                .remove_overlay_token(acos_parcel::binder::BinderId(token));
        }
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

    /// `AccessibilitySecurityPolicy.canRetrieveWindowContentLocked`: may the
    /// bound service read window content at all?
    #[must_use]
    pub fn can_retrieve_window_content(&self) -> bool {
        self.service_config
            .is_some_and(|config| config.can_retrieve_window_content())
    }

    /// `AccessibilitySecurityPolicy.canRetrieveWindowsLocked`: the window
    /// **list** (`getWindows`/`getWindow`) needs the content capability *and*
    /// `FLAG_RETRIEVE_INTERACTIVE_WINDOWS`.
    #[must_use]
    pub fn can_retrieve_windows(&self) -> bool {
        self.service_config
            .is_some_and(|config| config.can_retrieve_windows())
    }

    /// `AccessibilitySecurityPolicy.canTakeScreenshotLocked`: the service
    /// declared `canTakeScreenshot`.
    #[must_use]
    pub fn can_take_screenshot(&self) -> bool {
        self.service_config
            .is_some_and(|config| config.can_take_screenshot())
    }

    /// `AccessibilitySecurityPolicy.canPerformGestures`: the service declared
    /// `canPerformGestures`.
    #[must_use]
    pub fn can_perform_gestures(&self) -> bool {
        self.service_config
            .is_some_and(|config| config.can_perform_gestures())
    }

    /// `SystemClock.uptimeMillis()`-shaped now, for the screenshot rate limit.
    #[must_use]
    pub fn uptime_ms() -> i64 {
        crate::input_channel::monotonic_nanos() / 1_000_000
    }

    /// The token a gesture is published on.
    ///
    /// AOSP's `MotionEventInjector` injects into the display, and the display
    /// dispatch picks the window under the touch point. ACOS publishes per
    /// window channel, and the window a touch belongs to is the one holding
    /// input focus (an accessibility overlay is `FLAG_NOT_FOCUSABLE`), so that
    /// is the target — the same choice [`Self::back_token`] makes for keys.
    pub fn gesture_token(&self) -> Result<acos_parcel::BinderId> {
        let Some(input) = self.input.as_ref() else {
            return Err(ServiceError::ModuleRejected {
                module: "accessibility",
                detail: "dispatchGesture needs the session's input channels; this host has \
                         none (a bare host or the rust VM)"
                    .to_string(),
            });
        };
        input
            .focused_token()
            .ok_or_else(|| ServiceError::ModuleRejected {
                module: "accessibility",
                detail: "dispatchGesture: no focused window input channel yet".to_string(),
            })
    }

    /// Publish a decoded gesture on the session's input channel.
    ///
    /// The events are the guest's own samples (the framework's
    /// `GestureDescription$MotionEventGenerator` produced them), so their
    /// `eventTime`s are published unchanged and the stroke keeps its timing.
    pub fn publish_gesture(
        &self,
        token: acos_parcel::BinderId,
        events: &[crate::input_channel::MotionEvent],
    ) -> std::result::Result<(), String> {
        let Some(input) = self.input.as_ref() else {
            return Err("no session input channels".to_string());
        };
        let mut dispatch = input
            .state()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        dispatch
            .inject_motion_series(token, events)
            .map(|_gesture| ())
            .map_err(|error| error.to_string())
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

    /// Queue one `TYPE_WINDOWS_CHANGED` event for the bound service.
    ///
    /// Called by the window session when a window leaves the stack; AOSP's
    /// `AccessibilityWindowManager` queues exactly one event per change and
    /// `AbstractAccessibilityServiceConnection.notifyAccessibilityEvent`
    /// delivers it even to a service whose `eventTypes` do not include it,
    /// because `TYPE_WINDOWS_CHANGED` is cache-critical
    /// (`AccessibilityCache.CACHE_CRITICAL_EVENTS_MASK`).
    pub fn record_window_change(&mut self, window: u64, changes: i32) {
        if self.windows_changed.len() >= MAX_EVENTS {
            self.windows_changed.remove(0);
        }
        self.windows_changed
            .push(A11yWindowChange { window, changes });
    }

    /// Take the queued window changes (the caller builds the events).
    pub fn take_window_changes(&mut self) -> Vec<A11yWindowChange> {
        std::mem::take(&mut self.windows_changed)
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
            "installed_services": self.installed_services(),
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
            "service_bound": self.bound_service.is_some(),
            "windows": self.window_list_readonly().into_iter().map(|w| serde_json::json!({
                "id": w.id,
                "window": format!("{:#x}", w.window),
                "type": w.type_,
                "layer": w.layer,
                "bounds": [w.frame.0, w.frame.1, w.frame.2, w.frame.3],
                "focused": w.focused,
                "active": w.active,
                "package": w.package,
                "connection": w.connection.map(|c| format!("{c:#x}")),
            })).collect::<Vec<_>>(),
            "interactions": self.interactions.iter().map(|i| serde_json::json!({
                "code": i.code,
                "window": i.window,
                "interaction_id": i.interaction_id,
            })).collect::<Vec<_>>(),
            "bound_service": self.bound_service.as_ref().map(|b| serde_json::json!({
                "component": b.component,
                "binder": format!("{:#x}", b.binder),
                "connection_id": b.connection_id,
                "connection": format!("{:#x}", b.connection),
                "service_info_set": b.service_info.is_some(),
            })),
            "bind_pending": self.pending_bind.as_ref().map(|p| serde_json::json!({
                "component": p.component,
                "token": format!("{:#x}", p.token),
                "seq": p.seq,
            })),
            "app_thread": self.app_thread.map(|t| format!("{t:#x}")),
            "self_disabled": self.self_disabled,
            // MODA11Y6: what the bound service declared in its
            // `<accessibility-service>` block, and what that allows it to do
            // (`AccessibilitySecurityPolicy`).
            "service_config": self.service_config.as_ref().map(|config| serde_json::json!({
                "event_types": config.event_types,
                "feedback_type": config.feedback_type,
                "notification_timeout": config.notification_timeout,
                "flags": config.flags,
                "capabilities": config.capabilities,
                "can_retrieve_window_content": config.can_retrieve_window_content(),
                "can_retrieve_windows": config.can_retrieve_windows(),
                "can_perform_gestures": config.can_perform_gestures(),
                "can_take_screenshot": config.can_take_screenshot(),
            })),
            // MODA11YNATIVE: the two Android 12+ service APIs this module
            // serves, with the outcome of every call.
            "gestures": self.gestures.iter().map(|g| serde_json::json!({
                "sequence": g.sequence,
                "events": g.events,
                "delivered": g.delivered,
                "reason": g.reason,
            })).collect::<Vec<_>>(),
            "screenshots": self.screenshots.iter().map(|s| serde_json::json!({
                "display_id": s.display_id,
                "status": s.status,
            })).collect::<Vec<_>>(),
            "unsupported": self.unsupported,
        })
    }

    /// The accessibility window id for an `IWindow` binder, assigned on first
    /// use and kept for the life of the session (AOSP's `AccessibilityWindowId`
    /// is per window, not per registration).
    pub fn window_id_for(&mut self, window: u64) -> i32 {
        if let Some((_, id)) = self.window_ids.iter().find(|(w, _)| *w == window) {
            return *id;
        }
        let id = self.next_window_id;
        self.next_window_id = self.next_window_id.saturating_add(1);
        self.window_ids.push((window, id));
        id
    }

    /// Forget the id of a window that was removed.
    pub fn forget_window(&mut self, window: u64) {
        self.window_ids.retain(|(w, _)| *w != window);
    }

    /// The LAYER1 window stack, topmost first, with the accessibility facts
    /// AOSP's `AccessibilityManagerService.getWindows` reports.
    ///
    /// The order and the layer are AOSP's: `getWindows` is ordered from the
    /// topmost window down, and `getLayer()` is `count - 1 - index` (so the
    /// topmost window has the largest layer). Everything else is read from the
    /// window stack: the type, the frame and which window is focused.
    pub fn window_snapshot(&mut self) -> Vec<A11yWindow> {
        let Some(windows) = &self.windows else {
            return Vec::new();
        };
        let (entries, focused) = {
            let wms = windows
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let entries: Vec<(u64, i32, (i32, i32, i32, i32))> = wms
                .wm
                .layer_stack()
                .ordered()
                .into_iter()
                .map(|entry| {
                    (
                        entry.window,
                        entry.type_,
                        (
                            entry.frame.left,
                            entry.frame.top,
                            entry.frame.right,
                            entry.frame.bottom,
                        ),
                    )
                })
                .collect();
            (entries, wms.wm.focus)
        };
        let count = i32::try_from(entries.len()).unwrap_or(i32::MAX);
        let mut out = Vec::with_capacity(entries.len());
        // `ordered()` is bottom-first; AOSP reports topmost first.
        for (index, (window, type_, frame)) in entries.into_iter().rev().enumerate() {
            let id = self.window_id_for(window);
            let record = self.connections.iter().find(|c| c.window == window);
            let is_focused = focused == Some(window);
            out.push(A11yWindow {
                id,
                window,
                connection: record.map(|c| c.connection),
                package: record.map(|c| c.package.clone()).unwrap_or_default(),
                type_: a11y_window_type(type_),
                layer: count - 1 - i32::try_from(index).unwrap_or(0),
                frame,
                focused: is_focused,
                active: is_focused,
            });
        }
        out
    }

    /// The window token a `performGlobalAction(GLOBAL_ACTION_BACK)` goes to.
    ///
    /// Same rule as the session's own Back (`InputHost::key`, the CLI's
    /// `acos key back`): the most recently registered live window. For a dialog
    /// that is the dialog window, which is what makes AOSP's "dialog first,
    /// then the activity" fall out of the app's own key handling instead of a
    /// host-side guess.
    pub fn back_token(&self) -> Result<acos_parcel::BinderId> {
        let Some(input) = self.input.as_ref() else {
            return Err(ServiceError::ModuleRejected {
                module: "accessibility",
                detail: "performGlobalAction(BACK) needs the session's input channels; this host \
                         has none (a bare host or the rust VM)"
                    .to_string(),
            });
        };
        // Keys go to the focused window (`InputDispatcher::dispatchKeyLocked`
        // → `getFocusedWindowHandle`), never to "the newest window": a
        // `FLAG_NOT_FOCUSABLE` accessibility overlay added after the activity
        // would otherwise swallow Back.
        input
            .focused_token()
            .ok_or_else(|| ServiceError::ModuleRejected {
                module: "accessibility",
                detail: "performGlobalAction(BACK): no focused window input channel yet"
                    .to_string(),
            })
    }

    /// Publish one real `KEYCODE_BACK` on that window's `InputChannel`.
    ///
    /// This is the session's Back path: the guest's `InputEventReceiver` reads
    /// the key and the app's own `Window`/`ViewRootImpl` handles it (a dialog
    /// dismisses, the activity finishes). The module reports only that the
    /// action was dispatched; what the app does with it is the app's business.
    pub fn publish_back(&self, token: acos_parcel::BinderId) -> Result<()> {
        let Some(input) = self.input.as_ref() else {
            return Ok(());
        };
        let mut dispatch = input
            .state()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        dispatch
            .inject_key(token, crate::input_channel::KEYCODE_BACK)
            .map(|_gesture| ())
            .map_err(|error| ServiceError::ModuleRejected {
                module: "accessibility",
                detail: format!("performGlobalAction(BACK) could not publish the key: {error}"),
            })
    }

    /// The same list as [`Self::window_snapshot`] without assigning new ids
    /// (the report is built from `&self`); a window that was never registered
    /// or listed keeps id `0`.
    #[must_use]
    pub fn window_list_readonly(&self) -> Vec<A11yWindow> {
        let Some(windows) = &self.windows else {
            return Vec::new();
        };
        let (entries, focused) = {
            let wms = windows
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let entries: Vec<(u64, i32, (i32, i32, i32, i32))> = wms
                .wm
                .layer_stack()
                .ordered()
                .into_iter()
                .map(|entry| {
                    (
                        entry.window,
                        entry.type_,
                        (
                            entry.frame.left,
                            entry.frame.top,
                            entry.frame.right,
                            entry.frame.bottom,
                        ),
                    )
                })
                .collect();
            (entries, wms.wm.focus)
        };
        let count = i32::try_from(entries.len()).unwrap_or(i32::MAX);
        let mut out = Vec::with_capacity(entries.len());
        for (index, (window, type_, frame)) in entries.into_iter().rev().enumerate() {
            let id = self
                .window_ids
                .iter()
                .find(|(w, _)| *w == window)
                .map(|(_, id)| *id)
                .unwrap_or(0);
            let record = self.connections.iter().find(|c| c.window == window);
            let is_focused = focused == Some(window);
            out.push(A11yWindow {
                id,
                window,
                connection: record.map(|c| c.connection),
                package: record.map(|c| c.package.clone()).unwrap_or_default(),
                type_: a11y_window_type(type_),
                layer: count - 1 - i32::try_from(index).unwrap_or(0),
                frame,
                focused: is_focused,
                active: is_focused,
            });
        }
        out
    }

    /// The window a service means by `accessibilityWindowId`:
    /// `ACTIVE_WINDOW_ID` (and `ANY_WINDOW_ID`) resolve to the active one.
    #[must_use]
    pub fn resolve_window(&self, windows: &[A11yWindow], requested: i32) -> Option<usize> {
        const ACTIVE_WINDOW_ID: i32 = 0x7fff_ffff;
        const ANY_WINDOW_ID: i32 = -2;
        match requested {
            ACTIVE_WINDOW_ID | ANY_WINDOW_ID => windows.iter().position(|w| w.active),
            -1 => None,
            id => windows.iter().position(|w| w.id == id),
        }
    }

    /// The accessibility window info for one id.
    #[must_use]
    pub fn window_by_id(&self, windows: &[A11yWindow], id: i32) -> Option<A11yWindow> {
        windows.iter().find(|w| w.id == id).cloned()
    }

    /// Record one relayed interaction (bounded).
    pub fn record_interaction(&mut self, interaction: A11yInteraction) {
        if self.interactions.len() >= MAX_INTERACTIONS {
            self.interactions.remove(0);
        }
        self.interactions.push(interaction);
    }

    /// The accessibility services the **installed package** declares.
    ///
    /// AOSP's `getInstalledAccessibilityServiceList` reads
    /// `PackageManager.queryIntentServices(Intent(SERVICE_INTERFACE))` and keeps
    /// the services guarded by `BIND_ACCESSIBILITY_SERVICE`; the module attach
    /// happens before the APK is installed, so the list must be read lazily here
    /// (ADR-0029 amendment) instead of being copied from the launch config.
    #[must_use]
    pub fn installed_services(&self) -> Vec<String> {
        let Some(pms) = &self.pms else {
            return Vec::new();
        };
        let pms = pms
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        pms.packages
            .iter()
            .flat_map(|installed| installed.services.iter())
            .filter(|service| service.permission == "android.permission.BIND_ACCESSIBILITY_SERVICE")
            .map(|service| {
                let package = service.package_name.clone();
                let class = service
                    .name
                    .strip_prefix(&format!("{package}."))
                    .map(|short| format!(".{short}"))
                    .unwrap_or_else(|| service.name.clone());
                format!("{package}/{class}")
            })
            .collect()
    }

    /// Record the app process's `IApplicationThread` (`attachApplication`).
    pub fn set_app_thread(&mut self, binder: u64) {
        self.app_thread = Some(binder);
    }

    /// Record the `AccessibilityServiceInfo` the service set on connect.
    pub fn set_service_info(&mut self, value: Option<crate::value::Value>) {
        if let Some(bound) = self.bound_service.as_mut() {
            bound.service_info = value;
        } else {
            self.unsupported
                .push("setServiceInfo before the service was bound".into());
        }
    }

    /// The info `getServiceInfo` answers: what the service set, else the info
    /// the bind was made with.
    #[must_use]
    pub fn bound_info(&self) -> Option<crate::value::Value> {
        let bound = self.bound_service.as_ref()?;
        Some(
            bound
                .service_info
                .clone()
                .unwrap_or(crate::value::Value::Parcelable(bound.bind_info.clone())),
        )
    }

    /// Record a completed `publishService` handshake.
    pub fn bind_service(
        &mut self,
        pending: &A11yPendingBind,
        binder: u64,
        connection_id: i32,
        connection: u64,
        window_token: u64,
    ) {
        if binder != 0 {
            // The system calls into the bound service's
            // `IAccessibilityServiceClient` for every event; the host holds the
            // reference for as long as the service is bound.
            self.refs.hold(acos_parcel::BinderId(binder));
        }
        self.bound_service = Some(A11yBoundService {
            component: pending.component.clone(),
            package: pending.package.clone(),
            class: pending.class.clone(),
            binder,
            connection_id,
            connection,
            window_token,
            service_info: None,
            bind_info: pending.info.clone(),
        });
    }
}

/// `AccessibilityWindowInfo`'s view of a window type, from the AOSP mapping
/// (`AccessibilityWindowInfo.TYPE_*`).
///
/// `TYPE_APPLICATION` = 1, `TYPE_INPUT_METHOD` = 2, `TYPE_SYSTEM` = 3,
/// `TYPE_ACCESSIBILITY_OVERLAY` = 4, `TYPE_SPLIT_SCREEN_DIVIDER` = 5,
/// `TYPE_MAGNIFICATION_OVERLAY` = 6. Every other window kind is a system window
/// (`TYPE_SYSTEM`), which is what AOSP reports for a window that is not one of
/// the five named ones.
#[must_use]
pub fn a11y_window_type(type_: i32) -> i32 {
    use acos_wm::layers::{
        WindowKind, TYPE_ACCESSIBILITY_MAGNIFICATION_OVERLAY, TYPE_ACCESSIBILITY_OVERLAY,
        TYPE_APPLICATION_MEDIA, TYPE_INPUT_METHOD, TYPE_INPUT_METHOD_DIALOG,
        TYPE_MAGNIFICATION_OVERLAY,
    };
    if type_ == TYPE_INPUT_METHOD || type_ == TYPE_INPUT_METHOD_DIALOG {
        return 2;
    }
    if type_ == TYPE_ACCESSIBILITY_OVERLAY {
        return 4;
    }
    if type_ == TYPE_MAGNIFICATION_OVERLAY || type_ == TYPE_ACCESSIBILITY_MAGNIFICATION_OVERLAY {
        return 6;
    }
    match WindowKind::of(type_) {
        WindowKind::Application => 1,
        // `TYPE_APPLICATION_MEDIA` is a sub-window drawn behind its activity;
        // AOSP reports sub-windows as `TYPE_APPLICATION`.
        WindowKind::SubWindow if type_ == TYPE_APPLICATION_MEDIA => 1,
        WindowKind::SubWindow => 1,
        _ => 3,
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
                                                 // No `mObservedMotionEventSources`: the image's framework
                                                 // `AccessibilityServiceInfo.writeToParcel`/`initFromParcel` stop at
                                                 // `mMotionEventSources` (see the spec in `parcelable.rs`), so a
                                                 // trailing int here would be 4 stray bytes the guest's reader
                                                 // leaves in the parcel.
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
    /// The a11y manager's registrations (`addClient`,
    /// `addAccessibilityInteractionConnection`, the bound service) are host-held
    /// strong references: `A11yState::refs` is the one ledger, and this object
    /// (or the service-connection node, whichever serves the transaction) is
    /// what reports it to the host.
    fn drain_retained(&mut self) -> Vec<acos_parcel::BinderId> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .refs
            .drain_retained()
    }

    fn drain_released(&mut self) -> Vec<acos_parcel::BinderId> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .refs
            .drain_released()
    }

    fn release_all_refs(&mut self) -> Vec<acos_parcel::BinderId> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .refs
            .release_all()
    }

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
                    if !state.clients.contains(&client.0) {
                        state.clients.push(client.0);
                    }
                    // `AccessibilityManager.addClient` is a registration: the
                    // host keeps the client node alive until `removeClient`.
                    state.refs.hold(client);
                }
                let id = state.next_client_id;
                state.next_client_id += 1;
                let _ = id;
                // The app's `AccessibilityManager` connects here on start-up:
                // this is where AOSP's `AccessibilityManagerService` would
                // have bound the enabled services (`bindServiceAsUser`), so the
                // system -> app bind is planned from this call. A bind that
                // cannot start yet (no `attachApplication` yet, or nothing
                // installed) must not fail the app's `addClient`: it is
                // recorded and retried on the next call.
                let deliveries = match crate::a11y_bind::plan_bind(&mut state) {
                    Ok(deliveries) => deliveries,
                    Err(err) => {
                        let reason = format!("bind not started: {err}");
                        if !state.unsupported.contains(&reason) {
                            state.unsupported.push(reason);
                        }
                        Vec::new()
                    }
                };
                Ok(CallResult {
                    reply: vec![Value::Long(
                        (i64::from(state_flags) << 32) | i64::from(0x7fff_ffff),
                    )],
                    // The state is in the return value on this image; a
                    // delivery would be a second, unsolicited `setState`.
                    deliveries,
                })
            }
            "getInstalledAccessibilityServiceList" | "getEnabledAccessibilityServiceList" => {
                // `getInstalled...` reads the **package manager** (the installed
                // APK's `<service android:permission="…BIND_ACCESSIBILITY_SERVICE">`
                // records), exactly like AOSP's `queryIntentServices`; the
                // launch config only decides what is *enabled*. `getEnabled...`
                // adds the feedback-type filter AOSP applies.
                let enabled =
                    matches!(method.name.as_str(), "getInstalledAccessibilityServiceList")
                        || state.enabled;
                let installed = state.installed_services();
                let mut items = Vec::new();
                if enabled {
                    for service in &state.enabled_services {
                        if installed.iter().any(|i| i == service) {
                            if let Some(info) = service_info_value(service) {
                                items.push(info);
                            }
                        }
                    }
                }
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
                if connection != 0 {
                    // The window's node tree is reachable only through this
                    // connection; the host holds it for as long as the app keeps
                    // it registered (`removeAccessibilityInteractionConnection`
                    // releases it).
                    state.refs.hold(acos_parcel::BinderId(connection));
                }
                // `AccessibilityWindowId`: one id per `IWindow`, assigned on
                // first use and stable for the session. The same id is what
                // `AccessibilityWindowInfo.getId()` reports, so a service that
                // lists the windows can query the connection by id.
                let id = state.window_id_for(window);
                Ok(CallResult {
                    reply: vec![Value::Int(id)],
                    deliveries: Vec::new(),
                })
            }
            "removeAccessibilityInteractionConnection" => {
                let window = args.first().and_then(Value::as_binder).map(|b| b.0);
                if let Some(window) = window {
                    let gone: Vec<u64> = state
                        .connections
                        .iter()
                        .filter(|c| c.window == window)
                        .map(|c| c.connection)
                        .collect();
                    for connection in gone {
                        state.refs.release(acos_parcel::BinderId(connection));
                    }
                    state.connections.retain(|c| c.window != window);
                    state.forget_window(window);
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
            "removeClient" => {
                // `removeClient(IAccessibilityManagerClient)`: the decoded table
                // loses the client argument, so every client this service holds
                // is released (there is one per process).
                let named = args.first().and_then(Value::as_binder);
                match named {
                    Some(client) => {
                        state.clients.retain(|held| *held != client.0);
                        state.refs.release(client);
                    }
                    None => {
                        for client in std::mem::take(&mut state.clients) {
                            state.refs.release(acos_parcel::BinderId(client));
                        }
                    }
                }
                Ok(CallResult {
                    reply: vec![Value::Bool(true)],
                    deliveries: Vec::new(),
                })
            }
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
    /// Strong references the host holds on the registered clients
    /// (`IAutoFillManagerClient`). A registration *is* a reference: it keeps the
    /// guest's node rooted until `removeClient`, and one ledger covers every
    /// module (`crate::refs`).
    pub refs: crate::refs::GuestRefs,
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
                    if client.0 != 0 && !state.clients.contains(&client.0) {
                        state.clients.push(client.0);
                        // The service stores the client: one strong reference.
                        state.refs.hold(client);
                    }
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
            "removeClient" => {
                if let Some(client) = args.first().and_then(Value::as_binder) {
                    state.clients.retain(|held| *held != client.0);
                    state.refs.release(client);
                }
                Vec::new()
            }
            "finishSession" | "cancelSession" => Vec::new(),
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

    fn drain_retained(&mut self) -> Vec<acos_parcel::BinderId> {
        self.state.lock().unwrap().refs.drain_retained()
    }

    fn drain_released(&mut self) -> Vec<acos_parcel::BinderId> {
        self.state.lock().unwrap().refs.drain_released()
    }

    fn release_all_refs(&mut self) -> Vec<acos_parcel::BinderId> {
        self.state.lock().unwrap().refs.release_all()
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
