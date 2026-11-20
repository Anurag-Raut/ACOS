//! `android.gui.ISurfaceComposer` — the compositor-client surface stock `libgui`
//! talks to (W-R option A, tasks T1/T2).
//!
//! `SurfaceComposerClient`/`DisplayEventReceiver` reach the compositor through
//! `ComposerServiceAIDL::getComposerService()`, which is
//! `ServiceManager.getService("SurfaceFlinger")` + `ISurfaceComposer::asInterface`.
//! [`crate::registry::install`] therefore registers this node under the
//! `ServiceManager` name `SurfaceFlinger`, and this module answers the subset of
//! transactions the frame-clock half of the launch needs.
//!
//! # What this node is, and is not
//!
//! It is the **client half** of a compositor: the bookkeeping calls a client
//! makes before it can hold a `BitTube`. It is not a compositor — there is no
//! layer, no display, no transaction applier. `createConnection`
//! (`ISurfaceComposerClient`, `SurfaceSession`, W-R task T1) is deliberately
//! **not** implemented here and answers a structured
//! `UNSUPPORTED_ANDROID_API`, because `SurfaceSession` is the surface half of
//! W-R and this task is the clock half.
//!
//! # Wire evidence
//!
//! Every code below was read out of the **image's** `libgui.so`: the generated
//! `android::gui::BpSurfaceComposer::<method>` passes the transaction code as an
//! immediate to `IBinder::transact` (`.cache/art/sysroot/system/lib64/libgui.so`,
//! e.g. `createDisplayEventConnection` at `0x15499b` writes `0x2`,
//! `getMaxAcquiredBufferCount` at `0x15c655` writes `0x39`).
//!
//! **The image is not the `android-14.0.0_r75` tag for the tail of this
//! interface**: counting the tag's `ISurfaceComposer.aidl` in declaration order
//! gives `getMaxAcquiredBufferCount` = 65, the image answers 57. The methods this
//! task uses (`createDisplayEventConnection` 2, `createConnection` 3,
//! `getPhysicalDisplayIds` 6, `getPhysicalDisplayToken` 7,
//! `getSupportedFrameTimestamps` 8, `getDisplayStats` 10, `getDisplayState` 11)
//! agree with the tag; [`codes`] records the image values and pins them in a
//! test. The same check found the image has **no** `getSchedulingPolicy` on
//! `IDisplayEventConnection` (see [`crate::display_events::codes`]).
//!
//! Argument shapes come from the image's proxy disassembly plus the tag's AIDL:
//! `VsyncSource`/`EventRegistration` are `byte`-backed AIDL enums, and
//! `libbinder`'s `Parcel::writeByte` is `writeInt32` (`Parcel.cpp:1395`), so both
//! are 4 bytes; `long[]` is `int32 count` + `int64` elements
//! (`Parcel::readInt64Vector`).
//!
//! # What is synthesized (and how)
//!
//! * `getPhysicalDisplayIds` answers the one logical display ACOS has
//!   (`DisplayInfo::hello_views`), and `getPhysicalDisplayToken` answers a real
//!   aux binder so the token the framework passes back to
//!   `getDisplayStats`/`getDisplayState` is routable.
//! * `getDisplayStats.vsyncTime` is `CLOCK_MONOTONIC` **now** and
//!   `vsyncPeriod` is the same 60 Hz period the vsync thread uses, so
//!   `Surface::getDisplayRefreshCycleDuration` and the clock agree.
//! * `getMaxAcquiredBufferCount` is [`DEFAULT_MAX_ACQUIRED_BUFFERS`] (2), the
//!   value a real `SurfaceFlinger` computes from the refresh rate and the
//!   app/SF pipeline depth (`SurfaceFlinger::calculateMaxAcquiredBufferCount`
//!   is `max(min_acquired_buffers, ceil(presentLatency/period) - 1)`: 1..=2 at
//!   60 Hz, 3 at 120 Hz — never 12+). `BufferQueueCore`'s own ctor default is
//!   1 and BLAST's `setMaxDequeuedBufferCount(2)` adds the other 2 slots. BLAST sizes its whole buffer pool as
//!   `maxDequeued(2) + maxAcquired`, so this number is also ACOS's per-session
//!   frame budget *if* released slots were not recycled. They are: C1 returns a
//!   slot on every latched frame through the stock contract —
//!   `FrameEventHistoryStats.frameNumber` names the frame just latched and
//!   `previousReleaseCallbackId` names the buffer it replaced, which is what
//!   `BLASTBufferQueue` recycles (+ its stale-release sweep for merged frames).
//!   W11 measured 4 allocations across a >60-frame interactive session at this
//!   default, so no larger pool is needed. `ACOS_MAX_ACQUIRED_BUFFERS` overrides
//!   it for measurement (1..=64).
//! * `getSupportedFrameTimestamps` answers `POSTED..RELEASE` (0..=9, i.e. every
//!   `FrameEvent` except `EVENT_COUNT`). ACOS produces none of them; the list
//!   only tells the framework which `FrameEventHistory` slots exist.

