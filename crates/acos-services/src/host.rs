//! `ServiceHost` — the in-process Binder dispatcher.
//!
//! The later native layer calls
//!
//! ```text
//! host.transact(binder_handle, code, &data, &mut reply, flags) -> Result<()>
//! ```
//!
//! `transact` peeks the interface token, looks up `(descriptor, code)` in the
//! **mechanically decoded** `acos-aidl` tables (never a hand-typed code table),
//! decodes the request by the decoded `WireOp` layout, invokes the lazily
//! constructed service state machine, and writes the exception header plus the
//! decoded reply layout. Unknown pairs are a structured
//! `UNSUPPORTED_ANDROID_API`, never a default success.
//!
//! ## Intentional deviations from AOSP (keep both; `docs/FINDINGS.md` F-0015)
//!
//! The `android.os.IServiceManager` surface follows AOSP's
//! `frameworks/native/cmds/servicemanager/ServiceManager.cpp` (registered names
//! answer `Status::ok` with a possibly-null binder; `unregisterForNotifications`
//! answers `EX_ILLEGAL_STATE` when nothing is pending) with two deliberate
//! differences:
//!
//! * `registerForNotifications` for a name that is **neither registered nor
//!   declared by a shipped module** is refused with `EX_ILLEGAL_ARGUMENT`
//!   naming it. AOSP accepts it and never fires the callback; ACOS has no VINTF
//!   manifest and no lazy service start, so such a name can never appear and
//!   waiting for it would hang the caller forever.
//! * `isDeclared` answers from the **loaded module manifests**
//!   (`ModuleManifest::provided_names`), not a VINTF device manifest, which ACOS
//!   does not have. This is what gives `waitForDeclaredService`/`waitForService`
//!   something real to wait on.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::sync::Mutex;

use acos_aidl::{InterfaceInfo, MethodInfo, WireOp};
use acos_parcel::{BinderId, BinderKind, BinderRegistry, Parcel};
use acos_protocol::Event;

use crate::codec;
use crate::error::{codes, Result, ServiceError};
use crate::exception::{write_exception, write_no_exception, EX_SERVICE_SPECIFIC};
use crate::trace::{self, CallTrace, TraceSink};

/// Ceiling on binder owner records (one per app binder the service layer saw).
///
/// A real session exports a handful per process (`IApplicationThread`, a few
/// `IInterface` callbacks); 8192 is generous, and exceeding it is counted, not
/// hidden.
pub const MAX_BINDER_OWNERS: usize = 8192;

/// A binder-to-binder delivery the native layer must enqueue on the target
/// looper (the `system -> app` direction).
#[derive(Debug)]
pub struct PendingDelivery {
    /// Target binder (an app-side object such as `IApplicationThread`).
    pub target: BinderId,
    /// AIDL interface descriptor.
    pub interface: String,
    /// Transaction code.
    pub code: u32,
    /// Method name.
    pub method: String,
    /// Fully encoded request parcel (interface token already written).
    pub data: Parcel,
}

/// One `IServiceManager.registerForNotifications` request waiting for its name
/// to appear (ADR-0029: a module that is not attached yet).
#[derive(Clone, Debug)]
struct PendingNotification {
    /// Service name the client is waiting for.
    name: String,
    /// The client's `android.os.IServiceCallback` binder.
    callback: BinderId,
}

/// `android.os.IServiceManager` name, for the notification frame.
const SERVICE_MANAGER_DESCRIPTOR: &str = "android.os.IServiceManager";
/// `android.os.IServiceCallback`, the client's registration callback.
const SERVICE_CALLBACK_DESCRIPTOR: &str = "android.os.IServiceCallback";
/// `IServiceCallback.onRegistration(String, IBinder)`.
const SERVICE_CALLBACK_ON_REGISTRATION: u32 = 1;

/// Bit 32 of a binder id: every binder object owned by the **guest** JVM
/// arrives with it (`acos-binder-peer`'s `JAVA_TAG`, tagged where the parcel is
/// decoded). The driver owns those nodes, so a host reference on one is a
/// reference the driver can act on even though the host's [`BinderRegistry`]
/// never registered the handle itself.
pub const GUEST_BINDER_TAG: u64 = 1 << 32;

/// The `IServiceCallback.onRegistration(@utf8InCpp String, IBinder)` frame.
///
/// The AIDL is `oneway interface IServiceCallback { void onRegistration(
/// @utf8InCpp String name, IBinder binder); }`
/// (`frameworks/native/libs/binder/aidl/android/os/IServiceCallback.aidl`), so the
/// name is `Parcel::writeUtf8` (= `writeString8`) and a oneway frame carries no
/// exception header.
fn registration_delivery(callback: BinderId, name: &str, binder: BinderId) -> PendingDelivery {
    let mut data = Parcel::new();
    let _ = data.write_interface_token(SERVICE_CALLBACK_DESCRIPTOR);
    let _ = data.write_string8(name);
    data.write_strong_binder(binder);
    PendingDelivery {
        target: callback,
        interface: SERVICE_CALLBACK_DESCRIPTOR.to_string(),
        code: SERVICE_CALLBACK_ON_REGISTRATION,
        method: "onRegistration".to_string(),
        data,
    }
}

/// The wakeup a `system -> app` delivery needs when it is queued **outside**
/// the binder peer's turn.
///
/// A delivery produced while the peer is answering a transaction rides the
/// reply: `Peer::handle` drains the queue at the end of its turn. A delivery
/// produced by another thread (the launcher injecting a key that makes the
/// in-process IME hide itself, a compositor callback) has no turn to ride, and
/// the peer is blocked in `poll(2)` on its socket. `DeliveryWake` is that
/// missing edge: the producer writes one byte, the peer's idle wait returns and
/// it drains the queue.
///
/// This is a self-pipe, not a timer: an idle session with nothing queued never
/// wakes (the 1 s receive timeout this replaces only noticed `stop`).
#[derive(Debug)]
pub struct DeliveryWake {
    reader: std::os::unix::net::UnixStream,
    writer: std::os::unix::net::UnixStream,
}

impl DeliveryWake {
    /// Create the pair. The owner keeps it for the session's lifetime.
    ///
    /// A `SOCK_STREAM` socketpair is `CLOEXEC`-like (Rust creates it with
    /// `SOCK_CLOEXEC`, so no fd leaks into the guest) and a non-blocking write
    /// of one byte can never park a producer.
    ///
    /// # Errors
    ///
    /// `socketpair(2)`'s error, or the one from setting non-blocking mode.
    pub fn new() -> std::io::Result<Self> {
        use std::os::unix::net::UnixStream;
        let (reader, writer) = UnixStream::pair()?;
        reader.set_nonblocking(true)?;
        writer.set_nonblocking(true)?;
        Ok(Self { reader, writer })
    }

    /// The end the peer's idle wait polls.
    #[must_use]
    pub fn read_fd(&self) -> std::os::unix::io::RawFd {
        use std::os::unix::io::AsRawFd as _;
        self.reader.as_raw_fd()
    }

    /// Signal that a delivery is queued. Coalescing is intentional: one byte
    /// means "drain the queue", and the drain empties it.
    pub fn wake(&self) {
        use std::io::Write as _;
        let byte = 1u8;
        // A failed write means the peer is gone; the delivery is then moot and
        // this must never become a producer-side error.
        let mut writer = &self.writer;
        let _ = writer.write(&[byte]);
    }

    /// Consume every pending wakeup byte (the queue, not the count, is the
    /// state).
    pub fn drain(&self) {
        use std::io::Read as _;
        let mut buf = [0u8; 64];
        let mut reader = &self.reader;
        while let Ok(read) = reader.read(&mut buf) {
            if read == 0 {
                return;
            }
        }
    }
}

/// The result of one service handler call.
#[derive(Debug, Default)]
pub struct CallResult {
    /// Reply values in decoded order.
    pub reply: Vec<ValueOut>,
    /// `system -> app` deliveries to enqueue.
    pub deliveries: Vec<PendingDelivery>,
}

/// Alias so the public signature reads clearly.
pub type ValueOut = crate::value::Value;

/// A service state machine.
pub trait ServiceObject: Send {
    /// Handle one decoded transaction.
    fn call(
        &mut self,
        interface: &str,
        method: &MethodInfo,
        args: &[crate::value::Value],
    ) -> Result<CallResult>;

    /// Drain lifecycle/log events produced by this service.
    fn drain_events(&mut self) -> Vec<Event> {
        Vec::new()
    }

    /// Downcast hook so the host can configure a specific service (for
    /// example, point the package manager at the APK being launched). Default
    /// services are not configurable this way.
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        None
    }

    /// Handle a transaction whose wire format the decoded `WireOp` vocabulary
    /// cannot express. `data` is positioned just after the interface token and
    /// the implementation writes the **whole** reply (exception header first).
    ///
    /// The one interface that needs this today is `IBulkCursor`: it is a
    /// hand-written `Binder` subclass (no generated `$Stub`), so it has no
    /// entry in the `acos-aidl` tables, and its `getWindow` reply is a
    /// `CursorWindow` frame rather than an AIDL type.
    fn raw_transact(
        &mut self,
        interface: &str,
        code: u32,
        data: &mut Parcel,
        reply: &mut Parcel,
    ) -> Result<bool> {
        let _ = (interface, code, data, reply);
        Ok(false)
    }

    /// `IContentProvider.query`: a provider returns its rows here and the host
    /// allocates the `IBulkCursor` binder and writes the framework's
    /// `BulkCursorDescriptor` + `CursorWindow` reply.
    ///
    /// `data` is positioned after the interface token. `Ok(None)` means the
    /// object does not serve `query` (the dispatcher then reports the usual
    /// structured unsupported error).
    fn content_query(
        &mut self,
        interface: &str,
        data: &mut Parcel,
    ) -> Result<Option<crate::cursor::CursorResult>> {
        let _ = (interface, data);
        Ok(None)
    }

    /// `true` when a dynamically registered object (an `IBulkCursor`) is done
    /// and the host may drop it after this transaction.
    fn finished(&self) -> bool {
        false
    }

    /// Reverse transactions a `raw_transact` handler queued during the call.
    ///
    /// The decoded path returns deliveries in [`CallResult`]; a raw handler
    /// (the compositor's `ITransactionCompletedListener` callbacks) has no
    /// `CallResult`, so it parks them here and the dispatcher appends them to
    /// the host queue that the runtime already turns into frames (C1).
    fn drain_raw_deliveries(&mut self) -> Vec<PendingDelivery> {
        Vec::new()
    }

    /// The next instant at which this object's **own schedule** will have a
    /// `system -> app` delivery ready (`None` = it has no timed feed).
    ///
    /// A timed feed is driven by the peer, not by a timer thread: the peer's
    /// idle turn asks for this, sleeps until exactly that instant (or until a
    /// guest frame arrives, whichever is first) and then calls
    /// [`Self::advance_timed`]. That is what keeps an idle session with no timed
    /// module at zero wakeups, and it is why a scheduled delivery lands on its
    /// deadline instead of on the next poll.
    fn next_delivery_deadline(&self) -> Option<std::time::Instant> {
        None
    }

    /// Advance this object's schedule to `now`, queueing every delivery that is
    /// due (and at most one extra, so a late wakeup never storms the guest).
    ///
    /// Called only from [`ServiceHost::take_timed_deliveries`], i.e. from the
    /// peer's idle turn, for the same locking reason as
    /// [`Self::drain_raw_deliveries`].
    fn advance_timed(&mut self, now: std::time::Instant) {
        let _ = now;
    }

    /// Binders this handler started holding a strong reference to since the
    /// last drain (a registered callback, a stored `IIntentReceiver`, ...).
    ///
    /// A registration *is* a reference: the framework's `Parcel` took one when
    /// the binder was flattened, and the service that stored the handle owns it
    /// until it drops the registration. Reporting it here is what lets the host
    /// keep the guest's node alive for as long as the registration exists and
    /// release it (→ `BR_RELEASE` + `BR_DECREFS`) when it does not.
    ///
    /// Drained after **both** dispatch paths (the decoded `acos-aidl` `call`
    /// and `raw_transact`), so a module uses one mechanism wherever its
    /// interface is served. The host applies the retained set before the
    /// released one, so a handler that re-registers the same binder in one
    /// transaction nets out.
    fn drain_retained(&mut self) -> Vec<BinderId> {
        Vec::new()
    }

    /// Binders the handler dropped its registration (strong reference) for.
    fn drain_released(&mut self) -> Vec<BinderId> {
        Vec::new()
    }

    /// Drop **every** reference this handler still holds and return them.
    ///
    /// Called when the handler goes away while the guest may still be alive:
    /// module detach (the object is dropped, so a later transient can never
    /// report the release) and session teardown. A handler holds its
    /// registrations in shared state, so the release has to be reported here
    /// rather than in `Drop` (which has no host to report to).
    fn release_all_refs(&mut self) -> Vec<BinderId> {
        Vec::new()
    }
}

/// A node a service created while answering a transaction, waiting for the
/// host to install it (see [`NodeRegistrar`]).
pub struct PendingNode {
    binder: BinderId,
    descriptor: &'static str,
    object: Box<dyn ServiceObject>,
}

