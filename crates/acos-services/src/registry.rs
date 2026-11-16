//! `IServiceManager` semantics and the ACOS owner map.
//!
//! Android's `ServiceManager` is the chokepoint for every other interface:
//! `checkService(name)` returns the binder or `null`. ACOS answers from this
//! table, **lazily** constructing the service object on first request. Names
//! the framework asks for at launch are listed in
//! `research/ambient-state/services_requested.tsv`; unknown names return `None`
//! so the framework takes its real "service is missing" path.
//!
//! Interfaces owned by another ACOS component (currently `acos-wm`) are listed
//! in [`NOT_OWNED`] so the dispatcher returns a structured
//! `UNSUPPORTED_ANDROID_API` naming the owner instead of a default success.

use crate::host::ServiceHost;

/// Descriptors implemented by another component.
pub const NOT_OWNED: &[(&str, &str)] = &[
    ("android.view.IWindowManager", "acos-wm"),
    // `android.view.IWindowSession` is served by `crate::window_session` over
    // the same `WmsState` (W1): the `ServiceHost` registers it as a dynamic
    // node so the binder `IWindowManager.openSession` returns is routable.
    ("android.view.IWindow", "acos-wm"),
    ("android.view.IWindowSessionCallback", "acos-wm"),
    // IME app-side objects: these are *delivered* to the app by the VM, they
    // are not services (`IInputMethodManager`/`IInputMethodSession`/`IImeTracker`
    // are served by `acos-services::ime`).
    (
        "com.android.internal.inputmethod.IRemoteInputConnection",
        "acos-vm (delivery)",
    ),
    (
        "com.android.internal.inputmethod.IInputMethodClient",
        "acos-vm (delivery)",
    ),
];

/// The owning component for a descriptor, when it is not `acos-services`.
#[must_use]
pub fn owner_of(descriptor: &str) -> Option<&'static str> {
    NOT_OWNED
        .iter()
        .find(|(d, _)| *d == descriptor)
        .map(|(_, o)| *o)
}

