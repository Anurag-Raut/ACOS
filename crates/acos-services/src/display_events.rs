//! The frame clock: `android.gui.IDisplayEventConnection` + a request-driven
//! vsync timer (W-R option A, task T2).
//!
//! `Choreographer` is created in `ViewRootImpl`'s constructor and its
//! `FrameDisplayEventReceiver` walks this chain (`native/libs/gui/DisplayEventReceiver.cpp`):
//!
//! ```text
//! DisplayEventReceiver::DisplayEventReceiver
//!   -> ComposerServiceAIDL::getComposerService()          [ServiceManager "SurfaceFlinger"]
//!   -> ISurfaceComposer.createDisplayEventConnection(vsyncSource, eventRegistration, layer)
//!   -> IDisplayEventConnection.stealReceiveChannel(out BitTube)     <- TWO fds
//!   -> (later) setVsyncRate / requestNextVsync / getLatestVsyncEventData
//! ```
//!
//! This module owns the second half of that chain: the connection object, the
//! `BitTube` socketpair, the `DisplayEventReceiver::Event` bytes, and the timer
//! thread that writes them. [`crate::compositor_client`] owns the first half
//! (the `ISurfaceComposer` node).
//!
//! # Wire evidence
//!
//! * Transaction codes: the **image's** `libgui.so`
//!   (`BpDisplayEventConnection::<method>` writes the code as an immediate to
//!   `IBinder::transact`; `.cache/art/sysroot/system/lib64/libgui.so`):
//!   `stealReceiveChannel` = 1, `setVsyncRate` = 2, `requestNextVsync` = 3,
//!   `getLatestVsyncEventData` = 4. The image has **no** `getSchedulingPolicy`
//!   (the `android-14.0.0_r75` tag does), so the tag's AIDL would have been one
//!   method long — see [`codes`].
//! * `stealReceiveChannel`'s request carries **only** the interface token: the
//!   generated proxy writes no placeholder for the `out BitTube` (verified in
//!   the image's disassembly, `0x161270`), and the reply is
//!   `binder::Status` (int32 0) followed by `gui::BitTube::readFromParcel`,
//!   which dups **two** descriptors in order (receive, then send) —
//!   `native/libs/gui/BitTube.cpp::writeToParcel`/`readFromParcel`.
//! * `VsyncSource`/`EventRegistration` are AIDL enums with the default `byte`
//!   backing, but `libbinder`'s `Parcel::writeByte` is `writeInt32(int32_t(val))`
//!   (`native/libs/binder/Parcel.cpp:1395`), so both are 4 bytes on the wire.
//! * `sizeof(DisplayEventReceiver::Event)` in the image is **216** (`0xd8`),
//!   read out of the immediate `BitTube::sendObjects(..., objSize)` in
//!   `DisplayEventReceiver::sendEvents` (`0x118b84`) and `getEvents`
//!   (`0xef9e4`). [`EVENT_SIZE`] pins the same number and [`encode_vsync_event`]
//!   writes exactly the layout that produces it
//!   (`native/libs/gui/include/gui/DisplayEventReceiver.h`).
//!
//! # What is deliberately not real
//!
//! * There is no display hardware: the vsync grid is a 60 Hz timer on
//!   `CLOCK_MONOTONIC`, not a hardware vblank. Timestamps are the **scheduled**
//!   grid times (what a real vblank timestamp is), so the spacing stays exact
//!   even when the thread wakes late.
//! * `frameTimelines` carries **one** entry with a synthesized
//!   `expectedPresentationTime` (`vsync + frameInterval`) and a monotonic
//!   `vsyncId`. `SurfaceFlinger`'s `EventThread` can report up to
//!   `kFrameTimelinesCapacity` (7) timelines; ACOS has no scheduler, so it
//!   reports the single 60 Hz timeline and says so.
//! * No `DISPLAY_EVENT_HOTPLUG` / `MODE_CHANGE` / `FRAME_RATE_OVERRIDE` is ever
//!   sent: nothing in the launch path asks for them (`DisplayEventReceiver`'s
//!   `eventRegistration` is recorded and ignored), and a spurious hotplug would
//!   make the framework tear down and re-create the display.

use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use acos_aidl::MethodInfo;
use acos_parcel::{BinderId, Parcel};
use rustix::net::{RecvFlags, SendFlags, SocketFlags, SocketType};

use crate::error::{Result as SResult, ServiceError};
use crate::exception;
use crate::host::ServiceObject;