use std::sync::{Arc, Mutex};

use acos_aidl::MethodInfo;
use acos_parcel::{BinderId, Parcel};

use crate::display_events::{self, now_ns, ConnectionPool, PERIOD_NS_60HZ};
use crate::error::{Result as SResult, ServiceError};
use crate::exception;
use crate::host::ServiceObject;
use crate::surface_client::{self, SurfaceState};

/// The `ISurfaceComposer` transaction codes of the **API-34 image**.
pub mod codes {
    /// `bootFinished()`.
    pub const BOOT_FINISHED: u32 = 1;
    /// `createDisplayEventConnection(VsyncSource, EventRegistration, IBinder layer)`
    /// → `IDisplayEventConnection`.
    pub const CREATE_DISPLAY_EVENT_CONNECTION: u32 = 2;
    /// `createConnection()` → `ISurfaceComposerClient` (W-R T1, not this task).
    pub const CREATE_CONNECTION: u32 = 3;
    /// `getPhysicalDisplayIds()` → `long[]`.
    pub const GET_PHYSICAL_DISPLAY_IDS: u32 = 6;
    /// `getPhysicalDisplayToken(long displayId)` → `IBinder`.
    pub const GET_PHYSICAL_DISPLAY_TOKEN: u32 = 7;
    /// `getSupportedFrameTimestamps()` → `FrameEvent[]`.
    pub const GET_SUPPORTED_FRAME_TIMESTAMPS: u32 = 8;
    /// `getDisplayStats(IBinder display)` → `DisplayStatInfo`.
    pub const GET_DISPLAY_STATS: u32 = 10;
    /// `getDisplayState(IBinder display)` → `DisplayState`.
    pub const GET_DISPLAY_STATE: u32 = 11;
    /// `getMaxAcquiredBufferCount()` → `int`.
    ///
    /// The image's value. The `android-14.0.0_r75` AIDL order gives 65 (see the
    /// module docs) — a table transcribed from the tag would be eight off.
    pub const GET_MAX_ACQUIRED_BUFFER_COUNT: u32 = 57;
}

/// The descriptor every `ISurfaceComposer` transaction carries.
pub const DESCRIPTOR: &str = "android.gui.ISurfaceComposer";

/// `ServiceManager` name the framework looks the compositor up by
/// (`ComposerServiceAIDL.cpp`: `getService(String16("SurfaceFlinger"))`).
pub const SERVICE_NAME: &str = "SurfaceFlinger";

/// The API-34 **AIDL** name. `SurfaceComposerClient.cpp:146`
/// (`ComposerServiceAIDL::connectLocked`) asks for `SurfaceFlingerAIDL` first;
/// `SurfaceFlinger` is the pre-AIDL name the same library still falls back to.
/// The image's `DisplayEventReceiver` therefore never reaches the legacy name
/// unless this alias exists (observed: `waitForService for
/// SurfaceFlingerAIDL` failing, L1 §2).
pub const AIDL_SERVICE_NAME: &str = "SurfaceFlingerAIDL";

