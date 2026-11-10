//! `android.view.DisplayEventReceiver` natives + the deterministic virtual vsync
//! source (BACKLOG T-30).
//!
//! The real framework `Choreographer` owns a `DisplayEventReceiver` and calls
//! `nativeScheduleVsync` when it wants a frame pulse. This module models the
//! display's vsync as a **pull** source:
//!
//! * `nativeInit` creates a non-blocking `eventfd` and registers it on the
//!   `MessageQueue`'s real looper epoll set (`LooperHandle::add_fd`).
//! * `nativeScheduleVsync` marks the receiver pending and writes the eventfd.
//!   No timer is armed: with no request the eventfd is quiet and the looper's
//!   `epoll_wait` blocks forever (zero idle CPU / zero wakeups).
//! * When the looper wakes, [`on_looper_wake`] drains the eventfd and, for each
//!   pending receiver, generates the next tick on a boot-anchored 60 Hz grid and
//!   invokes the real `DisplayEventReceiver.onVsync(timestamp, displayId, frame,
//!   VsyncEventData)` on the looper's green thread.
//!
//! The timeline is deterministic and configurable via [`set_interval_nanos`].
//! The first tick is anchored at the first `nativeScheduleVsync`; subsequent
//! ticks advance by exactly one interval. The framework's `onVsync` clamps a
//! future timestamp to its own `System.nanoTime`, so the Java frame times remain
//! real, monotonic, and close to the requested grid.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::os::fd::OwnedFd;

use rustix::event::epoll::EventFlags;
use rustix::event::{eventfd, EventfdFlags};

use crate::class::NativeCtx;
use crate::error::{VmError, VmResult};
use crate::heap::{Handle, NULL};
use crate::javabridge::JavaValue;
use crate::vm::Vm;

use super::looper::{handle_for_ptr, LooperHandle};

/// `ACOS_VSYNC_TRACE=1` prints every vsync request and dispatch (timestamp,
/// id, frame, physical display). Inert unless the env var is set.
fn trace_enabled() -> bool {
    std::env::var_os("ACOS_VSYNC_TRACE").is_some()
}

/// Finalizer token returned by `nativeGetDisplayEventReceiverFinalizer`.
///
/// The framework's `NativeAllocationRegistry` hands this token back to
/// `applyFreeFunction` when the receiver is released; `fwk_surface` forwards
/// unknown tokens to [`free_receiver`].
pub(crate) const FREE_DISPLAY_EVENT_RECEIVER: u64 = 0x4456_5359; // "DVSY"

/// Reference display refresh: 60 Hz.
pub const DEFAULT_INTERVAL_NANOS: i64 = 16_666_667;

/// `android.view.DisplayEventReceiver$VsyncEventData`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VsyncEventData {
    /// Nanosecond timestamp of the tick.
    pub timestamp_nanos: i64,
    /// Monotonic vsync id.
    pub vsync_id: i64,
    /// Frame interval in nanoseconds.
    pub interval_nanos: i64,
    /// Physical display id reported to Java.
    pub physical_display_id: i64,
    /// Frame counter reported to Java.
    pub frame: i32,
}

impl VsyncEventData {
    /// An all-zero event used before the first tick.
    #[must_use]
    pub const fn zero(interval_nanos: i64) -> Self {
        Self {
            timestamp_nanos: 0,
            vsync_id: 0,
            interval_nanos,
            physical_display_id: 1,
            frame: 0,
        }
    }
}

#[derive(Debug)]
#[allow(dead_code)] // vsync_source/event_registration/layer_handle are kept for the
                   // nativeInit contract and future VRR/event-registration work.
struct ReceiverState {
    eventfd: OwnedFd,
    looper: u64,
    weak_ref: Handle,
    event_data_weak: Handle,
    pending: bool,
    base_nanos: i64,
    tick_count: i64,
    last_tick: i64,
    vsync_id: i64,
    interval: i64,
    vsync_source: i32,
    event_registration: i32,
    layer_handle: i64,
}

#[derive(Debug)]
struct VsyncState {
    next_id: u64,
    receivers: BTreeMap<u64, ReceiverState>,
    interval_nanos: i64,
}

impl VsyncState {
    fn new() -> Self {
        Self {
            next_id: 1,
            receivers: BTreeMap::new(),
            interval_nanos: DEFAULT_INTERVAL_NANOS,
        }
    }
}

thread_local! {
    static STATE: RefCell<VsyncState> = RefCell::new(VsyncState::new());
}