/// The `IDisplayEventConnection` transaction codes of the **API-34 image**.
///
/// The image's `libgui.so` has no `getSchedulingPolicy` (`BpDisplayEventConnection`
/// exports only four methods), so this table is the whole interface. A table
/// transcribed from the `android-14.0.0_r75` AIDL would carry a fifth entry.
pub mod codes {
    /// `stealReceiveChannel(out BitTube)`.
    pub const STEAL_RECEIVE_CHANNEL: u32 = 1;
    /// `setVsyncRate(in int count)`.
    pub const SET_VSYNC_RATE: u32 = 2;
    /// `requestNextVsync()` (oneway).
    pub const REQUEST_NEXT_VSYNC: u32 = 3;
    /// `getLatestVsyncEventData()` → `ParcelableVsyncEventData`.
    pub const GET_LATEST_VSYNC_EVENT_DATA: u32 = 4;
}

/// The descriptor every `IDisplayEventConnection` transaction carries.
pub const DESCRIPTOR: &str = "android.gui.IDisplayEventConnection";

/// `fourcc('v','s','y','n')` — `DisplayEventReceiver::DISPLAY_EVENT_VSYNC`.
pub const DISPLAY_EVENT_VSYNC: u32 = 0x7673_796e;
/// `fourcc('p','l','u','g')` — `DISPLAY_EVENT_HOTPLUG` (never sent, see module docs).
pub const DISPLAY_EVENT_HOTPLUG: u32 = 0x706c_7567;
/// `fourcc('m','o','d','e')` — `DISPLAY_EVENT_MODE_CHANGE` (never sent).
pub const DISPLAY_EVENT_MODE_CHANGE: u32 = 0x6d6f_6465;

/// `VsyncEventData::kFrameTimelinesCapacity`.
pub const FRAME_TIMELINES_CAPACITY: usize = 7;

/// `sizeof(DisplayEventReceiver::Event)` in the API-34 image (`0xd8`).
pub const EVENT_SIZE: usize = 216;

/// Offset of `Event::header::type`.
pub const EVENT_OFFSET_TYPE: usize = 0;
/// Offset of `Event::header::displayId` (`aligned(8)`).
pub const EVENT_OFFSET_DISPLAY_ID: usize = 8;
/// Offset of `Event::header::timestamp` (`aligned(8)`).
pub const EVENT_OFFSET_TIMESTAMP: usize = 16;
/// Offset of `Event::vsync::count`.
pub const EVENT_OFFSET_VSYNC_COUNT: usize = 24;
/// Offset of `Event::vsync::vsyncData` (the inline `VsyncEventData`).
pub const EVENT_OFFSET_VSYNC_DATA: usize = 32;

/// The nominal refresh rate ACOS synthesizes (60 Hz).
pub const PERIOD_NS_60HZ: i64 = 16_666_667;

/// One `VsyncEventData::FrameTimeline` (24 bytes, `alignas(8)`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FrameTimeline {
    /// The vsync id the frame should be scheduled with (`setFrameTimelineVsync`).
    pub vsync_id: i64,
    /// `CLOCK_MONOTONIC` ns by which the app must finish the frame.
    pub deadline_timestamp: i64,
    /// The anticipated presentation time of the frame.
    pub expected_presentation_time: i64,
}

/// `android::gui::VsyncEventData` — plain old data, embedded inline in
/// [`DisplayEventReceiver::Event`](EVENT_SIZE) and also written as a parcelable
/// (`ParcelableVsyncEventData::writeToParcel`, `VsyncEventData.cpp`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VsyncEventData {
    /// The current frame interval in ns when this frame was scheduled.
    pub frame_interval: i64,
    /// Index into `frame_timelines` of the platform's preferred timeline.
    pub preferred_frame_timeline_index: u32,
    /// Number of valid entries in `frame_timelines`.
    pub frame_timelines_length: u32,
    /// Up to [`FRAME_TIMELINES_CAPACITY`] timelines, sorted.
    pub frame_timelines: [FrameTimeline; FRAME_TIMELINES_CAPACITY],
}

impl Default for VsyncEventData {
    fn default() -> Self {
        Self {
            frame_interval: 0,
            preferred_frame_timeline_index: 0,
            frame_timelines_length: 0,
            frame_timelines: [FrameTimeline::default(); FRAME_TIMELINES_CAPACITY],
        }
    }
}

impl VsyncEventData {
    /// The `EventThread`-shaped single-timeline payload ACOS synthesizes.
    #[must_use]
    pub fn single_timeline(period_ns: i64, vsync_id: i64, timestamp: i64) -> Self {
        let mut out = Self {
            frame_interval: period_ns,
            preferred_frame_timeline_index: 0,
            frame_timelines_length: 1,
            ..Self::default()
        };
        out.frame_timelines[0] = FrameTimeline {
            vsync_id,
            deadline_timestamp: timestamp,
            expected_presentation_time: timestamp + period_ns,
        };
        out
    }

