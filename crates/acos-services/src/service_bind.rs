//! AOSP `ActiveServices` for a **guest-initiated, cross-process** `bindService`
//! (MPROC8).
//!
//! On a device `ActivityManagerService.bindService` resolves the intent to a
//! `ServiceRecord` (`ActiveServices.retrieveServiceLocked`), brings the target
//! package's process up (`bringUpServiceLocked` → `startProcessLocked`, lazily,
//! exactly like an activity launch), asks its `IApplicationThread` to create and
//! bind the service (`realStartServiceLocked`:
//! `scheduleCreateService` then `scheduleBindService`), and answers the caller's
//! `IServiceConnection` once the app reports the binder back through
//! `IActivityManager.publishService` (`publishServiceLocked` →
//! `ConnectionRecord.conn.connected(name, service, false)`). An `unbindService`
//! schedules `scheduleUnbindService`/`scheduleStopService`, and a dead process
//! reaches its clients as `connected(name, null, true)` — which
//! `LoadedApk.ServiceDispatcher` turns into `onServiceDisconnected`.
//!
//! ACOS's service layer plays system_server here and the app's real
//! `ActivityThread` does the rest, unchanged. Nothing is synthesised: the
//! `ServiceInfo`, `Intent` and `CompatibilityInfo` payloads are written by the
//! framework's own field order ([`crate::pms::ServiceInfo::to_parcelable`],
//! [`crate::client_transaction::intent`]), the service binder is the app's own
//! `Binder`, and every payload the host does not know is a structured error.
//!
//! ## The service binder crosses processes
//!
//! The binder `Service.onBind` returns lives in B's process, so its
//! [`BinderId`] is a `JAVA_TAG` id carrying B's process tag. Delivering
//! `IServiceConnection.connected` to A's process goes through the peer's
//! `export_for_process`, which rewrites such an id into the synthetic foreign
//! node MPROC3 built: A gets a real `IBinder` whose transactions the peer
//! forwards to B and whose replies it relays back (and whose `linkToDeath` the
//! peer reports through `F_DEATH`). That is the same one routing path every
//! other cross-process binder uses.
//!
//! ## References
//!
//! The `IServiceConnection` A hands over is a binder the host stores for as long
//! as the binding lives, so it is a strong reference: the host keeps it in its
//! reference table ([`crate::host::ServiceHost::retain_binder`], the BINDERREF2
//! ledger), which is what keeps A's node rooted, and drops it on unbind
//! (`unbindService`, a dead client, or session teardown), which is what produces
//! the `F_REF` release.

use acos_parcel::{BinderId, Parcel};

use crate::ams::{DeclaredComponent, IntentFacts};
use crate::client_transaction;
use crate::error::{Result, ServiceError};
use crate::host::PendingDelivery;
use crate::pms::{AppInfo, ServiceInfo};
use crate::value::{Parcelable, Value};

/// The app-side process interface the bind is delivered to.
pub const APP_THREAD: &str = "android.app.IApplicationThread";
/// The caller-side interface the host answers with `connected`.
pub const SERVICE_CONNECTION: &str = "android.app.IServiceConnection";
/// `ActivityThread.PROCESS_STATE_TOP`: the state AMS reports for a bind it is
/// about to deliver.
const PROCESS_STATE_TOP: i32 = 2;

/// `IApplicationThread.scheduleCreateService(IBinder, ServiceInfo,
/// CompatibilityInfo, int)` (API 34 AIDL order, verified against the image).
pub const SCHEDULE_CREATE_SERVICE: u32 = 3;
/// `IApplicationThread.scheduleStopService(IBinder)`.
pub const SCHEDULE_STOP_SERVICE: u32 = 4;
/// `IApplicationThread.scheduleBindService(IBinder, Intent, boolean, int,
/// long)`.
pub const SCHEDULE_BIND_SERVICE: u32 = 11;
/// `IApplicationThread.scheduleUnbindService(IBinder, Intent)`.
pub const SCHEDULE_UNBIND_SERVICE: u32 = 12;
/// `IServiceConnection.connected(ComponentName, IBinder, boolean dead)`.
pub const CONNECTED: u32 = 1;