/// `ISurfaceComposer.VsyncSource.eVsyncSourceApp`.
pub const VSYNC_SOURCE_APP: i32 = 0;
/// `ISurfaceComposer.VsyncSource.eVsyncSourceSurfaceFlinger`.
pub const VSYNC_SOURCE_SURFACE_FLINGER: i32 = 1;
/// `ISurfaceComposer.EventRegistration.modeChanged`.
pub const EVENT_REGISTRATION_MODE_CHANGED: i32 = 1;
/// `ISurfaceComposer.EventRegistration.frameRateOverride`.
pub const EVENT_REGISTRATION_FRAME_RATE_OVERRIDE: i32 = 2;

/// The display ACOS reports: one internal logical display.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DisplayDescriptor {
    /// `PhysicalDisplayId.value` (`getPhysicalDisplayIds`).
    pub display_id: u64,
    /// `DisplayState.layerStack`.
    pub layer_stack: i32,
    /// `DisplayState.orientation` (`Rotation.Rotation0` = 0).
    pub orientation: i32,
    /// `DisplayState.layerStackSpaceRect.width`.
    pub width: i32,
    /// `DisplayState.layerStackSpaceRect.height`.
    pub height: i32,
}

impl DisplayDescriptor {
    /// The reference device's single display (`DisplayInfo::hello_views`).
    #[must_use]
    pub fn reference() -> Self {
        let info = crate::display::DisplayInfo::hello_views();
        Self {
            display_id: info.display_id as u64,
            layer_stack: info.layer_stack,
            orientation: info.rotation,
            width: info.logical_width,
            height: info.logical_height,
        }
    }
}

/// The `ISurfaceComposer` node.
pub struct SurfaceComposerService {
    pool: Arc<Mutex<ConnectionPool>>,
    display: DisplayDescriptor,
    /// The client-side surface bookkeeping `createConnection` hands out and
    /// `SET_TRANSACTION_STATE` applies into.
    surface: Arc<Mutex<SurfaceState>>,
    /// The `IBinder` `getPhysicalDisplayToken` answers, so a caller can pass it
    /// back to `getDisplayStats`/`getDisplayState`.
    display_token: BinderId,
    /// `createDisplayEventConnection` arguments seen, in call order. The
    /// framework passes `eVsyncSourceApp` + no registration on the
    /// `Choreographer` path; recording them is what makes a wrong value visible
    /// in the evidence stream instead of silent.
    connections: Vec<ConnectionRequest>,
}

/// One recorded `createDisplayEventConnection` call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConnectionRequest {
    /// `VsyncSource` as the caller sent it.
    pub vsync_source: i32,
    /// `EventRegistration` flags as the caller sent them.
    pub event_registration: i32,
    /// `true` when a non-null layer handle accompanied the request.
    pub has_layer: bool,
}

impl SurfaceComposerService {
    /// Serve `pool` for `display`; `display_token` is the aux binder
    /// `getPhysicalDisplayToken` returns.
    #[must_use]
    pub fn new(
        pool: Arc<Mutex<ConnectionPool>>,
        display: DisplayDescriptor,
        display_token: BinderId,
        surface: Arc<Mutex<SurfaceState>>,
    ) -> Self {
        Self {
            pool,
            display,
            display_token,
            surface,
            connections: Vec::new(),
        }
    }

    /// The surface state, for a test/runtime that installs this node directly.
    #[must_use]
    pub fn surface_state(&self) -> Arc<Mutex<SurfaceState>> {
        Arc::clone(&self.surface)
    }

    /// The `createDisplayEventConnection` calls seen so far.
    #[must_use]
    pub fn connections(&self) -> &[ConnectionRequest] {
        &self.connections
    }
}