    /// `ParcelableVsyncEventData::writeToParcel`: `frameInterval` (int64),
    /// `preferredFrameTimelineIndex` (uint32), `frameTimelinesLength` (uint32),
    /// then three int64 per timeline.
    pub fn write_to_parcel(&self, parcel: &mut Parcel) {
        parcel.write_int64(self.frame_interval);
        parcel.write_uint32(self.preferred_frame_timeline_index);
        parcel.write_uint32(self.frame_timelines_length);
        for i in 0..self.frame_timelines_length.min(FRAME_TIMELINES_CAPACITY as u32) as usize {
            let t = &self.frame_timelines[i];
            parcel.write_int64(t.vsync_id);
            parcel.write_int64(t.deadline_timestamp);
            parcel.write_int64(t.expected_presentation_time);
        }
    }

    /// `ParcelableVsyncEventData::readFromParcel`, in the same order.
    pub fn read_from_parcel(parcel: &mut Parcel) -> SResult<Self> {
        let mut out = Self {
            frame_interval: parcel.read_int64().map_err(perr)?,
            preferred_frame_timeline_index: parcel.read_uint32().map_err(perr)?,
            frame_timelines_length: parcel.read_uint32().map_err(perr)?,
            ..Self::default()
        };
        let n = (out.frame_timelines_length as usize).min(FRAME_TIMELINES_CAPACITY);
        for slot in out.frame_timelines.iter_mut().take(n) {
            slot.vsync_id = parcel.read_int64().map_err(perr)?;
            slot.deadline_timestamp = parcel.read_int64().map_err(perr)?;
            slot.expected_presentation_time = parcel.read_int64().map_err(perr)?;
        }
        Ok(out)
    }
}

/// Encode one `DisplayEventReceiver::Event` with `type = DISPLAY_EVENT_VSYNC`.
///
/// Layout (216 bytes, `include/gui/DisplayEventReceiver.h`):
///
/// ```text
///   0  u32  header.type
///   4  --   padding (displayId is aligned(8))
///   8  u64  header.displayId
///  16  i64  header.timestamp
///  24  u32  vsync.count
///  28  --   padding (vsyncData is 8-aligned)
///  32  ...  vsync.vsyncData  (184 bytes: i64, u32, u32, 7 x {i64,i64,i64})
/// 216  end
/// ```
#[must_use]
pub fn encode_vsync_event(
    display_id: u64,
    timestamp_ns: i64,
    count: u32,
    data: &VsyncEventData,
) -> [u8; EVENT_SIZE] {
    let mut buf = [0u8; EVENT_SIZE];
    buf[EVENT_OFFSET_TYPE..EVENT_OFFSET_TYPE + 4].copy_from_slice(&DISPLAY_EVENT_VSYNC.to_ne_bytes());
    buf[EVENT_OFFSET_DISPLAY_ID..EVENT_OFFSET_DISPLAY_ID + 8]
        .copy_from_slice(&display_id.to_ne_bytes());
    buf[EVENT_OFFSET_TIMESTAMP..EVENT_OFFSET_TIMESTAMP + 8]
        .copy_from_slice(&timestamp_ns.to_ne_bytes());
    buf[EVENT_OFFSET_VSYNC_COUNT..EVENT_OFFSET_VSYNC_COUNT + 4].copy_from_slice(&count.to_ne_bytes());

    let mut at = EVENT_OFFSET_VSYNC_DATA;
    let put_i64 = |buf: &mut [u8; EVENT_SIZE], at: &mut usize, v: i64| {
        buf[*at..*at + 8].copy_from_slice(&v.to_ne_bytes());
        *at += 8;
    };
    put_i64(&mut buf, &mut at, data.frame_interval);
    buf[at..at + 4].copy_from_slice(&data.preferred_frame_timeline_index.to_ne_bytes());
    at += 4;
    buf[at..at + 4].copy_from_slice(&data.frame_timelines_length.to_ne_bytes());
    at += 4;
    for t in &data.frame_timelines {
        put_i64(&mut buf, &mut at, t.vsync_id);
        put_i64(&mut buf, &mut at, t.deadline_timestamp);
        put_i64(&mut buf, &mut at, t.expected_presentation_time);
    }
    debug_assert_eq!(at, EVENT_SIZE);
    buf
}