/// One `IServiceConnection` a client bound with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceConnection {
    /// The caller's `IServiceConnection` binder (an app binder, process-tagged).
    pub binder: u64,
}

/// One bound service (AOSP `ServiceRecord`, reduced to what a session needs).
///
/// The record lives from the first `bindService` until the last connection
/// leaves **and** the service has been stopped in the app, or until the owning
/// process dies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundService {
    /// Owning package.
    pub package: String,
    /// Service class (fully qualified).
    pub class: String,
    /// The host-minted service token (AOSP's `ServiceRecord.token`): the binder
    /// the app receives in `scheduleCreateService`/`scheduleBindService` and
    /// hands back in `publishService`.
    pub token: u64,
    /// Callers still bound.
    pub connections: Vec<ServiceConnection>,
    /// `scheduleCreateService` was delivered (the app has the service in
    /// `mServices`).
    pub created: bool,
    /// `scheduleBindService` was delivered (`onBind` is about to run).
    pub bound: bool,
    /// The binder `Service.onBind` returned (`0` until `publishService`).
    pub binder: u64,
    /// The process tag that owns the service, once it attached.
    pub owner_tag: Option<u32>,
    /// The owner's `IApplicationThread` (the delivery target for
    /// `scheduleUnbindService`/`scheduleStopService`), once it attached.
    pub app_thread: u64,
    /// The `bindSeq` the last `scheduleBindService` carried.
    pub bind_seq: i64,
}

impl BoundService {
    /// `package/class`, the key AOSP keeps the record under.
    #[must_use]
    pub fn key(&self) -> String {
        format!("{}/{}", self.package, self.class)
    }

    /// The component the caller sees in `connected(ComponentName, …)`.
    #[must_use]
    pub fn component(&self) -> String {
        format!("{}/{}", self.package, self.class)
    }
}

/// The session's guest-initiated bind table (one per `AmsState`).
#[derive(Debug, Default, Clone)]
pub struct HostServices {
    /// Live/starting bound services.
    pub services: Vec<BoundService>,
    /// Next `bindSeq` (AOSP's monotonic bind sequence).
    next_bind_seq: i64,
    /// The session runs **real app processes**, so the host performs AOSP's
    /// `ActiveServices` work (create/bind/publish/unbind) instead of leaving the
    /// bind to the in-process runtime. Set by the ART launcher
    /// ([`crate::host::ServiceHost::set_host_side_services`]).
    pub enabled: bool,
    /// Binds refused because the accounting could not be kept (bounded, and
    /// never silently guessed).
    pub refused: u64,
}

impl HostServices {
    /// An empty table.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The record with this token.
    #[must_use]
    pub fn by_token(&self, token: u64) -> Option<&BoundService> {
        self.services.iter().find(|s| s.token == token)
    }

    /// The record with this token, mutably.
    pub fn by_token_mut(&mut self, token: u64) -> Option<&mut BoundService> {
        self.services.iter_mut().find(|s| s.token == token)
    }

    /// The records whose owner process is `tag`.
    #[must_use]
    pub fn of_process(&self, tag: u32) -> Vec<&BoundService> {
        self.services
            .iter()
            .filter(|s| s.owner_tag == Some(tag))
            .collect()
    }