/// `ServiceManager` name -> AIDL descriptor for the ACOS-owned set.
pub const NAMES: &[(&str, &str)] = &[
    ("activity", "android.app.IActivityManager"),
    // `window` is answered by `crates/acos-services/src/wms.rs` + the
    // `acos-runtime` wire codecs (T-29).
    ("window", "android.view.IWindowManager"),
    ("activity_task", "android.app.IActivityTaskManager"),
    ("package", "android.content.pm.IPackageManager"),
    ("display", "android.hardware.display.IDisplayManager"),
    (
        "accessibility",
        "android.view.accessibility.IAccessibilityManager",
    ),
    ("autofill", "android.view.autofill.IAutoFillManager"),
    (
        "textservices",
        "com.android.internal.textservice.ITextServicesManager",
    ),
    ("audio", "android.media.IAudioService"),
    ("graphicsstats", "android.view.IGraphicsStats"),
    ("network_management", "android.os.INetworkManagementService"),
    ("game", "android.app.IGameManagerService"),
    (
        "virtualdevice",
        "android.companion.virtual.IVirtualDeviceManager",
    ),
    (
        "content_capture",
        "android.view.contentcapture.IContentCaptureManager",
    ),
    (
        "voiceinteraction",
        "com.android.internal.app.IVoiceInteractionManagerService",
    ),
    ("content", "android.content.IContentService"),
    // Test seam: the Settings provider has no `ServiceManager` name on a real
    // device (it is reached through `IActivityManager.getContentProvider`), but
    // exposing it here lets the T-27 Java test drive the real hand-written
    // `ContentProviderProxy` directly without reimplementing the provider
    // acquisition path (which is T-23/T-24 work).
    ("settings", "android.content.IContentProvider"),
    ("input", "android.hardware.input.IInputManager"),
    (
        "input_method",
        "com.android.internal.view.IInputMethodManager",
    ),
    (
        "ime_session",
        "com.android.internal.inputmethod.IInputMethodSession",
    ),
    (
        "ime_tracker",
        "com.android.internal.inputmethod.IImeTracker",
    ),
    ("clipboard", "android.content.IClipboard"),
    ("user", "android.os.IUserManager"),
    // W157-A: React Native's real `AndroidInfoModule` reads the real
    // `UiModeManager`; the framework asks `ServiceManager` for `uimode`.
    ("uimode", "android.app.IUiModeManager"),
    ("notification", "android.app.INotificationManager"),
    (
        "statusbar",
        "com.android.internal.statusbar.IStatusBarService",
    ),
    ("permission", "android.permission.IPermissionManager"),
    ("appops", "com.android.internal.app.IAppOpsService"),
    ("connectivity", "android.net.IConnectivityManager"),
    ("androidfuture", "com.android.internal.infra.IAndroidFuture"),
    // W-R T2: the compositor's client surface. `ComposerServiceAIDL` looks the
    // compositor up as `getService("SurfaceFlinger")`, so the name must resolve
    // to the `android.gui.ISurfaceComposer` node `compositor_client` installs.
    ("SurfaceFlinger", "android.gui.ISurfaceComposer"),
    // W26: the framework's `SystemSensorManager` (React Native debug builds
    // reach it in the dev-support shake detector) asks for `sensorservice`
    // during `nativeCreate`. Answering null makes libbinder's
    // `ServiceManagerShim::getService` retry `checkService` for 5000 ms per
    // lookup (`.cache/aosp/native/libs/binder/IServiceManager.cpp:309`), which
    // spent ~11.9 s of the launcher's 12 s resume budget (W25 §blocker 2). The
    // minimal stock-shaped server lives in `misc::SensorServer`.
    ("sensorservice", "android.gui.SensorServer"),
    // K2/R1: services a real Android device always publishes. `getSystemService`
    // returns a manager object for every one of these, so answering null is an
    // NPE in app code (the K1 corpus hit JobScheduler, ShortcutManager, storage,
    // PowerManager and AccountManager this way). The handlers live in
    // `crate::extras`; they answer the device's real offline state.
    ("mount", "android.os.storage.IStorageManager"),
    ("shortcut", "android.content.pm.IShortcutService"),
    ("jobscheduler", "android.app.job.IJobScheduler"),
    ("power", "android.os.IPowerManager"),
    ("account", "android.accounts.IAccountManager"),
    ("thermalservice", "android.os.IThermalService"),
    ("alarm", "android.app.IAlarmManager"),
    // `TrafficStats.init` asks for this binder on every process start; it only
    // has to resolve, not answer a network query (there is no network).
    ("netstats", "android.net.INetworkStatsService"),
    // K5: services a real device always publishes and that framework
    // constructors resolve without a null check. `Geocoder.isPresent()` asks
    // `location` (a null binder is an NPE in `Objects.requireNonNull`), and
    // `new KeyguardManager(context)` resolves `trust` with
    // `ServiceManager.getServiceOrThrow` (a missing service makes
    // `Context.getSystemService(KeyguardManager.class)` return null, which app
    // code dereferences). Both handlers are in `crate::extras`; they answer a
    // device with no geocoder and no credential/keyguard.
    ("location", "android.location.ILocationManager"),
    ("trust", "android.app.trust.ITrustManager"),
    // `RestrictionsManager` is also always resolvable; `com.tailscale.ipn`
    // dereferences it in `Application.onCreate` (an unmanaged device answers an
    // empty restrictions Bundle).
    ("restrictions", "android.content.IRestrictionsManager"),
];

/// Names in [`NAMES`] that are backed by an **aux** binder rather than a service
/// object.
///
/// `netstats` is registered because `TrafficStats.init` asks for the binder on
/// every process start; it has to *resolve*, it is never transacted on. The
/// binder is created with [`ServiceHost::register_aux`], which deliberately has
/// no `ServiceManager` name, so `checkService("netstats")` answers `null`
/// (exactly as it did before the module framework) and the `core` module does
/// not claim the name in its manifest.
pub const AUX_BACKED_NAMES: &[&str] = &["netstats"];

/// Descriptor for a `ServiceManager` name.
#[must_use]
pub fn descriptor_for(name: &str) -> Option<&'static str> {
    NAMES.iter().find(|(n, _)| *n == name).map(|(_, d)| *d)
}

