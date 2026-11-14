//! A deterministic fake session backend for the engine tests.
//!
//! It is deliberately not a renderer: it models the *shape* of a counter app
//! (launch -> tree with `count=0` -> tap Increment -> `count=1` -> reset ->
//! `count=0`) so the engine plumbing, the verdict, the capability reporting and
//! the mode rule can be tested without an ART sysroot. It is compiled only for
//! tests, so it can never be selected by `acos verify`.
//!
//! It models the protocol's frame rules too: every command publishes a **new**
//! frame, so `frame_seq` advances on each reply. The failure modes
//! ([`FakeSession::stale_tap`], [`FakeSession::stale_capture`],
//! [`FakeSession::no_live_frame`]) deliberately break one rule each, so the
//! engine tests can prove the error is reported instead of passed.

use std::path::Path;
use std::sync::{Arc, Mutex};

use acos_protocol::ui::{Bounds, UiNode, UiTree};
use acos_protocol::{FrameMeta, Selector};
use acos_verify::{BackendError, LaunchOutcome};

use super::{
    live_frame_unavailable, Engine, FrameEvidence, Mode, ObservedFrame, SessionBackend,
    SessionCapabilities,
};

/// A 1x1 PNG, so capture evidence is a real image file.
pub const TINY_PNG: &[u8] = &[
    137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 2, 0,
    0, 0, 144, 119, 83, 222, 0, 0, 0, 12, 73, 68, 65, 84, 120, 156, 99, 248, 255, 255, 63, 0, 5,
    254, 2, 254, 13, 239, 70, 184, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
];

/// Every command the fake received, for assertions about routing.
pub type Calls = Arc<Mutex<Vec<String>>>;

/// How the fake misbehaves, if it does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fault {
    None,
    /// A tap returns without publishing a new frame (a dropped/unhandled tap
    /// that a careless backend could report as a successful action).
    StaleTap,
    /// `capture` returns the previous frame instead of a new one.
    StaleCapture,
    /// No frame is ever produced (a session that cannot show anything).
    NoLiveFrame,
}

/// The fake counter session.
pub struct FakeSession {
    mode: Mode,
    fault: Fault,
    count: u32,
    seq: u64,
    launched: bool,
    calls: Calls,
    /// When false, `type_text` reports the same gap the real ART protocol has.
    supports_type: bool,
    /// P12b: the last tap's strict settle timed out. The frame is still
    /// published (it is fresh; it just never settled) so the engine must
    /// refuse the step and keep the frame as evidence.
    settle_timed_out: bool,
    /// P12b: settle evidence attached to input delivery (including
    /// `pixels_changed:false` for a no-visual-change tap).
    settle_json: Option<serde_json::Value>,
    /// The bound frame of the most recent completed command.
    cached: Option<ObservedFrame>,
}

impl FakeSession {
    /// A live fake (real-Activity semantics, so `mode: live`).
    pub fn live() -> Self {
        Self::with_mode(Mode::Live, Fault::None)
    }

    /// A preview fake: renders offline, so it can never pass.
    pub fn preview() -> Self {
        Self::with_mode(Mode::Preview, Fault::None)
    }

    /// A live fake that also accepts `type_text` (models an engine with the
    /// full input path, for engine-side `clear` handling).
    pub fn live_with_type() -> Self {
        let mut fake = Self::with_mode(Mode::Live, Fault::None);
        fake.supports_type = true;
        fake
    }

    /// A live fake whose tap never completes a new frame.
    pub fn stale_tap() -> Self {
        Self::with_mode(Mode::Live, Fault::StaleTap)
    }

    /// A live fake whose `capture` reuses the previous frame.
    pub fn stale_capture() -> Self {
        Self::with_mode(Mode::Live, Fault::StaleCapture)
    }

    /// A live session that never produces a frame at all.
    pub fn no_live_frame() -> Self {
        Self::with_mode(Mode::Live, Fault::NoLiveFrame)
    }

