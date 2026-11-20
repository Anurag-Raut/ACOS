//! MPROC8: a **cross-process** `bindService`, host-side, end to end.
//!
//! The flow this pins is AOSP's `ActiveServices` on one `ServiceHost`:
//!
//! 1. A calls `IActivityManager.bindService` for a service declared by B's
//!    package (`dev.acos.fixtures.procb/.EchoService`);
//! 2. the host records the bind and, because B's process is already attached,
//!    schedules `IApplicationThread.scheduleCreateService` then
//!    `scheduleBindService` **to B's own app thread** (lazy spawn is the other
//!    arm: the bind waits and records a spawn request);
//! 3. B's framework reports the binder its `Service.onBind` returned through
//!    `IActivityManager.publishService(token, intent, binder)`;
//! 4. the host answers A's `IServiceConnection.connected(name, binder, false)`,
//!    carrying B's binder (which the peer exports to A as a foreign node — this
//!    test checks the payload, the peer's routing test checks the crossing);
//! 5. `unbindService` schedules `scheduleUnbindService` + `scheduleStopService`
//!    and drops the reference the host held on A's connection;
//! 6. a dead B process reaches A as `connected(name, null, true)`.
//!
//! Everything is driven through the **real** transaction path
//! (`ServiceHost::transact`), so a codec mistake fails here, not on ART.

use std::path::{Path, PathBuf};

use acos_parcel::{BinderId, Parcel};
use acos_services::atms::{AtmsInterface, AtmsService};
use acos_services::value::Value;
use acos_services::ServiceHost;

const AM: &str = "android.app.IActivityManager";
const B_PACKAGE: &str = "dev.acos.fixtures.procb";
const B_CLASS: &str = "dev.acos.fixtures.procb.EchoService";
/// A's process tag (the primary app) and B's (the second process).
const TAG_A: u32 = 0;
const TAG_B: u32 = 1;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/build/apps")
        .join(name)
}

fn java_binder(tag: u32, wire: u32) -> BinderId {
    // The peer's encoding: JAVA_TAG (bit 32) | tag << 34 | wire.
    BinderId((1u64 << 32) | (u64::from(tag) << 34) | u64::from(wire))
}

/// A host with A installed (as the launched package) and B installed as an extra
/// APK at its own uid, which is exactly what the launcher does for a
/// two-process session.
fn two_package_host() -> ServiceHost {
    let mut host = ServiceHost::standard();
    host.set_host_side_services(true);
    let apk_a = fixture("proc-a.apk");
    let apk_b = fixture("proc-b.apk");
    host.install_package_from_apk_as(&apk_a, Some("/data/app/proc-a.apk"))
        .expect("install proc-a");
    let uid_b = host
        .install_extra_package_from_apk_as(
            &apk_b,
            Some("/data/app/proc-b.apk"),
            Some("/data/app-lib-extra/dev.acos.fixtures.procb/x86_64"),
        )
        .expect("install proc-b");
    assert!(uid_b > 0, "the extra package gets its own uid");
    // The activity manager's manifest table is the resolver for `bindService`,
    // so B's declarations are seeded under the same uid the package manager gave
    // it (the launcher does exactly this).
    let object = host.ensure("android.app.IActivityTaskManager").unwrap();
    let atms = object
        .as_any_mut()
        .and_then(|any| any.downcast_mut::<AtmsService>())
        .expect("atms");
    atms.seed_manifest_from_apk_with_uid(&apk_b, uid_b)
        .expect("seed proc-b manifest");
    atms.seed_manifest_from_apk(&apk_a)
        .expect("seed proc-a manifest");
    host
}

/// Register B's process and attach its `IApplicationThread` for real: the peer
/// learns the tag from the handshake and the attach from the app.
fn attach_b(host: &mut ServiceHost, app_thread: BinderId) {
    let object = host.ensure("android.app.IActivityTaskManager").unwrap();
    let atms = object
        .as_any_mut()
        .and_then(|any| any.downcast_mut::<AtmsService>())
        .expect("atms");
    atms.note_process_started(TAG_B, B_PACKAGE, 0, 0);
    let mut state = atms.shared_state();
    let seq = 1;
    state
        .lock()
        .unwrap()
        .attach_application_for(TAG_B, app_thread.0, seq, B_PACKAGE);
    let _ = AtmsService::new(
        AtmsInterface::ActivityTaskManager,
        std::sync::Arc::clone(&state),
        Default::default(),
    );
    state
        .lock()
        .unwrap()
        .finish_attach_application_for(TAG_B, seq)
        .expect("finish attach");
}