/// The handle a service uses to create a **new host node mid-transaction**.
///
/// Stock Android does exactly this inside `IWindowManager.openSession`: the
/// system process runs `new Session(this, callback)` and answers the caller
/// with that new `IWindowSession` binder, so every app process talks to its own
/// session object. ACOS's `ServiceHost` owns its dispatch table (`&mut self`
/// during `transact`), so a service cannot insert an entry itself; it allocates
/// the binder and parks the object here, and the host installs it when the
/// transaction that created it returns — before the caller can use the handle.
#[derive(Clone)]
pub struct NodeRegistrar {
    registry: Arc<BinderRegistry>,
    owners: Arc<Mutex<std::collections::HashMap<u64, u32>>>,
    pending: Arc<Mutex<Vec<PendingNode>>>,
}

impl std::fmt::Debug for NodeRegistrar {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NodeRegistrar")
            .field(
                "pending",
                &self.pending.lock().map(|p| p.len()).unwrap_or(0),
            )
            .finish()
    }
}

impl NodeRegistrar {
    /// Allocate a binder and park `object` for installation under it.
    pub fn register(&self, descriptor: &'static str, object: Box<dyn ServiceObject>) -> BinderId {
        let binder = self.registry.register_local(descriptor);
        self.pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(PendingNode {
                binder,
                descriptor,
                object,
            });
        binder
    }

    /// The app process that owns `binder`, when the host recorded it.
    ///
    /// This is the same record the host keeps: a binder an app process handed
    /// over in a request parcel is owned by that process. A window session uses
    /// it to learn which process its `IWindow` belongs to (stock
    /// `Session.mClientPid`, which AOSP reads from `Binder.getCallingPid`).
    #[must_use]
    pub fn binder_owner(&self, binder: BinderId) -> Option<u32> {
        if binder.is_null() {
            return None;
        }
        self.owners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&binder.0)
            .copied()
    }
}

/// Wiring handed to a lazily-constructed service.
#[derive(Clone, Debug)]
pub struct Wiring {
    /// This service's own binder handle.
    pub self_binder: BinderId,
    /// The `IActivityClientController` binder (for ATMS).
    pub controller: BinderId,
    /// The `IContentProvider` binder for `settings`.
    pub content_provider: BinderId,
    /// The `IPackageInstaller` binder (`IPackageManager.getPackageInstaller`).
    /// It is an aux node: a package query names it, but ACOS never installs.
    pub package_installer: BinderId,
    /// MPROC5c: how a service creates a new host node while answering a
    /// transaction (AOSP's `IWindowManager.openSession` → `new Session(...)`).
    pub nodes: NodeRegistrar,
}

impl Default for Wiring {
    fn default() -> Self {
        Self {
            self_binder: BinderId(0),
            controller: BinderId(0),
            content_provider: BinderId(0),
            package_installer: BinderId(0),
            nodes: NodeRegistrar {
                registry: Arc::new(BinderRegistry::new()),
                owners: Arc::new(Mutex::new(std::collections::HashMap::new())),
                pending: Arc::new(Mutex::new(Vec::new())),
            },
        }
    }
}

type Factory = Arc<dyn Fn(Wiring) -> Box<dyn ServiceObject> + Send + Sync>;

/// `(sender binder, code, fill-in) -> evidence` for one `IIntentSender.send`.
///
/// The delivery is `AmsState::send_pending_intent_with_fill_in`, which owns the
/// sender table, the `Intent.fillIn` merge and the activity/broadcast/service
/// start paths; publishing a closure keeps that state out of the module API
/// while still letting a module fire a sender for real:
///
/// * `notifications` `tap`/`action` pass `None` (a notification action carries
///   no fill-in),
/// * `location` passes a fill-in whose extras carry the fix, exactly like
///   `LocationProviderManager$LocationPendingIntentTransport`.
pub type FireSender = Arc<
    dyn Fn(
            u64,
            i32,
            Option<crate::ams::IntentFacts>,
        ) -> crate::error::Result<crate::pending_intent::SendEvidence>
        + Send
        + Sync,
>;

/// The registration tables as they were at [`ServiceHost::begin_scope`].
///
/// ADR-0029 requires that detaching a module leaves **no** registration behind,
/// including the ones a module registered indirectly (an aux binder, a dynamic
/// node, an alias). Keeping the pre-attach snapshot is the only way to prove
/// that by construction instead of trusting each module's `detach` to enumerate
/// what its `attach` did.
pub struct RegistrationScope {
    entries: Vec<&'static str>,
    names: HashMap<&'static str, &'static str>,
    aux: HashMap<&'static str, BinderId>,
    dynamic: Vec<u64>,
    binder_descriptors: HashMap<u64, &'static str>,
    notification: Option<std::sync::Arc<std::sync::Mutex<crate::declared::NotificationState>>>,
    permission_store: Option<std::sync::Arc<std::sync::Mutex<crate::permissions::PermissionStore>>>,
    pending_intent_fire: Option<FireSender>,
    /// Index of this scope's overwrite log in [`ServiceHost::overwrites`].
    ///
    /// A module that replaces a `core` name registers the same descriptor, so
    /// deleting what was added cannot describe the rollback: `end_scope` must
    /// restore the **factory and binder** that descriptor had before. Without
    /// this a detached module's factory kept answering (MODNOTIF2).
    depth: usize,
}

impl core::fmt::Debug for RegistrationScope {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // `FireSender` is a closure, which has no `Debug`; every other field is
        // sized data a caller may want in a log.
        f.debug_struct("RegistrationScope")
            .field("entries", &self.entries.len())
            .field("names", &self.names.len())
            .field("aux", &self.aux.len())
            .field("dynamic", &self.dynamic.len())
            .field("binder_descriptors", &self.binder_descriptors.len())
            .field("notification", &self.notification.is_some())
            .field("permission_store", &self.permission_store.is_some())
            .field("pending_intent_fire", &self.pending_intent_fire.is_some())
            .field("depth", &self.depth)
            .finish()
    }
}

struct ServiceEntry {
    binder: BinderId,
    factory: Factory,
    object: Option<Box<dyn ServiceObject>>,
}

/// The dispatcher.
pub struct ServiceHost {
    /// `Arc` because a service that must mint a **fresh** binder handle while
    /// answering a transaction cannot reach `&mut ServiceHost`: the IME's
    /// `InputBindResult` channel token is one `openInputChannelPair`'s own
    /// `BBinder` (see [`crate::ime::ImeState::start_input`]), so the IME state
    /// holds an `Arc` of the same registry the host allocates from.
    registry: Arc<BinderRegistry>,
    /// One entry per registered descriptor.
    entries: HashMap<&'static str, ServiceEntry>,
    /// Descriptor for every binder handle (including secondary binders).
    binder_descriptors: HashMap<u64, &'static str>,
    /// Service-manager name -> descriptor.
    names: HashMap<&'static str, &'static str>,
    /// Extra binders that are not top-level services (`IActivityClientController`,
    /// `IContentProvider`).
    aux: HashMap<&'static str, BinderId>,
    /// Binders created while answering a transaction (the `IBulkCursor` a
    /// `query` hands back), keyed by binder id.
    dynamic: HashMap<u64, Box<dyn ServiceObject>>,

    /// `ServiceManager` names a shipped module may provide later: AOSP's VINTF
    /// declaration (`isDeclared`), which is what lets `waitForService` wait for a
    /// service that is not running yet.
    declared: Vec<&'static str>,
    /// Pending `IServiceManager.registerForNotifications` callbacks (ADR-0029:
    /// a live module attach is the event they wait for).
    notifications: Vec<PendingNotification>,

    /// Owner process tag for every binder that lives in an app process
    /// (MPROC2 step 3a). A host service node has no entry; an app binder is
    /// recorded the first time its process hands it to the service layer
    /// (`addService`, `attachApplication`, any request parcel).
    binder_owners: Arc<Mutex<HashMap<u64, u32>>>,
    /// Binder records refused because the table was full (never a silent drop).
    binder_owner_drops: u64,
    /// Nodes services parked during a transaction ([`NodeRegistrar`]).
    pending_nodes: Arc<Mutex<Vec<PendingNode>>>,
    /// The registrar handed to every lazily-constructed service.
    nodes: NodeRegistrar,
    deliveries: Vec<PendingDelivery>,
    /// Host-held strong references per binder: one per live registration
    /// (telephony listener, `IIntentReceiver`, `IAccessibilityManagerClient`,
    /// PendingIntent sender, ...). The count is what the peer reports to the
    /// driver as `F_REF`, so a node exists exactly while the host references it.
    refs: HashMap<u64, u32>,
    /// Binders whose last host reference was dropped and that the peer still has
    /// to tell the driver about (`F_REF` release).
    released_binders: Vec<BinderId>,
    trace: Option<Box<dyn TraceSink>>,
    seq: AtomicU64,
    constructions: u64,
    /// Binder handles the guest still holds from a **detached** module, mapped
    /// to the module id. ADR-0029 item 2: a detach removes the registration, but
    /// the guest's `BinderProxy` still carries the old handle; a transaction on
    /// it must answer the structured "module is off, attach it" error naming the
    /// module, never a bare `unknown binder handle` that no agent can act on.
    dead: HashMap<u64, &'static str>,
    /// The `INotificationManager` state, kept here so the runtime (which plays
    /// the system-UI role) can surface a toast the app asked for. Android shows
    /// a text toast from `NotificationManagerService` in the system process;
    /// ACOS has one process, so the runtime renders the recorded request.
    notification: Option<std::sync::Arc<std::sync::Mutex<crate::declared::NotificationState>>>,
    /// The accessibility module's state, when a session installed it. The
    /// dispatcher needs it for the two system -> app steps that are not an
    /// `IAccessibilityManager` call: capturing the app thread from
    /// `attachApplication` and answering `IActivityManager.publishService`
    /// (MODA11Y2).
    /// Held **weakly**: the accessibility state's lifetime must end with the
    /// registration that owns it (ADR-0029 detach), and a strong handle here
    /// would keep the settings provider's generation memfd alive after detach
    /// (the conformance kit's `detach-restores` baseline).
    a11y: Option<std::sync::Weak<std::sync::Mutex<crate::misc::A11yState>>>,
    /// The `IAccessibilityServiceConnection` node handed to a bound service.
    a11y_connection: Option<BinderId>,
    /// The display's accessibility-overlay window token (`init`'s third
    /// argument; `getOverlayWindowToken` answers it).
    a11y_window_token: Option<BinderId>,
    /// The one runtime-permission table `ActivityManager.checkPermission`,
    /// `PackageManager.checkPermission` and `IPermissionManager` all read
    /// (`crate::permissions`). Modules that own the name share it through this
    /// slot, exactly like the notification state.
    permission_store: Option<std::sync::Arc<std::sync::Mutex<crate::permissions::PermissionStore>>>,
    /// MPROC8: this session runs **real app processes**, so the host performs
    /// AOSP's `ActiveServices` work for a guest `bindService` (create/bind the
    /// service in its own process, answer `IServiceConnection.connected` on
    /// `publishService`, unbind/stop, and report a dead process). The in-process
    /// runtime keeps its own bind path (`AmsState::pending_binds`), so the flag
    /// is off there: one implementation per seam, chosen by what the session
    /// actually is.
    host_side_services: bool,
    /// The one way to *fire* a `PendingIntent` sender (MODNOTIF4). A module
    /// owns the notification records but the sender table and the activity
    /// start live in the AMS, so the registry publishes the delivery here
    /// instead of a module reaching into the ATMS state.
    pending_intent_fire: Option<FireSender>,
    /// The host paths of this session's WebView endpoints, published by the
    /// launcher (WV8 `webview` module). The module answers `status` from them
    /// instead of recomputing a path the launcher already decided, so the two
    /// cannot name different sockets.
    webview_gate: Option<WebviewGate>,
    /// Overwrite logs, one per open [`RegistrationScope`].
    overwrites: Vec<Vec<(&'static str, BinderId, Factory)>>,
    /// Where a producer signals that it queued a `system -> app` delivery with
    /// no transaction to ride (see [`DeliveryWake`]). `None` keeps the host
    /// synchronous-only: the VM transport never has an idle peer to wake.
    delivery_wake: Option<Arc<DeliveryWake>>,
}

/// The host endpoints the launcher bound for a session's WebView (ADR-0031).
///
/// `socket` is the per-session gate the guest connects to (mounted at
/// `/run/acos/webview.sock`); `broker_socket` is the shared daemon's endpoint,
/// which exists only while a broker runs (one Chromium per host).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WebviewGate {
    /// The session's gate socket (host path).
    pub socket: Option<std::path::PathBuf>,
    /// The shared broker's endpoint (host path), if the launcher named one.
    pub broker_socket: Option<std::path::PathBuf>,
}

impl core::fmt::Debug for ServiceHost {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ServiceHost")
            .field("registered_descriptors", &self.entries.len())
            .field("registered_names", &self.names.len())
            .field("aux", &self.aux.len())
            .field("constructions", &self.constructions)
            .field("deliveries", &self.deliveries.len())
            .field("trace", &self.trace.is_some())
            .finish()
    }
}

impl Default for ServiceHost {
    fn default() -> Self {
        Self::new()
    }
}

