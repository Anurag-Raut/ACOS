//! `svc.vsync`: the DisplayEventReceiver / Choreographer vsync generator.
//!
//! The framework's `Choreographer.scheduleVsync` reaches
//! `DisplayEventReceiver.nativeScheduleVsync`, which asks this generator for the
//! next display deadline. The generator holds **no periodic timer**: a deadline
//! exists only while at least one client has an outstanding request, and it is
//! disarmed as soon as the delivery is made. With no request,
//! [`VsyncGenerator::next_deadline`] returns `None` and the runtime's event loop
//! blocks forever.
//!
//! Determinism: [`VsyncMode::Fixed`] places deadlines on a boot-anchored grid at
//! the display refresh rate (60 Hz on the reference), driven by a [`Clock`], so
//! tests advance virtual time and observe exact `vsync_id`s. [`VsyncMode::Unlimited`]
//! is the "as fast as possible" mode the task allows.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::clock::Clock;
use crate::display::DisplaySpec;

/// A vsync client (one `DisplayEventReceiver`).
pub type ClientId = u64;

/// One frame's timeline entry (`DisplayEventReceiver.VsyncEventData.FrameTimeline`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameTimeline {
    /// The vsync id this timeline predicts.
    pub vsync_id: i64,
    /// Expected present time in nanoseconds.
    pub expected_present_time: i64,
    /// The vsync id expected to be the present deadline.
    pub expected_present_time_vsync_id: i64,
    /// The frame deadline.
    pub deadline: i64,
    /// The frame interval.
    pub frame_interval_nanos: i64,
}

/// `android.view.DisplayEventReceiver$VsyncEventData`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VsyncEventData {
    /// `frameInterval`.
    pub frame_interval_nanos: i64,
    /// `vsyncId`.
    pub vsync_id: i64,
    /// `preferredFrameTimelineIndex`.
    pub preferred_frame_timeline_index: i32,
    /// `frameTimelines`.
    pub frame_timelines: Vec<FrameTimeline>,
}

impl VsyncEventData {
    /// Build a one-entry timeline at `timestamp` (the common case).
    #[must_use]
    pub fn single(timestamp: i64, vsync_id: i64, interval: i64) -> Self {
        Self {
            frame_interval_nanos: interval,
            vsync_id,
            preferred_frame_timeline_index: 0,
            frame_timelines: vec![FrameTimeline {
                vsync_id,
                expected_present_time: timestamp,
                expected_present_time_vsync_id: vsync_id,
                deadline: timestamp,
                frame_interval_nanos: interval,
            }],
        }
    }

    /// The preferred timeline, when present.
    #[must_use]
    pub fn preferred(&self) -> Option<FrameTimeline> {
        let idx = self.preferred_frame_timeline_index.max(0) as usize;
        self.frame_timelines.get(idx).copied()
    }
}

/// One `onVsync` delivery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VsyncDelivery {
    /// Target client.
    pub client: ClientId,
    /// `timestampNanos`.
    pub timestamp_nanos: i64,
    /// `physicalDisplayId`.
    pub physical_display_id: i64,
    /// `frame` (a.k.a. the count).
    pub frame: i32,
    /// `VsyncEventData`.
    pub data: VsyncEventData,
}