    /// A live session whose next tap publishes a fresh frame but reports a
    /// strict settle timeout (the P12b timeout shape).
    pub fn timed_out_tap() -> Self {
        let mut fake = Self::with_mode(Mode::Live, Fault::None);
        fake.settle_timed_out = true;
        fake
    }

    /// A live session whose tap settles after the action draw but whose pixels
    /// are identical to the pre-action capture (`pixels_changed:false`).
    pub fn settled_no_pixel_change() -> Self {
        let mut fake = Self::with_mode(Mode::Live, Fault::None);
        fake.settle_json = Some(serde_json::json!({
            "mode": "strict",
            "settled": true,
            "bound": "strict-event",
            "strict_rule": "event",
            "pixels_changed": false,
            "reason": null,
            "animating": false
        }));
        fake
    }

    fn with_mode(mode: Mode, fault: Fault) -> Self {
        FakeSession {
            mode,
            fault,
            count: 0,
            seq: 0,
            launched: false,
            calls: Arc::new(Mutex::new(Vec::new())),
            supports_type: false,
            settle_timed_out: false,
            settle_json: None,
            cached: None,
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
            rfc3339(now_ms()),
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

    fn evidence(&self, previous: u64) -> FrameEvidence {
        let captured_at_ms = now_ms();
        FrameEvidence {
            frame_seq: self.seq,
            previous_seq: previous,
            captured_at_ms,
            captured_at: rfc3339(captured_at_ms),
            md5: format!("fake-{}-{}", self.count, self.seq),
            png: self.png(),
        }
    }

    fn observation(&self, previous: u64) -> ObservedFrame {
        ObservedFrame {
            tree: self.build_tree(),
            frame: self.evidence(previous),
        }
    }

    /// Publish a frame: the sequence number and the pixels are assigned together.
    fn publish(&mut self, previous: u64) {
        self.seq += 1;
        self.cached = Some(self.observation(previous));
    }

    fn bump(&mut self) {
        self.seq += 1;
    }

    fn tap_selector(&mut self, selector: &Selector) -> Result<(), BackendError> {
        let tree = self.build_tree();
        let node = tree.find_one(selector).ok_or_else(|| BackendError::Failed {
            code: acos_protocol::ErrorCode::AssertionFailed,
            detail: format!("fake tree has no node {selector}"),
        })?;
        let previous = self.seq;
        match node.resource_id.as_str() {
            id if id.ends_with(":id/inc") => {
                self.count += 1;
                self.publish(previous);
                Ok(())
            }
            id if id.ends_with(":id/reset") => {
                self.count = 0;
                self.publish(previous);
                Ok(())
            }
            other => Err(BackendError::Failed {
                code: acos_protocol::ErrorCode::AssertionFailed,
                detail: format!("fake node {other:?} has no click listener"),
            }),
        }
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn rfc3339(unix_ms: u64) -> String {
    acos_verify::clock::rfc3339_from_unix_ms(unix_ms as i64)
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
            // The fake session has no display-rotation command.
            rotate: false,
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
        if self.fault == Fault::NoLiveFrame {
            return Ok(LaunchOutcome {
                session_id: "s_fake".to_string(),
                package: "dev.acos.fixtures.counter".to_string(),
                frame: None,
                screenshot_png: None,
                crashed: None,
                detail: serde_json::json!({"engine": "fake", "mode": self.mode.as_str()}),
            });
        }
        let previous = self.seq;
        self.publish(previous);
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
        self.frame().map(|frame| frame.tree)
    }

    fn frame(&mut self) -> Result<ObservedFrame, BackendError> {
        self.record("frame");
        if self.fault == Fault::NoLiveFrame {
            return Err(live_frame_unavailable(
                "the fake session never completed a frame",
            ));
        }
        let previous = self.seq;
        self.publish(previous);
        Ok(self.cached.clone().expect("just published"))
    }

    fn capture(&mut self) -> Result<ObservedFrame, BackendError> {
        self.record("capture");
        if self.fault == Fault::StaleCapture {
            // Reuse the previous frame: the engine must detect the missing
            // sequence advance instead of reporting a fresh capture.
            return self.cached.clone().ok_or_else(|| {
                live_frame_unavailable("the fake session has no frame to reuse")
            });
        }
        self.frame()
    }

    fn screenshot(&mut self) -> Result<FrameEvidence, BackendError> {
        self.record("screenshot");
        if self.fault == Fault::NoLiveFrame {
            return Err(live_frame_unavailable(
                "the fake session never completed a frame",
            ));
        }
        let previous = self.seq;
        self.bump();
        Ok(self.evidence(previous))
    }

    fn last_frame(&self) -> Option<ObservedFrame> {
        self.cached.clone()
    }

    fn last_frame_seq(&self) -> u64 {
        self.seq
    }

    fn tap(&mut self, x: i32, y: i32) -> Result<(), BackendError> {
        self.record(&format!("tap({x},{y})"));
        if self.fault == Fault::StaleTap {
            // The command is accepted but completes no new frame.
            return Ok(());
        }
        let tree = self.build_tree();
        let node = tree.all_nodes().into_iter().find(|node| {
            node.clickable
                && node.bounds.left() <= x
                && x <= node.bounds.right()
                && node.bounds.top() <= y
                && y <= node.bounds.bottom()
        });
        let previous = self.seq;
        match node.map(|node| node.resource_id.clone()) {
            Some(id) if id.ends_with(":id/inc") => {
                self.count += 1;
                self.publish(previous);
                // The tap's next frame is published either way; the flag only
                // changes what the settle block says.
                Ok(())
            }
            Some(id) if id.ends_with(":id/reset") => {
                self.count = 0;
                self.publish(previous);
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
        if self.fault == Fault::StaleTap {
            // Same dropped tap as `tap`: accepted, but no new frame.
            return Ok(());
        }
        self.tap_selector(&Selector::by_text(text))
    }

    fn type_text(&mut self, text: &str) -> Result<(), BackendError> {
        if self.supports_type {
            self.record(&format!("type_text({text})"));
            let previous = self.seq;
            self.publish(previous);
            return Ok(());
        }
        self.record("type_text-unsupported");
        Err(BackendError::unsupported(
            "input.type_text",
            "the fake models the ART protocol's missing `type_text` command",
        ))
    }

    fn key(&mut self, key: &str) -> Result<(), BackendError> {
        self.record(&format!("key({key})"));
        let previous = self.seq;
        self.publish(previous);
        Ok(())
    }

    fn reset(&mut self) -> Result<(), BackendError> {
        self.record("reset");
        self.count = 0;
        self.settle_timed_out = false;
        let previous = self.seq;
        self.publish(previous);
        Ok(())
    }

    fn close(&mut self) -> Result<(), BackendError> {
        self.record("close");
        Ok(())
    }

    fn teardown(&mut self) -> Result<super::TeardownReport, BackendError> {
        self.record("teardown");
        self.close()?;
        Ok(super::TeardownReport {
            process_group: None,
            survivors: Vec::new(),
            verified: true,
        })
    }

    fn last_input_delivery(&self) -> Option<acos_verify::backend::InputDelivery> {
        self.settle_json.as_ref().map(|settle| acos_verify::backend::InputDelivery {
            delivered: true,
            visual_change: false,
            acks: Vec::new(),
            settle: Some(settle.clone()),
        })
    }

    fn input_settle_timeout(&self) -> Option<serde_json::Value> {
        self.settle_timed_out.then(|| {
            serde_json::json!({
                "mode": "strict",
                "settled": false,
                "bound": "timeout",
                "reason": "no-draw-after-action",
                "animating": false,
                "pixels_changed": false,
                "probe_seq": self.seq,
                "tree_age_ms": 0
            })
        })
    }
}