impl ServiceHost {
    /// An empty host. Call [`ServiceHost::install_standard`] to register the
    /// ACOS service set.
    #[must_use]
    pub fn new() -> Self {
        let registry = Arc::new(BinderRegistry::new());
        let binder_owners = Arc::new(Mutex::new(HashMap::new()));
        let pending_nodes: Arc<Mutex<Vec<PendingNode>>> = Arc::new(Mutex::new(Vec::new()));
        Self {
            registry: Arc::clone(&registry),
            binder_owners: Arc::clone(&binder_owners),
            pending_nodes: Arc::clone(&pending_nodes),
            nodes: NodeRegistrar {
                registry,
                owners: binder_owners,
                pending: pending_nodes,
            },
            entries: HashMap::new(),
            binder_descriptors: HashMap::new(),
            names: HashMap::new(),
            aux: HashMap::new(),
            dynamic: HashMap::new(),

            declared: Vec::new(),
            notifications: Vec::new(),
            host_side_services: false,

            binder_owner_drops: 0,
            deliveries: Vec::new(),
            dead: HashMap::new(),
            refs: HashMap::new(),
            released_binders: Vec::new(),
            trace: None,
            seq: AtomicU64::new(1),
            constructions: 0,
            notification: None,
            a11y: None,
            a11y_connection: None,
            a11y_window_token: None,
            permission_store: None,
            pending_intent_fire: None,
            webview_gate: None,
            overwrites: Vec::new(),
            delivery_wake: None,
        }
    }

    /// MPROC5c: the handle a service uses to create a new host node while
    /// answering a transaction.
    #[must_use]
    pub fn node_registrar(&self) -> NodeRegistrar {
        self.nodes.clone()
    }

    /// Install every node a service parked during the transaction that just
    /// ran.
    ///
    /// Called by [`ServiceHost::transact`] before the reply is handed back, so
    /// the binder a reply names (a fresh `IWindowSession`) is routable the
    /// moment the caller can use it. The peer's `direct_dispatch` path (a
    /// registered service reached around the ownership marker) runs the object
    /// itself, so it calls this too.
    pub fn install_pending_nodes(&mut self) -> usize {
        let pending: Vec<PendingNode> = {
            let mut queue = self
                .pending_nodes
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            std::mem::take(&mut *queue)
        };
        let count = pending.len();
        for node in pending {
            self.binder_descriptors
                .insert(node.binder.0, node.descriptor);
            self.dynamic.insert(node.binder.0, node.object);
        }
        count
    }

    /// Drop a dynamically registered node (the object that owned it is gone:
    /// a window session's process died). A later transaction on the handle
    /// fails structurally instead of reaching a stale object.
    pub fn drop_dynamic_node(&mut self, binder: BinderId) -> bool {
        self.binder_descriptors.remove(&binder.0);
        self.registry.remove(binder);
        self.dynamic.remove(&binder.0).is_some()
    }

    /// Attach the session's delivery wakeup (ART transport).
    ///
    /// Services that queue deliveries from outside a transaction get the same
    /// handle through the registry, so the queue and the wake can never
    /// disagree about which session they belong to.
    pub fn set_delivery_wake(&mut self, wake: Arc<DeliveryWake>) {
        self.delivery_wake = Some(wake);
    }

    /// The session's delivery wakeup, when the transport has one.
    #[must_use]
    pub fn delivery_wake(&self) -> Option<Arc<DeliveryWake>> {
        self.delivery_wake.clone()
    }

    /// Register a service name and descriptor.
    pub fn register(
        &mut self,
        name: &'static str,
        descriptor: &'static str,
        factory: impl Fn(Wiring) -> Box<dyn ServiceObject> + Send + Sync + 'static,
    ) -> BinderId {
        let binder = self.registry.register_local(descriptor);
        self.binder_descriptors.insert(binder.0, descriptor);
        self.names.insert(name, descriptor);
        let factory: Factory = Arc::new(factory);
        // A registration over a name that exists is recorded so the open scope
        // (if any) can put the old factory back on rollback. Modules replacing a
        // `core` name do exactly this.
        if let Some(previous) = self.entries.get(descriptor) {
            if let Some(log) = self.overwrites.last_mut() {
                log.push((descriptor, previous.binder, Arc::clone(&previous.factory)));
            }
        }
        // MODTAIL2: a module may depend on a registration appearing later in
        // the same attach (the bluetooth/wifi nodes). Both hooks run.
        self.notify_registered(name);
        self.entries.insert(
            descriptor,
            ServiceEntry {
                binder,
                factory,
                object: None,
            },
        );
        binder
    }

    /// Register an object under a binder created now (a binder a service
    /// returns inside a reply, e.g. the `IBulkCursor` of a `query`).
    ///
    /// The object is addressed by id only; it has no `ServiceManager` name and
    /// is not a lazily constructed service.
    pub fn register_dynamic(
        &mut self,
        descriptor: &'static str,
        object: Box<dyn ServiceObject>,
    ) -> BinderId {
        let binder = self.registry.register_local(descriptor);
        self.binder_descriptors.insert(binder.0, descriptor);
        self.dynamic.insert(binder.0, object);
        binder
    }

    /// Number of live dynamic binder objects.
    #[must_use]
    pub fn dynamic_objects(&self) -> usize {
        self.dynamic.len()
    }

    /// Register a dynamic (`raw_transact`-served) node **and** give it a
    /// `ServiceManager` name.
    ///
    /// Interfaces outside the decoded `acos-aidl` tables (`android.gui.*`, the
    /// W-R compositor surface) have no factory to construct lazily, so they are
    /// registered by object; they still need a name because the framework
    /// reaches them through `ServiceManager.getService` (W-R T2:
    /// `getService("SurfaceFlinger")`).
    pub fn register_dynamic_named(
        &mut self,
        name: &'static str,
        descriptor: &'static str,
        object: Box<dyn ServiceObject>,
    ) -> BinderId {
        // One node per descriptor: a module taking over a name `core` serves
        // (`location`) replaces that node instead of adding a second binder with
        // the same descriptor, which would make `checkService` ambiguous and
        // leave a stray binder behind when one of them is removed.
        if self.dynamic_descriptor(descriptor).is_some() {
            self.remove_descriptor(descriptor);
        }
        let binder = self.register_dynamic(descriptor, object);
        self.names.insert(name, descriptor);
        self.notify_registered(name);
        binder
    }

    /// Drop any dynamic node registered under `descriptor` (there is at most one
    /// after [`Self::register_dynamic_named`]'s takeover rule, but a node may
    /// have been registered by [`Self::register_dynamic`] without a name).
    fn dynamic_descriptor(&self, descriptor: &str) -> Option<u64> {
        self.binder_descriptors
            .iter()
            .find(|(id, existing)| **existing == descriptor && self.dynamic.contains_key(id))
            .map(|(id, _)| *id)
    }

    fn remove_descriptor(&mut self, descriptor: &'static str) -> Option<BinderId> {
        let id = self.dynamic_descriptor(descriptor)?;
        self.dynamic.remove(&id);
        self.binder_descriptors.remove(&id);
        self.registry.remove(BinderId(id));
        Some(BinderId(id))
    }

    /// Give a descriptor that is already registered a second
    /// `ServiceManager` name (an alias).
    ///
    /// The API-34 image asks for the AIDL compositor under
    /// `SurfaceFlingerAIDL` (`SurfaceComposerClient.cpp:146`) while the older
    /// path uses `SurfaceFlinger`; both must resolve to the same node, and a
    /// second `register_dynamic_named` would allocate a second binder.
    pub fn alias_service_name(&mut self, name: &'static str, descriptor: &'static str) {
        self.names.insert(name, descriptor);
        self.notify_registered(name);
    }

    /// Register an auxiliary binder (a binder returned by another service).
    pub fn register_aux(&mut self, descriptor: &'static str) -> BinderId {
        let binder = self.registry.register_local(descriptor);
        self.binder_descriptors.insert(binder.0, descriptor);
        self.aux.insert(descriptor, binder);
        binder
    }

    /// Register the host-minted activity token of process `tag` (MPROC7).
    ///
    /// On a device the activity token is a `Binder` created in system_server
    /// (`ActivityRecord.token`). ACOS's system service side is the host, so the
    /// host mints it here. Its id carries `tag` in the same bits the peer uses
    /// for a guest-minted binder ([`crate::atms::process_tag_of`] reads bits
    /// 34..40), so "which process owns this activity" stays derivable from the
    /// token alone, exactly as for the guest-minted tokens of MPROC5.
    ///
    /// The node is routable: a transaction addressed to it reaches the host
    /// dispatcher like any other auxiliary node (the activity token is an
    /// opaque `Binder` to the framework; it is never used as an interface).
    pub fn register_activity_token(&mut self, tag: u32) -> BinderId {
        const PROCESS_TAG_SHIFT: u32 = 34;
        const PROCESS_TAG_MASK: u64 = 0x3f;
        const LOW_MASK: u64 = (1 << PROCESS_TAG_SHIFT) - 1;
        let binder = self.registry.register_local("android.app.ActivityToken");
        // Free the untagged id: only the tagged one is the token.
        self.registry.remove(binder);
        self.binder_descriptors.remove(&binder.0);
        let id = (u64::from(tag) & PROCESS_TAG_MASK) << PROCESS_TAG_SHIFT | (binder.0 & LOW_MASK);
        self.registry
            .register_at(id, "android.app.ActivityToken", BinderKind::Local);
        self.binder_descriptors
            .insert(id, "android.app.ActivityToken");
        BinderId(id)
    }

    /// Record which app process owns a binder (MPROC2 step 3a).
    ///
    /// Called whenever a process hands the service layer a binder of its own:
    /// `IServiceManager.addService`, `attachApplication` (its
    /// `IApplicationThread`) and every binder object in a request parcel. The
    /// record is what lets a delivery be addressed to the process that can
    /// receive it, and what a process teardown drops.
    ///
    /// The table is bounded: a guest that keeps exporting binders must not own
    /// an unbounded host allocation. A refusal is counted
    /// ([`ServiceHost::binder_owner_drops`]), never silently discarded, and the
    /// binder is still usable — only its owner record is missing.
    ///
    /// Returns `true` when the owner was recorded (or already was that owner).
    pub fn set_binder_owner(&mut self, binder: BinderId, tag: u32) -> bool {
        if binder.is_null() {
            return false;
        }
        let mut owners = self
            .binder_owners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(existing) = owners.get(&binder.0) {
            if *existing == tag {
                return true;
            }
        } else if owners.len() >= MAX_BINDER_OWNERS {
            drop(owners);
            self.binder_owner_drops = self.binder_owner_drops.saturating_add(1);
            eprintln!(
                "acos-services: ART_RESOURCE_LIMIT: binder owner table is full 
                 ({MAX_BINDER_OWNERS} records); binder {:#x} of process {tag} is not recorded",
                binder.0
            );
            return false;
        }
        owners.insert(binder.0, tag);
        true
    }

    /// The app process that owns `binder`, when it is an app binder.
    #[must_use]
    pub fn binder_owner(&self, binder: BinderId) -> Option<u32> {
        self.binder_owners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&binder.0)
            .copied()
    }

    /// Every binder recorded as owned by process `tag`, ascending.
    #[must_use]
    pub fn binders_of_process(&self, tag: u32) -> Vec<BinderId> {
        let owners = self
            .binder_owners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut v: Vec<BinderId> = owners
            .iter()
            .filter(|(_, owner)| **owner == tag)
            .map(|(id, _)| BinderId(*id))
            .collect();
        v.sort_unstable_by_key(|b| b.0);
        v
    }

    /// Drop every binder record owned by process `tag` (the process exited).
    ///
    /// Returns how many records were dropped. A later delivery to one of those
    /// ids fails structurally instead of reaching a stale handle.
    pub fn drop_process_binders(&mut self, tag: u32) -> usize {
        let mut owners = self
            .binder_owners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let before = owners.len();
        owners.retain(|_, owner| *owner != tag);
        before - owners.len()
    }

    /// Binder owner records refused because the table was full.
    #[must_use]
    pub fn binder_owner_drops(&self) -> u64 {
        self.binder_owner_drops
    }

    /// Look up an auxiliary binder.
    #[must_use]
    pub fn aux(&self, descriptor: &str) -> Option<BinderId> {
        self.aux.get(descriptor).copied()
    }

    /// The binder registry.
    #[must_use]
    pub fn binder_registry(&self) -> &BinderRegistry {
        &self.registry
    }

    /// The binder registry, shareable with a service that must mint a new
    /// handle per object (the IME's per-pair `InputChannel` token).
    #[must_use]
    pub fn binder_registry_arc(&self) -> Arc<BinderRegistry> {
        Arc::clone(&self.registry)
    }

    /// Every binder handle this host knows, as `(id, descriptor, kind)` sorted
    /// by id. Snapshot of the process binder-id layout (T-43/W102-A).
    #[must_use]
    pub fn export_binder_layout(&self) -> Vec<(u64, String, BinderKind)> {
        self.registry.binders()
    }

    /// Re-create the binder handles at their boot-process ids and re-point the
    /// dispatch tables at them (T-43/W102-A restore re-seed).
    ///
    /// A restored heap still carries the boot process's raw `mObject` /
    /// `mNativeData` values; without this a `BinderProxy` transaction reaches a
    /// fresh host whose ids were allocated in a different order and fails with
    /// `unknown binder handle`.
    pub fn import_binder_layout(&mut self, layout: &[(u64, String, BinderKind)]) {
        for (id, desc, kind) in layout {
            self.registry.register_at(*id, desc.clone(), *kind);
        }
        // Re-point the dispatch tables. Iterating by ascending id means the
        // highest id for a descriptor wins, matching the boot host (a later
        // `register` of the same descriptor, e.g. the recording ATMS wrapper,
        // is what `entries[descriptor].binder` held).
        for (id, desc, _) in layout {
            let static_desc = self
                .entries
                .keys()
                .copied()
                .find(|k| *k == desc.as_str())
                .or_else(|| self.aux.keys().copied().find(|k| *k == desc.as_str()));
            if let Some(d) = static_desc {
                self.binder_descriptors.insert(*id, d);
                if let Some(entry) = self.entries.get_mut(d) {
                    entry.binder = BinderId(*id);
                }
                if self.aux.contains_key(d) {
                    self.aux.insert(d, BinderId(*id));
                }
            }
        }
    }