impl VsyncDelivery {
    /// The parameter list of `DisplayEventReceiver.dispatchVsync(JJI, VsyncEventData)`.
    ///
    /// Encoded into the receiver's event pipe as:
    /// `i64 timestamp`, `i64 physicalDisplayId`, `i32 frame`,
    /// `i64 frameInterval`, `i64 vsyncId`, `i32 preferredIndex`,
    /// `i32 timelineCount`, then per timeline
    /// `i64 vsyncId, i64 expectedPresentTime, i64 expectedPresentTimeVsyncId,
    /// i64 deadline, i64 frameInterval`.
    #[must_use]
    pub fn encode_pipe_record(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(64);
        out.extend_from_slice(&self.timestamp_nanos.to_le_bytes());
        out.extend_from_slice(&self.physical_display_id.to_le_bytes());
        out.extend_from_slice(&self.frame.to_le_bytes());
        out.extend_from_slice(&self.data.frame_interval_nanos.to_le_bytes());
        out.extend_from_slice(&self.data.vsync_id.to_le_bytes());
        out.extend_from_slice(&self.data.preferred_frame_timeline_index.to_le_bytes());
        out.extend_from_slice(&(self.data.frame_timelines.len() as i32).to_le_bytes());
        for t in &self.data.frame_timelines {
            out.extend_from_slice(&t.vsync_id.to_le_bytes());
            out.extend_from_slice(&t.expected_present_time.to_le_bytes());
            out.extend_from_slice(&t.expected_present_time_vsync_id.to_le_bytes());
            out.extend_from_slice(&t.deadline.to_le_bytes());
            out.extend_from_slice(&t.frame_interval_nanos.to_le_bytes());
        }
        out
    }
}

/// Vsync timing mode.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum VsyncMode {
    /// Deadlines on a boot-anchored grid at `hz` (deterministic).
    Fixed {
        /// Refresh rate in Hz.
        hz: f32,
    },
    /// Deliver at the earliest opportunity (`nativeScheduleVsync` returns
    /// immediately with `now`).
    Unlimited,
}

/// Display events other than vsync.
#[derive(Debug, Clone, PartialEq)]
pub enum DisplayEvent {
    /// The display was connected (`onHotplug`).
    Hotplug {
        /// Display id.
        display_id: i32,
        /// Whether it is now connected.
        connected: bool,
    },
    /// The display mode changed (`onModeChanged`).
    ModeChanged {
        /// Display id.
        display_id: i32,
        /// New refresh rate.
        refresh_hz: f32,
    },
    /// A per-client frame-rate override was applied.
    FrameRateOverride {
        /// Client uid.
        uid: i32,
        /// Override in Hz.
        refresh_hz: f32,
    },
}

/// The vsync generator.
pub struct VsyncGenerator {
    /// Display whose grid we follow.
    pub display: DisplaySpec,
    /// Clock seam (virtual in tests).
    pub clock: Arc<dyn Clock>,
    /// Timing mode.
    pub mode: VsyncMode,
    /// Current refresh rate (VRR query).
    pub refresh_hz: f32,
    /// Boot anchor for the 60 Hz grid.
    pub boot_nanos: i64,
    /// Grid index of the next vsync.
    next_index: i64,
    /// Outstanding requests: client → request time.
    pending: BTreeMap<ClientId, i64>,
    /// The single armed deadline.
    armed: Option<i64>,
    /// Generated deliveries in order.
    deliveries: Vec<VsyncDelivery>,
    /// Display events queue.
    events: Vec<DisplayEvent>,
    /// Total requests received.
    requests: u64,
    /// Monotonic frame counter.
    frame_counter: i32,
}

impl std::fmt::Debug for VsyncGenerator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VsyncGenerator")
            .field("display", &self.display.id)
            .field("mode", &self.mode)
            .field("refresh_hz", &self.refresh_hz)
            .field("pending", &self.pending.len())
            .field("armed", &self.armed)
            .field("deliveries", &self.deliveries.len())
            .finish()
    }
}

impl VsyncGenerator {
    /// A generator for `display`, anchored at the clock's current time.
    #[must_use]
    pub fn new(display: DisplaySpec, clock: Arc<dyn Clock>) -> Self {
        let boot = clock.now_nanos();
        let refresh_hz = display.refresh_hz;
        Self {
            display,
            clock,
            mode: VsyncMode::Fixed { hz: refresh_hz },
            refresh_hz,
            boot_nanos: boot,
            next_index: 0,
            pending: BTreeMap::new(),
            armed: None,
            deliveries: Vec::new(),
            events: Vec::new(),
            requests: 0,
            frame_counter: 0,
        }
    }