fn transact(host: &mut ServiceHost, service: &str, method: &str, args: Vec<Value>) -> Vec<Value> {
    let binder = host
        .check_service(service)
        .unwrap_or_else(|e| panic!("{service}: {e}"))
        .unwrap_or_else(|| panic!("{service} is registered"));
    let descriptor = if service == "activity" { AM } else { service };
    let info = acos_services::registry::descriptor_for(service)
        .unwrap_or_else(|| panic!("{service} has a descriptor"));
    assert_eq!(info, descriptor, "the table's descriptor");
    let method_info = acos_aidl::lookup_by_name(descriptor, method)
        .unwrap_or_else(|| panic!("{descriptor} has no {method}"));
    let code = method_info.code;
    let mut data = acos_services::codec::build_request(descriptor, method_info, &args)
        .expect("encode request");
    data.set_data_position(0);
    let mut reply = Parcel::new();
    host.transact(binder, code, &mut data, &mut reply, 0)
        .unwrap_or_else(|e| panic!("{method}: {e}"));
    let raw = reply.marshall();
    reply.set_data_position(0);
    let parsed = acos_services::codec::parse_reply(descriptor, method_info, &mut reply);
    if let Err(error) = &parsed {
        panic!("{method}: {error}; reply bytes {raw:02x?}");
    }
    parsed.expect("reply")
}

fn bind_intent() -> Value {
    Value::NamedParcelable(acos_services::client_transaction::intent_payload(
        None,
        None,
        0,
        Some(B_PACKAGE),
        Some((B_PACKAGE, B_CLASS)),
        &[],
        None,
    ))
}