/// `ServiceManager` name for a descriptor.
#[must_use]
pub fn name_for(descriptor: &str) -> Option<&'static str> {
    NAMES
        .iter()
        .find(|(_, d)| *d == descriptor)
        .map(|(n, _)| *n)
}

/// Live host-side handles for one interactive session.
///
/// The standard install owns the window session's input producer and the
/// compositor-client state privately. W10's live session needs both: it
/// injects real taps through the same `InputHost` the window session uses, and
/// it captures screenshots from C1's stored buffer. Returning clones keeps the
/// install path unchanged for every other caller.
#[derive(Clone, Debug)]
pub struct LiveHandles {
    /// The one authoritative `InputChannel` producer (I2).
    pub input: crate::window_session::input_dispatch::InputHost,
    /// The compositor-client (C1) state that keeps the newest buffer.
    pub compositor: std::sync::Arc<std::sync::Mutex<crate::surface_client::SurfaceState>>,
    /// ROT3: the live `IWindowManager` state (`WmsState`). A display rotation
    /// is a WMS operation, so the interactive session drives the same state the
    /// `IWindowSession` relayout replies are computed from.
    pub wms: std::sync::Arc<std::sync::Mutex<crate::wms::WmsState>>,
}

/// Install the standard ACOS service set on `host`.
pub fn install(host: &mut ServiceHost) {
    let _ = install_with_handles(host);
}

/// Install the standard ACOS service set and return the handles a live session
/// drives (`InputHost`, C1 state).
pub fn install_with_handles(host: &mut ServiceHost) -> LiveHandles {
    install_with_handles_and_insets(host, None)
}