    /// `nativeScheduleVsync(client)`: record the request and arm the deadline if
    /// this is the first outstanding one.
    ///
    /// Returns the deadline the runtime should sleep to (or `None` if unlimited
    /// and a delivery is already due).
    pub fn schedule_vsync(&mut self, client: ClientId) -> Option<i64> {
        self.requests += 1;
        let now = self.clock.now_nanos();
        self.pending.insert(client, now);
        if self.armed.is_none() {
            let deadline = self.next_deadline_after(now);
            self.armed = Some(deadline);
            self.clock.arm_timer(deadline);
        }
        self.armed
    }

    fn next_deadline_after(&mut self, now: i64) -> i64 {
        match self.mode {
            VsyncMode::Unlimited => now,
            VsyncMode::Fixed { .. } => {
                let interval = self.display.frame_interval_nanos;
                // The first vsync is one interval after boot; advance the grid
                // until the deadline is strictly after `now`.
                let mut deadline = self.boot_nanos + (self.next_index + 1) * interval;
                while deadline <= now {
                    self.next_index += 1;
                    deadline = self.boot_nanos + (self.next_index + 1) * interval;
                }
                deadline
            }
        }
    }

    /// The armed deadline, if any (the runtime's `epoll_wait` timeout).
    #[must_use]
    pub fn next_deadline(&self) -> Option<i64> {
        self.armed
    }

