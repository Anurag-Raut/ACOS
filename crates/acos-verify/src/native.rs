//! The native ACOS backend: `acos-apk` for static facts and `acos-runtime` to
//! boot the application process to the M2 attach point.
//!
//! What it does **not** do today (and reports as `unsupported` rather than
//! guessing): UI tree, screenshots, input, state reset, environment knobs and an
//! app log stream. Those arrive with M3; switching them on here is the only
//! change the scenario schema allows.

use std::path::{Path, PathBuf};

use acos_protocol::commands::ResetScope;
use acos_protocol::{Backend as ProtocolBackend, ErrorCode, Fidelity};
use acos_runtime::{BootError, Runtime};
use acos_vm::{CoreLib, VmError, VmOptions};

use crate::backend::{
    Backend, BackendError, Capabilities, EnvironmentChange, InputAction, LaunchOutcome,
    LaunchRequest, LogRecord, ManifestInfo, SessionObservation,
};
use crate::manifest::load_manifest;

/// A backend that executes the real ACOS runtime.
#[derive(Default)]
pub struct NativeBackend {
    runtime: Option<Runtime>,
    manifest: Option<ManifestInfo>,
    session_seq: u64,
    manifest_calls: u64,
}

impl std::fmt::Debug for NativeBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeBackend")
            .field("has_runtime", &self.runtime.is_some())
            .field("package", &self.manifest.as_ref().map(|m| &m.package))
            .finish()
    }
}

impl NativeBackend {
    /// A fresh native backend with no session.
    pub fn new() -> Self {
        NativeBackend::default()
    }
}

impl Backend for NativeBackend {
    fn id(&self) -> &'static str {
        "acos-native"
    }

    fn protocol_backend(&self) -> ProtocolBackend {
        ProtocolBackend::AcosNative
    }

    fn fidelity(&self) -> Fidelity {
        Fidelity::NativeRuntime
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            static_manifest: true,
            launch: true,
            ui_tree: false,
            logs: false,
            screenshot: false,
            input: false,
            reset: false,
            environment: false,
            crash_reports: false,
        }
    }

    fn manifest(&mut self, apk: &Path) -> Result<ManifestInfo, BackendError> {
        self.manifest_calls += 1;
        let info = load_manifest(apk)?;
        self.manifest = Some(info.clone());
        Ok(info)
    }

    fn launch(&mut self, request: &LaunchRequest) -> Result<LaunchOutcome, BackendError> {
        let package = self
            .manifest
            .as_ref()
            .map(|m| m.package.clone())
            .unwrap_or_default();
        let boot_dir = std::env::var_os("ACOS_BOOT_DIR").map(PathBuf::from);
        let options = VmOptions {
            corelib: CoreLib::Real,
            boot_dir,
            ..VmOptions::default()
        };
        let mut runtime = match Runtime::standard_with(options) {
            Ok(runtime) => runtime,
            Err(VmError::BootClasspathMissing(detail)) => {
                return Err(BackendError::unsupported(
                    "lifecycle",
                    format!("real boot classpath unavailable: {detail}"),
                ));
            }
            Err(error) => {
                let code = ErrorCode::from_str_opt(error.code()).unwrap_or(ErrorCode::Internal);
                return Err(BackendError::Error {
                    code,
                    detail: error.to_string(),
                });
            }
        };

        let report = match runtime.boot_app_process(&request.apk) {
            Ok(report) => report,
            Err(BootError::Stage { stage, source }) => {
                return Err(BackendError::unsupported(
                    "lifecycle",
                    format!("boot stopped at stage `{stage}`: {source}"),
                ));
            }
            Err(error) => {
                return Err(BackendError::Error {
                    code: ErrorCode::AppCrashed,
                    detail: error.to_string(),
                });
            }
        };

        let detail = serde_json::json!({
            "stages": report.stages,
            "classes_loaded": report.classes_loaded,
            "boot_classpath_classes": report.boot_classpath_classes,
            "attach_binder": report.attach.app_thread.map(|binder| binder.0),
            "attach_start_seq": report.attach.start_seq,
            "elapsed_ms": report.elapsed.as_millis() as u64,
            "fidelity_note": "M2 attach handshake only: onCreate/lifecycle and UI tree are not available yet",
        });

        self.session_seq += 1;
        self.runtime = Some(runtime);
        Ok(LaunchOutcome {
            session_id: format!("s_native_{}", self.session_seq),
            package,
            frame: None,
            screenshot_png: None,
            crashed: None,
            detail,
        })
    }

    // Capabilities that are off return the trait default `unsupported`; these
    // overrides exist to make the exact missing-capability message precise.
    fn observe(&mut self) -> Result<SessionObservation, BackendError> {
        Err(BackendError::unsupported(
            "ui.tree",
            "the native runtime does not expose a UI tree yet (M3)",
        ))
    }

    fn logs_since(&mut self, _since: u64) -> Result<Vec<LogRecord>, BackendError> {
        Err(BackendError::unsupported(
            "logs",
            "the native runtime does not expose an app log stream yet",
        ))
    }

    fn input(&mut self, action: &InputAction) -> Result<SessionObservation, BackendError> {
        Err(BackendError::unsupported(
            "input",
            format!("the native runtime cannot deliver `{}` yet (M3)", action.kind()),
        ))
    }

    fn reset(&mut self, _scope: ResetScope) -> Result<(), BackendError> {
        Err(BackendError::unsupported(
            "reset",
            "the native runtime does not implement session reset yet",
        ))
    }

    fn configure(&mut self, change: &EnvironmentChange) -> Result<(), BackendError> {
        Err(BackendError::unsupported(
            "environment",
            format!("the native runtime cannot apply `{}` yet", change.kind()),
        ))
    }

    fn capture(&mut self) -> Result<SessionObservation, BackendError> {
        Err(BackendError::unsupported(
            "screenshot",
            "the native runtime does not render frames yet (M3)",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_capabilities_are_honest() {
        let backend = NativeBackend::new();
        let caps = backend.capabilities();
        assert!(caps.static_manifest);
        assert!(caps.launch);
        assert!(!caps.ui_tree);
        assert!(!caps.input);
        assert!(!caps.screenshot);
        assert_eq!(backend.fidelity(), Fidelity::NativeRuntime);
    }

    #[test]
    fn unimplemented_observation_is_unsupported() {
        let mut backend = NativeBackend::new();
        let error = backend.observe().unwrap_err();
        assert_eq!(error.code(), ErrorCode::UnsupportedBackend);
        assert!(error.to_protocol_error().details.unwrap()["capability"]
            .as_str()
            .unwrap()
            .contains("ui.tree"));
    }
}