/// Decode an event's header (the parts the tests and the log surface need).
#[must_use]
pub fn decode_event(buf: &[u8; EVENT_SIZE]) -> DecodedEvent {
    let mut vsync_id = [0u8; 8];
    vsync_id.copy_from_slice(&buf[EVENT_OFFSET_VSYNC_DATA + 16..EVENT_OFFSET_VSYNC_DATA + 24]);
    DecodedEvent {
        kind: u32::from_ne_bytes(
            buf[EVENT_OFFSET_TYPE..EVENT_OFFSET_TYPE + 4]
                .try_into()
                .expect("4 bytes"),
        ),
        display_id: u64::from_ne_bytes(
            buf[EVENT_OFFSET_DISPLAY_ID..EVENT_OFFSET_DISPLAY_ID + 8]
                .try_into()
                .expect("8 bytes"),
        ),
        timestamp_ns: i64::from_ne_bytes(
            buf[EVENT_OFFSET_TIMESTAMP..EVENT_OFFSET_TIMESTAMP + 8]
                .try_into()
                .expect("8 bytes"),
        ),
        count: u32::from_ne_bytes(
            buf[EVENT_OFFSET_VSYNC_COUNT..EVENT_OFFSET_VSYNC_COUNT + 4]
                .try_into()
                .expect("4 bytes"),
        ),
        frame_interval: i64::from_ne_bytes(
            buf[EVENT_OFFSET_VSYNC_DATA..EVENT_OFFSET_VSYNC_DATA + 8]
                .try_into()
                .expect("8 bytes"),
        ),
        first_vsync_id: i64::from_ne_bytes(vsync_id),
    }
}

/// The fields of a `DisplayEventReceiver::Event` ACOS writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DecodedEvent {
    /// `header.type` (`DISPLAY_EVENT_VSYNC` for every event ACOS sends).
    pub kind: u32,
    /// `header.displayId`.
    pub display_id: u64,
    /// `header.timestamp` (CLOCK_MONOTONIC ns).
    pub timestamp_ns: i64,
    /// `vsync.count`.
    pub count: u32,
    /// `vsync.vsyncData.frameInterval`.
    pub frame_interval: i64,
    /// `vsync.vsyncData.frameTimelines[0].vsyncId`.
    pub first_vsync_id: i64,
}

/// `CLOCK_MONOTONIC` in nanoseconds — what `systemTime(SYSTEM_TIME_MONOTONIC)`
/// returns and what every `DisplayEventReceiver::Event` timestamp is in.
#[must_use]
pub fn now_ns() -> i64 {
    use rustix::time::{clock_gettime, ClockId};
    let ts = clock_gettime(ClockId::Monotonic);
    ts.tv_sec as i64 * 1_000_000_000 + ts.tv_nsec as i64
}

/// The server end of a `gui::BitTube`: an `AF_UNIX`/`SOCK_SEQPACKET` pair with
/// the receive end handed to the client and the send end kept here.
///
/// `SOCK_SEQPACKET` is not a preference: `BitTube::recvObjects` asserts the read
/// length is a whole multiple of the object size, which only a message-oriented
/// socket guarantees.
#[derive(Debug, Default)]
pub struct BitTube {
    receive: Option<OwnedFd>,
    send: Option<OwnedFd>,
}

impl BitTube {
    /// `socketpair(AF_UNIX, SOCK_SEQPACKET, 0, ...)` with `CLOEXEC`.
    pub fn new() -> std::io::Result<Self> {
        let (a, b) = rustix::net::socketpair(
            rustix::net::AddressFamily::UNIX,
            SocketType::SEQPACKET,
            SocketFlags::CLOEXEC,
            None,
        )?;
        Ok(Self {
            receive: Some(a),
            send: Some(b),
        })
    }

