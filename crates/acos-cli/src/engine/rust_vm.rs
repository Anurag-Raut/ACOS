//! The existing Rust VM route as a [`SessionBackend`].
//!
//! `acos verify --engine rust-vm` forwards straight to the verifier's
//! `NativeBackend` (so default behaviour does not move); this adapter exposes the
//! same route through the session seam, so an engine can be swapped without
//! changing the scenario schema. Everything it cannot do is reported by name.

use std::path::Path;

use acos_protocol::commands::{KeyName, ResetScope};
use acos_protocol::UiTree;
use acos_verify::backend::LaunchRequest;
use acos_verify::{
    Backend, BackendError, InputAction, LaunchOutcome, SessionObservation,
};

use super::{Engine, Mode, ObservedFrame, SessionBackend, SessionCapabilities};

/// The Rust VM (native runtime) behind the session protocol.
///
/// `acos verify --engine rust-vm` forwards to `NativeBackend` directly to keep
/// behaviour identical; this adapter is the seam implementation for tools and
/// tests that need the VM through the same command surface as ART.
#[allow(dead_code)]
pub struct RustVmSession {
    backend: Box<dyn Backend>,
    package: String,
    cached_png: Option<Vec<u8>>,
    frame_seq: u64,
}

#[allow(dead_code)]
impl RustVmSession {
    /// Wrap an existing verifier backend (the `acos-native` route).
    pub fn new(backend: Box<dyn Backend>) -> Self {
        RustVmSession {
            backend,
            package: String::new(),
            cached_png: None,
            frame_seq: 0,
        }
    }

    fn observation(&mut self, observation: SessionObservation) -> SessionObservation {
        self.cached_png = observation.screenshot_png.clone();
        self.frame_seq = observation.frame.frame_seq;
        observation
    }

    fn observed(
        &mut self,
        observation: SessionObservation,
    ) -> Result<ObservedFrame, BackendError> {
        let png = observation
            .screenshot_png
            .clone()
            .ok_or_else(|| BackendError::unsupported("screenshot", "the VM published no frame"))?;
        let tree = observation.tree.clone();
        self.observation(observation);
        Ok(ObservedFrame { tree, png })
    }

    fn unsupported<T>(&self, capability: &str, detail: &str) -> Result<T, BackendError> {
        Err(BackendError::unsupported(capability, detail))
    }
}

impl SessionBackend for RustVmSession {
    fn engine(&self) -> Engine {
        Engine::RustVm
    }

    fn mode(&self) -> Mode {
        Mode::Live
    }

    fn capabilities(&self) -> SessionCapabilities {
        let caps = self.backend.capabilities();
        SessionCapabilities {
            launch: caps.launch,
            tree: caps.ui_tree,
            screenshot: caps.screenshot,
            tap: caps.input,
            tap_text: caps.input,
            type_text: caps.input,
            key: caps.input,
            reset: caps.reset,
            close: true,
            logs: caps.logs,
            environment: caps.environment,
            crash_reports: caps.crash_reports,
        }
    }

    fn launch(&mut self, apk: &Path) -> Result<LaunchOutcome, BackendError> {
        let manifest = self.backend.manifest(apk)?;
        self.package = manifest.package.clone();
        let activity = manifest.launcher_activity.clone();
        let request = LaunchRequest {
            apk: apk.to_path_buf(),
            activity,
            intent_action: None,
            data: None,
            extras: Default::default(),
            clear_state: true,
            isolation: acos_protocol::identity::IsolationLevel::Process,
            display: acos_protocol::identity::DisplaySpec::offscreen(1080, 2400, 420),
            cause_id: "launch".to_string(),
        };
        let outcome = self.backend.launch(&request)?;
        if !outcome.package.is_empty() {
            self.package = outcome.package.clone();
        }
        self.cached_png = outcome.screenshot_png.clone();
        self.frame_seq = outcome.frame.as_ref().map(|f| f.frame_seq).unwrap_or(0);
        Ok(outcome)
    }

    fn tree(&mut self) -> Result<UiTree, BackendError> {
        Ok(self.frame()?.tree)
    }

    fn frame(&mut self) -> Result<ObservedFrame, BackendError> {
        let observation = self.backend.observe()?;
        self.observed(observation)
    }

    fn capture(&mut self) -> Result<ObservedFrame, BackendError> {
        let observation = self.backend.capture()?;
        self.observed(observation)
    }

    fn screenshot(&mut self) -> Result<Vec<u8>, BackendError> {
        self.cached_png
            .clone()
            .ok_or_else(|| BackendError::unsupported("screenshot", "the VM has published no frame"))
    }

    fn tap(&mut self, x: i32, y: i32) -> Result<(), BackendError> {
        let observation = self.backend.input(&InputAction::Tap {
            selector: None,
            x: Some(x),
            y: Some(y),
        })?;
        self.observation(observation);
        Ok(())
    }

    fn tap_text(&mut self, text: &str) -> Result<(), BackendError> {
        let observation = self.backend.input(&InputAction::Tap {
            selector: Some(acos_protocol::Selector::by_text(text)),
            x: None,
            y: None,
        })?;
        self.observation(observation);
        Ok(())
    }

    fn type_text(&mut self, text: &str) -> Result<(), BackendError> {
        let observation = self.backend.input(&InputAction::Type {
            selector: None,
            text: text.to_string(),
            clear: false,
        })?;
        self.observation(observation);
        Ok(())
    }

    fn key(&mut self, key: &str) -> Result<(), BackendError> {
        let key = match key {
            "back" => KeyName::Back,
            "home" => KeyName::Home,
            "enter" => KeyName::Enter,
            "tab" => KeyName::Tab,
            "delete" => KeyName::Delete,
            "escape" => KeyName::Escape,
            "volume_up" => KeyName::VolumeUp,
            "volume_down" => KeyName::VolumeDown,
            "power" => KeyName::Power,
            "menu" => KeyName::Menu,
            "dpad_up" => KeyName::DpadUp,
            "dpad_down" => KeyName::DpadDown,
            "dpad_left" => KeyName::DpadLeft,
            "dpad_right" => KeyName::DpadRight,
            _ => return self.unsupported("input.key", &format!("unknown key {key:?}")),
        };
        let observation = self.backend.input(&InputAction::Key(key))?;
        self.observation(observation);
        Ok(())
    }

    fn reset(&mut self) -> Result<(), BackendError> {
        self.backend.reset(ResetScope::App)
    }

    fn close(&mut self) -> Result<(), BackendError> {
        Ok(())
    }
}