    /// Register a caller's bind and return the deliveries that bring the
    /// service up.
    ///
    /// * `app_thread` is the target process's `IApplicationThread` when it is
    ///   already attached. `None` means the process must start first (the caller
    ///   recorded a spawn request); the deliveries are produced by
    ///   [`Self::flush_for_process`] once it attaches — this is the same lazy
    ///   spawn an activity launch uses.
    /// * `service_info`/`app_info` come from the package manager, so the payload
    ///   is the framework's own `ServiceInfo.writeToParcel` shape.
    #[allow(clippy::too_many_arguments)]
    pub fn bind(
        &mut self,
        resolved: &DeclaredComponent,
        token_binder: BinderId,
        connection: u64,
        intent: &IntentFacts,
        service_info: &ServiceInfo,
        app_info: &AppInfo,
        app_thread: Option<(u32, u64)>,
    ) -> Result<Vec<PendingDelivery>> {
        if !self.enabled {
            // The in-process runtime owns its binds (`AmsState::pending_binds`);
            // a second implementation here would double-deliver.
            return Ok(Vec::new());
        }
        if connection == 0 {
            return Err(ServiceError::BadRequest {
                interface: "android.app.IActivityManager".into(),
                method: "bindService".into(),
                detail: "a null IServiceConnection cannot be bound".into(),
            });
        }
        let key = format!("{}/{}", resolved.package, resolved.class);
        if let Some(record) = self.services.iter_mut().find(|s| s.key() == key) {
            if !record.connections.iter().any(|c| c.binder == connection) {
                record
                    .connections
                    .push(ServiceConnection { binder: connection });
            }
            let mut out = Vec::new();
            if let Some((tag, thread)) = app_thread {
                out.extend(ensure_running(
                    record,
                    token_binder,
                    tag,
                    thread,
                    intent,
                    service_info,
                    app_info,
                    &mut self.next_bind_seq,
                )?);
            }
            return Ok(out);
        }
        let mut record = BoundService {
            package: resolved.package.clone(),
            class: resolved.class.clone(),
            token: token_binder.0,
            connections: vec![ServiceConnection { binder: connection }],
            created: false,
            bound: false,
            binder: 0,
            owner_tag: None,
            app_thread: 0,
            bind_seq: 0,
        };
        let mut out = Vec::new();
        if let Some((tag, thread)) = app_thread {
            out.extend(ensure_running(
                &mut record,
                token_binder,
                tag,
                thread,
                intent,
                service_info,
                app_info,
                &mut self.next_bind_seq,
            )?);
        }
        self.services.push(record);
        Ok(out)
    }

    /// The `(package, class)` records of `package` that still wait for their
    /// process (`owner_tag` unset).
    ///
    /// Called when a process attaches: the host resolves each record's
    /// `ServiceInfo` (it owns the package manager) and then calls
    /// [`Self::assign_process`], so no lock is held across two hosts' state.
    #[must_use]
    pub fn waiting_for_process(&self, package: &str) -> Vec<(String, String)> {
        self.services
            .iter()
            .filter(|s| s.package == package && s.owner_tag.is_none())
            .map(|s| (s.package.clone(), s.class.clone()))
            .collect()
    }

    /// AOSP `realStartServiceLocked` for a record whose process just attached:
    /// deliver `scheduleCreateService` then `scheduleBindService`.
    ///
    /// This is what `ActivityManagerService.attachApplicationLocked` does for
    /// `mServices` before the app's `bindApplication` returns.
    pub fn assign_process(
        &mut self,
        tag: u32,
        app_thread: u64,
        package: &str,
        class: &str,
        service_info: &ServiceInfo,
        app_info: &AppInfo,
    ) -> Result<Vec<PendingDelivery>> {
        let Some(record) = self
            .services
            .iter_mut()
            .find(|s| s.package == package && s.class == class && s.owner_tag.is_none())
        else {
            return Ok(Vec::new());
        };
        let intent = bind_intent(package, class);
        let token = BinderId(record.token);
        ensure_running(
            record,
            token,
            tag,
            app_thread,
            &intent_facts(package, class),
            service_info,
            app_info,
            &mut self.next_bind_seq,
        )
    }

    /// `IActivityManager.publishService(token, intent, binder)`: the app reports
    /// the binder `Service.onBind` returned.
    ///
    /// Returns the `connected` deliveries for every caller still waiting. `None`
    /// when this token is not a guest-initiated bind (another subsystem owns it).
    pub fn publish_service(
        &mut self,
        token: u64,
        binder: u64,
    ) -> Result<Option<Vec<PendingDelivery>>> {
        let Some(index) = self.services.iter().position(|s| s.token == token) else {
            return Ok(None);
        };
        if binder == 0 {
            // `Service.onBind` returned null: AOSP tells the caller
            // `connected(name, null, false)`, i.e. `onNullBinding`. That is a
            // real answer, never a guessed success.
            let record = &mut self.services[index];
            let deliveries = connected_deliveries(record, 0, false)?;
            self.services.remove(index);
            return Ok(Some(deliveries));
        }
        let record = &mut self.services[index];
        record.binder = binder;
        Ok(Some(connected_deliveries(record, binder, false)?))
    }