    /// The descriptor the client reads from (the first fd in the reply parcel).
    #[must_use]
    pub fn receive_fd(&self) -> Option<BorrowedFd<'_>> {
        self.receive.as_ref().map(AsFd::as_fd)
    }

    /// The descriptor ACOS writes events into (the second fd in the reply parcel).
    #[must_use]
    pub fn send_fd(&self) -> Option<BorrowedFd<'_>> {
        self.send.as_ref().map(AsFd::as_fd)
    }

    /// `BitTube::writeToParcel`: the receive fd first, then the send fd, both
    /// `writeDupFileDescriptor` (so the client dups two live descriptors).
    ///
    /// Mirrors the stock reset semantics: after a successful steal the server
    /// keeps no receive end.
    pub fn write_to_parcel(&mut self, reply: &mut Parcel) -> SResult<()> {
        let Some(receive) = self.receive.take() else {
            return Err(ServiceError::Internal(
                "IDisplayEventConnection: receive channel already stolen".into(),
            ));
        };
        let send = self.send.as_ref().ok_or_else(|| {
            ServiceError::Internal("IDisplayEventConnection: send channel is gone".into())
        })?;
        reply.write_file_descriptor(receive.as_fd()).map_err(perr)?;
        reply.write_file_descriptor(send.as_fd()).map_err(perr)?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// The request-driven clock
// ---------------------------------------------------------------------------

/// The state a writer thread and the service object share.
struct ClockState {
    tube: BitTube,
    /// `setVsyncRate(count)`: 0 = only on `requestNextVsync`.
    rate: u32,
    /// Outstanding one-shot `requestNextVsync` calls.
    pending: u32,
    /// `vsync.count`, incremented per event.
    count: u32,
    /// The last grid time written (0 = none yet).
    last_vsync_ns: i64,
    /// Events actually written (evidence for the tests and the log).
    sent: u64,
    stopped: bool,
}

struct ClockShared {
    state: Mutex<ClockState>,
    wake: Condvar,
    period_ns: i64,
    display_id: u64,
}

/// One `IDisplayEventConnection`: a `BitTube` plus the thread that fills it.
///
/// The timer is **request-driven** (W-R §4 step 4): it writes an event only
/// when `requestNextVsync` is outstanding or `setVsyncRate` is non-zero. A
/// free-running 60 Hz stream would make `FrameDisplayEventReceiver` re-enter
/// `doFrame` continuously, which is a hot loop rather than a frame clock.
pub struct ConnectionClock {
    shared: Arc<ClockShared>,
    writer: Option<std::thread::JoinHandle<()>>,
}

impl std::fmt::Debug for ConnectionClock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        f.debug_struct("ConnectionClock")
            .field("display_id", &self.shared.display_id)
            .field("period_ns", &self.shared.period_ns)
            .field("rate", &state.rate)
            .field("pending", &state.pending)
            .field("count", &state.count)
            .field("sent", &state.sent)
            .finish()
    }
}

impl ConnectionClock {
    /// Open a connection: one `SOCK_SEQPACKET` pair and one writer thread.
    pub fn new(display_id: u64, period_ns: i64) -> SResult<Self> {
        let tube = BitTube::new().map_err(|e| {
            ServiceError::Internal(format!("IDisplayEventConnection: socketpair: {e}"))
        })?;
        let send = tube
            .send_fd()
            .ok_or_else(|| ServiceError::Internal("bit tube has no send end".into()))?
            .try_clone_to_owned()
            .map_err(|e| ServiceError::Internal(format!("dup send end: {e}")))?;
        let shared = Arc::new(ClockShared {
            state: Mutex::new(ClockState {
                tube,
                rate: 0,
                pending: 0,
                count: 0,
                last_vsync_ns: 0,
                sent: 0,
                stopped: false,
            }),
            wake: Condvar::new(),
            period_ns: period_ns.max(1),
            display_id,
        });
        let thread_shared = Arc::clone(&shared);
        let writer = std::thread::Builder::new()
            .name("acos-vsync".into())
            .spawn(move || writer_loop(thread_shared, send))
            .map_err(|e| ServiceError::Internal(format!("vsync thread: {e}")))?;
        Ok(Self {
            shared,
            writer: Some(writer),
        })
    }

    /// `stealReceiveChannel`: hand the two descriptors to the reply.
    pub fn steal_receive_channel(&self, reply: &mut Parcel) -> SResult<()> {
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        state.tube.write_to_parcel(reply)
    }

    /// `setVsyncRate(count)`: 0 means "only on request".
    pub fn set_vsync_rate(&self, count: u32) {
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        state.rate = count;
        drop(state);
        self.shared.wake.notify_all();
    }

    /// `requestNextVsync`: one event, at the next grid time.
    pub fn request_next_vsync(&self) {
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        // "has no effect if the vsync rate is > 0" (IDisplayEventConnection.aidl).
        if state.rate == 0 {
            state.pending = state.pending.saturating_add(1);
        }
        drop(state);
        self.shared.wake.notify_all();
    }

    /// The `ParcelableVsyncEventData` `getLatestVsyncEventData` answers.
    #[must_use]
    pub fn latest_vsync_data(&self) -> VsyncEventData {
        let state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        let timestamp = if state.last_vsync_ns == 0 {
            now_ns()
        } else {
            state.last_vsync_ns
        };
        VsyncEventData::single_timeline(
            self.shared.period_ns,
            i64::from(state.count) + 1,
            timestamp,
        )
    }

    /// Events written so far (test/log evidence).
    #[must_use]
    pub fn events_sent(&self) -> u64 {
        self.shared
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .sent
    }
}