impl ServiceObject for SurfaceComposerService {
    fn call(
        &mut self,
        interface: &str,
        method: &MethodInfo,
        _args: &[crate::value::Value],
    ) -> SResult<crate::host::CallResult> {
        // `android.gui` has no `acos-aidl` tables (W-R §1): every transaction of
        // this native interface is served by `raw_transact`.
        Err(ServiceError::UnsupportedAndroidApi {
            interface: interface.to_string(),
            code: method.code,
            method: method.name.clone(),
        })
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn drain_raw_deliveries(&mut self) -> Vec<crate::host::PendingDelivery> {
        self.surface
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take_deliveries()
    }

    fn raw_transact(
        &mut self,
        interface: &str,
        code: u32,
        data: &mut Parcel,
        reply: &mut Parcel,
    ) -> SResult<bool> {
        // `Transaction::apply` uses the **legacy** interface
        // (`android.ui.ISurfaceComposer`, `SET_TRANSACTION_STATE` = 8) on the
        // same `SurfaceFlinger` binder the AIDL calls use. Dispatch on the
        // token before the code, because code 8 means different methods on the
        // two interfaces.
        if interface == surface_client::LEGACY_COMPOSER_DESCRIPTOR {
            return match code {
                surface_client::SET_TRANSACTION_STATE => {
                    surface_client::apply_transaction_state(&self.surface, data, reply)
                }
                _ => Err(ServiceError::UnsupportedAndroidApi {
                    interface: surface_client::LEGACY_COMPOSER_DESCRIPTOR.to_string(),
                    code,
                    method: format!("code {code}"),
                }),
            };
        }
        if interface != DESCRIPTOR {
            return Ok(false);
        }
        match code {
            codes::BOOT_FINISHED => {
                exception::write_no_exception(reply);
                Ok(true)
            }
            codes::CREATE_CONNECTION => {
                exception::write_no_exception(reply);
                reply.write_strong_binder_opt(Some(
                    self.surface
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .client_binder,
                ));
                Ok(true)
            }
            codes::CREATE_DISPLAY_EVENT_CONNECTION => {
                // VsyncSource (int32: a byte-backed AIDL enum on a writeByte
                // that libbinder implements as writeInt32), EventRegistration
                // (int32), then the nullable layer handle.
                let vsync_source = data.read_int32().map_err(perr)?;
                let event_registration = data.read_int32().map_err(perr)?;
                let layer = data.read_strong_binder().map_err(perr)?;
                if vsync_source != VSYNC_SOURCE_APP && vsync_source != VSYNC_SOURCE_SURFACE_FLINGER
                {
                    return Err(ServiceError::BadRequest {
                        interface: DESCRIPTOR.to_string(),
                        method: "createDisplayEventConnection".into(),
                        detail: format!("unknown VsyncSource {vsync_source}"),
                    });
                }
                self.connections.push(ConnectionRequest {
                    vsync_source,
                    event_registration,
                    has_layer: layer.is_some(),
                });
                let binder = self
                    .pool
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .open_next(self.display.display_id, PERIOD_NS_60HZ)?;
                exception::write_no_exception(reply);
                reply.write_strong_binder_opt(Some(binder));
                Ok(true)
            }
            codes::GET_PHYSICAL_DISPLAY_IDS => {
                exception::write_no_exception(reply);
                reply
                    .write_int64_array(Some(&[self.display.display_id as i64]))
                    .map_err(perr)?;
                Ok(true)
            }
            codes::GET_PHYSICAL_DISPLAY_TOKEN => {
                let _display_id = data.read_int64().map_err(perr)?;
                exception::write_no_exception(reply);
                reply.write_strong_binder_opt(Some(self.display_token));
                Ok(true)
            }
            codes::GET_SUPPORTED_FRAME_TIMESTAMPS => {
                exception::write_no_exception(reply);
                // FrameEvent::POSTED (0) .. FrameEvent::RELEASE (9);
                // EVENT_COUNT (10) is not an event.
                reply
                    .write_int32_array(Some(&[0, 1, 2, 3, 4, 5, 6, 7, 8, 9]))
                    .map_err(perr)?;
                Ok(true)
            }
            codes::GET_DISPLAY_STATS => {
                let _display = data.read_strong_binder().map_err(perr)?;
                exception::write_no_exception(reply);
                // DisplayStatInfo { long vsyncTime; long vsyncPeriod; }
                reply.write_int64(now_ns());
                reply.write_int64(PERIOD_NS_60HZ);
                Ok(true)
            }
            codes::GET_DISPLAY_STATE => {
                let _display = data.read_strong_binder().map_err(perr)?;
                exception::write_no_exception(reply);
                // DisplayState { int layerStack; Rotation orientation; Size
                // layerStackSpaceRect; } — Rotation is @Backing(type="int") and
                // Size is {int width; int height}.
                reply.write_int32(self.display.layer_stack);
                reply.write_int32(self.display.orientation);
                reply.write_int32(self.display.width);
                reply.write_int32(self.display.height);
                Ok(true)
            }
            codes::GET_MAX_ACQUIRED_BUFFER_COUNT => {
                exception::write_no_exception(reply);
                // The stock answer is small; the release contract (not the
                // pool depth) is what keeps a session's frame budget bounded
                // (W11). Never a display-pipeline number larger than the phone's.
                let acquired = std::env::var("ACOS_MAX_ACQUIRED_BUFFERS")
                    .ok()
                    .and_then(|v| v.trim().parse::<u32>().ok())
                    .filter(|v| *v > 0 && *v <= 64)
                    .unwrap_or(DEFAULT_MAX_ACQUIRED_BUFFERS);
                reply.write_int32(acquired as i32);
                Ok(true)
            }
            _ => Err(ServiceError::UnsupportedAndroidApi {
                interface: DESCRIPTOR.to_string(),
                code,
                method: method_name(code).map_or_else(|| format!("code {code}"), str::to_string),
            }),
        }
    }
}

/// BLAST's acquired-buffer budget when `ACOS_MAX_ACQUIRED_BUFFERS` is unset.
///
/// The stock-like value (see the module docs); the buffer *pool* is
/// `maxDequeued(2) + this`, and the release path (not the pool) is what makes a
/// long session allocate a bounded number of buffers (W11: 2 allocations across
/// 832 stored frames).
pub const DEFAULT_MAX_ACQUIRED_BUFFERS: u32 = 2;

/// Best-effort method name for a code, for the structured-unsupported record.
#[must_use]
pub fn method_name(code: u32) -> Option<&'static str> {
    Some(match code {
        codes::BOOT_FINISHED => "bootFinished",
        codes::CREATE_DISPLAY_EVENT_CONNECTION => "createDisplayEventConnection",
        codes::CREATE_CONNECTION => "createConnection",
        codes::GET_PHYSICAL_DISPLAY_IDS => "getPhysicalDisplayIds",
        codes::GET_PHYSICAL_DISPLAY_TOKEN => "getPhysicalDisplayToken",
        codes::GET_SUPPORTED_FRAME_TIMESTAMPS => "getSupportedFrameTimestamps",
        codes::GET_DISPLAY_STATS => "getDisplayStats",
        codes::GET_DISPLAY_STATE => "getDisplayState",
        codes::GET_MAX_ACQUIRED_BUFFER_COUNT => "getMaxAcquiredBufferCount",
        _ => return None,
    })
}