    /// `unbindService(connection)`: drop one caller.
    ///
    /// Returns the deliveries that tell the app to unbind/stop the service (only
    /// when the last connection left) and the records that are gone.
    pub fn unbind(&mut self, connection: u64) -> (Vec<PendingDelivery>, Vec<BoundService>) {
        let Some(index) = self
            .services
            .iter()
            .position(|s| s.connections.iter().any(|c| c.binder == connection))
        else {
            return (Vec::new(), Vec::new());
        };
        let record = &mut self.services[index];
        record.connections.retain(|c| c.binder != connection);
        if !record.connections.is_empty() {
            return (Vec::new(), Vec::new());
        }
        let mut out = Vec::new();
        let thread = record.app_thread;
        if thread != 0 {
            let intent = bind_intent(&record.package, &record.class);
            if record.created {
                if let Ok(data) = request(
                    SCHEDULE_UNBIND_SERVICE,
                    vec![
                        Value::Binder(Some(BinderId(record.token))),
                        Value::Parcelable(intent),
                    ],
                ) {
                    out.push(PendingDelivery {
                        target: BinderId(thread),
                        interface: APP_THREAD.to_string(),
                        code: SCHEDULE_UNBIND_SERVICE,
                        method: "scheduleUnbindService".into(),
                        data,
                    });
                }
                if let Ok(data) = request(
                    SCHEDULE_STOP_SERVICE,
                    vec![Value::Binder(Some(BinderId(record.token)))],
                ) {
                    out.push(PendingDelivery {
                        target: BinderId(thread),
                        interface: APP_THREAD.to_string(),
                        code: SCHEDULE_STOP_SERVICE,
                        method: "scheduleStopService".into(),
                        data,
                    });
                }
            }
        }
        let gone = self.services.remove(index);
        (out, vec![gone])
    }

    /// A process died: every caller of its services hears
    /// `connected(name, null, true)` (which `LoadedApk` maps to
    /// `onServiceDisconnected`), and the records go.
    pub fn process_died(&mut self, tag: u32) -> Result<Vec<PendingDelivery>> {
        let doomed: Vec<usize> = self
            .services
            .iter()
            .enumerate()
            .filter(|(_, s)| s.owner_tag == Some(tag))
            .map(|(i, _)| i)
            .collect();
        let mut out = Vec::new();
        for index in doomed.into_iter().rev() {
            let record = &mut self.services[index];
            out.extend(connected_deliveries(record, 0, true)?);
            self.services.remove(index);
        }
        Ok(out)
    }

    /// Every connection binder the table still holds (the host's reference
    /// ledger releases them on session teardown).
    #[must_use]
    pub fn connection_binders(&self) -> Vec<u64> {
        let mut out: Vec<u64> = self
            .services
            .iter()
            .flat_map(|s| s.connections.iter().map(|c| c.binder))
            .collect();
        out.sort_unstable();
        out.dedup();
        out
    }
}