impl Drop for ConnectionClock {
    fn drop(&mut self) {
        {
            let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
            state.stopped = true;
        }
        self.shared.wake.notify_all();
        if let Some(handle) = self.writer.take() {
            let _ = handle.join();
        }
    }
}

fn writer_loop(shared: Arc<ClockShared>, send: OwnedFd) {
    loop {
        let target = {
            let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
            loop {
                if state.stopped {
                    return;
                }
                if state.rate > 0 || state.pending > 0 {
                    break;
                }
                state = shared
                    .wake
                    .wait(state)
                    .unwrap_or_else(|e| e.into_inner());
            }
            let now = now_ns();
            let period = shared.period_ns;
            let step = period.saturating_mul(i64::from(state.rate.max(1)));
            let mut target = if state.last_vsync_ns == 0 {
                // First event: the next boundary of the 60 Hz grid.
                align_up(now, period)
            } else {
                state.last_vsync_ns.saturating_add(step)
            };
            if target <= now {
                let missed = (now - target) / period + 1;
                target = target.saturating_add(missed.saturating_mul(period));
            }
            target
        };

        // Sleep to the grid time in short hops so `stop` is honoured promptly.
        loop {
            let now = now_ns();
            if now >= target {
                break;
            }
            let remaining = Duration::from_nanos((target - now) as u64);
            std::thread::sleep(remaining.min(Duration::from_millis(4)));
            if shared
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .stopped
            {
                return;
            }
        }

        let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.stopped {
            return;
        }
        if state.rate == 0 && state.pending == 0 {
            // The request was withdrawn while we slept (a `setVsyncRate(0)`).
            continue;
        }
        let data = VsyncEventData::single_timeline(
            shared.period_ns,
            i64::from(state.count) + 1,
            target,
        );
        let event = encode_vsync_event(shared.display_id, target, state.count, &data);
        if rustix::net::send(&send, &event, SendFlags::DONTWAIT | SendFlags::NOSIGNAL).is_ok() {
            state.sent += 1;
        }
        // On EAGAIN the client's 4 KiB buffer is full (it is not reading).
        // Dropping the event is what `BitTube::write` does with EAGAIN: it
        // returns the error to `EventThread`, which drops the frame.
        state.count = state.count.wrapping_add(1);
        state.last_vsync_ns = target;
        if state.pending > 0 {
            state.pending -= 1;
        }
    }
}

fn align_up(now: i64, period: i64) -> i64 {
    let period = period.max(1);
    ((now / period) + 1).saturating_mul(period)
}

// ---------------------------------------------------------------------------
// The service objects
// ---------------------------------------------------------------------------

/// One reusable connection slot. `createDisplayEventConnection` allocates the
/// clock; `stealReceiveChannel` hands out the descriptors.
#[derive(Debug)]
pub struct ConnectionSlot {
    /// The binder this slot answers on, bound by [`install`] right after the
    /// host allocated it (`ServiceHost::register_dynamic` needs the object
    /// before it returns the id).
    binder: AtomicU64,
    clock: Mutex<Option<ConnectionClock>>,
}

impl Default for ConnectionSlot {
    fn default() -> Self {
        Self::new()
    }
}

impl ConnectionSlot {
    /// An unbound slot; [`install`] calls [`Self::bind`] immediately after.
    #[must_use]
    pub fn new() -> Self {
        Self {
            binder: AtomicU64::new(0),
            clock: Mutex::new(None),
        }
    }

    /// Record the binder the host allocated for this slot.
    pub fn bind(&self, binder: BinderId) {
        self.binder.store(binder.0, Ordering::Relaxed);
    }

    /// The binder this slot answers on.
    #[must_use]
    pub fn binder(&self) -> BinderId {
        BinderId(self.binder.load(Ordering::Relaxed))
    }

    /// Start (or restart) the clock for a fresh connection and answer its binder.
    pub fn open(&self, display_id: u64, period_ns: i64) -> SResult<BinderId> {
        let clock = ConnectionClock::new(display_id, period_ns)?;
        let mut slot = self.clock.lock().unwrap_or_else(|e| e.into_inner());
        *slot = Some(clock);
        Ok(self.binder())
    }

    fn with_clock<T>(&self, f: impl FnOnce(&ConnectionClock) -> T) -> Option<T> {
        let slot = self.clock.lock().unwrap_or_else(|e| e.into_inner());
        slot.as_ref().map(f)
    }
}

/// The `IDisplayEventConnection` node.
pub struct DisplayEventConnectionService {
    slot: Arc<ConnectionSlot>,
}

impl DisplayEventConnectionService {
    /// Serve `slot`.
    #[must_use]
    pub fn new(slot: Arc<ConnectionSlot>) -> Self {
        Self { slot }
    }
}