/// W7b: [`install_with_handles`] with an explicit app-window inset model.
pub fn install_with_handles_and_insets(
    host: &mut ServiceHost,
    insets: Option<crate::wms::InsetsMode>,
) -> LiveHandles {
    use crate::host::ServiceObject;
    use std::sync::{Arc, Mutex};

    // The dispatcher answers `IServiceManager` itself; give it a binder so the
    // native layer can address it like any other service.
    host.register_aux("android.os.IServiceManager");

    // Auxiliary binders returned by other services.
    let controller = host.register_aux("android.app.IActivityClientController");
    let provider = host.register_aux("android.content.IContentProvider");
    host.register_aux("android.content.pm.IPackageInstaller");

    // WMS state machine (T-29); the runtime transport owns the wire codecs.
    let wms_state = crate::wms::install_with_insets(host, insets);

    // W-R T2: the frame clock. `android.gui.ISurfaceComposer` is served under
    // the `ServiceManager` name `SurfaceFlinger` (that is how
    // `ComposerServiceAIDL::getComposerService()` finds it), and it hands out
    // `android.gui.IDisplayEventConnection` nodes from a small pre-allocated
    // pool — `Choreographer`'s `DisplayEventReceiver` steals the `BitTube` from
    // one of them. See `results/art-runtime/V1.md`.
    //
    // It also installs the compositor-**client** node (C1) and returns its
    // shared state; the session installs it first so `relayout` can bind the
    // window's `SurfaceControl` to a real layer (W5).
    let (_frame_clock, compositor) = crate::compositor_client::install_full(
        host,
        crate::compositor_client::DisplayDescriptor::reference(),
    );

    // W1: the `IWindowSession` node. `IWindowManager.openSession` answers this
    // binder; every `ViewRootImpl` window call lands here, byte-exact (see
    // `window_session.rs`). `IWindowId` is an aux binder so `getWindowId`
    // returns a routable handle instead of a fabricated id.
    let window_id = host.register_aux("android.view.IWindowId");
    let session_state = Arc::clone(&wms_state);
    let input_host = crate::window_session::input_dispatch::InputHost::from_env();
    // MODA11Y4: `performGlobalAction(GLOBAL_ACTION_BACK)` goes through the same
    // InputChannel as a scenario `back`, so keep a handle before the session
    // service takes the original.
    let a11y_input = input_host.clone();
    let live = LiveHandles {
        input: input_host.clone(),
        compositor: Arc::clone(&compositor),
        wms: Arc::clone(&wms_state),
    };
    let session_binder = host.register_dynamic(
        "android.view.IWindowSession",
        Box::new(
            crate::window_session::WindowSessionService::new(session_state, window_id)
                .with_compositor(Arc::clone(&compositor))
                // I2: the one authoritative InputChannel producer. It publishes
                // real FOCUS/TOUCH_MODE messages on `addToDisplayAsUser` and,
                // when `ACOS_INPUT_TAP`/`ACOS_INPUT_KEY` is set, injects after
                // the first relayout and logs the FINISHED/TIMELINE replies.
                .with_input(input_host),
        ),
    );
    wms_state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .host_session = session_binder;

    // Shared state. The PMS parses the APK lazily inside its factory so
    // installing the host touches no package data.
    let atms = Arc::new(Mutex::new(crate::atms::AtmsState::new()));
    let settings = Arc::new(Mutex::new(crate::provider::SettingsState::new()));
    let display =
        Arc::new(Mutex::new(crate::display::DisplayState::new(
            crate::display::DisplayInfo::hello_views_with(insets.unwrap_or_else(|| {
                crate::wms::InsetsMode::from_env(crate::wms::DEFAULT_INSETS_MODE)
            })),
        )));
    let a11y = Arc::new(Mutex::new(crate::misc::A11yState::with_settings(
        Arc::clone(&settings),
    )));
    // MODA11Y2: the accessibility module reads the **installed** service list
    // from the package manager (lazily, after the APK is installed) instead of
    // trusting the launch config, and the dispatcher needs the state for the
    // system -> app bind (`attachApplication` / `publishService`).
    let pms_shared = Arc::new(Mutex::new(crate::pms::PmsState::hello_views()));
    {
        let mut a11y_state = a11y
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        a11y_state.pms = Some(Arc::clone(&pms_shared));
        // MODA11Y3: `getWindows`/`getWindow` answer from the LAYER1 window
        // stack, which is also what composition, the UI tree and input read.
        a11y_state.windows = Some(Arc::clone(&wms_state));
        // MODA11Y4: the same input channels the session's Back uses.
        a11y_state.input = Some(a11y_input);
    }
    host.set_accessibility_state(Arc::clone(&a11y));
    let input = Arc::new(Mutex::new(crate::misc::InputState::new()));
    let textservices = Arc::new(Mutex::new(crate::misc::TextServicesState::new()));
    let autofill = Arc::new(Mutex::new(crate::misc::AutofillState::new()));
    let audio = Arc::new(Mutex::new(crate::misc::AudioState::new()));
    let gfx = Arc::new(Mutex::new(crate::misc::GraphicsStatsState::new()));
    let net = Arc::new(Mutex::new(crate::misc::NetState::new()));
    let game = Arc::new(Mutex::new(crate::misc::GameState::new()));
    let virtualdevice = Arc::new(Mutex::new(crate::misc::VirtualDeviceState::new()));
    let contentcapture = Arc::new(Mutex::new(crate::misc::ContentCaptureState::new()));
    let voice = Arc::new(Mutex::new(crate::misc::VoiceState::new()));
    let future = Arc::new(Mutex::new(crate::misc::FutureState::new()));
    let ime = crate::ime::shared();
    let clipboard = crate::clipboard::shared();
    let appops = Arc::new(Mutex::new(crate::declared::AppOpsState::new()));
    let permission = Arc::new(Mutex::new(crate::declared::PermissionState::new()));
    let users = Arc::new(Mutex::new(crate::declared::UserState::new()));
    let uimode = Arc::new(Mutex::new(crate::declared::UiModeState::new()));
    let notification = Arc::new(Mutex::new(crate::declared::NotificationState::new()));
    let statusbar = Arc::new(Mutex::new(crate::declared::StatusBarState::new()));
    let connectivity = Arc::new(Mutex::new(crate::connectivity::ConnectivityState::new()));
    let shortcuts = Arc::new(Mutex::new(crate::extras::ShortcutState::new()));
    let jobs = Arc::new(Mutex::new(crate::extras::JobState::new()));
    let power = Arc::new(Mutex::new(crate::extras::PowerState::new()));
    let alarm = Arc::new(Mutex::new(crate::extras::AlarmState::new()));

    // ATMS shares one state across the three interfaces.
    let atms_am = Arc::clone(&atms);
    let atms_at = Arc::clone(&atms);

    host.register("activity", "android.app.IActivityManager", move |w| {
        Box::new(crate::atms::AtmsService::new(
            crate::atms::AtmsInterface::ActivityManager,
            Arc::clone(&atms_am),
            w,
        )) as Box<dyn ServiceObject>
    });
    host.register(
        "activity_task",
        "android.app.IActivityTaskManager",
        move |w| {
            Box::new(crate::atms::AtmsService::new(
                crate::atms::AtmsInterface::ActivityTaskManager,
                Arc::clone(&atms_at),
                w,
            )) as Box<dyn ServiceObject>
        },
    );
    host.register("package", "android.content.pm.IPackageManager", {
        let pms_state = Arc::clone(&pms_shared);
        move |w| {
            let pms = Arc::clone(&pms_state);
            Box::new(crate::pms::PmsService::new(pms, w)) as Box<dyn ServiceObject>
        }
    });
    host.register(
        "display",
        "android.hardware.display.IDisplayManager",
        move |_w| {
            Box::new(crate::display::DisplayService::new(Arc::clone(&display)))
                as Box<dyn ServiceObject>
        },
    );
    host.register(
        "accessibility",
        "android.view.accessibility.IAccessibilityManager",
        move |_w| {
            Box::new(crate::misc::A11yService::new(Arc::clone(&a11y))) as Box<dyn ServiceObject>
        },
    );
    host.register(
        "autofill",
        "android.view.autofill.IAutoFillManager",
        move |_w| {
            Box::new(crate::misc::AutofillService::new(Arc::clone(&autofill)))
                as Box<dyn ServiceObject>
        },
    );
    host.register(
        "textservices",
        "com.android.internal.textservice.ITextServicesManager",
        move |_w| {
            Box::new(crate::misc::TextServicesService::new(Arc::clone(
                &textservices,
            ))) as Box<dyn ServiceObject>
        },
    );
    host.register("audio", "android.media.IAudioService", move |_w| {
        Box::new(crate::misc::AudioService::new(Arc::clone(&audio))) as Box<dyn ServiceObject>
    });
    host.register("graphicsstats", "android.view.IGraphicsStats", move |_w| {
        Box::new(crate::misc::GraphicsStatsService::new(Arc::clone(&gfx))) as Box<dyn ServiceObject>
    });
    host.register(
        "network_management",
        "android.os.INetworkManagementService",
        move |_w| {
            Box::new(crate::misc::NetService::new(Arc::clone(&net))) as Box<dyn ServiceObject>
        },
    );
    host.register("game", "android.app.IGameManagerService", move |_w| {
        Box::new(crate::misc::GameService::new(Arc::clone(&game))) as Box<dyn ServiceObject>
    });
    host.register(
        "virtualdevice",
        "android.companion.virtual.IVirtualDeviceManager",
        move |_w| {
            Box::new(crate::misc::VirtualDeviceService::new(Arc::clone(
                &virtualdevice,
            ))) as Box<dyn ServiceObject>
        },
    );
    host.register(
        "content_capture",
        "android.view.contentcapture.IContentCaptureManager",
        move |_w| {
            Box::new(crate::misc::ContentCaptureService::new(Arc::clone(
                &contentcapture,
            ))) as Box<dyn ServiceObject>
        },
    );
    host.register(
        "voiceinteraction",
        "com.android.internal.app.IVoiceInteractionManagerService",
        move |_w| {
            Box::new(crate::misc::VoiceService::new(Arc::clone(&voice))) as Box<dyn ServiceObject>
        },
    );
    let future_service = Arc::clone(&future);
    host.register(
        "androidfuture",
        "com.android.internal.infra.IAndroidFuture",
        move |_w| {
            Box::new(crate::misc::FutureService::new(Arc::clone(&future_service)))
                as Box<dyn ServiceObject>
        },
    );
    // W26: the sensor server (see the `NAMES` entry above for why it exists).
    // It is a raw node (`android.gui.*` has no decoded table) published under
    // the `ServiceManager` name the framework asks for, so `checkService`
    // answers on the first try and `SensorManager` never enters its retry loop.
    //
    // MODSENS: the node is `core`'s, with **no feed** — the stock answer of a
    // device without sensors (empty list). The `sensors` module installs a feed
    // into this same node while attached, so a hot attach does not invalidate a
    // binder the app already holds (an app keeps the `ISensorServer` proxy for
    // its whole life). `core`'s manifest therefore does not claim the name; see
    // `core_module::FILLED_BY_MODULE`.
    let sensors = Arc::new(Mutex::new(crate::sensors::SensorHub::default()));
    crate::sensors::install(host, Arc::clone(&sensors));
    // K5: the two raw nodes above. Neither interface has a decoded
    // `acos-aidl` table (their own `raw_transact` answers the exact wire
    // shape), so they are dynamic nodes registered under their real
    // `ServiceManager` names — `checkService` answers on the first try.
    host.register_dynamic_named(
        "location",
        crate::extras::LOCATION_DESCRIPTOR,
        Box::new(crate::extras::LocationService::new()),
    );
    host.register_dynamic_named(
        "trust",
        crate::extras::TRUST_DESCRIPTOR,
        Box::new(crate::extras::TrustService::new()),
    );
    host.register_dynamic_named(
        "restrictions",
        crate::extras::RESTRICTIONS_DESCRIPTOR,
        Box::new(crate::extras::RestrictionsService::new()),
    );
    let contentservice = Arc::clone(&settings);
    host.register("content", "android.content.IContentService", move |_w| {
        Box::new(crate::provider::ContentServiceService::new(Arc::clone(
            &contentservice,
        ))) as Box<dyn ServiceObject>
    });
    host.register("input", "android.hardware.input.IInputManager", move |_w| {
        Box::new(crate::misc::InputService::new(Arc::clone(&input))) as Box<dyn ServiceObject>
    });

    // T-38: the null in-process IME. `IInputMethodSession`/`IImeTracker` are
    // auxiliary binders a real IME would hold; registering them keeps the
    // decoded transactions routable instead of silently missing.
    host.register_aux("com.android.internal.inputmethod.IInputMethodSession");
    host.register_aux("com.android.internal.inputmethod.IImeTracker");
    // W13: the focus path's `startInputOrWindowGainedFocus` carries a real
    // `EditorInfo` and returns a real `InputBindResult`, neither of which the
    // generic `Value` codec can express. Register the manager as a dynamic
    // node so `ServiceHost` routes it to `raw_transact`; every other method
    // delegates to `ImeService`'s generic layout (see `ime_wire.rs`).
    let ime_manager = Arc::clone(&ime);
    host.register_dynamic_named(
        "input_method",
        "com.android.internal.view.IInputMethodManager",
        Box::new(crate::ime_wire::ImeWireService::new(ime_manager)),
    );
    let ime_session = Arc::clone(&ime);
    host.register(
        "ime_session",
        "com.android.internal.inputmethod.IInputMethodSession",
        move |_w| {
            Box::new(crate::ime::ImeSessionService::new(Arc::clone(&ime_session)))
                as Box<dyn ServiceObject>
        },
    );
    let ime_tracker = Arc::clone(&ime);
    host.register(
        "ime_tracker",
        "com.android.internal.inputmethod.IImeTracker",
        move |_w| {
            Box::new(crate::ime::ImeTrackerService::new(Arc::clone(&ime_tracker)))
                as Box<dyn ServiceObject>
        },
    );

    // T-39: clipboard + the declared service set.
    host.register("clipboard", "android.content.IClipboard", move |_w| {
        Box::new(crate::clipboard::ClipboardService::new(Arc::clone(
            &clipboard,
        ))) as Box<dyn ServiceObject>
    });
    host.register(
        "appops",
        "com.android.internal.app.IAppOpsService",
        move |_w| {
            Box::new(crate::declared::AppOpsService::new(Arc::clone(&appops)))
                as Box<dyn ServiceObject>
        },
    );
    host.register(
        "permission",
        "android.permission.IPermissionManager",
        move |_w| {
            Box::new(crate::declared::PermissionService::new(Arc::clone(
                &permission,
            ))) as Box<dyn ServiceObject>
        },
    );
    host.register("user", "android.os.IUserManager", move |_w| {
        Box::new(crate::declared::UserService::new(Arc::clone(&users))) as Box<dyn ServiceObject>
    });
    host.register("uimode", "android.app.IUiModeManager", move |_w| {
        Box::new(crate::declared::UiModeService::new(Arc::clone(&uimode))) as Box<dyn ServiceObject>
    });
    // The runtime plays the system-UI role for toasts, so it needs the state.
    host.set_notification_state(Arc::clone(&notification));
    host.register(
        "notification",
        "android.app.INotificationManager",
        move |_w| {
            Box::new(crate::declared::NotificationService::new(Arc::clone(
                &notification,
            ))) as Box<dyn ServiceObject>
        },
    );
    host.register(
        "statusbar",
        "com.android.internal.statusbar.IStatusBarService",
        move |_w| {
            Box::new(crate::declared::StatusBarService::new(Arc::clone(
                &statusbar,
            ))) as Box<dyn ServiceObject>
        },
    );
    host.register(
        "connectivity",
        "android.net.IConnectivityManager",
        move |_w| {
            Box::new(crate::connectivity::ConnectivityService::new(Arc::clone(
                &connectivity,
            ))) as Box<dyn ServiceObject>
        },
    );
    host.register("mount", "android.os.storage.IStorageManager", move |_w| {
        Box::new(crate::extras::StorageService::new()) as Box<dyn ServiceObject>
    });
    host.register(
        "shortcut",
        "android.content.pm.IShortcutService",
        move |_w| {
            Box::new(crate::extras::ShortcutService::new(Arc::clone(&shortcuts)))
                as Box<dyn ServiceObject>
        },
    );
    host.register("jobscheduler", "android.app.job.IJobScheduler", move |_w| {
        Box::new(crate::extras::JobSchedulerService::new(Arc::clone(&jobs)))
            as Box<dyn ServiceObject>
    });
    host.register("power", "android.os.IPowerManager", move |_w| {
        Box::new(crate::extras::PowerService::new(Arc::clone(&power))) as Box<dyn ServiceObject>
    });
    host.register("account", "android.accounts.IAccountManager", move |_w| {
        Box::new(crate::extras::AccountService::new()) as Box<dyn ServiceObject>
    });
    host.register("thermalservice", "android.os.IThermalService", move |_w| {
        Box::new(crate::extras::ThermalService::new()) as Box<dyn ServiceObject>
    });
    host.register("alarm", "android.app.IAlarmManager", move |_w| {
        Box::new(crate::extras::AlarmService::new(Arc::clone(&alarm))) as Box<dyn ServiceObject>
    });
    // TrafficStats only needs the binder to resolve; a real network manager is
    // out of scope on a device with no network.
    host.register_aux("android.net.INetworkStatsService");
    host.alias_service_name("permissionmgr", "android.permission.IPermissionManager");

    // IActivityClientController and IContentProvider are addressed through the
    // aux binders but still need dispatch objects.
    let atms_acc2 = Arc::clone(&atms);
    host.register(
        "activity_client_controller",
        "android.app.IActivityClientController",
        move |w| {
            Box::new(crate::atms::AtmsService::new(
                crate::atms::AtmsInterface::ClientController,
                Arc::clone(&atms_acc2),
                w,
            )) as Box<dyn ServiceObject>
        },
    );
    // Register the content-provider binder so `getContentProvider` can return a
    // handle the dispatcher routes. Record the binder for the runtime AMS
    // bridge, which materialises the `ContentProviderHolder` reply.
    let settings_provider = Arc::clone(&settings);
    let settings_binder =
        host.register("settings", "android.content.IContentProvider", move |_w| {
            Box::new(crate::provider::ProviderService::new(Arc::clone(
                &settings_provider,
            ))) as Box<dyn ServiceObject>
        });
    crate::provider::set_settings_provider_binder(settings_binder.0);
    // The `ContentProviderConnection` token a `getContentProvider` holder
    // carries; the framework writes it back to `removeContentProvider` /
    // `refContentProvider`, never transacts on it.
    let connection = host.register_dynamic(
        "com.android.server.am.ContentProviderConnection",
        Box::new(crate::provider::ProviderConnectionService),
    );
    // Serve `settings` from the provider registry: `IActivityManager`
    // `getContentProvider("settings")` answers with this binder and the
    // Settings-backed `IContentProvider` behind it.
    if let Ok(mut state) = atms.lock() {
        state
            .provider_registry
            .register(crate::provider::ProviderEntry {
                authority: "settings".into(),
                package: "com.android.providers.settings".into(),
                class: "SettingsProvider".into(),
                process_name: "android.process.settings".into(),
                read_permission: "android.permission.READ_SETTINGS".into(),
                write_permission: "android.permission.WRITE_SETTINGS".into(),
                exported: false,
                grant_uri_permissions: false,
                init_order: 0,
                binder: settings_binder.0,
                connection: connection.0,
                // The provider is served in-process, but the holder still
                // carries a real connection token so the framework's own
                // reference counting and release path are exercised.
                no_release_needed: false,
            });
    }
    // Point the aux binder at the registered controller entry.
    let _ = (controller, provider);
    live
}