#[test]
fn a_bind_to_a_running_second_process_is_served_by_the_host() {
    let mut host = two_package_host();
    let app_thread_b = java_binder(TAG_B, 3);
    attach_b(&mut host, app_thread_b);
    let connection = java_binder(TAG_A, 5);

    // Round-trip check of the bind Intent: the decoded facts must name B's
    // component (a codec that drops it makes `bindService` unresolvable).
    {
        let Value::NamedParcelable(payload) = bind_intent() else {
            panic!("bind_intent is a parcelable");
        };
        let mut parcel = Parcel::new();
        acos_services::parcelable::write_payload(&mut parcel, &payload).expect("encode intent");
        parcel.set_data_position(0);
        let decoded =
            acos_services::parcelable::read_payload("android.content.Intent", &mut parcel)
                .expect("decode intent");
        let facts = acos_services::ams::IntentFacts::from_parcelable(&decoded);
        assert_eq!(
            facts.component,
            Some((B_PACKAGE.to_string(), B_CLASS.to_string())),
            "the decoded Intent keeps its component"
        );
    }

    // Precondition: the activity manager's resolver knows B's service (the
    // manifest table is what `bindService` resolves against).
    {
        let object = host.ensure("android.app.IActivityTaskManager").unwrap();
        let atms = object
            .as_any_mut()
            .and_then(|any| any.downcast_mut::<AtmsService>())
            .expect("atms");
        let state = atms.shared_state();
        let state = state.lock().unwrap();
        let mut facts = acos_services::ams::IntentFacts::default();
        facts.component = Some((B_PACKAGE.to_string(), B_CLASS.to_string()));
        let resolved = state
            .ams
            .manifests
            .resolve_service(&facts)
            .expect("the manifest table declares B's service");
        assert_eq!(resolved.class, B_CLASS);
    }

    // 1. A binds B's service. The activity manager's own answer is a boolean
    //    (the manifest row resolved), so nothing depends on the synthetic binder
    //    the in-process runtime would name.
    let reply = transact(
        &mut host,
        "activity",
        "bindService",
        vec![
            Value::Binder(Some(java_binder(TAG_A, 1))), // IApplicationThread (caller)
            Value::Binder(None),                        // token
            bind_intent(),
            Value::Str(None), // resolvedType
            Value::Binder(Some(connection)),
            Value::Long(1), // flags (BIND_AUTO_CREATE); the AIDL spells it long
            Value::Str(Some("test".into())), // callingPackage
            Value::Int(0),  // userId
        ],
    );
    assert_eq!(reply.len(), 1, "bindService answers a boolean/int");
    let deliveries = host.take_deliveries();
    let created = deliveries
        .iter()
        .find(|d| d.method == "scheduleCreateService")
        .expect("scheduleCreateService to B");
    let bound = deliveries
        .iter()
        .find(|d| d.method == "scheduleBindService")
        .expect("scheduleBindService to B");
    assert_eq!(
        created.target, app_thread_b,
        "create goes to B's app thread"
    );
    assert_eq!(bound.target, app_thread_b, "bind goes to B's app thread");
    assert_eq!(created.interface, "android.app.IApplicationThread");
    // The token the schedule calls carry is the one the app must publish.
    let mut token_parcel = created.data.clone();
    token_parcel.set_data_position(0);
    token_parcel
        .enforce_interface("android.app.IApplicationThread")
        .expect("interface token");
    let token = token_parcel
        .read_strong_binder()
        .expect("token binder")
        .expect("a real token");
    assert_ne!(token.0, 0);

    // 2. B's `Service.onBind` returned a binder that lives in B's process.
    let service_binder = java_binder(TAG_B, 9);
    let _ = transact(
        &mut host,
        "activity",
        "publishService",
        vec![
            Value::Binder(Some(token)),
            bind_intent(),
            Value::Binder(Some(service_binder)),
        ],
    );
    let deliveries = host.take_deliveries();
    let connected = deliveries
        .iter()
        .find(|d| d.method == "connected")
        .expect("connected to A's IServiceConnection");
    assert_eq!(connected.target, connection, "the caller's own connection");
    assert_eq!(connected.interface, "android.app.IServiceConnection");
    // The payload really carries B's binder (the peer turns it into A's foreign
    // node) and `dead = false`.
    let mut parcel = connected.data.clone();
    parcel.set_data_position(0);
    parcel
        .enforce_interface("android.app.IServiceConnection")
        .expect("interface token");
    assert_eq!(parcel.read_int32().expect("presence marker"), 1);
    // `ComponentName.writeToParcel` is two `writeString` (String16) fields.
    let component = parcel
        .read_string16()
        .expect("component package")
        .expect("non-null package");
    assert_eq!(component, B_PACKAGE);
    let _ = parcel.read_string16().expect("component class");
    let service = parcel
        .read_strong_binder()
        .expect("service binder")
        .expect("non-null");
    assert_eq!(service, service_binder, "the service's own binder crosses");
    assert_eq!(parcel.read_int32().expect("dead flag"), 0, "not dead");
    // The host holds A's connection: one strong reference on a guest node
    // (BINDERREF2). The peer reports it to the driver as `F_REF`.
    assert_eq!(host.held_refs(connection), 1);

    // 3. `unbindService`: the last connection leaves, so the app is told to
    //    unbind and stop, and the reference is dropped (the driver gets the
    //    release).
    let reply = transact(
        &mut host,
        "activity",
        "unbindService",
        vec![Value::Binder(Some(connection))],
    );
    assert_eq!(reply, vec![Value::Bool(true)], "unbindService answers true");
    let deliveries = host.take_deliveries();
    assert!(
        deliveries
            .iter()
            .any(|d| d.method == "scheduleUnbindService"),
        "the app is told to unbind: {deliveries:?}"
    );
    assert!(
        deliveries.iter().any(|d| d.method == "scheduleStopService"),
        "the app is told to stop the service: {deliveries:?}"
    );
    assert_eq!(
        host.held_refs(connection),
        0,
        "the host no longer holds the caller's connection"
    );
    assert_eq!(
        host.take_released_binders(),
        vec![connection],
        "the release is what the peer turns into F_REF"
    );
}