/// Configure the virtual refresh interval in nanoseconds (test/VRR hook).
pub fn set_interval_nanos(interval: i64) {
    if interval > 0 {
        STATE.with(|s| s.borrow_mut().interval_nanos = interval);
    }
}

/// The configured interval in nanoseconds.
#[must_use]
pub fn interval_nanos() -> i64 {
    STATE.with(|s| s.borrow().interval_nanos)
}

/// Clear all receivers and restore the 60 Hz interval (test isolation).
pub fn reset() {
    STATE.with(|s| {
        let mut s = s.borrow_mut();
        s.receivers.clear();
        s.next_id = 1;
        s.interval_nanos = DEFAULT_INTERVAL_NANOS;
    });
}

/// Number of generated ticks across all receivers.
#[must_use]
pub fn tick_count() -> u64 {
    STATE.with(|s| {
        s.borrow()
            .receivers
            .values()
            .map(|r| r.tick_count.max(0) as u64)
            .sum()
    })
}

/// The latest generated tick timestamp, or 0.
#[must_use]
pub fn last_tick() -> i64 {
    STATE.with(|s| {
        s.borrow()
            .receivers
            .values()
            .map(|r| r.last_tick)
            .max()
            .unwrap_or(0)
    })
}

/// Receivers with an outstanding `nativeScheduleVsync`.
#[must_use]
pub fn pending_count() -> usize {
    STATE.with(|s| s.borrow().receivers.values().filter(|r| r.pending).count())
}

/// `true` when no receiver has an outstanding request.
#[must_use]
pub fn is_idle() -> bool {
    STATE.with(|s| s.borrow().receivers.values().all(|r| !r.pending))
}

fn now_nanos() -> i64 {
    // The virtual timeline MUST share the clock `Choreographer`/`Handler` use
    // for `Message.when` and frame times: `SystemClock.uptimeMillis()`. The
    // framework's `FrameDisplayEventReceiver.onVsync` divides the timestamp by
    // `TimeUtils.NANOS_PER_MS` and calls `Handler.sendMessageAtTime`, whose
    // deadline is compared against the uptime clock. Using epoch time here made
    // `when` land ~56 years in the future, so `doFrame` never ran.
    super::os_clock::uptime_nanos()
}

fn bridge_err(e: crate::javabridge::BridgeError) -> VmError {
    match e {
        crate::javabridge::BridgeError::Vm(e) => e,
        other => VmError::Internal(other.to_string()),
    }
}

/// Register the `DisplayEventReceiver` natives.
pub(crate) fn install(vm: &mut Vm) -> VmResult<()> {
    const DER: &str = "Landroid/view/DisplayEventReceiver;";
    vm.register_native(
        DER,
        "nativeGetDisplayEventReceiverFinalizer",
        "()J",
        native_finalizer,
    );
    vm.register_native(
        DER,
        "nativeGetLatestVsyncEventData",
        "(J)Landroid/view/DisplayEventReceiver$VsyncEventData;",
        native_get_latest_vsync_event_data,
    );
    vm.register_native(
        DER,
        "nativeInit",
        "(Ljava/lang/ref/WeakReference;Ljava/lang/ref/WeakReference;Landroid/os/MessageQueue;IIJ)J",
        native_init,
    );
    vm.register_native(DER, "nativeScheduleVsync", "(J)V", native_schedule_vsync);
    Ok(())
}

fn native_finalizer(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    vm.ret_i64(ctx, FREE_DISPLAY_EVENT_RECEIVER as i64);
    Ok(())
}