/// AOSP `realStartServiceLocked`: create the service (once), then bind it.
fn ensure_running(
    record: &mut BoundService,
    token_binder: BinderId,
    tag: u32,
    thread: u64,
    intent: &IntentFacts,
    service_info: &ServiceInfo,
    app_info: &AppInfo,
    next_bind_seq: &mut i64,
) -> Result<Vec<PendingDelivery>> {
    let mut out = Vec::new();
    record.owner_tag = Some(tag);
    record.app_thread = thread;
    if thread == 0 {
        return Ok(out);
    }
    if !record.created {
        let data = request(
            SCHEDULE_CREATE_SERVICE,
            vec![
                Value::Binder(Some(token_binder)),
                Value::Parcelable(service_info.to_parcelable(app_info)),
                Value::Parcelable(default_compat_info()),
                Value::Int(PROCESS_STATE_TOP),
            ],
        )?;
        out.push(PendingDelivery {
            target: BinderId(thread),
            interface: APP_THREAD.to_string(),
            code: SCHEDULE_CREATE_SERVICE,
            method: "scheduleCreateService".into(),
            data,
        });
        record.created = true;
    }
    if !record.bound {
        let seq = *next_bind_seq;
        *next_bind_seq += 1;
        let data = request(
            SCHEDULE_BIND_SERVICE,
            vec![
                Value::Binder(Some(token_binder)),
                Value::Parcelable(bind_intent(&record.package, &record.class)),
                // `rebind = false`: the framework runs `Service.onBind` and
                // reports the binder through `publishService`.
                Value::Bool(false),
                Value::Int(PROCESS_STATE_TOP),
                Value::Long(seq),
            ],
        )?;
        out.push(PendingDelivery {
            target: BinderId(thread),
            interface: APP_THREAD.to_string(),
            code: SCHEDULE_BIND_SERVICE,
            method: "scheduleBindService".into(),
            data,
        });
        record.bound = true;
        record.bind_seq = seq;
        let _ = intent;
    }
    Ok(out)
}

/// `IServiceConnection.connected(ComponentName, IBinder, boolean dead)` to every
/// caller of `record`.
fn connected_deliveries(
    record: &BoundService,
    binder: u64,
    dead: bool,
) -> Result<Vec<PendingDelivery>> {
    let mut out = Vec::with_capacity(record.connections.len());
    for connection in &record.connections {
        let mut data = Parcel::new();
        data.write_interface_token(SERVICE_CONNECTION)
            .map_err(|e| ServiceError::Internal(format!("connected: token: {e}")))?;
        // AIDL `in ComponentName` is a `writeTypedObject`: presence marker then
        // the parcelable body.
        data.write_int32(1);
        crate::parcelable::write_payload(
            &mut data,
            &component_name(&record.package, &record.class),
        )
        .map_err(|e| ServiceError::Internal(format!("connected: component: {e}")))?;
        data.write_strong_binder(BinderId(binder));
        data.write_int32(i32::from(dead));
        out.push(PendingDelivery {
            target: BinderId(connection.binder),
            interface: SERVICE_CONNECTION.to_string(),
            code: CONNECTED,
            method: "connected".into(),
            data,
        });
    }
    Ok(out)
}

/// `IntentFacts` naming the explicit service component.
fn intent_facts(package: &str, class: &str) -> IntentFacts {
    let mut facts = IntentFacts::default();
    facts.component = Some((package.to_string(), class.to_string()));
    facts
}

/// The bind `Intent` AOSP hands the service: the caller's action/data plus the
/// explicit component (`ActiveServices` sets it before `scheduleBindService`).
fn bind_intent(package: &str, class: &str) -> Parcelable {
    client_transaction::intent_payload(
        None,
        None,
        // `Context.BIND_AUTO_CREATE` lives in the *argument*, not the Intent.
        0,
        Some(package),
        Some((package, class)),
        &[],
        None,
    )
}

/// `android.content.ComponentName` in `writeToParcel` order.
fn component_name(package: &str, class: &str) -> Parcelable {
    Parcelable::ordered(
        "android.content.ComponentName",
        vec![
            Value::Str8(Some(package.to_string())),
            Value::Str8(Some(class.to_string())),
        ],
    )
}

/// `CompatibilityInfo`'s default instance (four scalars, both scales 1.0).
fn default_compat_info() -> Parcelable {
    Parcelable::ordered(
        "android.content.res.CompatibilityInfo",
        vec![
            Value::Int(0),     // compatibilityFlags (DEFAULT)
            Value::Int(160),   // applicationDensity (DENSITY_DEFAULT)
            Value::Float(1.0), // applicationScale
            Value::Float(1.0), // applicationInvertedScale
        ],
    )
}