    /// Remove the `ServiceManager` name `name` and whatever binder it resolved
    /// to, returning the freed handle.
    ///
    /// Used when a module attaches over a tombstone: the tombstone (a dynamic
    /// node with no factory) must disappear, otherwise the name keeps resolving
    /// to an object that reports "module not attached" while the module *is*
    /// attached. Entries registered with [`Self::register`] are removed too.
    pub fn remove_service(&mut self, name: &str) -> Option<BinderId> {
        let descriptor = self.names.remove(name)?;
        if let Some(entry) = self.entries.remove(descriptor) {
            self.binder_descriptors.remove(&entry.binder.0);
            self.registry.remove(entry.binder);
            return Some(entry.binder);
        }
        let binder = self.binder_for_descriptor(descriptor)?;
        self.dynamic.remove(&binder.0);
        self.aux.remove(descriptor);
        self.binder_descriptors.remove(&binder.0);
        self.registry.remove(binder);
        Some(binder)
    }

    /// Remember that a detached module used to answer `binder`, so the guest's
    /// stale proxy gets the structured "module is off" error (see `dead`).
    pub fn mark_detached_binder(&mut self, binder: BinderId, module: &'static str) {
        self.dead.insert(binder.0, module);
    }

    /// Drop every dead-handle record (a module re-attach replaces them).
    pub fn clear_detached_binders(&mut self, module: &str) {
        self.dead.retain(|_, owner| *owner != module);
    }

    /// Drop every strong reference `descriptor`'s handler holds on guest
    /// binders and release them on the host, returning what was released.
    ///
    /// This is the seam for a module that installs a feed/back-end into a node
    /// it does **not** own (the `sensors` module serves through `core`'s
    /// `sensorservice` node, whose lifetime is the session's): the object
    /// survives the module's scope, so `end_scope` does not see the
    /// registrations the module caused and the module's `detach` reports them
    /// here. A descriptor that was never constructed holds nothing.
    pub fn release_object_refs(&mut self, descriptor: &str) -> Vec<BinderId> {
        let held = match self.entries.get_mut(descriptor) {
            Some(entry) => match entry.object.as_mut() {
                Some(object) => object.release_all_refs(),
                None => Vec::new(),
            },
            None => {
                // A node registered with `register_dynamic` (the compositor, the
                // sensor/display event connections, `location`'s
                // `ILocationManager`) serves through the dynamic table.
                let Some(binder) = self.binder_for_descriptor(descriptor) else {
                    return Vec::new();
                };
                match self.dynamic.get_mut(&binder.0) {
                    Some(object) => object.release_all_refs(),
                    None => Vec::new(),
                }
            }
        };
        for id in &held {
            self.release_binder(*id);
        }
        held
    }

    /// Apply the reference changes every handler reported outside a binder
    /// transaction.
    ///
    /// A handler's `drain_retained`/`drain_released` runs after both dispatch
    /// paths, but a module can also answer a pending request from a **control
    /// op** (`hw.biometric`'s scripted `auth`, a fixture driving a module from
    /// the harness): there is no transaction to drain after, so without this
    /// the one-shot release would wait for the guest's next call — the node
    /// stays rooted (and the app's callback object stays alive) in between.
    ///
    /// Returns `(retained, released)` for evidence.
    pub fn drain_refs(&mut self) -> (Vec<BinderId>, Vec<BinderId>) {
        let mut retained: Vec<BinderId> = Vec::new();
        let mut released: Vec<BinderId> = Vec::new();
        for entry in self.entries.values_mut() {
            if let Some(object) = entry.object.as_mut() {
                retained.append(&mut object.drain_retained());
                released.append(&mut object.drain_released());
            }
        }
        for object in self.dynamic.values_mut() {
            retained.append(&mut object.drain_retained());
            released.append(&mut object.drain_released());
        }
        for id in &retained {
            self.retain_binder(*id);
        }
        for id in &released {
            self.release_binder(*id);
        }
        (retained, released)
    }

    /// Release **every** reference every live handler holds (session teardown:
    /// the guest process is gone, so its nodes must not stay rooted on a
    /// long-lived peer).
    pub fn release_all_refs(&mut self) -> Vec<BinderId> {
        let mut held: Vec<BinderId> = Vec::new();
        for entry in self.entries.values_mut() {
            if let Some(object) = entry.object.as_mut() {
                held.extend(object.release_all_refs());
            }
        }
        for object in self.dynamic.values_mut() {
            held.extend(object.release_all_refs());
        }
        for id in &held {
            self.release_binder(*id);
        }
        held
    }

    /// Snapshot the registration tables so a module's registrations can be
    /// rolled back as a unit (ADR-0029 detach).
    #[must_use]
    pub fn begin_scope(&mut self) -> RegistrationScope {
        self.overwrites.push(Vec::new());
        RegistrationScope {
            entries: self.entries.keys().copied().collect(),
            names: self.names.clone(),
            aux: self.aux.clone(),
            dynamic: self.dynamic.keys().copied().collect(),
            binder_descriptors: self.binder_descriptors.clone(),
            notification: self.notification.as_ref().map(std::sync::Arc::clone),
            permission_store: self.permission_store.as_ref().map(std::sync::Arc::clone),
            pending_intent_fire: self.pending_intent_fire.as_ref().map(std::sync::Arc::clone),
            depth: self.overwrites.len() - 1,
        }
    }