fn perr(e: impl std::fmt::Display) -> ServiceError {
    ServiceError::Internal(format!("{DESCRIPTOR}: parcel: {e}"))
}

/// The auxiliary binder descriptor `getPhysicalDisplayToken` answers with.
pub const DISPLAY_TOKEN_DESCRIPTOR: &str = "android.gui.PhysicalDisplayToken";

/// Install the `SurfaceFlinger` node and its connection pool on `host`.
///
/// Returns the pool so a test (or the runtime) can inspect the connection
/// objects without going through `ServiceHost::as_any_mut`.
pub fn install(
    host: &mut crate::host::ServiceHost,
    display: DisplayDescriptor,
) -> Arc<Mutex<ConnectionPool>> {
    install_full(host, display).0
}

/// Install both halves and return `(frame-clock pool, surface state)`.
///
/// `registry::install` only needs the pool (it is what
/// `createDisplayEventConnection` allocates from); a test that drives
/// `createConnection`/`createSurface`/`setTransactionState` needs the surface
/// state to assert the latest-buffer store and the queued callbacks without a
/// guest process.
pub fn install_full(
    host: &mut crate::host::ServiceHost,
    display: DisplayDescriptor,
) -> (Arc<Mutex<ConnectionPool>>, Arc<Mutex<SurfaceState>>) {
    let surface = surface_client::install(host, display);
    let pool = display_events::install(host, CONNECTION_POOL_SIZE);
    let token = host.register_aux(DISPLAY_TOKEN_DESCRIPTOR);
    // `raw_transact`-served (no `acos-aidl` table for `android.gui`), but named,
    // because `ComposerServiceAIDL` reaches it through `getService("SurfaceFlinger")`.
    host.register_dynamic_named(
        SERVICE_NAME,
        DESCRIPTOR,
        Box::new(SurfaceComposerService::new(
            Arc::clone(&pool),
            display,
            token,
            Arc::clone(&surface),
        )),
    );
    // Same node, the name API-34 `libgui` actually asks for (see
    // `AIDL_SERVICE_NAME`).
    host.alias_service_name(AIDL_SERVICE_NAME, DESCRIPTOR);
    (pool, surface)
}

