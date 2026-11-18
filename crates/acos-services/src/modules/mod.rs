//! The shipped modules (ADR-0029). `core` lives in [`crate::core_module`].

pub mod accessibility;
pub mod biometric;
pub mod camera;
pub mod camera_metadata;
pub mod bluetooth;
pub mod location;
pub mod network;
pub mod notifications;
pub mod permissions;
pub mod sensors;
pub mod tail_stubs;
pub mod telephony;
pub mod vibrator;
pub mod webview;

/// The host APK a module's declared `system_app` installs (ADR-0029: the
/// launcher stages it before the guest boots, `module::system_apks`).
///
/// One dispatch point, so a manifest's `system_apps` entry is resolved by the
/// module that owns it and `module::system_apks` stays a pure function of the
/// specs. `None` when the module does not own `app`.
#[must_use]
pub fn system_app_apk(
    module: &str,
    app: &str,
    config: &crate::module::ModuleConfig,
) -> Option<std::path::PathBuf> {
    webview::system_app_apk(module, app, config)
        .or_else(|| permissions::system_app_apk(module, app, config))
        .or_else(|| notifications::system_app_apk(module, app, config))
}
