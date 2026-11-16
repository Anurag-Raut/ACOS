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

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use acos_aidl::{InterfaceInfo, MethodInfo, WireOp};
use acos_parcel::{BinderId, BinderKind, BinderRegistry, Parcel};
use acos_protocol::Event;

use crate::codec;
use crate::error::{codes, ServiceError, Result};
use crate::exception::{write_exception, write_no_exception, EX_SERVICE_SPECIFIC};
use crate::trace::{self, CallTrace, TraceSink};

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
}

/// Wiring handed to a lazily-constructed service.
#[derive(Clone, Copy, Debug)]
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
}

impl Default for Wiring {
    fn default() -> Self {
        Self {
            self_binder: BinderId(0),
            controller: BinderId(0),
            content_provider: BinderId(0),
            package_installer: BinderId(0),
        }
    }
}

type Factory = Arc<dyn Fn(Wiring) -> Box<dyn ServiceObject> + Send + Sync>;

/// The registration tables as they were at [`ServiceHost::begin_scope`].
///
/// ADR-0029 requires that detaching a module leaves **no** registration behind,
/// including the ones a module registered indirectly (an aux binder, a dynamic
/// node, an alias). Keeping the pre-attach snapshot is the only way to prove
/// that by construction instead of trusting each module's `detach` to enumerate
/// what its `attach` did.
#[derive(Debug)]
pub struct RegistrationScope {
    entries: Vec<&'static str>,
    names: HashMap<&'static str, &'static str>,
    aux: HashMap<&'static str, BinderId>,
    dynamic: Vec<u64>,
    binder_descriptors: HashMap<u64, &'static str>,
    notification: bool,
}

struct ServiceEntry {
    binder: BinderId,
    factory: Factory,
    object: Option<Box<dyn ServiceObject>>,
}

/// The dispatcher.
pub struct ServiceHost {
    registry: BinderRegistry,
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
    deliveries: Vec<PendingDelivery>,
    trace: Option<Box<dyn TraceSink>>,
    seq: AtomicU64,
    constructions: u64,
    /// The `INotificationManager` state, kept here so the runtime (which plays
    /// the system-UI role) can surface a toast the app asked for. Android shows
    /// a text toast from `NotificationManagerService` in the system process;
    /// ACOS has one process, so the runtime renders the recorded request.
    notification: Option<std::sync::Arc<std::sync::Mutex<crate::declared::NotificationState>>>,
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
        Self {
            registry: BinderRegistry::new(),
            entries: HashMap::new(),
            binder_descriptors: HashMap::new(),
            names: HashMap::new(),
            aux: HashMap::new(),
            dynamic: HashMap::new(),
            deliveries: Vec::new(),
            trace: None,
            seq: AtomicU64::new(1),
            constructions: 0,
            notification: None,
        }
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
        self.entries.insert(
            descriptor,
            ServiceEntry {
                binder,
                factory: Arc::new(factory),
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
        let binder = self.register_dynamic(descriptor, object);
        self.names.insert(name, descriptor);
        binder
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
    }

    /// Register an auxiliary binder (a binder returned by another service).
    pub fn register_aux(&mut self, descriptor: &'static str) -> BinderId {        let binder = self.registry.register_local(descriptor);
        self.binder_descriptors.insert(binder.0, descriptor);
        self.aux.insert(descriptor, binder);
        binder
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
                .or_else(|| {
                    self.aux
                        .keys()
                        .copied()
                        .find(|k| *k == desc.as_str())
                });
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

    /// Snapshot the registration tables so a module's registrations can be
    /// rolled back as a unit (ADR-0029 detach).
    #[must_use]
    pub fn begin_scope(&self) -> RegistrationScope {
        RegistrationScope {
            entries: self.entries.keys().copied().collect(),
            names: self.names.clone(),
            aux: self.aux.clone(),
            dynamic: self.dynamic.keys().copied().collect(),
            binder_descriptors: self.binder_descriptors.clone(),
            notification: self.notification.is_some(),
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
            if let Some(entry) = self.entries.remove(key) {
                freed.push(entry.binder);
            }
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
        for (name, descriptor) in &scope.names {
            self.names.insert(name, descriptor);
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
            self.dynamic.remove(&id);
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
        if !scope.notification {
            self.notification = None;
        }
        for binder in &freed {
            self.registry.remove(*binder);
        }
        self.deliveries.retain(|delivery| {
            !freed.iter().any(|binder| binder.0 == delivery.target.0)
        });
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

    /// The `INotificationManager` state, when the standard set is installed.
    #[must_use]
    pub fn notification_state(
        &self,
    ) -> Option<std::sync::Arc<std::sync::Mutex<crate::declared::NotificationState>>> {
        self.notification.as_ref().map(std::sync::Arc::clone)
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

    /// True when `name` is registered.
    #[must_use]
    pub fn is_declared(&self, name: &str) -> bool {
        self.names.contains_key(name)
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

    /// Re-point the in-process package manager at `apk_path`.
    ///
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
        let object = self.ensure("android.content.pm.IPackageManager")?;
        let service = object
            .as_any_mut()
            .and_then(|any| any.downcast_mut::<crate::pms::PmsService>())
            .ok_or_else(|| {
                ServiceError::Internal("package service is not a PmsService".into())
            })?;
        service.set_package_from_apk_as(apk_path, guest_source_dir)
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
        let binder_descriptor = self
            .binder_descriptors
            .get(&binder.0)
            .copied()
            .ok_or(ServiceError::UnknownBinder { handle: binder.0 })?
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
            let object = self
                .dynamic
                .get_mut(&binder.0)
                .expect("checked above");
            let handled = object.raw_transact(&descriptor, code, data, reply)?;
            let finished = object.finished();
            let mut raw_deliveries = object.drain_raw_deliveries();
            if finished {
                self.dynamic.remove(&binder.0);
            }
            self.deliveries.append(&mut raw_deliveries);
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
        self.deliveries.append(&mut call.deliveries);

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

    /// Drain `system -> app` deliveries queued by service handlers.
    pub fn take_deliveries(&mut self) -> Vec<PendingDelivery> {
        std::mem::take(&mut self.deliveries)
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