    /// Remove every registration made since `scope` was taken and return the
    /// binder handles that were freed.
    ///
    /// Names and aux binders that existed before the scope are restored (a
    /// module may legitimately re-register a name another module owns only if
    /// the manifest overlap check rejects it, so this is a safety net, not a
    /// feature). Binders are dropped from the [`BinderRegistry`] as well, so a
    /// subsequent `checkService` answers `null` exactly as it did before the
    /// attach.
    pub fn end_scope(&mut self, scope: RegistrationScope) -> Vec<BinderId> {
        let mut freed: Vec<BinderId> = Vec::new();
        let before_entries: std::collections::HashSet<&'static str> =
            scope.entries.iter().copied().collect();
        let added_entries: Vec<&'static str> = self
            .entries
            .keys()
            .copied()
            .filter(|key| !before_entries.contains(key))
            .collect();
        for key in added_entries {
            if let Some(mut entry) = self.entries.remove(key) {
                // The object is about to be dropped. Any callback registration
                // it still holds has to be released *now*: `Drop` has no host to
                // report to, and the guest may outlive the detach.
                if let Some(mut object) = entry.object.take() {
                    for id in object.release_all_refs() {
                        self.release_binder(id);
                    }
                }
                freed.push(entry.binder);
            }
        }
        // Restore every descriptor this scope overwrote: the module's factory
        // and binder go away (the binder is in `freed` below), core's factory
        // and binder come back. The object is dropped, not kept, so the
        // restored factory constructs core's service again with its own shared
        // state (the state is behind an `Arc`, so this is not a reset).
        let overwritten: Vec<(&'static str, BinderId, Factory)> =
            if self.overwrites.len() > scope.depth {
                self.overwrites.split_off(scope.depth).concat()
            } else {
                Vec::new()
            };
        for (descriptor, binder, factory) in overwritten {
            let mut dropped_refs: Vec<BinderId> = Vec::new();
            if let Some(entry) = self.entries.get_mut(descriptor) {
                if let Some(mut object) = entry.object.take() {
                    dropped_refs = object.release_all_refs();
                }
                freed.push(entry.binder);
                self.registry.remove(entry.binder);
                self.binder_descriptors.remove(&entry.binder.0);
            }
            for id in dropped_refs {
                self.release_binder(id);
            }
            self.entries.insert(
                descriptor,
                ServiceEntry {
                    binder,
                    factory,
                    object: None,
                },
            );
            self.binder_descriptors.insert(binder.0, descriptor);
        }
        let added_names: Vec<&'static str> = self
            .names
            .keys()
            .copied()
            .filter(|key| !scope.names.contains_key(key))
            .collect();
        for key in added_names {
            self.names.remove(key);
        }
        // Restore any pre-existing alias the module overwrote.
        let restored: Vec<&'static str> = scope.names.keys().copied().collect();
        for (name, descriptor) in &scope.names {
            self.names.insert(name, descriptor);
        }
        for name in restored {
            self.notify_registered(name);
        }
        let added_aux: Vec<&'static str> = self
            .aux
            .keys()
            .copied()
            .filter(|key| !scope.aux.contains_key(key))
            .collect();
        for key in added_aux {
            if let Some(binder) = self.aux.remove(key) {
                freed.push(binder);
            }
        }
        for (descriptor, binder) in &scope.aux {
            self.aux.insert(descriptor, *binder);
        }
        let known_dynamic: std::collections::HashSet<u64> = scope.dynamic.iter().copied().collect();
        let added_dynamic: Vec<u64> = self
            .dynamic
            .keys()
            .copied()
            .filter(|id| !known_dynamic.contains(id))
            .collect();
        for id in added_dynamic {
            if let Some(mut object) = self.dynamic.remove(&id) {
                for held in object.release_all_refs() {
                    self.release_binder(held);
                }
            }
            freed.push(BinderId(id));
        }
        let added_descriptors: Vec<u64> = self
            .binder_descriptors
            .keys()
            .copied()
            .filter(|id| !scope.binder_descriptors.contains_key(id))
            .collect();
        for id in added_descriptors {
            self.binder_descriptors.remove(&id);
            if !freed.iter().any(|binder| binder.0 == id) {
                freed.push(BinderId(id));
            }
        }
        self.overwrites.truncate(scope.depth);
        if !scope.notification.as_ref().is_some_and(|before| {
            self.notification
                .as_ref()
                .is_some_and(|now| std::sync::Arc::ptr_eq(before, now))
        }) {
            self.notification = scope.notification.clone();
        }
        if !scope.permission_store.as_ref().is_some_and(|before| {
            self.permission_store
                .as_ref()
                .is_some_and(|now| std::sync::Arc::ptr_eq(before, now))
        }) {
            self.permission_store = scope.permission_store.clone();
        }
        if !scope.pending_intent_fire.as_ref().is_some_and(|before| {
            self.pending_intent_fire
                .as_ref()
                .is_some_and(|now| std::sync::Arc::ptr_eq(before, now))
        }) {
            self.pending_intent_fire = scope.pending_intent_fire.clone();
        }
        for binder in &freed {
            self.registry.remove(*binder);
        }
        self.deliveries
            .retain(|delivery| !freed.iter().any(|binder| binder.0 == delivery.target.0));
        freed.sort_unstable_by_key(|binder| binder.0);
        freed.dedup_by_key(|binder| binder.0);
        freed
    }

    /// Register the `INotificationManager` state (called by
    /// [`crate::registry::install`]).
    pub fn set_notification_state(
        &mut self,
        state: std::sync::Arc<std::sync::Mutex<crate::declared::NotificationState>>,
    ) {
        self.notification = Some(state);
    }

    /// MPROC8: this session runs real app processes, so the host performs AOSP's
    /// `ActiveServices` work for a guest `bindService` (create/bind the service
    /// in its own process, answer the caller's `IServiceConnection` on
    /// `publishService`, unbind/stop on `unbindService`, disconnect on process
    /// death).
    ///
    /// Called by the ART launcher, which is the only component that knows the
    /// session has guest processes; the in-process runtime keeps its own bind
    /// path and leaves this off ([`crate::ams::AmsState::pending_binds`]).
    pub fn set_host_side_services(&mut self, enabled: bool) {
        if self.host_side_services == enabled {
            return;
        }
        self.host_side_services = enabled;
        if let Some(state) = self.ams_state() {
            state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .ams
                .host_services
                .enabled = enabled;
        }
    }

    /// Whether this session performs host-side service binds (MPROC8).
    #[must_use]
    pub fn host_side_services(&self) -> bool {
        self.host_side_services
    }

    /// The activity manager's state, when the session has one (constructed on
    /// first use).
    fn ams_state(&mut self) -> Option<std::sync::Arc<Mutex<crate::atms::AtmsState>>> {
        let object = self.ensure("android.app.IActivityTaskManager").ok()?;
        object
            .as_any_mut()
            .and_then(|any| any.downcast_mut::<crate::atms::AtmsService>())
            .map(|service| service.shared_state())
    }

    /// The `IActivityManager` calls whose answer needs the cross-service
    /// `ActiveServices` table (MPROC8).
    ///
    /// AOSP's `ActivityManagerService` does this work itself; ACOS has no system
    /// process, so the host dispatcher is where the service table lives. Each
    /// arm only *records* and *queues*: the reply the guest sees is the activity
    /// manager's own (a `bindService` boolean), and the app-side work is a
    /// delivery to the process that must perform it.
    fn host_service_hook(&mut self, method: &str, args: &[crate::value::Value]) {
        match method {
            "bindService" | "bindServiceInstance" => {
                // `bindService(caller, token, Intent, resolvedType,
                // IServiceConnection, flags, instanceName, callingPackage,
                // userId)`.
                let Some(intent) = args
                    .get(2)
                    .and_then(crate::value::Value::as_parcelable)
                    .map(crate::ams::IntentFacts::from_parcelable)
                else {
                    return;
                };
                let connection = args
                    .get(4)
                    .and_then(crate::value::Value::as_binder)
                    .map(|b| b.0)
                    .unwrap_or(0);
                // A host-side bind only performs work for a **resolved**
                // component: an implicit service intent is resolved by the
                // decoded dispatch (`AmsState::bind_service`), which is where
                // the rule lives.
                let Some((package, class)) = intent.component.clone() else {
                    return;
                };
                let Some((service_info, app_info)) = self.service_record(&package, &class) else {
                    return;
                };
                let token = self.node_registrar().register(
                    "android.app.ServiceToken",
                    Box::new(crate::service_bind::ServiceToken),
                );
                let Some(state) = self.ams_state() else {
                    return;
                };
                // The target process, when it is already attached: the same
                // `ProcessRecord` the activity launch path uses. `None` means
                // the launcher must spawn it first, exactly like a launch.
                let target = {
                    let state = state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    if std::env::var_os("ACOS_SERVICE_BIND_TRACE").is_some() {
                        eprintln!(
                            "acos-services: bind {package}/{class}: processes={:?}",
                            state
                                .processes
                                .iter()
                                .map(|p| (p.tag, p.package_name.clone(), p.app_thread, p.attached))
                                .collect::<Vec<_>>()
                        );
                    }
                    state
                        .processes
                        .iter()
                        .find(|p| p.package_name == package && p.attached)
                        .and_then(|p| p.app_thread.map(|thread| (p.tag, thread)))
                };
                let resolved = crate::ams::DeclaredComponent {
                    package: package.clone(),
                    class: class.clone(),
                    exported: service_info.exported,
                    launch_mode: 0,
                    filters: Vec::new(),
                };
                let bound = {
                    let mut state = state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    let result = state.ams.host_services.bind(
                        &resolved,
                        token,
                        connection,
                        &intent,
                        &service_info,
                        &app_info,
                        target,
                    );
                    if target.is_none() && result.is_ok() {
                        state.ams.request_process(&package, &class, "bindService");
                    }
                    result
                };
                match bound {
                    Ok(deliveries) => {
                        // The caller's `IServiceConnection` is a binder the host
                        // now stores: one strong reference (BINDERREF2), which is
                        // what keeps the caller's node rooted until the unbind.
                        if connection != 0 {
                            self.retain_binder(BinderId(connection));
                        }
                        self.deliveries.extend(deliveries);
                    }
                    Err(error) => {
                        eprintln!(
                            "acos-services: bindService({package}/{class}) could not be \
                             recorded: {error}"
                        );
                    }
                }
            }
            "unbindService" => {
                let connection = args
                    .first()
                    .and_then(crate::value::Value::as_binder)
                    .map(|b| b.0)
                    .unwrap_or(0);
                if connection == 0 {
                    return;
                }
                let Some(state) = self.ams_state() else {
                    return;
                };
                let (deliveries, gone) = {
                    let mut state = state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    state.ams.host_services.unbind(connection)
                };
                if !gone.is_empty() {
                    // The host no longer stores the caller's connection: drop
                    // the reference it held (the driver gets the `F_REF`
                    // release, so the caller's node may be freed).
                    self.release_binder(BinderId(connection));
                }
                self.deliveries.extend(deliveries);
            }
            "attachApplication" => {
                let thread = args
                    .first()
                    .and_then(crate::value::Value::as_binder)
                    .map(|b| b.0)
                    .unwrap_or(0);
                let Some(state) = self.ams_state() else {
                    return;
                };
                let record = {
                    let state = state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    state
                        .processes
                        .iter()
                        .find(|p| p.app_thread == Some(thread) && p.attached)
                        .map(|p| (p.tag, p.package_name.clone()))
                };
                let Some((tag, package)) = record else {
                    return;
                };
                let waiting = {
                    let state = state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    state.ams.host_services.waiting_for_process(&package)
                };
                for (package, class) in waiting {
                    let Some((info, app)) = self.service_record(&package, &class) else {
                        continue;
                    };
                    let assigned = {
                        let mut state = state
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        state
                            .ams
                            .host_services
                            .assign_process(tag, thread, &package, &class, &info, &app)
                    };
                    match assigned {
                        Ok(deliveries) => self.deliveries.extend(deliveries),
                        Err(error) => eprintln!(
                            "acos-services: cannot bring up {package}/{class} for process {tag}: \
                             {error}"
                        ),
                    }
                }
            }
            _ => {}
        }
    }

    /// MPROC8: process `tag` died — every caller bound to one of its services
    /// hears `connected(name, null, true)` (which `LoadedApk.ServiceDispatcher`
    /// turns into `onServiceDisconnected`), and the host drops the references it
    /// held on those callers' `IServiceConnection`s.
    ///
    /// Called by the peer when a driver connection closes, beside the activity
    /// manager's own `ProcessRecord` teardown, so a dead process can never leave
    /// a caller waiting for a callback that will not come.
    pub fn service_process_died(&mut self, tag: u32) {
        if !self.host_side_services {
            return;
        }
        let Some(state) = self.ams_state() else {
            return;
        };
        let (deliveries, released) = {
            let mut state = state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let deliveries = state.ams.host_services.process_died(tag);
            (deliveries, Vec::<u64>::new())
        };
        let _ = released;
        match deliveries {
            Ok(deliveries) => {
                for delivery in &deliveries {
                    eprintln!(
                        "acos-services: process {tag} died: {} to {:#x}",
                        delivery.method, delivery.target.0
                    );
                }
                self.deliveries.extend(deliveries);
            }
            Err(error) => {
                eprintln!("acos-services: process {tag} died: service disconnect failed: {error}");
            }
        }
    }

    /// The package manager's `ServiceInfo`/`AppInfo` for `package`'s `class`
    /// (MPROC8: the payload of `scheduleCreateService`).
    fn service_record(
        &mut self,
        package: &str,
        class: &str,
    ) -> Option<(crate::pms::ServiceInfo, crate::pms::AppInfo)> {
        let object = self.ensure("android.content.pm.IPackageManager").ok()?;
        let state = object
            .as_any_mut()
            .and_then(|any| any.downcast_mut::<crate::pms::PmsService>())
            .map(|service| service.shared_state())?;
        let state = state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.packages.iter().find_map(|installed| {
            installed
                .services
                .iter()
                .find(|s| s.package_name == package && s.name == class)
                .cloned()
                .map(|service| (service, installed.app.clone()))
        })
    }

    /// Install the accessibility state the dispatcher hooks into (called by
    /// [`crate::registry::install`]).
    pub fn set_accessibility_state(
        &mut self,
        state: std::sync::Arc<std::sync::Mutex<crate::misc::A11yState>>,
    ) {
        self.a11y = Some(std::sync::Arc::downgrade(&state));
    }

    /// The accessibility state, when the session installed it.
    #[must_use]
    pub fn accessibility_state(
        &self,
    ) -> Option<std::sync::Arc<std::sync::Mutex<crate::misc::A11yState>>> {
        self.a11y.as_ref().and_then(std::sync::Weak::upgrade)
    }

    /// The `IAccessibilityServiceConnection` node a bound service receives.
    ///
    /// Registered once, lazily: the node is a dynamic (`raw_transact`-style)
    /// object, and `IAccessibilityServiceConnection` has no `acos-aidl` table
    /// (the service-side accessibility AIDL is not in the decoded set), so it
    /// answers through [`ServiceObject::call`] with the method names it knows
    /// and a structured refusal otherwise.
    pub fn accessibility_connection_node(&mut self) -> BinderId {
        if let Some(binder) = self.a11y_connection {
            return binder;
        }
        let Some(state) = self.a11y.as_ref().and_then(std::sync::Weak::upgrade) else {
            return BinderId(0);
        };
        let binder = self.register_dynamic(
            crate::a11y_bind::SERVICE_CONNECTION,
            Box::new(crate::a11y_bind::A11yConnectionNode::new(state)),
        );
        self.a11y_connection = Some(binder);
        binder
    }

    /// The display's accessibility-overlay window token (`init`'s third
    /// argument). One per session, like one display.
    pub fn accessibility_window_token(&mut self) -> BinderId {
        if let Some(binder) = self.a11y_window_token {
            return binder;
        }
        let binder = self.register_aux("dev.acos.a11y.OverlayWindowToken");
        self.a11y_window_token = Some(binder);
        binder
    }

    /// The `INotificationManager` state, when the standard set is installed.
    #[must_use]
    pub fn notification_state(
        &self,
    ) -> Option<std::sync::Arc<std::sync::Mutex<crate::declared::NotificationState>>> {
        self.notification.as_ref().map(std::sync::Arc::clone)
    }

    /// Publish the session's `PendingIntent` delivery path (called by
    /// [`crate::registry::install`]).
    pub fn set_pending_intent_fire(&mut self, fire: FireSender) {
        self.pending_intent_fire = Some(fire);
    }

    /// The delivery path a `notifications` `tap` uses.
    #[must_use]
    pub fn pending_intent_fire(&self) -> Option<FireSender> {
        self.pending_intent_fire.as_ref().map(std::sync::Arc::clone)
    }

    /// Publish the session's one runtime-permission table (called by
    /// [`crate::registry::install`]).
    pub fn set_permission_store(
        &mut self,
        store: std::sync::Arc<std::sync::Mutex<crate::permissions::PermissionStore>>,
    ) {
        self.permission_store = Some(store);
    }

    /// The runtime-permission table, when a standard set (or the `permissions`
    /// module) published one.
    #[must_use]
    pub fn permission_store(
        &self,
    ) -> Option<std::sync::Arc<std::sync::Mutex<crate::permissions::PermissionStore>>> {
        self.permission_store.as_ref().map(std::sync::Arc::clone)
    }

    /// Publish this session's WebView endpoints (WV8: the launcher binds the
    /// gate socket, so it is the one that knows the paths).
    pub fn set_webview_gate(&mut self, gate: WebviewGate) {
        self.webview_gate = Some(gate);
    }

    /// The WebView endpoints, when the launcher published them.
    #[must_use]
    pub fn webview_gate(&self) -> Option<&WebviewGate> {
        self.webview_gate.as_ref()
    }

    /// Enable the per-call trace.
    pub fn enable_trace(&mut self, sink: Box<dyn TraceSink>) {
        self.trace = Some(sink);
    }

    /// Disable the per-call trace.
    pub fn disable_trace(&mut self) -> Option<Box<dyn TraceSink>> {
        self.trace.take()
    }

    /// Number of service objects constructed so far (the lazy-construction
    /// metric).
    #[must_use]
    pub fn constructions(&self) -> u64 {
        self.constructions
    }

    /// Descriptors with a registered service.
    #[must_use]
    pub fn descriptors(&self) -> Vec<&'static str> {
        let mut v: Vec<_> = self.entries.keys().copied().collect();
        v.sort_unstable();
        v
    }