#[test]
fn a_dead_service_process_reaches_its_caller_as_a_disconnect() {
    let mut host = two_package_host();
    let app_thread_b = java_binder(TAG_B, 3);
    attach_b(&mut host, app_thread_b);
    let connection = java_binder(TAG_A, 5);
    let _ = transact(
        &mut host,
        "activity",
        "bindService",
        vec![
            Value::Binder(Some(java_binder(TAG_A, 1))),
            Value::Binder(None),
            bind_intent(),
            Value::Str(None),
            Value::Binder(Some(connection)),
            Value::Long(1),
            Value::Str(Some("test".into())),
            Value::Int(0),
        ],
    );
    let _ = host.take_deliveries();
    // B's process dies (the peer drops its connection).
    host.service_process_died(TAG_B);
    let deliveries = host.take_deliveries();
    let disconnected = deliveries
        .iter()
        .find(|d| d.method == "connected")
        .expect("connected(dead=true) to the caller");
    assert_eq!(disconnected.target, connection);
    let mut parcel = disconnected.data.clone();
    parcel.set_data_position(0);
    parcel
        .enforce_interface("android.app.IServiceConnection")
        .expect("interface token");
    parcel.read_int32().expect("presence marker");
    let _ = parcel.read_string16();
    let _ = parcel.read_string16();
    let binder = parcel.read_strong_binder().expect("binder field");
    assert!(binder.is_none(), "a dead service is a null binder");
    assert_eq!(parcel.read_int32().expect("dead flag"), 1, "dead = true");
}

/// MPROC8/PROCUID: AOSP's `realStartServiceLocked` for a process that exists
/// **only** because of the bind.
///
/// A `bindService` to a package with no process is `startProcessLocked`: the
/// launcher starts one, the process names its `IApplicationThread` and it has no
/// activity at all (nothing asked for one). AOSP delivers the service work in
/// `attachApplicationLocked` — `mServices.attachApplicationLocked` runs there,
/// after the app's `bindApplication` is sent and before the app can report back.
/// Waiting for the process to *finish* attaching (`finishAttachApplication`) or
/// for an activity launch strands the bind: the caller waits out its timeout and
/// the service never exists.
#[test]
fn a_bind_only_process_gets_create_and_bind_when_it_attaches() {
    let mut host = two_package_host();
    let connection = java_binder(TAG_A, 5);

    // 1. No process for B yet: the bind is recorded and a spawn is requested
    //    (nothing can be delivered to a thread that does not exist).
    let reply = transact(
        &mut host,
        "activity",
        "bindService",
        vec![
            Value::Binder(Some(java_binder(TAG_A, 1))),
            Value::Binder(None),
            bind_intent(),
            Value::Str(None),
            Value::Binder(Some(connection)),
            Value::Long(1),
            Value::Str(Some("test".into())),
            Value::Int(0),
        ],
    );
    assert_eq!(reply.len(), 1, "bindService answers its boolean");
    assert!(
        host.take_deliveries().is_empty(),
        "there is no app thread to deliver create/bind to yet"
    );
    let requests = {
        let object = host.ensure("android.app.IActivityTaskManager").unwrap();
        let atms = object
            .as_any_mut()
            .and_then(|any| any.downcast_mut::<AtmsService>())
            .expect("atms");
        atms.take_spawn_requests()
    };
    assert_eq!(requests.len(), 1, "exactly one spawn request: {requests:?}");
    assert_eq!(requests[0].package, B_PACKAGE);
    assert_eq!(requests[0].reason, "bindService");
    assert_eq!(requests[0].component, format!("{B_PACKAGE}/{B_CLASS}"));

    // 2. The launcher started it: the peer records the process from the guest's
    //    handshake (its own uid) and the guest attaches its app thread. Nothing
    //    else happens — a bind-only process never launches an activity, so this
    //    `attachApplication` is the only thing the host will ever hear from it.
    let app_thread_b = java_binder(TAG_B, 3);
    {
        let object = host.ensure("android.app.IActivityTaskManager").unwrap();
        let atms = object
            .as_any_mut()
            .and_then(|any| any.downcast_mut::<AtmsService>())
            .expect("atms");
        atms.note_process_started(TAG_B, B_PACKAGE, 0, 0);
    }
    let _ = transact(
        &mut host,
        "activity",
        "attachApplication",
        vec![Value::Binder(Some(app_thread_b)), Value::Long(1)],
    );

    // 3. AOSP order at attach: create, then bind, both to B's own app thread.
    let deliveries = host.take_deliveries();
    let created = deliveries
        .iter()
        .find(|d| d.method == "scheduleCreateService")
        .unwrap_or_else(|| panic!("scheduleCreateService to B: {deliveries:?}"));
    let bound = deliveries
        .iter()
        .find(|d| d.method == "scheduleBindService")
        .unwrap_or_else(|| panic!("scheduleBindService to B: {deliveries:?}"));
    assert_eq!(created.target, app_thread_b, "create goes to B's app thread");
    assert_eq!(bound.target, app_thread_b, "bind goes to B's app thread");
    assert_eq!(created.interface, "android.app.IApplicationThread");
    let create_at = deliveries
        .iter()
        .position(|d| d.method == "scheduleCreateService")
        .expect("create present");
    let bind_at = deliveries
        .iter()
        .position(|d| d.method == "scheduleBindService")
        .expect("bind present");
    assert!(
        create_at < bind_at,
        "`realStartServiceLocked` creates before it binds: {deliveries:?}"
    );

    // 4. The service's `onBind` result reaches A's connection, so the bind-only
    //    spawn is a *served* bind, not just two queued deliveries.
    let mut token_parcel = created.data.clone();
    token_parcel.set_data_position(0);
    token_parcel
        .enforce_interface("android.app.IApplicationThread")
        .expect("interface token");
    let token = token_parcel
        .read_strong_binder()
        .expect("token binder")
        .expect("a real token");
    let service_binder = java_binder(TAG_B, 9);
    let _ = transact(
        &mut host,
        "activity",
        "publishService",
        vec![
            Value::Binder(Some(token)),
            bind_intent(),
            Value::Binder(Some(service_binder)),
        ],
    );
    let deliveries = host.take_deliveries();
    let connected = deliveries
        .iter()
        .find(|d| d.method == "connected")
        .unwrap_or_else(|| panic!("connected to A's connection: {deliveries:?}"));
    assert_eq!(connected.target, connection);
}

