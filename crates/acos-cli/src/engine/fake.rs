//! A deterministic fake session backend for the engine tests.
//!
//! It is deliberately not a renderer: it models the *shape* of a counter app
//! (launch -> tree with `count=0` -> tap Increment -> `count=1` -> reset ->
//! `count=0`) so the engine plumbing, the verdict, the capability reporting and
//! the mode rule can be tested without an ART sysroot. It is compiled only for
//! tests, so it can never be selected by `acos verify`.

use std::path::Path;
use std::sync::{Arc, Mutex};

use acos_protocol::ui::{Bounds, UiNode, UiTree};
use acos_protocol::{FrameMeta, Selector};
use acos_verify::{BackendError, LaunchOutcome};

use super::{Engine, Mode, ObservedFrame, SessionBackend, SessionCapabilities};

/// A 1x1 PNG, so capture evidence is a real image file.
pub const TINY_PNG: &[u8] = &[
    137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 2, 0,
    0, 0, 144, 119, 83, 222, 0, 0, 0, 12, 73, 68, 65, 84, 120, 156, 99, 248, 255, 255, 63, 0, 5,
    254, 2, 254, 13, 239, 70, 184, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
];

/// Every command the fake received, for assertions about routing.
pub type Calls = Arc<Mutex<Vec<String>>>;

/// The fake counter session.
pub struct FakeSession {
    mode: Mode,
    count: u32,
    seq: u64,
    launched: bool,
    calls: Calls,
    /// When false, `type_text` reports the same gap the real ART protocol has.
    supports_type: bool,
}

impl FakeSession {
    /// A live fake (real-Activity semantics, so `mode: live`).
    pub fn live() -> Self {
        Self::with_mode(Mode::Live)
    }

    /// A preview fake: renders offline, so it can never pass.
    pub fn preview() -> Self {
        Self::with_mode(Mode::Preview)
    }

    fn with_mode(mode: Mode) -> Self {
        FakeSession {
            mode,
            count: 0,
            seq: 0,
            launched: false,
            calls: Arc::new(Mutex::new(Vec::new())),
            supports_type: false,
        }
    }

    /// The command log (cloneable handle).
    pub fn calls(&self) -> Calls {
        Arc::clone(&self.calls)
    }

    fn record(&self, cmd: &str) {
        self.calls.lock().unwrap().push(cmd.to_string());
    }

    fn frame_meta(&self) -> FrameMeta {
        FrameMeta::new(
            self.seq,
            "2026-11-11T00:00:00Z",
            "fake",
            1080,
            2400,
            420,
            format!("fake-{}-{}", self.count, self.seq),
            true,
        )
    }

    fn build_tree(&self) -> UiTree {
        let count = UiNode::new(
            "n1",
            "android.widget.TextView",
            Bounds::new(42, 317, 63, 368),
        )
        .with_resource_id("dev.acos.fixtures.counter:id/count")
        .with_text(self.count.to_string());
        let inc = UiNode::new(
            "n2",
            "android.widget.Button",
            Bounds::new(42, 368, 273, 494),
        )
        .with_resource_id("dev.acos.fixtures.counter:id/inc")
        .with_text("Increment")
        .clickable(true);
        let reset = UiNode::new(
            "n3",
            "android.widget.Button",
            Bounds::new(42, 494, 273, 620),
        )
        .with_resource_id("dev.acos.fixtures.counter:id/reset")
        .with_text("Reset")
        .clickable(true);
        let root = UiNode::new(
            "n0",
            "android.widget.LinearLayout",
            Bounds::new(0, 0, 1080, 2400),
        )
        .with_children(vec![count, inc, reset]);
        UiTree::new(
            self.frame_meta(),
            "dev.acos.fixtures.counter",
            vec![root],
        )
    }

    /// PNG bytes that change with the visible state, so the advisor's
    /// "tree and screenshot both changed" rule is exercised honestly.
    fn png(&self) -> Vec<u8> {
        let mut bytes = TINY_PNG.to_vec();
        bytes.extend_from_slice(format!("#count={};seq={}", self.count, self.seq).as_bytes());
        bytes
    }

    fn bump(&mut self) {
        self.seq += 1;
    }

