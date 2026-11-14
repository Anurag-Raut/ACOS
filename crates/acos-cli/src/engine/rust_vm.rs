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

use super::{
    stale_frame, Engine, FrameEvidence, Mode, ObservedFrame, SessionBackend, SessionCapabilities,
};

/// The Rust VM (native runtime) behind the session protocol.
///
/// `acos verify --engine rust-vm` forwards to `NativeBackend` directly to keep
/// behaviour identical; this adapter is the seam implementation for tools and
/// tests that need the VM through the same command surface as ART.
#[allow(dead_code)]
pub struct RustVmSession {
    backend: Box<dyn Backend>,
    package: String,
    cached: Option<ObservedFrame>,
    frame_seq: u64,
}

#[allow(dead_code)]
impl RustVmSession {
    /// Wrap an existing verifier backend (the `acos-native` route).
    pub fn new(backend: Box<dyn Backend>) -> Self {
        RustVmSession {
            backend,
            package: String::new(),
            cached: None,
            frame_seq: 0,
        }
    }

    fn observation(&mut self, observation: SessionObservation) -> SessionObservation {
        self.frame_seq = self.frame_seq.max(observation.frame.frame_seq);
        observation
    }

    fn observed(
        &mut self,
        observation: SessionObservation,
    ) -> Result<ObservedFrame, BackendError> {
        let png = observation
            .screenshot_png
            .clone()
            .filter(|png| !png.is_empty())
            .ok_or_else(|| {
                super::live_frame_unavailable("the VM published no frame pixels for this tree")
            })?;
        let previous = self.frame_seq;
        let tree = observation.tree.clone();
        let captured_at_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let frame = FrameEvidence {
            frame_seq: observation.frame.frame_seq,
            previous_seq: previous,
            captured_at_ms,
            captured_at: acos_verify::clock::rfc3339_from_unix_ms(captured_at_ms as i64),
            md5: observation.frame.content_hash.clone(),
            png,
        };
        let observed = ObservedFrame { tree, frame };
        self.cached = Some(observed.clone());
        self.frame_seq = self.frame_seq.max(observed.frame_seq());
        Ok(observed)
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
            // The frozen Rust VM keeps its own relaunch path
            // (`SessionBackend::relaunch` on the VM adapter); this session
            // seam has no rotation command, so it must not claim one.
            rotate: false,
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
        self.frame_seq = outcome.frame.as_ref().map(|f| f.frame_seq).unwrap_or(0);
        if let (Some(frame), Some(png)) = (&outcome.frame, &outcome.screenshot_png) {
            let captured_at_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            self.cached = Some(ObservedFrame {
                tree: acos_protocol::UiTree::new(
                    frame.clone(),
                    self.package.clone(),
                    Vec::new(),
                ),
                frame: FrameEvidence {
                    frame_seq: frame.frame_seq,
                    previous_seq: 0,
                    captured_at_ms,
                    captured_at: acos_verify::clock::rfc3339_from_unix_ms(captured_at_ms as i64),
                    md5: frame.content_hash.clone(),
                    png: png.clone(),
                },
            });
        }
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
        let previous = self.frame_seq;
        let observation = self.backend.capture()?;
        let observed = self.observed(observation)?;
        if observed.frame_seq() <= previous {
            return Err(stale_frame(previous, observed.frame_seq(), "capture"));
        }
        Ok(observed)
    }

    fn last_frame(&self) -> Option<ObservedFrame> {
        self.cached.clone()
    }

    fn last_frame_seq(&self) -> u64 {
        self.frame_seq
    }

    fn screenshot(&mut self) -> Result<FrameEvidence, BackendError> {
        // A fresh capture, not the last cached PNG.
        Ok(self.capture()?.frame)
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
