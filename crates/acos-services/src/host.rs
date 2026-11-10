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
use acos_parcel::{BinderId, BinderRegistry, Parcel};
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
}

impl Default for Wiring {
    fn default() -> Self {
        Self {
            self_binder: BinderId(0),
            controller: BinderId(0),
            content_provider: BinderId(0),
        }
    }
}

type Factory = Arc<dyn Fn(Wiring) -> Box<dyn ServiceObject> + Send + Sync>;

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
    deliveries: Vec<PendingDelivery>,
    trace: Option<Box<dyn TraceSink>>,
    seq: AtomicU64,
    constructions: u64,
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
            deliveries: Vec::new(),
            trace: None,
            seq: AtomicU64::new(1),
            constructions: 0,
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

    /// Register an auxiliary binder (a binder returned by another service).
    pub fn register_aux(&mut self, descriptor: &'static str) -> BinderId {
        let binder = self.registry.register_local(descriptor);
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
    pub fn check_service(&mut self, name: &str) -> Result<Option<BinderId>> {        let Some(descriptor) = self.names.get(name).copied() else {
            return Ok(None);
        };
        let Some(entry) = self.entries.get(&descriptor) else {
            return Ok(None);
        };
        let binder = entry.binder;
        self.ensure(descriptor)?;
        Ok(Some(binder))
    }

    /// Resolve a descriptor's binder without constructing the service.
    #[must_use]
    pub fn binder_for_descriptor(&self, descriptor: &str) -> Option<BinderId> {
        self.entries
            .get(descriptor)
            .map(|e| e.binder)
            .or_else(|| self.aux.get(descriptor).copied())
    }

    /// The descriptor a binder handle was registered under.
    #[must_use]
    pub fn descriptor_of(&self, binder: BinderId) -> Option<&'static str> {
        self.binder_descriptors.get(&binder.0).copied()
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

    /// Re-point the in-process package manager at `apk_path`.
    ///
    /// `ServiceHost::standard()` seeds the PMS with the `hello-views` fixture;
    /// the runtime calls this before boot so the framework's
    /// `getApplicationInfo(<launched package>)` resolves the real APK instead
    /// of raising `unknown service name <package>`. Returns an error (never a
    /// silent no-op) when the package service is missing or mistyped.
    pub fn install_package_from_apk(&mut self, apk_path: &std::path::Path) -> Result<()> {
        let object = self.ensure("android.content.pm.IPackageManager")?;
        let service = object
            .as_any_mut()
            .and_then(|any| any.downcast_mut::<crate::pms::PmsService>())
            .ok_or_else(|| {
                ServiceError::Internal("package service is not a PmsService".into())
            })?;
        service.set_package_from_apk(apk_path)
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
            let method = acos_aidl::lookup(&descriptor, code)
                .map(|m| m.name.clone())
                .unwrap_or_else(|| format!("code {code}"));
            return Err(ServiceError::NotOwned {
                interface: descriptor,
                method,
                owner,
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
        let args = codec::read_args(data, &method.args, &descriptor, &method.name)?;
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