impl ServiceObject for DisplayEventConnectionService {
    fn call(
        &mut self,
        interface: &str,
        method: &MethodInfo,
        _args: &[crate::value::Value],
    ) -> SResult<crate::host::CallResult> {
        // `android.gui` has no `acos-aidl` tables (W-R §1): the native interface
        // is served entirely by `raw_transact`.
        Err(ServiceError::UnsupportedAndroidApi {
            interface: interface.to_string(),
            code: method.code,
            method: method.name.clone(),
        })
    }

    fn raw_transact(
        &mut self,
        interface: &str,
        code: u32,
        data: &mut Parcel,
        reply: &mut Parcel,
    ) -> SResult<bool> {
        if interface != DESCRIPTOR {
            return Ok(false);
        }
        match code {
            codes::STEAL_RECEIVE_CHANNEL => {
                // The request carries no arguments (the `out BitTube` is not
                // written by the proxy): only the interface token, already
                // consumed by the dispatcher.
                exception::write_no_exception(reply);
                // `out BitTube` is a native parcelable: `Parcel::readData(Parcelable*)`
                // (`readParcelable`) reads an `int32 present` that must be
                // `kNonNullParcelableFlag` (1) **before** the parcelable body.
                // Without it the framework reads the flag out of the first
                // `flat_binder_object` header, which overlaps the object table
                // (`PERMISSION_DENIED` from `validateReadData`) and
                // `stealReceiveChannel` fails with
                // `Status(-129, …): 'PERMISSION_DENIED: '` (L1 §3).
                reply
                    .write_int32(1);
                let mut slot = self
                    .slot
                    .clock
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                let clock = slot.as_mut().ok_or_else(|| {
                    ServiceError::Internal(
                        "IDisplayEventConnection: stealReceiveChannel before a connection exists"
                            .into(),
                    )
                })?;
                clock.steal_receive_channel(reply)?;
                Ok(true)
            }
            codes::SET_VSYNC_RATE => {
                let count = data.read_int32().map_err(perr)?;
                exception::write_no_exception(reply);
                let _ = self
                    .slot
                    .with_clock(|c| c.set_vsync_rate(count.max(0) as u32));
                Ok(true)
            }
            codes::REQUEST_NEXT_VSYNC => {
                let _ = self.slot.with_clock(ConnectionClock::request_next_vsync);
                // oneway: no reply body.
                Ok(true)
            }
            codes::GET_LATEST_VSYNC_EVENT_DATA => {
                exception::write_no_exception(reply);
                let data = self
                    .slot
                    .with_clock(ConnectionClock::latest_vsync_data)
                    .unwrap_or_default();
                data.write_to_parcel(reply);
                Ok(true)
            }
            _ => Err(ServiceError::UnsupportedAndroidApi {
                interface: DESCRIPTOR.to_string(),
                code,
                method: format!("code {code}"),
            }),
        }
    }
}

/// A round-robin pool of connection nodes.
///
/// `createDisplayEventConnection` may be called more than once per process (the
/// framework creates one receiver per `Choreographer` thread), but the
/// `ServiceHost` allocates binder ids at install time, not at call time. The
/// pool therefore pre-allocates a fixed number of nodes and reuses them in
/// rotation; a second live connection on the same slot resets its `BitTube`,
/// which is exactly what `stealReceiveChannel` does to the previous one.
#[derive(Debug)]
pub struct ConnectionPool {
    slots: Vec<Arc<ConnectionSlot>>,
    next: usize,
}

impl ConnectionPool {
    /// A pool over `slots`.
    #[must_use]
    pub fn new(slots: Vec<Arc<ConnectionSlot>>) -> Self {
        Self { slots, next: 0 }
    }

    /// The number of pre-allocated connection nodes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.slots.len()
    }

    /// `true` when the pool has no slots (a misconfiguration: no connection can
    /// be created).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// Start the clock on the next slot and return its binder.
    pub fn open_next(&mut self, display_id: u64, period_ns: i64) -> SResult<BinderId> {
        if self.slots.is_empty() {
            return Err(ServiceError::Internal(
                "IDisplayEventConnection: no connection node is registered".into(),
            ));
        }
        let slot = Arc::clone(&self.slots[self.next % self.slots.len()]);
        self.next = self.next.wrapping_add(1);
        slot.open(display_id, period_ns)
    }
}

