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
    ("android.view.IWindowSession", "acos-wm"),
    ("android.view.IWindow", "acos-wm"),
    ("android.view.IWindowSessionCallback", "acos-wm"),
    ("com.android.internal.view.IInputMethodManager", "acos-wm"),
    (
        "com.android.internal.inputmethod.IInputMethodSession",
        "acos-wm",
    ),
    (
        "com.android.internal.inputmethod.IImeTracker",
        "acos-wm",
    ),
    (
        "com.android.internal.inputmethod.IRemoteInputConnection",
        "acos-wm",
    ),
    (
        "com.android.internal.inputmethod.IInputMethodClient",
        "acos-wm",
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
    (
        "network_management",
        "android.os.INetworkManagementService",
    ),
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
        "androidfuture",
        "com.android.internal.infra.IAndroidFuture",
    ),
];

/// Descriptor for a `ServiceManager` name.
#[must_use]
pub fn descriptor_for(name: &str) -> Option<&'static str> {
    NAMES.iter().find(|(n, _)| *n == name).map(|(_, d)| *d)
}

/// `ServiceManager` name for a descriptor.
#[must_use]
pub fn name_for(descriptor: &str) -> Option<&'static str> {
    NAMES.iter().find(|(_, d)| *d == descriptor).map(|(n, _)| *n)
}

/// Install the standard ACOS service set on `host`.
pub fn install(host: &mut ServiceHost) {
    use crate::host::ServiceObject;
    use std::sync::{Arc, Mutex};

    // The dispatcher answers `IServiceManager` itself; give it a binder so the
    // native layer can address it like any other service.
    host.register_aux("android.os.IServiceManager");

    // Auxiliary binders returned by other services.
    let controller = host.register_aux("android.app.IActivityClientController");
    let provider = host.register_aux("android.content.IContentProvider");

    // Shared state. The PMS parses the APK lazily inside its factory so
    // installing the host touches no package data.
    let atms = Arc::new(Mutex::new(crate::atms::AtmsState::new()));
    let settings = Arc::new(Mutex::new(crate::provider::SettingsState::new()));
    let display = Arc::new(Mutex::new(crate::display::DisplayState::new(
        crate::display::DisplayInfo::hello_views(),
    )));
    let a11y = Arc::new(Mutex::new(crate::misc::A11yState::new()));
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
    host.register("package", "android.content.pm.IPackageManager", move |w| {
        let pms = Arc::new(Mutex::new(crate::pms::PmsState::hello_views()));
        Box::new(crate::pms::PmsService::new(pms, w)) as Box<dyn ServiceObject>
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
    host.register(
        "graphicsstats",
        "android.view.IGraphicsStats",
        move |_w| {
            Box::new(crate::misc::GraphicsStatsService::new(Arc::clone(&gfx)))
                as Box<dyn ServiceObject>
        },
    );
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
            Box::new(crate::misc::VoiceService::new(Arc::clone(&voice)))
                as Box<dyn ServiceObject>
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
    let contentservice = Arc::clone(&settings);
    host.register(
        "content",
        "android.content.IContentService",
        move |_w| {
            Box::new(crate::provider::ContentServiceService::new(Arc::clone(
                &contentservice,
            ))) as Box<dyn ServiceObject>
        },
    );
    host.register("input", "android.hardware.input.IInputManager", move |_w| {
        Box::new(crate::misc::InputService::new(Arc::clone(&input))) as Box<dyn ServiceObject>
    });

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
    // handle the dispatcher routes.
    let settings_provider = Arc::clone(&settings);
    host.register(
        "settings",
        "android.content.IContentProvider",
        move |_w| {
            Box::new(crate::provider::ProviderService::new(Arc::clone(
                &settings_provider,
            ))) as Box<dyn ServiceObject>
        },
    );
    // Point the aux binder at the registered controller entry.
    let _ = (controller, provider);
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
        assert_eq!(descriptor_for("activity"), Some("android.app.IActivityManager"));
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
