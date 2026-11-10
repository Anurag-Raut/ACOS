//! Frame metadata: the evidence identity that makes stale screenshots
//! impossible to confuse with fresh ones.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Metadata for one produced frame.
///
/// A frame answers a *cause* (the request/action id that produced it) and has a
/// monotonically increasing per-session `frame_seq`. `content_hash` is the
/// SHA-256 of the encoded pixels. `meaningful` is false for blank/placeholder
/// frames; a placeholder must never satisfy a UI assertion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FrameMeta {
    /// Monotonic per-session frame sequence number.
    pub frame_seq: u64,
    /// RFC 3339 UTC timestamp when the frame was produced by ACOS.
    pub produced_at: String,
    /// The request id that caused this frame (for example `req_19`).
    pub cause_id: String,
    /// Frame width in pixels.
    pub width: u32,
    /// Frame height in pixels.
    pub height: u32,
    /// Display density in dots per inch.
    pub density: u32,
    /// SHA-256 of the encoded frame pixels (lowercase hex).
    pub content_hash: String,
    /// True only when the frame reflects real rendered UI content.
    pub meaningful: bool,
}

impl FrameMeta {
    /// Build frame metadata.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        frame_seq: u64,
        produced_at: impl Into<String>,
        cause_id: impl Into<String>,
        width: u32,
        height: u32,
        density: u32,
        content_hash: impl Into<String>,
        meaningful: bool,
    ) -> Self {
        FrameMeta {
            frame_seq,
            produced_at: produced_at.into(),
            cause_id: cause_id.into(),
            width,
            height,
            density,
            content_hash: content_hash.into(),
            meaningful,
        }
    }

    /// True when this frame was produced strictly after `after_seq`.
    ///
    /// A capture request with `after_seq = N` may only accept a frame whose
    /// `frame_seq > N`. This is the stale-frame rule.
    pub const fn is_fresh_after(&self, after_seq: u64) -> bool {
        self.frame_seq > after_seq
    }

    /// A stable, agent-readable label (`frame 19 at req_19`).
    pub fn label(&self) -> String {
        format!("frame {} at {}", self.frame_seq, self.cause_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn freshness_is_strict() {
        let f = FrameMeta::new(
            19,
            "2026-11-10T13:51:14Z",
            "req_19",
            1080,
            2400,
            420,
            "abc",
            true,
        );
        assert!(f.is_fresh_after(18));
        assert!(!f.is_fresh_after(19));
        assert!(!f.is_fresh_after(20));
        assert_eq!(f.label(), "frame 19 at req_19");
    }
}