/// Attach the aux binders to the host's registered entries.
///
/// `register_aux` creates a binder before the corresponding service entry is
/// registered; this reconciles the two so `getActivityClientController()` and
/// `getContentProvider()` return a binder the dispatcher can route.
pub fn reconcile_aux(host: &mut ServiceHost) {
    // Kept for callers that register aux binders explicitly; the standard
    // install order already produces a consistent table.
    let _ = host.aux("android.app.IActivityClientController");
}

#[cfg(test)]
mod t21_tests {
    use super::*;
    use crate::host::ServiceHost;

    #[test]
    fn names_map_to_descriptors_and_unknowns_are_none() {
        assert_eq!(
            descriptor_for("package"),
            Some("android.content.pm.IPackageManager")
        );
        assert_eq!(
            descriptor_for("activity"),
            Some("android.app.IActivityManager")
        );
        assert_eq!(name_for("android.app.IActivityManager"), Some("activity"));
        assert_eq!(descriptor_for("definitely.not.a.service"), None);
        assert_eq!(name_for("android.os.IBinder"), None);
    }

    #[test]
    fn check_service_returns_a_binder_for_a_known_name_and_none_otherwise() {
        let mut host = ServiceHost::standard();
        let package = host
            .check_service("package")
            .expect("lookup is infallible")
            .expect("package is registered");
        assert_eq!(
            host.descriptor_of(package),
            Some("android.content.pm.IPackageManager")
        );
        assert!(host.check_service("not.here").unwrap().is_none());
        assert!(host.service_names().contains(&"package"));
    }

    #[test]
    fn add_service_through_the_host_is_not_reachable_from_an_app() {
        // `IServiceManager.addService` is system-only; the host answers a
        // structured unsupported error rather than a silent success.
        let mut host = ServiceHost::standard();
        let sm = host
            .binder_for_descriptor("android.os.IServiceManager")
            .expect("service manager binder");
        let method = host
            .method("android.os.IServiceManager", "addService")
            .expect("addService table entry")
            .clone();
        let mut data = crate::codec::build_request(
            "android.os.IServiceManager",
            &method,
            &[
                crate::value::Value::Str(Some("test.svc".into())),
                crate::value::Value::Binder(None),
                crate::value::Value::Bool(false),
                crate::value::Value::Int(0),
            ],
        )
        .expect("build request");
        data.set_data_position(0);
        let mut reply = acos_parcel::Parcel::new();
        let err = host
            .transact(sm, method.code, &mut data, &mut reply, 0)
            .expect_err("addService must fail");
        assert!(matches!(
            err,
            crate::error::ServiceError::UnsupportedAndroidApi { .. }
        ));
    }
}