/// How many `IDisplayEventConnection` nodes are pre-allocated.
///
/// One `Choreographer` per process means one live connection; the pool exists so
/// a second `DisplayEventReceiver` (a second thread, or a second launch in the
/// same host) gets a routable binder without the host having to allocate ids at
/// call time.
pub const CONNECTION_POOL_SIZE: usize = 4;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_codes_are_pinned() {
        // Read out of the image's `BpSurfaceComposer::<method>` immediates.
        assert_eq!(codes::CREATE_DISPLAY_EVENT_CONNECTION, 2);
        assert_eq!(codes::CREATE_CONNECTION, 3);
        assert_eq!(codes::GET_PHYSICAL_DISPLAY_IDS, 6);
        assert_eq!(codes::GET_PHYSICAL_DISPLAY_TOKEN, 7);
        assert_eq!(codes::GET_SUPPORTED_FRAME_TIMESTAMPS, 8);
        assert_eq!(codes::GET_DISPLAY_STATS, 10);
        assert_eq!(codes::GET_DISPLAY_STATE, 11);
        // The image, not the r75 tag (the tag's declaration order gives 65).
        assert_eq!(codes::GET_MAX_ACQUIRED_BUFFER_COUNT, 57);
    }

    #[test]
    fn method_names_cover_every_implemented_code() {
        for code in [
            codes::BOOT_FINISHED,
            codes::CREATE_DISPLAY_EVENT_CONNECTION,
            codes::GET_PHYSICAL_DISPLAY_IDS,
            codes::GET_PHYSICAL_DISPLAY_TOKEN,
            codes::GET_SUPPORTED_FRAME_TIMESTAMPS,
            codes::GET_DISPLAY_STATS,
            codes::GET_DISPLAY_STATE,
            codes::GET_MAX_ACQUIRED_BUFFER_COUNT,
        ] {
            assert!(method_name(code).is_some(), "code {code} has no name");
        }
        assert!(method_name(9999).is_none());
    }

    #[test]
    fn reference_display_is_the_one_display_manager_reports() {
        let d = DisplayDescriptor::reference();
        assert_eq!(d.display_id, 0);
        assert_eq!((d.width, d.height), (1080, 2400));
        assert_eq!(d.orientation, 0);
    }
}