    /// Registered `ServiceManager` names.
    #[must_use]
    pub fn service_names(&self) -> Vec<&'static str> {
        let mut v: Vec<_> = self.names.keys().copied().collect();
        v.sort_unstable();
        v
    }

    /// True when `name` is registered **or** declared by a shipped module.
    ///
    /// AOSP answers this from the device's VINTF manifest
    /// (`ServiceManager::isDeclared`, `servicemanager/ServiceManager.cpp:596`):
    /// true for a service that exists on the device even while it is not
    /// running. ACOS's equivalent declaration is a module manifest's
    /// `services` list, registered here by [`Self::declare_service_name`], so
    /// `ServiceManager.isDeclared`/`waitForDeclaredService` agree with the
    /// modules the build can attach. **Intentional ACOS deviation** (no VINTF
    /// manifest exists); see the module docs and `docs/FINDINGS.md` F-0015.
    #[must_use]
    pub fn is_declared(&self, name: &str) -> bool {
        self.names.contains_key(name) || self.declared.iter().any(|declared| *declared == name)
    }

    /// Declare `name`: a shipped module can provide it even though it is not
    /// registered now (the module is `off` or not attached yet).
    pub fn declare_service_name(&mut self, name: &'static str) {
        if !self.names.contains_key(name) && !self.declared.contains(&name) {
            self.declared.push(name);
        }
    }

    /// The declared-but-not-registered names (the module-provided gaps).
    #[must_use]
    pub fn declared_names(&self) -> Vec<&'static str> {
        let mut names: Vec<&'static str> = self
            .declared
            .iter()
            .copied()
            .filter(|name| !self.names.contains_key(name))
            .collect();
        names.sort_unstable();
        names
    }

    /// Fire the pending `registerForNotifications` callbacks for `name`, if it
    /// resolves now.
    ///
    /// Called from every path that gives a descriptor a `ServiceManager` name
    /// (a live module attach, an alias), so a client blocked in
    /// `ServiceManager.waitForService` gets `IServiceCallback.onRegistration`
    /// exactly when the service appears — AOSP's `ServiceManager::addService`
    /// (`servicemanager/ServiceManager.cpp:480-499`).
    fn notify_registered(&mut self, name: &str) {
        if self.notifications.is_empty() {
            return;
        }
        let Ok(Some(binder)) = self.check_service(name) else {
            return;
        };
        let (fire, keep): (Vec<PendingNotification>, Vec<PendingNotification>) =
            std::mem::take(&mut self.notifications)
                .into_iter()
                .partition(|notification| notification.name == name);
        self.notifications = keep;
        for notification in fire {
            self.deliveries
                .push(registration_delivery(notification.callback, name, binder));
        }
    }

    /// How many registration callbacks are waiting (tests and the report).
    #[must_use]
    pub fn pending_notifications(&self) -> usize {
        self.notifications.len()
    }

    /// Resolve a service-manager name to its binder, constructing the service
    /// on first request. Unknown names return `None` (Android returns `null`).
    ///
    /// A descriptor served by a **dynamic** node and no factory (the
    /// `android.gui.ISurfaceComposer` node behind `SurfaceFlinger`, W-R T2)
    /// resolves to that node's binder: `raw_transact`-served interfaces have no
    /// `acos-aidl` table, so they are registered with
    /// [`ServiceHost::register_dynamic`] rather than a factory.
    pub fn check_service(&mut self, name: &str) -> Result<Option<BinderId>> {
        let Some(descriptor) = self.names.get(name).copied() else {
            return Ok(None);
        };
        if let Some(entry) = self.entries.get(&descriptor) {
            let binder = entry.binder;
            self.ensure(descriptor)?;
            return Ok(Some(binder));
        }
        Ok(self.binder_for_descriptor(descriptor))
    }

    /// Resolve a descriptor's binder without constructing the service.
    #[must_use]
    pub fn binder_for_descriptor(&self, descriptor: &str) -> Option<BinderId> {
        self.entries
            .get(descriptor)
            .map(|e| e.binder)
            .or_else(|| self.aux.get(descriptor).copied())
            .or_else(|| {
                // A dynamically registered node (e.g. the `IWindowSession` the
                // `window` service's `openSession` answers, W1).
                self.binder_descriptors
                    .iter()
                    .find(|(id, d)| **d == descriptor && self.dynamic.contains_key(id))
                    .map(|(id, _)| BinderId(*id))
            })
    }

    /// The descriptor a binder handle was registered under.
    #[must_use]
    pub fn descriptor_of(&self, binder: BinderId) -> Option<&'static str> {
        self.binder_descriptors.get(&binder.0).copied()
    }

    /// The `IContentProvider` binder for `authority`, plus the structured record
    /// of the lookup (a miss is `holder = null`, exactly what Android returns).
    ///
    /// This is the seam a service object uses to materialise a
    /// `ContentProviderHolder`; the registry itself lives in
    /// [`crate::provider::ProviderRegistry`].
    #[must_use]
    pub fn provider_lookups(&mut self) -> Vec<crate::provider::ProviderLookup> {
        let Ok(object) = self.ensure("android.app.IActivityManager") else {
            return Vec::new();
        };
        object
            .as_any_mut()
            .and_then(|any| any.downcast_mut::<crate::atms::AtmsService>())
            .map(crate::atms::AtmsService::provider_lookups)
            .unwrap_or_default()
    }

    /// Construct the service for `descriptor` if it is not built yet.
    pub fn ensure(&mut self, descriptor: &'static str) -> Result<&mut dyn ServiceObject> {
        let needs = match self.entries.get(descriptor) {
            Some(e) => e.object.is_none(),
            None => {
                return Err(ServiceError::UnsupportedAndroidApi {
                    interface: descriptor.to_string(),
                    code: 0,
                    method: "<service not registered>".into(),
                })
            }
        };
        if needs {
            let factory = self
                .entries
                .get(descriptor)
                .map(|e| Arc::clone(&e.factory))
                .ok_or_else(|| ServiceError::Internal("entry vanished".into()))?;
            let binder = self.entries[descriptor].binder;
            let wiring = Wiring {
                self_binder: binder,
                controller: self
                    .aux
                    .get("android.app.IActivityClientController")
                    .copied()
                    .unwrap_or(BinderId(0)),
                content_provider: self
                    .aux
                    .get("android.content.IContentProvider")
                    .copied()
                    .unwrap_or(BinderId(0)),
                package_installer: self
                    .aux
                    .get("android.content.pm.IPackageInstaller")
                    .copied()
                    .unwrap_or(BinderId(0)),
                nodes: self.nodes.clone(),
            };
            let object = factory(wiring);
            self.constructions += 1;
            if let Some(e) = self.entries.get_mut(descriptor) {
                e.object = Some(object);
            }
        }
        Ok(self
            .entries
            .get_mut(descriptor)
            .and_then(|e| e.object.as_mut())
            .map(Box::as_mut)
            .expect("service object was just constructed"))
    }

    /// The service object of a **dynamic** node, for a module that configures a
    /// node another module (`core`) published.
    ///
    /// ADR-0029's `as_any_mut` seam is "the host configures a specific service".
    /// `ensure` only walks the lazy `entries` table; a `raw_transact`-served node
    /// (`android.gui.*`, e.g. `sensorservice`) lives in `dynamic`. The `sensors`
    /// module takes the node `core` published and installs its feed into it, so
    /// the binder an app already holds keeps working (a hot attach must not
    /// invalidate a proxy).
    pub fn dynamic_object_mut(
        &mut self,
        descriptor: &str,
    ) -> Option<&mut (dyn ServiceObject + 'static)> {
        let binder = self.binder_for_descriptor(descriptor)?;
        self.dynamic.get_mut(&binder.0).map(Box::as_mut)
    }

    /// The ATMS attach record, if the activity service has one (T-43 re-seed).
    #[must_use]
    pub fn attached_process(&mut self) -> Option<(u64, i64, String)> {
        let object = self.ensure("android.app.IActivityManager").ok()?;
        object
            .as_any_mut()
            .and_then(|any| any.downcast_mut::<crate::atms::AtmsService>())
            .and_then(|s| s.attached_process())
    }

    /// Re-create the `attachApplication` record in a fresh host after a
    /// snapshot restore, so the restored framework's `finishAttachApplication`
    /// finds its process (T-43).
    pub fn seed_attached_process(
        &mut self,
        app_thread: u64,
        seq: i64,
        package: &str,
    ) -> Result<()> {
        let object = self.ensure("android.app.IActivityManager")?;
        let service = object
            .as_any_mut()
            .and_then(|any| any.downcast_mut::<crate::atms::AtmsService>())
            .ok_or_else(|| {
                ServiceError::Internal("activity service is not an AtmsService".into())
            })?;
        service.seed_attach(app_thread, seq, package);
        Ok(())
    }

    /// The dynamic receiver registrations `IActivityManager`
    /// (`registerReceiver[WithFeature]`) has recorded, with the decoded
    /// `IntentFilter` facts (W28). Oldest first.
    #[must_use]
    pub fn receiver_registrations(&mut self) -> Vec<crate::atms::ReceiverRegistration> {
        let Ok(object) = self.ensure("android.app.IActivityManager") else {
            return Vec::new();
        };
        object
            .as_any_mut()
            .and_then(|any| any.downcast_mut::<crate::atms::AtmsService>())
            .map(crate::atms::AtmsService::receiver_registrations)
            .unwrap_or_default()
    }

    /// Mirror a **guest** dynamic receiver registration into the session's
    /// host-side AMS state.
    ///
    /// The guest's `registerReceiver` is served by the runtime's AMS state
    /// (`ams_java`), while a broadcast the **system** originates — a module
    /// `control` op, and a fired `PendingIntent` whose sender record is
    /// host-side — is delivered through [`Self::broadcast_intent`]'s state. Both
    /// registers live in the same session; without this mirror the host-side
    /// matcher saw no dynamic receiver, so a scripted fix/SMS fired from a
    /// module reached nobody. The guest's own `broadcastIntent` keeps using the
    /// runtime state, so a receiver is never delivered twice.
    pub fn mirror_dynamic_receiver(
        &mut self,
        receiver: u64,
        actions: Vec<String>,
        categories: Vec<String>,
        flags: i32,
    ) -> Result<()> {
        let object = self.ensure("android.app.IActivityManager")?;
        let service = object
            .as_any_mut()
            .and_then(|any| any.downcast_mut::<crate::atms::AtmsService>())
            .ok_or_else(|| {
                ServiceError::Internal("activity service is not an AtmsService".into())
            })?;
        service.register_receiver(receiver, "", actions, categories, flags);
        Ok(())
    }

    /// Drop a mirrored registration when the guest unregisters it.
    pub fn mirror_unregister_receiver(&mut self, receiver: u64) -> Result<bool> {
        let object = self.ensure("android.app.IActivityManager")?;
        let service = object
            .as_any_mut()
            .and_then(|any| any.downcast_mut::<crate::atms::AtmsService>())
            .ok_or_else(|| {
                ServiceError::Internal("activity service is not an AtmsService".into())
            })?;
        Ok(service.unregister_receiver(receiver))
    }

    /// Publish (or withdraw) one `PackageManager.hasSystemFeature` answer.
    ///
    /// A host with no package manager has no `hasSystemFeature` that could
    /// disagree with the attached set (the conformance kit attaches a module to
    /// an otherwise empty host); every real session attaches `core`, which
    /// registers the package manager, before any other module.
    pub fn set_system_feature(&mut self, name: &str, enabled: bool) -> Result<()> {
        if !self
            .entries
            .contains_key("android.content.pm.IPackageManager")
        {
            return Ok(());
        }
        let object = self.ensure("android.content.pm.IPackageManager")?;
        let service = object
            .as_any_mut()
            .and_then(|any| any.downcast_mut::<crate::pms::PmsService>())
            .ok_or_else(|| ServiceError::Internal("package service is not a PmsService".into()))?;
        service.set_system_feature(name, enabled)
    }

    /// Queue a `system -> app` delivery a module produced outside a binder
    /// transaction (a scripted call-state event, an authentication result).
    ///
    /// The peer thread drains [`ServiceHost::take_deliveries`] after a control
    /// request, so a module's `control` can reach the app without waiting for
    /// the app to call back in.
    pub fn queue_delivery(&mut self, delivery: PendingDelivery) {
        self.deliveries.push(delivery);
    }

    /// Take one strong reference on `id` (a service stored the binder).
    ///
    /// Returns the new count. Two kinds of id can be referenced:
    ///
    /// * a handle the [`BinderRegistry`] knows (a local node, a proxy); and
    /// * a **guest** binder (`GUEST_BINDER_TAG`, the peer's `JAVA_TAG`), which
    ///   is the shape every callback reaches a module as. The registry does not
    ///   have to know it: the driver owns those nodes, and rooting them is
    ///   exactly what the reference is for.
    ///
    /// A null id is refused (`0`), and so is an unknown host-local id: a
    /// reference to a handle the host cannot name would be a reference to
    /// nothing.
    pub fn retain_binder(&mut self, id: BinderId) -> u32 {
        if id.0 == 0 || (!self.registry.contains(id) && id.0 & GUEST_BINDER_TAG == 0) {
            return 0;
        }
        let count = self.refs.entry(id.0).or_insert(0);
        *count = count.saturating_add(1);
        *count
    }

    /// Drop one strong reference. At zero the binder is queued for release, so
    /// the peer tells the driver to unroot the node (`F_REF` release →
    /// `BR_RELEASE` + `BR_DECREFS`). Returns the remaining count.
    pub fn release_binder(&mut self, id: BinderId) -> u32 {
        let Some(count) = self.refs.get_mut(&id.0) else {
            // Nothing was held: a release for a reference the host never took
            // must not tell the driver to unroot a node the guest may still be
            // using (`retain` refused, `release` silently ignored).
            return 0;
        };
        *count = count.saturating_sub(1);
        let remaining = *count;
        if remaining == 0 {
            self.refs.remove(&id.0);
            if !self.released_binders.contains(&id) {
                self.released_binders.push(id);
            }
        }
        remaining
    }

    /// Host-held reference count of `id` (diagnostics and tests).
    #[must_use]
    pub fn held_refs(&self, id: BinderId) -> u32 {
        self.refs.get(&id.0).copied().unwrap_or(0)
    }

    /// Every binder the host currently holds a strong reference on.
    ///
    /// A process teardown releases exactly the ones its own driver exported
    /// (the peer filters by owner tag), so the table never outlives a process.
    #[must_use]
    pub fn held_ref_ids(&self) -> Vec<BinderId> {
        let mut ids: Vec<BinderId> = self.refs.keys().map(|id| BinderId(*id)).collect();
        ids.sort_unstable_by_key(|b| b.0);
        ids
    }

    /// Binders whose last host reference was dropped since the last call.
    #[must_use]
    pub fn take_released_binders(&mut self) -> Vec<BinderId> {
        std::mem::take(&mut self.released_binders)
    }

    /// Deliver a broadcast the **host** originates (a module's control op).
    ///
    /// Goes through the session's own `IActivityManager` state, so the same
    /// dynamic receivers a guest broadcast reaches get it, and queues the
    /// resulting `IIntentReceiver.performReceive` deliveries. A broadcast the
    /// app registered no receiver for is not an error (Android's own rule); the
    /// matched count says what happened.
    pub fn broadcast_intent(&mut self, intent: &crate::ams::IntentFacts) -> Result<i32> {
        let object = self.ensure("android.app.IActivityManager")?;
        let service = object
            .as_any_mut()
            .and_then(|any| any.downcast_mut::<crate::atms::AtmsService>())
            .ok_or_else(|| {
                ServiceError::Internal("activity service is not an AtmsService".into())
            })?;
        let (matched, deliveries) = service.inject_broadcast(intent)?;
        self.deliveries.extend(deliveries);
        Ok(matched)
    }

    /// Re-point the in-process package manager at `apk_path`.    ///
    /// `ServiceHost::standard()` seeds the PMS with the `hello-views` fixture;
    /// the runtime calls this before boot so the framework's
    /// `getApplicationInfo(<launched package>)` resolves the real APK instead
    /// of raising `unknown service name <package>`. Returns an error (never a
    /// silent no-op) when the package service is missing or mistyped.
    pub fn install_package_from_apk(&mut self, apk_path: &std::path::Path) -> Result<()> {
        self.install_package_from_apk_as(apk_path, None)
    }

    /// The same install, publishing `guest_source_dir` as the APK's
    /// `sourceDir`/`publicSourceDir` (the path the confined guest can open).
    pub fn install_package_from_apk_as(
        &mut self,
        apk_path: &std::path::Path,
        guest_source_dir: Option<&str>,
    ) -> Result<()> {
        let service = self.pms_mut()?;
        service.set_package_from_apk_as(apk_path, guest_source_dir)
    }

    /// MPROC1: install an **additional** package into this session's package
    /// manager (the launched APK stays installed) and return its uid.
    pub fn install_extra_package_from_apk_as(
        &mut self,
        apk_path: &std::path::Path,
        guest_source_dir: Option<&str>,
        guest_native_lib_dir: Option<&str>,
    ) -> Result<i32> {
        let service = self.pms_mut()?;
        service.install_extra_package_from_apk_as(apk_path, guest_source_dir, guest_native_lib_dir)
    }

    fn pms_mut(&mut self) -> Result<&mut crate::pms::PmsService> {
        let object = self.ensure("android.content.pm.IPackageManager")?;
        object
            .as_any_mut()
            .and_then(|any| any.downcast_mut::<crate::pms::PmsService>())
            .ok_or_else(|| ServiceError::Internal("package service is not a PmsService".into()))
    }

    /// Dispatch one transaction.
    ///
    /// `data` must be positioned at the start (interface token first). `reply`
    /// is written from position 0 as a fresh reply.
    pub fn transact(
        &mut self,
        binder: BinderId,
        code: u32,
        data: &mut Parcel,
        reply: &mut Parcel,
        flags: u32,
    ) -> Result<()> {
        let oneway = flags & 1 != 0;
        let started = trace::start();
        let result = self.transact_inner(binder, code, data, reply, oneway);
        if result.is_ok() {
            self.install_pending_nodes();
        }
        let wall_ns = trace::elapsed_ns(started);

        if let Some(sink) = self.trace.as_mut() {
            let (interface, method, args, reply_summary, error) = match &result {
                Ok(info) => (
                    info.interface.clone(),
                    info.method.clone(),
                    info.args.clone(),
                    info.reply.clone(),
                    None,
                ),
                Err(e) => (
                    self.binder_descriptors
                        .get(&binder.0)
                        .copied()
                        .unwrap_or("<unknown>")
                        .to_string(),
                    format!("code {code}"),
                    String::new(),
                    String::new(),
                    Some(e.code()),
                ),
            };
            sink.record(&CallTrace {
                seq: self.seq.fetch_add(1, Ordering::Relaxed),
                interface,
                method,
                code,
                oneway,
                args,
                reply: reply_summary,
                wall_ns,
                error,
            });
        }
        if let Err(error) = &result {
            write_error_reply(reply, error);
        }
        result.map(|_| ())
    }

    fn transact_inner(
        &mut self,
        binder: BinderId,
        code: u32,
        data: &mut Parcel,
        reply: &mut Parcel,
        oneway: bool,
    ) -> Result<CallInfo> {
        if !self.binder_descriptors.contains_key(&binder.0) {
            if let Some(module) = self.dead.get(&binder.0).copied() {
                let interface = data
                    .peek_interface_token()
                    .ok()
                    .flatten()
                    .unwrap_or_else(|| "<unknown>".to_string());
                return Err(ServiceError::ModuleUnavailable {
                    service: format!("binder {:#x}", binder.0),
                    interface,
                    module,
                    attach: crate::module::attach_call(module, crate::module::Profile::Scripted),
                });
            }
            return Err(ServiceError::UnknownBinder { handle: binder.0 });
        }
        let binder_descriptor = self
            .binder_descriptors
            .get(&binder.0)
            .copied()
            .unwrap_or("<unknown>")
            .to_string();

        // The interface token is authoritative (it is what `enforceInterface`
        // checks). A missing token is a malformed request.
        let descriptor = match data.peek_interface_token()? {
            Some(d) => d,
            None => {
                return Err(ServiceError::BadRequest {
                    interface: binder_descriptor,
                    method: format!("code {code}"),
                    detail: "missing interface token".into(),
                })
            }
        };

        if let Some(owner) = crate::registry::owner_of(&descriptor) {
            // The ownership table is a marker for "another component owns this
            // policy", not a lock: when that component has installed a real
            // node on this host (the `window` service's `IWindowManager`, W1's
            // `IWindowSession`), the node is what must answer. Only a
            // descriptor with no node at all reports the owner.
            let served = self.entries.contains_key(descriptor.as_str())
                || self.dynamic.contains_key(&binder.0);
            if !served {
                let method = acos_aidl::lookup(&descriptor, code)
                    .map(|m| m.name.clone())
                    .unwrap_or_else(|| format!("code {code}"));
                return Err(ServiceError::NotOwned {
                    interface: descriptor,
                    method,
                    owner,
                });
            }
        }

        // A dynamically registered object (the `IBulkCursor` handed back by a
        // `query`) is not in the decoded `acos-aidl` tables: it is a
        // hand-written `Binder` subclass in the framework. Its transactions are
        // answered by the object itself, in the framework's own reply shape.
        if self.dynamic.contains_key(&binder.0) {
            data.enforce_interface(&descriptor)?;
            let object = self.dynamic.get_mut(&binder.0).expect("checked above");
            let handled = object.raw_transact(&descriptor, code, data, reply)?;
            let finished = object.finished();
            let mut raw_deliveries = object.drain_raw_deliveries();
            let mut raw_retained = object.drain_retained();
            let mut raw_released = object.drain_released();
            let mut finished_refs = if finished {
                object.release_all_refs()
            } else {
                Vec::new()
            };
            if finished {
                self.dynamic.remove(&binder.0);
            }
            self.deliveries.append(&mut raw_deliveries);
            for id in raw_retained.drain(..) {
                self.retain_binder(id);
            }
            for id in raw_released.drain(..).chain(finished_refs.drain(..)) {
                self.release_binder(id);
            }
            if !handled {
                return Err(ServiceError::UnsupportedAndroidApi {
                    interface: descriptor,
                    code,
                    method: format!("code {code}"),
                });
            }
            return Ok(CallInfo {
                interface: descriptor,
                method: format!("code {code}"),
                args: "<raw>".into(),
                reply: "<raw>".into(),
            });
        }

        let method = acos_aidl::lookup(&descriptor, code).ok_or_else(|| {
            ServiceError::UnsupportedAndroidApi {
                interface: descriptor.clone(),
                code,
                method: format!("code {code}"),
            }
        })?;

        // IServiceManager is answered by the host itself so it can construct
        // other services lazily.
        if descriptor == "android.os.IServiceManager" {
            let (info, values) = self.service_manager(method, data)?;
            if !oneway {
                reply.reset();
                write_no_exception(reply);
                codec::write_reply(reply, &method.reply, &values, &descriptor, &method.name)?;
            }
            return Ok(info);
        }

        // Advance past the interface token now that dispatch is chosen.
        data.enforce_interface(&descriptor)?;

        // `publishService` is the reply to a **system-initiated** or a
        // guest-initiated bind, and both are answered here before the decoded
        // dispatch because the state they need is cross-service:
        //
        // * the accessibility module owns its own bind token (MODA11Y2): the
        //   app reports the binder `Service.onBind` returned, and the module
        //   answers with `IAccessibilityServiceClient.init`;
        // * MPROC8: a bind a guest started (`bindService` for a service in
        //   another app's package) is recorded by the host's `ActiveServices`
        //   table, which answers every waiting `IServiceConnection`.
        //
        // The token decides which one it is (a token is opaque and unique), so
        // there is exactly one owner per bind and no guessing.
        if descriptor == "android.app.IActivityManager" && method.name == "publishService" {
            let args = codec::read_args(data, &method.args, &descriptor, &method.name)?;
            let token = args
                .first()
                .and_then(crate::value::Value::as_binder)
                .map(|b| b.0)
                .unwrap_or(0);
            // The accessibility module owns its token whenever it has a bind in
            // flight: a token that is not that one is *its* refusal ("does not
            // match the pending bind"), so the check runs before the general
            // table can claim the call.
            let a11y_pending = self
                .a11y
                .as_ref()
                .and_then(std::sync::Weak::upgrade)
                .and_then(|state| {
                    state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .pending_bind
                        .as_ref()
                        .map(|pending| pending.token)
                });
            let mut reply_note = "<guest bind>";
            if a11y_pending.is_some() {
                let connection = self.accessibility_connection_node();
                let window_token = self.accessibility_window_token();
                let a11y = self
                    .a11y
                    .as_ref()
                    .and_then(std::sync::Weak::upgrade)
                    .expect("checked above");
                let mut state = a11y
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let delivery = crate::a11y_bind::on_publish_service(
                    &mut state,
                    connection,
                    window_token,
                    &args,
                )?;
                drop(state);
                if let Some(delivery) = delivery {
                    self.deliveries.push(delivery);
                }
                reply_note = "<accessibility bind>";
            } else if self.host_side_services {
                let binder = args
                    .get(2)
                    .and_then(crate::value::Value::as_binder)
                    .map(|b| b.0)
                    .unwrap_or(0);
                if let Some(state) = self.ams_state() {
                    let published = {
                        let mut state = state
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        state.ams.host_services.publish_service(token, binder)?
                    };
                    if let Some(deliveries) = published {
                        self.deliveries.extend(deliveries);
                    }
                }
            }
            if !oneway {
                reply.reset();
                write_no_exception(reply);
                codec::write_reply(reply, &method.reply, &[], &descriptor, &method.name)?;
            }
            return Ok(CallInfo {
                interface: descriptor,
                method: method.name.clone(),
                args: "<publishService>".into(),
                reply: reply_note.into(),
            });
        }

        // `IContentProvider.query` is the one decoded reply the generic codec
        // cannot express: `ContentProviderNative` writes a hand-rolled
        // `BulkCursorDescriptor` + `CursorWindow`, and the proxy writes the
        // projection as `writeInt(count)` + strings (the generated table records
        // a single `Str16`). The provider returns rows; the host allocates the
        // `IBulkCursor` binder and writes the framework reply.
        if descriptor == "android.content.IContentProvider" && method.name == "query" {
            let cursor = {
                let object = self.ensure_for_binder(binder, &descriptor)?;
                object.content_query(&descriptor, data)?
            };
            if let Some(cursor) = cursor {
                let summary = format!(
                    "rows={} columns={}",
                    cursor.rows.len(),
                    cursor.columns.len()
                );
                let cursor_binder = self.register_dynamic(
                    crate::cursor::BULK_CURSOR_DESCRIPTOR,
                    Box::new(crate::cursor::BulkCursorService::new(cursor.clone())),
                );
                if oneway {
                    return Ok(CallInfo {
                        interface: descriptor,
                        method: method.name.clone(),
                        args: summary,
                        reply: "<oneway>".into(),
                    });
                }
                reply.reset();
                write_no_exception(reply);
                cursor.write_query_reply(reply, cursor_binder)?;
                return Ok(CallInfo {
                    interface: descriptor,
                    method: method.name.clone(),
                    args: summary.clone(),
                    reply: summary,
                });
            }
        }

        // W26: a handful of methods have an image-verified argument layout that
        // differs from the generated table (the scan could not follow a `Map`
        // writer). Prefer the verified layout; `None` means the table is right.
        let args = match codec::read_args_override(&descriptor, method.code, data)? {
            Some(args) => args,
            None => codec::read_args(data, &method.args, &descriptor, &method.name)?,
        };
        let args_summary = trace::summary_args(&args);

        let object = self.ensure_for_binder(binder, &descriptor)?;
        let mut call = object.call(&descriptor, method, &args)?;
        // The reference model is the same on both dispatch paths: a handler
        // that stores a callback binder reports it here, whether its interface
        // is decoded or hand-rolled.
        let mut retained = object.drain_retained();
        let mut released = object.drain_released();
        self.deliveries.append(&mut call.deliveries);
        for id in retained.drain(..) {
            self.retain_binder(id);
        }
        for id in released.drain(..) {
            self.release_binder(id);
        }

        // MODA11Y2: two system services are wired here rather than inside a
        // `ServiceObject`, because the state they need is cross-service:
        //
        // * `attachApplication` names the app's `IApplicationThread`, which is
        //   the target of every `system -> app` delivery (the accessibility
        //   bind has no target before it);
        // * `publishService` is the reply to a system-initiated bind: the app
        //   reports the binder its `Service.onBind` returned arm, and the
        //   accessibility module answers with `IAccessibilityServiceClient.init`.
        if let Some(a11y) = self.a11y.as_ref().and_then(std::sync::Weak::upgrade) {
            if descriptor == "android.app.IActivityManager" {
                match method.name.as_str() {
                    "attachApplication" => {
                        if let Some(thread) = args.first().and_then(crate::value::Value::as_binder)
                        {
                            a11y.lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner)
                                .set_app_thread(thread.0);
                        }
                    }
                    _ => {}
                }
            }
        }
        // MPROC8: AOSP's `ActiveServices` for a guest-initiated bind. Every step
        // runs here, on the thread that owns the service table, and produces the
        // `system -> app` deliveries the peer routes (create/bind to the
        // service's own process, `connected` to the caller's
        // `IServiceConnection`).
        if descriptor == "android.app.IActivityManager" && self.host_side_services {
            self.host_service_hook(&method.name, &args);
        }

        if oneway {
            return Ok(CallInfo {
                interface: descriptor,
                method: method.name.clone(),
                args: args_summary,
                reply: "<oneway>".into(),
            });
        }

        reply.reset();
        write_no_exception(reply);
        codec::write_reply(reply, &method.reply, &call.reply, &descriptor, &method.name)?;

        Ok(CallInfo {
            interface: descriptor,
            method: method.name.clone(),
            args: args_summary,
            reply: trace::summary_args(&call.reply),
        })
    }

    fn ensure_for_binder(
        &mut self,
        binder: BinderId,
        _descriptor: &str,
    ) -> Result<&mut dyn ServiceObject> {
        let descriptor = self
            .binder_descriptors
            .get(&binder.0)
            .copied()
            .ok_or(ServiceError::UnknownBinder { handle: binder.0 })?;
        if self.entries.contains_key(descriptor) {
            return self.ensure(descriptor);
        }
        Err(ServiceError::UnsupportedAndroidApi {
            interface: descriptor.to_string(),
            code: 0,
            method: "<aux binder is not a service>".into(),
        })
    }

    fn service_manager(
        &mut self,
        method: &MethodInfo,
        data: &mut Parcel,
    ) -> Result<(CallInfo, Vec<crate::value::Value>)> {
        let descriptor = "android.os.IServiceManager".to_string();
        data.enforce_interface(&descriptor)?;
        let args = codec::read_args(data, &method.args, &descriptor, &method.name)?;
        let args_summary = trace::summary_args(&args);

        let mut reply_values = Vec::new();
        match method.name.as_str() {
            "getService" | "checkService" => {
                let name = args.first().and_then(|v| v.as_str()).unwrap_or_default();
                let binder = self.check_service(name)?;
                reply_values.push(crate::value::Value::Binder(binder));
            }
            "listServices" => {
                let mut names = self.service_names();
                names.sort_unstable();
                reply_values.push(crate::value::Value::StringArray(Some(
                    names.into_iter().map(|n| Some(n.to_string())).collect(),
                )));
            }
            "isDeclared" => {
                let name = args.first().and_then(|v| v.as_str()).unwrap_or_default();
                reply_values.push(crate::value::Value::Bool(self.is_declared(name)));
            }
            // `registerForNotifications(String, IServiceCallback)`: AOSP stores
            // the callback and calls it at once when the service already exists
            // (`servicemanager/ServiceManager.cpp:527-570`, including the
            // `mNameToService.find(name) != end` immediate `onRegistration`).
            // **Intentional ACOS deviation** (keep it; `docs/FINDINGS.md`
            // F-0015): a name that is neither registered nor declared by a
            // shipped module can never appear here
            // (there is no VINTF manifest and no lazy service start), so the
            // registration is refused with `EX_ILLEGAL_ARGUMENT` and the reason.
            // AOSP would accept it and never fire, which is exactly the endless
            // `waitForService` sleep an agent hit on a module name (the
            // guest-side complaint travels back as the exception message, which
            // libbinder logs as "Failed to registerForNotifications ...").
            "registerForNotifications" => {
                let name = args
                    .first()
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                let Some(callback) = args.get(1).and_then(crate::value::Value::as_binder) else {
                    return Err(ServiceError::BadRequest {
                        interface: descriptor,
                        method: method.name.clone(),
                        detail: "registerForNotifications needs an IServiceCallback binder"
                            .to_string(),
                    });
                };
                if name.trim().is_empty() {
                    return Err(ServiceError::BadRequest {
                        interface: descriptor,
                        method: method.name.clone(),
                        detail: "registerForNotifications needs a non-empty service name"
                            .to_string(),
                    });
                }
                if !self.is_declared(&name) {
                    return Err(ServiceError::BadRequest {
                        interface: descriptor,
                        method: method.name.clone(),
                        detail: format!(
                            "no service {name:?} is registered or declared by any module; it can never appear, so waiting for it would never return (AOSP's servicemanager would accept this registration and never fire it)"
                        ),
                    });
                }
                self.notifications.push(PendingNotification {
                    name: name.clone(),
                    callback,
                });
                // AOSP fires immediately for an already-registered name.
                self.notify_registered(&name);
            }
            "unregisterForNotifications" => {
                let name = args
                    .first()
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                let callback = args.get(1).and_then(crate::value::Value::as_binder);
                let before = self.notifications.len();
                self.notifications.retain(|notification| {
                    !(notification.name == name && Some(notification.callback) == callback)
                });
                if self.notifications.len() == before {
                    // AOSP: EX_ILLEGAL_STATE, "Nothing to unregister."
                    return Err(ServiceError::IllegalState {
                        component: "servicemanager",
                        detail: format!(
                            "Trying to unregister callback, but none exists for {name:?} (AOSP: \"Nothing to unregister\")"
                        ),
                    });
                }
            }
            "addService" => {
                return Err(ServiceError::UnsupportedAndroidApi {
                    interface: descriptor,
                    code: method.code,
                    method: method.name.clone(),
                })
            }
            _ => {
                return Err(ServiceError::UnsupportedAndroidApi {
                    interface: descriptor,
                    code: method.code,
                    method: method.name.clone(),
                })
            }
        }
        let info = CallInfo {
            interface: descriptor,
            method: method.name.clone(),
            args: args_summary,
            reply: trace::summary_args(&reply_values),
        };
        Ok((info, reply_values))
    }

    /// Drain the raw (`raw_transact`-owned) deliveries of **every** live
    /// service object.
    ///
    /// The binder peer calls this on its idle tick so a delivery that was not
    /// caused by a transaction still reaches the guest — the in-process IME's
    /// hide is driven by a key on the input channel, and its `IWindow` insets
    /// callbacks are what make the app relayout for that key.
    pub fn drain_raw_deliveries_all(&mut self) -> Vec<PendingDelivery> {
        let mut out = Vec::new();
        for entry in self.entries.values_mut() {
            if let Some(object) = entry.object.as_mut() {
                out.extend(object.drain_raw_deliveries());
            }
        }
        for object in self.dynamic.values_mut() {
            out.extend(object.drain_raw_deliveries());
        }
        out
    }

    /// Drain `system -> app` deliveries queued by service handlers.
    pub fn take_deliveries(&mut self) -> Vec<PendingDelivery> {
        let mut out = std::mem::take(&mut self.deliveries);
        out.extend(self.take_ams_receiver_deliveries());
        out
    }

    /// The `IIntentReceiver.performReceive` deliveries the session's AMS state
    /// queued but no AMS transaction drained.
    ///
    /// `IIntentSender.send` is served by a **dynamic** object, so a fired
    /// `getBroadcast` sender's matches never pass through
    /// [`crate::atms::AtmsService::call`]. The peer drains the host after every
    /// transaction and control op, so the same drain carries them — one
    /// delivery path for host-originated broadcasts, module broadcasts and a
    /// fired `PendingIntent`.
    fn take_ams_receiver_deliveries(&mut self) -> Vec<PendingDelivery> {
        self.entries
            .get_mut("android.app.IActivityManager")
            .and_then(|entry| entry.object.as_mut())
            .and_then(|object| object.as_any_mut())
            .and_then(|any| any.downcast_mut::<crate::atms::AtmsService>())
            .map(crate::atms::AtmsService::take_receiver_deliveries)
            .unwrap_or_default()
    }

    /// Drain the deliveries a **dynamic** object produced from its own thread
    /// (a module's timed feed: `location`'s `play_track`, a sensor or camera
    /// stream).
    ///
    /// Only the peer's idle tick calls this (`Peer::tick`), because a dynamic
    /// object's `drain_raw_deliveries` may take its own state lock: a caller
    /// that already holds that lock (a test scoring the compositor state, an
    /// action handler) would deadlock, which is exactly why this is not folded
    /// into [`Self::take_deliveries`].
    pub fn take_timed_deliveries(&mut self) -> Vec<PendingDelivery> {
        let now = std::time::Instant::now();
        for object in self.dynamic.values_mut() {
            object.advance_timed(now);
        }
        let mut out = std::mem::take(&mut self.deliveries);
        for object in self.dynamic.values_mut() {
            out.append(&mut object.drain_raw_deliveries());
        }
        out.extend(self.take_ams_receiver_deliveries());
        out
    }

    /// The earliest instant at which a dynamic object's own schedule has a
    /// delivery ready (`None` = nothing scheduled, so the peer may sleep until
    /// a guest frame arrives).
    ///
    /// Read-only, and saturating: an object that is due *now* answers `now`.
    #[must_use]
    pub fn next_timed_deadline(&self) -> Option<std::time::Instant> {
        self.dynamic
            .values()
            .filter_map(|object| object.next_delivery_deadline())
            .min()
    }

    /// Drain lifecycle/log events from every constructed service.
    pub fn drain_events(&mut self) -> Vec<Event> {
        let mut out = Vec::new();
        for entry in self.entries.values_mut() {
            if let Some(obj) = entry.object.as_mut() {
                out.append(&mut obj.drain_events());
            }
        }
        out.sort_by_key(|e| e.seq);
        out
    }

    /// Register the ACOS standard service set.
    pub fn install_standard(&mut self) {
        crate::registry::install(self);
    }

    /// Convenience constructor with the standard service set.
    #[must_use]
    pub fn standard() -> Self {
        let mut host = Self::new();
        host.install_standard();
        host
    }

    /// W7b: the standard service set with an explicit app-window inset model.
    ///
    /// The process environment stays authoritative for [`Self::standard`];
    /// this constructor lets a test or replay driver pin the model without
    /// mutating process-global state.
    #[must_use]
    pub fn standard_with_insets(insets: crate::wms::InsetsMode) -> Self {
        let mut host = Self::new();
        crate::registry::install_with_handles_and_insets(&mut host, Some(insets));
        host
    }

    /// Run a request and return the reply values (used by the replay harness).
    pub fn invoke(
        &mut self,
        binder: BinderId,
        interface: &str,
        method_name: &str,
        args: &[crate::value::Value],
    ) -> Result<Vec<crate::value::Value>> {
        let info = acos_aidl::lookup_by_name(interface, method_name).ok_or_else(|| {
            ServiceError::UnsupportedAndroidApi {
                interface: interface.to_string(),
                code: 0,
                method: method_name.to_string(),
            }
        })?;
        let mut data = codec::build_request(interface, info, args)?;
        data.set_data_position(0);
        let mut reply = Parcel::new();
        self.transact(binder, info.code, &mut data, &mut reply, 0)?;
        reply.set_data_position(0);
        codec::parse_reply(interface, info, &mut reply)
    }

    /// The decoded interface table for an interface (test helper).
    #[must_use]
    pub fn interface(&self, descriptor: &str) -> Option<&'static InterfaceInfo> {
        acos_aidl::interface_for_descriptor(descriptor)
    }

    /// The decoded method table for a `(descriptor, name)` pair.
    #[must_use]
    pub fn method(&self, descriptor: &str, name: &str) -> Option<&'static MethodInfo> {
        acos_aidl::lookup_by_name(descriptor, name)
    }

    /// Render the decoded argument layout for evidence.
    #[must_use]
    pub fn layout(&self, descriptor: &str, name: &str) -> Option<Vec<WireOp>> {
        acos_aidl::lookup_by_name(descriptor, name).map(|m| m.args.clone())
    }
}