    fn tap_selector(&mut self, selector: &Selector) -> Result<(), BackendError> {
        let tree = self.build_tree();
        let node = tree.find_one(selector).ok_or_else(|| BackendError::Failed {
            code: acos_protocol::ErrorCode::AssertionFailed,
            detail: format!("fake tree has no node {selector:?}"),
        })?;
        match node.resource_id.as_str() {
            id if id.ends_with(":id/inc") => {
                self.count += 1;
                self.bump();
                Ok(())
            }
            id if id.ends_with(":id/reset") => {
                self.count = 0;
                self.bump();
                Ok(())
            }
            other => Err(BackendError::Failed {
                code: acos_protocol::ErrorCode::AssertionFailed,
                detail: format!("fake node {other:?} has no click listener"),
            }),
        }
    }
}

impl SessionBackend for FakeSession {
    fn engine(&self) -> Engine {
        Engine::Art
    }

    fn mode(&self) -> Mode {
        self.mode
    }

    fn capabilities(&self) -> SessionCapabilities {
        SessionCapabilities {
            launch: true,
            tree: true,
            screenshot: true,
            tap: true,
            tap_text: true,
            type_text: self.supports_type,
            key: true,
            reset: true,
            close: true,
            logs: false,
            environment: false,
            crash_reports: false,
        }
    }

    fn launch(&mut self, _apk: &Path) -> Result<LaunchOutcome, BackendError> {
        self.record("launch");
        self.launched = true;
        self.count = 0;
        self.bump();
        let frame = self.frame_meta();
        Ok(LaunchOutcome {
            session_id: "s_fake".to_string(),
            package: "dev.acos.fixtures.counter".to_string(),
            frame: Some(frame),
            screenshot_png: Some(self.png()),
            crashed: None,
            detail: serde_json::json!({"engine": "fake", "mode": self.mode.as_str()}),
        })
    }

    fn tree(&mut self) -> Result<UiTree, BackendError> {
        self.record("tree");
        Ok(self.build_tree())
    }

    fn frame(&mut self) -> Result<ObservedFrame, BackendError> {
        self.record("frame");
        Ok(ObservedFrame {
            tree: self.build_tree(),
            png: self.png(),
        })
    }

    fn capture(&mut self) -> Result<ObservedFrame, BackendError> {
        self.record("capture");
        // A fresh capture completes a new frame.
        self.bump();
        self.frame()
    }

    fn screenshot(&mut self) -> Result<Vec<u8>, BackendError> {
        self.record("screenshot");
        Ok(self.png())
    }

    fn tap(&mut self, x: i32, y: i32) -> Result<(), BackendError> {
        self.record(&format!("tap({x},{y})"));
        let tree = self.build_tree();
        let node = tree.all_nodes().into_iter().find(|node| {
            node.clickable
                && node.bounds.left() <= x
                && x <= node.bounds.right()
                && node.bounds.top() <= y
                && y <= node.bounds.bottom()
        });
        match node.map(|node| node.resource_id.clone()) {
            Some(id) if id.ends_with(":id/inc") => {
                self.count += 1;
                self.bump();
                Ok(())
            }
            Some(id) if id.ends_with(":id/reset") => {
                self.count = 0;
                self.bump();
                Ok(())
            }
            _ => Err(BackendError::Failed {
                code: acos_protocol::ErrorCode::AssertionFailed,
                detail: format!("fake: no clickable node at ({x},{y})"),
            }),
        }
    }

    fn tap_text(&mut self, text: &str) -> Result<(), BackendError> {
        self.record(&format!("tap_text({text})"));
        self.tap_selector(&Selector::by_text(text))
    }

    fn type_text(&mut self, _text: &str) -> Result<(), BackendError> {
        Err(BackendError::unsupported(
            "input.type_text",
            "the fake models the ART protocol's missing `type_text` command",
        ))
    }

    fn key(&mut self, key: &str) -> Result<(), BackendError> {
        self.record(&format!("key({key})"));
        self.bump();
        Ok(())
    }

    fn reset(&mut self) -> Result<(), BackendError> {
        self.record("reset");
        self.count = 0;
        self.bump();
        Ok(())
    }

    fn close(&mut self) -> Result<(), BackendError> {
        self.record("close");
        Ok(())
    }
}