fn native_init(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let weak_ref = vm.arg_ref(ctx, 0);
    let event_data_weak = vm.arg_ref(ctx, 1);
    let mq_obj = vm.arg_ref(ctx, 2);
    let vsync_source = vm.arg_i32(ctx, 3);
    let event_registration = vm.arg_i32(ctx, 4);
    let layer_handle = vm.arg_i64(ctx, 5);

    let looper_ptr = match vm.get_field(mq_obj, "mPtr") {
        Ok(JavaValue::Long(v)) => v as u64,
        _ => 0,
    };
    if weak_ref == 0 || event_data_weak == 0 || looper_ptr == 0 {
        return Err(VmError::unsupported_api(
            "DisplayEventReceiver.nativeInit",
            "null WeakReference/MessageQueue",
        ));
    }
    let looper = handle_for_ptr(looper_ptr).ok_or_else(|| {
        VmError::unsupported_api("DisplayEventReceiver.nativeInit", "dead MessageQueue")
    })?;
    let fd = eventfd(0, EventfdFlags::NONBLOCK | EventfdFlags::CLOEXEC).map_err(|e| {
        VmError::Internal(format!("DisplayEventReceiver.nativeInit: eventfd: {e}"))
    })?;
    looper.add_fd(&fd, EventFlags::IN, 0).map_err(|e| {
        VmError::Internal(format!("DisplayEventReceiver.nativeInit: add_fd: {e}"))
    })?;

    vm.root_handle(weak_ref);
    vm.root_handle(event_data_weak);
    let interval = interval_nanos();
    let id = STATE.with(|s| {
        let mut s = s.borrow_mut();
        let id = s.next_id;
        s.next_id = s.next_id.wrapping_add(1).max(1);
        s.receivers.insert(
            id,
            ReceiverState {
                eventfd: fd,
                looper: looper_ptr,
                weak_ref,
                event_data_weak,
                pending: false,
                base_nanos: 0,
                tick_count: 0,
                last_tick: 0,
                vsync_id: 0,
                interval,
                vsync_source,
                event_registration,
                layer_handle,
            },
        );
        id
    });
    set_latest_event_data(vm, id, VsyncEventData::zero(interval))?;
    vm.ret_i64(ctx, id as i64);
    Ok(())
}

fn native_schedule_vsync(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let ptr = vm.arg_i64(ctx, 0) as u64;
    if ptr == 0 {
        return Ok(());
    }
    let wrote = STATE.with(|s| {
        let mut s = s.borrow_mut();
        let Some(r) = s.receivers.get_mut(&ptr) else {
            return false;
        };
        r.pending = true;
        if r.base_nanos == 0 {
            r.base_nanos = now_nanos();
        }
        if trace_enabled() {
            eprintln!(
                "VSYNC-TRACE scheduleVsync id={} base={} now={}",
                ptr,
                r.base_nanos,
                now_nanos()
            );
        }
        let one: u64 = 1;
        rustix::io::write(&r.eventfd, &one.to_ne_bytes()).is_ok()
    });
    if !wrote {
        // A receiver that has been disposed is a no-op, matching the Java
        // `scheduleVsync` guard (`mReceiverPtr == 0`).
        return Ok(());
    }
    Ok(())
}

fn native_get_latest_vsync_event_data(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let ptr = vm.arg_i64(ctx, 0) as u64;
    let info = STATE.with(|s| {
        s.borrow().receivers.get(&ptr).map(|r| {
            (
                r.event_data_weak,
                VsyncEventData {
                    timestamp_nanos: r.last_tick,
                    vsync_id: r.vsync_id,
                    interval_nanos: r.interval,
                    physical_display_id: 1,
                    frame: r.tick_count as i32,
                },
            )
        })
    });
    let Some((event_data_weak, event)) = info else {
        vm.ret_ref(ctx, NULL);
        return Ok(());
    };
    let Some(obj) = resolve_weak(vm, event_data_weak) else {
        vm.ret_ref(ctx, NULL);
        return Ok(());
    };
    set_vsync_event_data(vm, obj, event)?;
    vm.ret_ref(ctx, obj);
    vm.release_bridge_root(obj);
    Ok(())
}

fn resolve_weak(vm: &mut Vm, weak: Handle) -> Option<Handle> {
    let referent = vm
        .call_method(weak, "get", "()Ljava/lang/Object;", &[])
        .ok()?;
    match referent {
        JavaValue::Object(h) if h != NULL => Some(h),
        _ => None,
    }
}

fn set_latest_event_data(vm: &mut Vm, ptr: u64, event: VsyncEventData) -> VmResult<()> {
    let event_data_weak = STATE.with(|s| s.borrow().receivers.get(&ptr).map(|r| r.event_data_weak));
    let Some(weak) = event_data_weak else {
        return Ok(());
    };
    let Some(obj) = resolve_weak(vm, weak) else {
        return Ok(());
    };
    set_vsync_event_data(vm, obj, event)?;
    vm.release_bridge_root(obj);
    Ok(())
}