struct CallInfo {
    interface: String,
    method: String,
    args: String,
    reply: String,
}

impl ServiceError {
    /// Render the structured error in the shape the agent evidence stream wants.
    #[must_use]
    pub fn evidence(&self) -> serde_json::Value {
        serde_json::json!({
            "code": self.code(),
            "message": self.to_string(),
            "exception_code": self.exception_code(),
            "exception": crate::exception::exception_class(self.exception_code()),
        })
    }
}

/// Write an error reply (exception header) for a failed `transact`.
pub fn write_error_reply(reply: &mut Parcel, error: &ServiceError) {
    reply.reset();
    let code = if error.exception_code() == crate::exception::EX_SERVICE_SPECIFIC {
        EX_SERVICE_SPECIFIC
    } else {
        error.exception_code()
    };
    // `write_exception` cannot fail on a fresh parcel unless the allocator does;
    // keep the signature infallible for the caller.
    let _ = write_exception(reply, code, &error.exception_message());
}

/// The error code for an interface that is deliberately not implemented here.
#[must_use]
pub fn not_owned(interface: &str, method: &str, owner: &'static str) -> ServiceError {
    ServiceError::NotOwned {
        interface: interface.to_string(),
        method: method.to_string(),
        owner,
    }
}

/// A stable error code used in tests.
pub const ERR_UNKNOWN: &str = codes::UNSUPPORTED_ANDROID_API;