/// MPROC8/PROCUID: a bind that lands while the target process is attaching.
///
/// The process record exists (the peer created it at the guest's handshake) and
/// the app thread is known, but the app has not finished `bindApplication`. AOSP
/// still queues the service work for that process — it runs on the app's looper
/// behind `BIND_APPLICATION`. Treating such a target as "no process" recorded a
/// spawn request for a package that is already running (which the launcher
/// correctly refuses) and left the bind waiting for an attach that had already
/// happened.
#[test]
fn a_bind_to_a_process_that_is_still_attaching_is_delivered_to_it() {
    let mut host = two_package_host();
    let app_thread_b = java_binder(TAG_B, 3);
    let connection = java_binder(TAG_A, 5);

    {
        let object = host.ensure("android.app.IActivityTaskManager").unwrap();
        let atms = object
            .as_any_mut()
            .and_then(|any| any.downcast_mut::<AtmsService>())
            .expect("atms");
        atms.note_process_started(TAG_B, B_PACKAGE, 0, 0);
    }
    let _ = transact(
        &mut host,
        "activity",
        "attachApplication",
        vec![Value::Binder(Some(app_thread_b)), Value::Long(1)],
    );
    assert!(
        host.take_deliveries().is_empty(),
        "nothing was waiting for B at its attach"
    );

    let _ = transact(
        &mut host,
        "activity",
        "bindService",
        vec![
            Value::Binder(Some(java_binder(TAG_A, 1))),
            Value::Binder(None),
            bind_intent(),
            Value::Str(None),
            Value::Binder(Some(connection)),
            Value::Long(1),
            Value::Str(Some("test".into())),
            Value::Int(0),
        ],
    );
    let deliveries = host.take_deliveries();
    assert!(
        deliveries
            .iter()
            .any(|d| d.method == "scheduleCreateService" && d.target == app_thread_b),
        "create goes to the attaching process: {deliveries:?}"
    );
    assert!(
        deliveries
            .iter()
            .any(|d| d.method == "scheduleBindService" && d.target == app_thread_b),
        "bind goes to the attaching process: {deliveries:?}"
    );
    // The resolution path (`AmsState::bind_service`) always records the need; the
    // launcher is the authority on what is running and refuses a spawn for a
    // package that already has a process (`SecondaryPool::spawn`). What must not
    // happen is what the old lookup did: treat the attaching process as absent
    // and therefore deliver nothing at all.
    let requests = {
        let object = host.ensure("android.app.IActivityTaskManager").unwrap();
        let atms = object
            .as_any_mut()
            .and_then(|any| any.downcast_mut::<AtmsService>())
            .expect("atms");
        atms.take_spawn_requests()
    };
    assert_eq!(
        requests.len(),
        1,
        "the bind resolution records the need (the launcher dedups it): {requests:?}"
    );
}