/// A `PendingDelivery` payload for an `IApplicationThread` call.
fn request(code: u32, args: Vec<Value>) -> Result<Parcel> {
    let method =
        acos_aidl::lookup(APP_THREAD, code).ok_or_else(|| ServiceError::UnsupportedAndroidApi {
            interface: APP_THREAD.to_string(),
            code,
            method: format!("code {code}"),
        })?;
    crate::codec::build_request(APP_THREAD, method, &args)
}

/// The opaque `ServiceRecord.token` (AOSP's system_server-side `Binder`).
///
/// A service token is a *handle*, not an interface: the framework passes it back
/// in `publishService`/`serviceDoneExecuting` and never transacts on it. It is
/// still a real host node, so a transaction addressed to it fails with a
/// structured `UNSUPPORTED_ANDROID_API` naming the method instead of reaching
/// whatever object reused the id.
pub struct ServiceToken;

impl crate::host::ServiceObject for ServiceToken {
    fn call(
        &mut self,
        interface: &str,
        method: &acos_aidl::MethodInfo,
        _args: &[crate::value::Value],
    ) -> Result<crate::host::CallResult> {
        Err(ServiceError::UnsupportedAndroidApi {
            interface: interface.to_string(),
            code: method.code,
            method: method.name.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn service_info() -> (ServiceInfo, AppInfo) {
        (
            ServiceInfo {
                name: "dev.acos.fixtures.procb.EchoService".into(),
                package_name: "dev.acos.fixtures.procb".into(),
                process_name: "dev.acos.fixtures.procb".into(),
                permission: String::new(),
                exported: true,
                flags: 0,
                foreground_service_type: 0,
                meta_data: Vec::new(),
            },
            AppInfo::hello_views_fallback(std::path::Path::new("/data/app/proc-b.apk")),
        )
    }

    fn facts() -> IntentFacts {
        let mut facts = IntentFacts::default();
        facts.component = Some((
            "dev.acos.fixtures.procb".into(),
            "dev.acos.fixtures.procb.EchoService".into(),
        ));
        facts
    }

    #[test]
    fn a_cross_process_bind_schedules_create_then_bind_and_answers_connected() {
        let mut services = HostServices::new();
        services.enabled = true;
        let (info, app) = service_info();
        let resolved = DeclaredComponent {
            package: "dev.acos.fixtures.procb".into(),
            class: "dev.acos.fixtures.procb.EchoService".into(),
            exported: true,
            launch_mode: 0,
            filters: Vec::new(),
        };
        // B is already attached as process 1.
        let out = services
            .bind(
                &resolved,
                BinderId(0x0000_0200_0000_0001),
                /* connection */ 0x0000_0001_0000_0007,
                &facts(),
                &info,
                &app,
                Some((1, 0x0000_0001_0000_0002)),
            )
            .expect("bind");
        assert_eq!(out.len(), 2, "scheduleCreateService + scheduleBindService");
        assert_eq!(out[0].code, SCHEDULE_CREATE_SERVICE);
        assert_eq!(out[1].code, SCHEDULE_BIND_SERVICE);
        assert_eq!(out[0].target.0, 0x0000_0001_0000_0002);

        // The app publishes the binder it made in its own process.
        let service_binder = 0x0000_0001_0000_0009;
        let connected = services
            .publish_service(0x0000_0200_0000_0001, service_binder)
            .expect("publish")
            .expect("a guest bind owns this token");
        assert_eq!(connected.len(), 1);
        assert_eq!(connected[0].target.0, 0x0000_0001_0000_0007);
        assert_eq!(connected[0].interface, SERVICE_CONNECTION);
        let bytes = connected[0].data.marshall();
        // The component name is a `String16` (UTF-16LE), so search the encoded
        // form: the payload really carries this package/class pair.
        let mut utf16 = Vec::new();
        for unit in "dev.acos.fixtures.procb".encode_utf16() {
            utf16.extend_from_slice(&unit.to_le_bytes());
        }
        assert!(
            bytes.windows(utf16.len()).any(|w| w == utf16),
            "the connected parcel names the service's package"
        );
    }

    #[test]
    fn a_bind_to_an_unattached_package_waits_for_its_attach() {
        let mut services = HostServices::new();
        services.enabled = true;
        let (info, app) = service_info();
        let resolved = DeclaredComponent {
            package: "dev.acos.fixtures.procb".into(),
            class: "dev.acos.fixtures.procb.EchoService".into(),
            exported: true,
            launch_mode: 0,
            filters: Vec::new(),
        };
        let out = services
            .bind(
                &resolved,
                BinderId(0x0000_0200_0000_0002),
                0x0000_0001_0000_0008,
                &facts(),
                &info,
                &app,
                None,
            )
            .expect("bind");
        assert!(
            out.is_empty(),
            "nothing to deliver before the process exists"
        );
        assert_eq!(
            services.waiting_for_process("dev.acos.fixtures.procb"),
            vec![(
                "dev.acos.fixtures.procb".to_string(),
                "dev.acos.fixtures.procb.EchoService".to_string()
            )]
        );
        let (info, app) = service_info();
        let flushed = services
            .assign_process(
                1,
                0x0000_0001_0000_0002,
                "dev.acos.fixtures.procb",
                "dev.acos.fixtures.procb.EchoService",
                &info,
                &app,
            )
            .expect("flush");
        assert_eq!(flushed.len(), 2);
    }

    #[test]
    fn a_dead_process_disconnects_its_callers() {
        let mut services = HostServices::new();
        services.enabled = true;
        let (info, app) = service_info();
        let resolved = DeclaredComponent {
            package: "dev.acos.fixtures.procb".into(),
            class: "dev.acos.fixtures.procb.EchoService".into(),
            exported: true,
            launch_mode: 0,
            filters: Vec::new(),
        };
        services
            .bind(
                &resolved,
                BinderId(0x0000_0200_0000_0003),
                0x0000_0001_0000_0007,
                &facts(),
                &info,
                &app,
                Some((1, 0x0000_0001_0000_0002)),
            )
            .expect("bind");
        let out = services.process_died(1).expect("death");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].target.0, 0x0000_0001_0000_0007);
        // dead = true is the last int of the `connected` parcel.
        let bytes = out[0].data.marshall();
        assert_eq!(bytes[bytes.len() - 4..], [1, 0, 0, 0]);
        assert!(services.services.is_empty());
    }

    #[test]
    fn unbind_schedules_unbind_and_stop_for_the_last_connection() {
        let mut services = HostServices::new();
        services.enabled = true;
        let (info, app) = service_info();
        let resolved = DeclaredComponent {
            package: "dev.acos.fixtures.procb".into(),
            class: "dev.acos.fixtures.procb.EchoService".into(),
            exported: true,
            launch_mode: 0,
            filters: Vec::new(),
        };
        services
            .bind(
                &resolved,
                BinderId(0x0000_0200_0000_0004),
                0x0000_0001_0000_0007,
                &facts(),
                &info,
                &app,
                Some((1, 0x0000_0001_0000_0002)),
            )
            .expect("bind");
        let (deliveries, gone) = services.unbind(0x0000_0001_0000_0007);
        assert_eq!(
            deliveries.len(),
            2,
            "scheduleUnbindService + scheduleStopService for the last connection"
        );
        assert_eq!(deliveries[0].code, SCHEDULE_UNBIND_SERVICE);
        assert_eq!(deliveries[1].code, SCHEDULE_STOP_SERVICE);
        assert_eq!(gone.len(), 1);
        assert!(services.services.is_empty());
    }

    #[test]
    fn the_table_is_disabled_for_the_in_process_runtime() {
        let mut services = HostServices::new();
        let (info, app) = service_info();
        let resolved = DeclaredComponent {
            package: "dev.acos.fixtures.procb".into(),
            class: "dev.acos.fixtures.procb.EchoService".into(),
            exported: true,
            launch_mode: 0,
            filters: Vec::new(),
        };
        let out = services
            .bind(
                &resolved,
                BinderId(0x0000_0200_0000_0005),
                0x0000_0001_0000_0007,
                &facts(),
                &info,
                &app,
                Some((1, 0x0000_0001_0000_0002)),
            )
            .expect("bind");
        assert!(out.is_empty(), "the VM runtime owns its own binds");
        assert!(services.services.is_empty());
    }
}