/// Register `count` `IDisplayEventConnection` nodes and return the pool the
/// `ISurfaceComposer` node allocates from.
pub fn install(host: &mut crate::host::ServiceHost, count: usize) -> Arc<Mutex<ConnectionPool>> {
    let mut slots = Vec::with_capacity(count);
    for _ in 0..count {
        let slot = Arc::new(ConnectionSlot::new());
        let binder = host.register_dynamic(
            DESCRIPTOR,
            Box::new(DisplayEventConnectionService::new(Arc::clone(&slot))),
        );
        slot.bind(binder);
        slots.push(slot);
    }
    Arc::new(Mutex::new(ConnectionPool::new(slots)))
}

/// Read the two `gui::BitTube` descriptors out of a reply `Parcel` the way the
/// framework does (`readFileDescriptor` twice: receive first, then send).
///
/// The numbers are owned by `parcel`; [`Parcel::borrowed_file_descriptor`]
/// turns one back into a borrow for as long as the parcel lives. Used by the
/// tests.
pub fn read_bit_tube(parcel: &mut Parcel) -> SResult<(std::os::fd::RawFd, std::os::fd::RawFd)> {
    // Native-parcelable wrapper: `Parcel::readParcelable` reads
    // `kNonNullParcelableFlag` (1) before `BitTube::readFromParcel` runs.
    let present = parcel.read_int32().map_err(perr)?;
    if present != 1 {
        return Err(ServiceError::Internal(format!(
            "BitTube: unexpected parcelable present flag {present}"
        )));
    }
    let receive = parcel.read_file_descriptor().map_err(perr)?;
    let send = parcel.read_file_descriptor().map_err(perr)?;
    match (receive, send) {
        (Some(receive), Some(send)) => Ok((receive, send)),
        _ => Err(ServiceError::Internal(
            "BitTube: reply did not carry two descriptors".into(),
        )),
    }
}

/// Receive one event from a `BitTube` receive descriptor.
///
/// One `recv` per event: `SOCK_SEQPACKET` preserves message boundaries, so a
/// buffer of [`EVENT_SIZE`] returns exactly one event (the framework does the
/// same with `BitTube::recvObjects`).
pub fn recv_event(fd: BorrowedFd<'_>) -> std::io::Result<Option<[u8; EVENT_SIZE]>> {
    let mut buf = [0u8; EVENT_SIZE];
    match rustix::net::recv(fd, &mut buf, RecvFlags::DONTWAIT) {
        Ok((0, _)) => Ok(None),
        Ok((n, _)) if n == EVENT_SIZE => Ok(Some(buf)),
        Ok(_) => Ok(None),
        Err(rustix::io::Errno::AGAIN) => Ok(None),
        Err(e) => Err(std::io::Error::from_raw_os_error(e.raw_os_error())),
    }
}

fn perr(e: impl std::fmt::Display) -> ServiceError {
    ServiceError::Internal(format!("{DESCRIPTOR}: parcel: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_size_matches_the_image() {
        // 24-byte header + 4-byte pad + 184-byte VsyncEventData.
        assert_eq!(EVENT_SIZE, 24 + 8 + 184);
        assert_eq!(EVENT_OFFSET_VSYNC_DATA + 8 + 4 + 4 + 7 * 24, EVENT_SIZE);
    }

    #[test]
    fn vsync_event_round_trips_through_the_encoder() {
        let data = VsyncEventData::single_timeline(PERIOD_NS_60HZ, 7, 1_000);
        let buf = encode_vsync_event(0x1234, 1_000, 3, &data);
        let ev = decode_event(&buf);
        assert_eq!(ev.kind, DISPLAY_EVENT_VSYNC);
        assert_eq!(ev.display_id, 0x1234);
        assert_eq!(ev.timestamp_ns, 1_000);
        assert_eq!(ev.count, 3);
        assert_eq!(ev.frame_interval, PERIOD_NS_60HZ);
        assert_eq!(ev.first_vsync_id, 7);
    }

    #[test]
    fn fourcc_values_match_display_event_receiver_h() {
        assert_eq!(DISPLAY_EVENT_VSYNC, u32::from_be_bytes(*b"vsyn"));
        assert_eq!(DISPLAY_EVENT_HOTPLUG, u32::from_be_bytes(*b"plug"));
        assert_eq!(DISPLAY_EVENT_MODE_CHANGE, u32::from_be_bytes(*b"mode"));
    }

    #[test]
    fn vsync_event_data_parcelable_round_trips() {
        let data = VsyncEventData::single_timeline(PERIOD_NS_60HZ, 11, 5_000);
        let mut p = Parcel::new();
        data.write_to_parcel(&mut p);
        p.set_data_position(0);
        assert_eq!(VsyncEventData::read_from_parcel(&mut p).unwrap(), data);
        assert_eq!(p.data_avail(), 0);
    }
}