    /// Number of outstanding requests.
    #[must_use]
    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }

    /// Advance to `now` and produce every due delivery.
    ///
    /// This is the only place a delivery is generated; it disarms the timer when
    /// nothing remains outstanding.
    pub fn poll(&mut self, now: i64) -> Vec<VsyncDelivery> {
        let Some(deadline) = self.armed else {
            return Vec::new();
        };
        if now < deadline {
            return Vec::new();
        }
        let interval = self.display.frame_interval_nanos;
        let vsync_id = self.next_index.max(0);
        self.next_index += 1;
        let clients: Vec<ClientId> = self.pending.keys().copied().collect();
        let mut out = Vec::with_capacity(clients.len());
        for client in clients {
            self.pending.remove(&client);
            self.frame_counter += 1;
            let data = VsyncEventData::single(deadline, vsync_id, interval);
            let d = VsyncDelivery {
                client,
                timestamp_nanos: deadline,
                physical_display_id: i64::from(self.display.id) + 1,
                frame: self.frame_counter,
                data,
            };
            self.deliveries.push(d.clone());
            out.push(d);
        }
        // Disarm; re-arm only if a request raced in.
        self.clock.disarm_timer();
        self.armed = None;
        if !self.pending.is_empty() {
            let next = self.next_deadline_after(now);
            self.armed = Some(next);
            self.clock.arm_timer(next);
        }
        out
    }

    /// All deliveries generated so far.
    #[must_use]
    pub fn deliveries(&self) -> &[VsyncDelivery] {
        &self.deliveries
    }

    /// Number of vsync requests received.
    #[must_use]
    pub fn request_count(&self) -> u64 {
        self.requests
    }

    /// `true` when the generator is completely idle (no request, no timer).
    #[must_use]
    pub fn is_idle(&self) -> bool {
        self.pending.is_empty() && self.armed.is_none()
    }

    /// `nativeGetLatestVsyncEventData` for `client`.
    #[must_use]
    pub fn latest_vsync_event_data(&self, _client: ClientId) -> VsyncEventData {
        let interval = self.display.frame_interval_nanos;
        let last = self.deliveries.last();
        VsyncEventData::single(
            last.map_or(0, |d| d.timestamp_nanos),
            last.map_or(0, |d| d.data.vsync_id),
            interval,
        )
    }

    /// The current refresh rate (VRR query).
    #[must_use]
    pub fn refresh_rate(&self) -> f32 {
        self.refresh_hz
    }

    /// Apply a frame-rate override (VRR).
    pub fn set_refresh_rate(&mut self, hz: f32) {
        self.refresh_hz = hz;
        if hz > 0.0 {
            self.display.frame_interval_nanos = (1_000_000_000.0 / f64::from(hz)) as i64;
        }
        self.mode = VsyncMode::Fixed { hz };
    }

    /// Queue a non-vsync display event.
    pub fn push_event(&mut self, event: DisplayEvent) {
        self.events.push(event);
    }

    /// Drain queued display events.
    pub fn take_events(&mut self) -> Vec<DisplayEvent> {
        std::mem::take(&mut self.events)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::VirtualClock;
    use crate::display::DisplaySpec;

    fn gen() -> (Arc<VirtualClock>, VsyncGenerator) {
        let clock = Arc::new(VirtualClock::new());
        let g = VsyncGenerator::new(DisplaySpec::reference_1080x2400(), clock.clone());
        (clock, g)
    }

    #[test]
    fn idle_has_no_timer_and_zero_arms() {
        let (clock, mut g) = gen();
        assert!(g.is_idle());
        assert_eq!(g.next_deadline(), None);
        assert_eq!(clock.arm_count(), 0);
        // Simulate five seconds of idle; nothing happens.
        clock.advance(5_000_000_000);
        assert_eq!(g.poll(clock.now_nanos()).len(), 0);
        assert_eq!(clock.arm_count(), 0);
        assert!(g.is_idle());
    }

    #[test]
    fn fixed_grid_is_deterministic() {
        let (clock, mut g) = gen();
        g.schedule_vsync(1);
        assert_eq!(g.next_deadline(), Some(16_666_667));
        assert!(clock.timer_armed());
        clock.advance(16_666_667);
        let d = g.poll(clock.now_nanos());
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].timestamp_nanos, 16_666_667);
        assert_eq!(d[0].data.vsync_id, 0);
        assert!(g.is_idle());
        assert!(!clock.timer_armed());

        g.schedule_vsync(1);
        assert_eq!(g.next_deadline(), Some(33_333_334));
        clock.advance(16_666_667);
        let d = g.poll(clock.now_nanos());
        assert_eq!(d[0].data.vsync_id, 1);
    }

    #[test]
    fn concurrent_clients_share_one_deadline() {
        let (clock, mut g) = gen();
        g.schedule_vsync(1);
        g.schedule_vsync(2);
        assert_eq!(clock.arm_count(), 1, "one timer for both clients");
        clock.advance(16_666_667);
        let d = g.poll(clock.now_nanos());
        assert_eq!(d.len(), 2);
        assert_eq!(clock.disarm_count(), 1);
    }

    #[test]
    fn pipe_record_layout_is_stable() {
        let d = VsyncDelivery {
            client: 1,
            timestamp_nanos: 123,
            physical_display_id: 1,
            frame: 7,
            data: VsyncEventData::single(123, 4, 16_666_667),
        };
        let bytes = d.encode_pipe_record();
        assert_eq!(i64::from_le_bytes(bytes[0..8].try_into().unwrap()), 123);
        assert_eq!(i64::from_le_bytes(bytes[8..16].try_into().unwrap()), 1);
        assert_eq!(i32::from_le_bytes(bytes[16..20].try_into().unwrap()), 7);
        assert_eq!(i32::from_le_bytes(bytes[40..44].try_into().unwrap()), 1);
        assert_eq!(bytes.len(), 44 + 40);
    }

    #[test]
    fn unlimited_mode_delivers_immediately() {
        let (clock, mut g) = gen();
        g.mode = VsyncMode::Unlimited;
        let now = clock.now_nanos();
        let dl = g.schedule_vsync(9);
        assert_eq!(dl, Some(now));
        let d = g.poll(now);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].timestamp_nanos, now);
    }
}