fn set_vsync_event_data(vm: &mut Vm, obj: Handle, event: VsyncEventData) -> VmResult<()> {
    vm.set_field(obj, "frameInterval", JavaValue::Long(event.interval_nanos))
        .map_err(bridge_err)?;
    vm.set_field(
        obj,
        "frameTimelinesLength",
        JavaValue::Int(if event.vsync_id > 0 { 1 } else { 0 }),
    )
    .map_err(bridge_err)?;
    vm.set_field(obj, "preferredFrameTimelineIndex", JavaValue::Int(0))
        .map_err(bridge_err)?;
    if event.vsync_id > 0 {
        let array = match vm.get_field(obj, "frameTimelines") {
            Ok(JavaValue::Object(h)) if h != NULL => h,
            _ => return Ok(()),
        };
        let timeline = match vm.get_array_element(array, 0) {
            Ok(JavaValue::Object(h)) if h != NULL => h,
            _ => return Ok(()),
        };
        vm.set_field(timeline, "vsyncId", JavaValue::Long(event.vsync_id))
            .map_err(bridge_err)?;
        vm.set_field(
            timeline,
            "expectedPresentationTime",
            JavaValue::Long(event.timestamp_nanos),
        )
        .map_err(bridge_err)?;
        vm.set_field(
            timeline,
            "deadline",
            JavaValue::Long(event.timestamp_nanos),
        )
        .map_err(bridge_err)?;
    }
    Ok(())
}

/// Drain this looper's vsync eventfds and dispatch every pending tick on the
/// current (looper) green thread.
///
/// Called from `MessageQueue.nativePollOnce` after the real `epoll` wake. Only
/// receivers with an outstanding request generate a tick.
pub(crate) fn on_looper_wake(vm: &mut Vm, looper: u64) -> VmResult<bool> {
    let ready = STATE.with(|s| {
        let mut s = s.borrow_mut();
        let mut ready = Vec::new();
        for (id, r) in &mut s.receivers {
            if r.looper != looper {
                continue;
            }
            // Drain the eventfd; the write count is not interesting, only the
            // pending flag the writer set.
            let mut buf = [0u8; 8];
            while rustix::io::read(&r.eventfd, &mut buf).is_ok() {}
            if !r.pending {
                continue;
            }
            r.pending = false;
            r.tick_count += 1;
            r.last_tick = r.base_nanos.saturating_add(r.interval.saturating_mul(r.tick_count));
            r.vsync_id = r.tick_count;
            ready.push((
                *id,
                r.weak_ref,
                r.event_data_weak,
                VsyncEventData {
                    timestamp_nanos: r.last_tick,
                    vsync_id: r.vsync_id,
                    interval_nanos: r.interval,
                    physical_display_id: 1,
                    frame: r.tick_count as i32,
                },
            ));
        }
        ready
    });
    let mut any = false;
    for (_id, weak_ref, event_data_weak, event) in ready {
        let Some(receiver) = resolve_weak(vm, weak_ref) else {
            continue;
        };
        let Some(event_obj) = resolve_weak(vm, event_data_weak) else {
            vm.release_bridge_root(receiver);
            continue;
        };
        if trace_enabled() {
            eprintln!(
                "VSYNC-TRACE dispatchVsync timestamp={} vsyncId={} frame={} display={}",
                event.timestamp_nanos, event.vsync_id, event.frame, event.physical_display_id
            );
        }
        set_vsync_event_data(vm, event_obj, event)?;
        let res = vm.call_method(
            receiver,
            "onVsync",
            "(JJILandroid/view/DisplayEventReceiver$VsyncEventData;)V",
            &[
                JavaValue::Long(event.timestamp_nanos),
                JavaValue::Long(event.physical_display_id),
                JavaValue::Int(event.frame),
                JavaValue::Object(event_obj),
            ],
        );
        vm.release_bridge_root(event_obj);
        vm.release_bridge_root(receiver);
        res.map_err(bridge_err)?;
        any = true;
    }
    Ok(any)
}

/// Release a receiver's eventfd and roots. Called from the
/// `NativeAllocationRegistry` finalizer path.
pub(crate) fn free_receiver(vm: &mut Vm, ptr: u64) -> bool {
    let removed = STATE.with(|s| s.borrow_mut().receivers.remove(&ptr));
    let Some(r) = removed else {
        return false;
    };
    if let Some(handle) = handle_for_ptr(r.looper) {
        let _ = handle.remove_fd(&r.eventfd);
    }
    vm.release_bridge_root(r.weak_ref);
    vm.release_bridge_root(r.event_data_weak);
    true
}

/// The state's `LooperHandle` type is only used by `native_init`; keep the
/// import honest for the non-test build.
#[allow(dead_code)]
fn _looper_type_anchor(_: &LooperHandle) {}
