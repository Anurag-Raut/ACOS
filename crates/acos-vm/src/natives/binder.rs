//! `android.os.Binder` / `android.os.BinderProxy` / `BinderInternal` natives
//! and the Binder transport seam (T-19).
//!
//! This file is the VM end of the Binder replacement. The real Android 14
//! framework bytecode (`Binder.transact` -> `Binder.onTransact`,
//! `BinderProxy.transact` -> `BinderProxy.transactNative`,
//! `Binder.execTransact` -> `Binder.execTransactInternal` -> `onTransact`) is
//! executed unchanged; ACOS only replaces the *transport* behind
//! `transactNative` and the binder handle plumbing.
//!
//! # Native surface
//!
//! Enumerated from the Android 14 boot DEX
//! (`.cache/boot/android-34/dex/framework/classes3.dex` / `classes4.dex`):
//!
//! `Landroid/os/Binder;`:
//! `blockUntilThreadAvailable`, `clearCallingIdentity`, `clearCallingWorkSource`,
//! `flushPendingCommands`, `getCallingPid`, `getCallingUid`,
//! `getCallingWorkSourceUid`, `getNativeBBinderHolder`, `getNativeFinalizer`,
//! `getThreadStrictModePolicy`, `hasExplicitIdentity`,
//! `isDirectlyHandlingTransaction`, `restoreCallingIdentity`,
//! `restoreCallingWorkSource`, `setCallingWorkSourceUid`,
//! `setThreadStrictModePolicy`, `forceDowngradeToSystemStability`,
//! `getExtension`, `markVintfStability`, `setExtension`.
//!
//! `Landroid/os/BinderProxy;`: `getNativeFinalizer`, `getExtension`,
//! `getInterfaceDescriptor`, `isBinderAlive`, `linkToDeath`, `pingBinder`,
//! `transactNative`, `unlinkToDeath`.
//!
//! `Lcom/android/internal/os/BinderInternal;`: `getContextObject` plus the
//! bookkeeping no-ops (`disableBackgroundScheduling`, `handleGc`,
//! `joinThreadPool`, `nGetBinderProxyCount`, `nGetBinderProxyPerUidCounts`,
//! `nSetBinderProxyCountEnabled`, `nSetBinderProxyCountWatermarks`,
//! `setMaxThreads`).
//!
//! # The transport seam
//!
//! [`BinderTransport`] is installed by `acos-runtime` (the only crate that owns
//! a `ServiceHost`). It maps a Java `IBinder` to the process-local `BinderId`
//! that rides in a `Parcel`, maps a `BinderId` back to a Java `IBinder`, and
//! dispatches one transaction. `transact` returns any `system -> app`
//! [`BinderDelivery`]s produced by the service handler; this file then invokes
//! the real Java `Binder.execTransact(code, dataPtr, replyPtr, flags)` on the
//! current looper thread, so the app's own `*$Stub.onTransact` runs.
//!
//! When no transport is installed (bootstrap corelib, isolated parcel tests) a
//! small in-memory identity map keeps `writeStrongBinder`/`readStrongBinder`
//! round-tripping, exactly like the default [`super::parcel::BinderObjectSink`].

use std::cell::{Cell, RefCell};
use std::collections::HashMap;

use acos_parcel::binder::BinderId;
use acos_parcel::registry::{
    clear_calling_identity, calling_pid, calling_uid, restore_calling_identity,
    set_thread_strict_mode_policy, set_thread_work_source_uid, thread_strict_mode_policy,
    thread_work_source_uid, IdentityToken,
};
use acos_parcel::{Parcel, ParcelError};

use crate::class::NativeCtx;
use crate::error::{VmError, VmResult};
use crate::heap::{Handle, NULL};
use crate::javabridge::JavaValue;
use crate::vm::Vm;

// ---------------------------------------------------------------------------
// Transport seam
// ---------------------------------------------------------------------------

/// An outgoing transaction waiting for the target app looper.
#[derive(Debug)]
pub struct BinderDelivery {
    /// Target binder (`BinderId` of an app-side Java `Binder`).
    pub target: u64,
    /// Transaction code.
    pub code: u32,
    /// Transaction flags (`1` = oneway).
    pub flags: u32,
    /// Fully encoded request parcel (interface token already written).
    pub data: Parcel,
}

/// The transport implementation installed by `acos-runtime`.
///
/// Every method is called only while the VM is executing on the host thread, so
/// implementations may freely touch the VM (the `ServiceHost` is single
/// threaded and owned by the same runtime).
pub trait BinderTransport {
    /// Allocate a fresh local binder id (`Binder.getNativeBBinderHolder`).
    fn new_local_binder(&mut self, vm: &mut Vm) -> u64 {
        let _ = vm;
        next_fallback_id()
    }

    /// The binder returned by `BinderInternal.getContextObject()`, or `0`.
    fn context_object(&mut self, vm: &mut Vm) -> u64 {
        let _ = vm;
        0
    }

    /// Map a Java `IBinder` handle to the id written into a `Parcel`.
    fn binder_id_of(&mut self, vm: &mut Vm, obj: Handle) -> Option<BinderId>;

    /// Map a `BinderId` back to a Java `IBinder` handle.
    fn binder_object(&mut self, vm: &mut Vm, id: BinderId) -> Option<Handle>;

    /// Dispatch one transaction. Returns the `system -> app` deliveries the
    /// handler produced (empty for a normal synchronous reply).
    #[allow(clippy::too_many_arguments)]
    fn transact(
        &mut self,
        vm: &mut Vm,
        binder: BinderId,
        code: u32,
        data: &mut Parcel,
        reply: &mut Parcel,
        flags: u32,
    ) -> Result<Vec<BinderDelivery>, String>;

    /// The interface descriptor of a binder (`BinderProxy.getInterfaceDescriptor`).
    fn descriptor(&mut self, id: BinderId) -> Option<String>;

    /// `BinderProxy.isBinderAlive` / `pingBinder`.
    fn is_alive(&mut self, id: BinderId) -> bool;

    /// `linkToDeath`. ACOS binders never die; recorded but never fired.
    fn link_to_death(&mut self, _id: BinderId, _recipient: Handle) -> bool {
        true
    }

    /// `unlinkToDeath`.
    fn unlink_to_death(&mut self, _id: BinderId, _recipient: Handle) -> bool {
        true
    }
}

thread_local! {
    static TRANSPORT: RefCell<Option<Box<dyn BinderTransport>>> = const { RefCell::new(None) };
    /// Fallback identity map used when no transport is installed.
    static FALLBACK: RefCell<FallbackSink> = RefCell::new(FallbackSink::default());
    /// `Binder.clearCallingIdentity` tokens, one stack per host thread.
    static IDENTITY: RefCell<Vec<IdentityToken>> = const { RefCell::new(Vec::new()) };
    static NEXT_ID: Cell<u64> = const { Cell::new(1) };
}

/// Install the process Binder transport. Replaces any previous transport.
pub fn install_transport(transport: Box<dyn BinderTransport>) {
    TRANSPORT.with(|t| *t.borrow_mut() = Some(transport));
}

/// Remove the installed transport (test teardown).
pub fn clear_transport() {
    TRANSPORT.with(|t| *t.borrow_mut() = None);
}

/// True when a transport is installed on this host thread.
#[must_use]
pub fn has_transport() -> bool {
    TRANSPORT.with(|t| t.borrow().is_some())
}

fn with_transport<R>(f: impl FnOnce(&mut dyn BinderTransport) -> R) -> Option<R> {
    TRANSPORT.with(|t| {
        let mut slot = t.borrow_mut();
        slot.as_mut().map(|b| f(b.as_mut()))
    })
}

fn next_fallback_id() -> u64 {
    NEXT_ID.with(|c| {
        let v = c.get();
        c.set(v.wrapping_add(1).max(1));
        v
    })
}

/// In-memory `Handle <-> BinderId` map used without a transport.
#[derive(Default)]
struct FallbackSink {
    by_handle: HashMap<u32, BinderId>,
    by_id: HashMap<u64, u32>,
}

impl FallbackSink {
    fn binder_id(&mut self, handle: Handle) -> Option<BinderId> {
        if handle == NULL {
            return None;
        }
        if let Some(&id) = self.by_handle.get(&handle) {
            return Some(id);
        }
        let id = BinderId(next_fallback_id());
        self.by_handle.insert(handle, id);
        self.by_id.insert(id.0, handle);
        Some(id)
    }

    fn binder_object(&mut self, id: BinderId) -> Option<Handle> {
        self.by_id.get(&id.0).copied()
    }
}

// ---------------------------------------------------------------------------
// Strong binders: `Parcel.writeStrongBinder` / `readStrongBinder`
// ---------------------------------------------------------------------------

fn binder_id_of(vm: &mut Vm, obj: Handle) -> Option<BinderId> {
    if obj == NULL {
        return None;
    }
    // A `BinderProxy` carries its handle in `mNativeData`. Read it directly
    // instead of asking the transport: materialising a reply that contains a
    // binder (e.g. `ContentProviderHolder`) runs *inside* the transport's
    // `transact`, and re-borrowing the transport RefCell for the same lookup
    // would panic.
    if let Ok(JavaValue::Long(v)) = vm.get_field(obj, "mNativeData") {
        if v != 0 {
            return Some(BinderId(v as u64));
        }
    }
    let mapped = with_transport(|t| t.binder_id_of(vm, obj)).flatten();
    if mapped.is_some() {
        return mapped;
    }
    // No transport: fall back to an identity map (bootstrap / isolated tests).
    let is_proxy = vm
        .get_field(obj, "mNativeData")
        .ok()
        .and_then(|v| match v {
            JavaValue::Long(_) | JavaValue::Int(_) => Some(()),
            _ => None,
        })
        .is_some();
    if is_proxy {
        // A proxy without a transport has no service behind it.
        return None;
    }
    FALLBACK.with(|f| f.borrow_mut().binder_id(obj))
}

fn binder_object(vm: &mut Vm, id: BinderId) -> Option<Handle> {
    if id.is_null() {
        return None;
    }
    if let Some(obj) = with_transport(|t| t.binder_object(vm, id)).flatten() {
        return Some(obj);
    }
    FALLBACK.with(|f| f.borrow_mut().binder_object(id))
}

fn write_strong_binder(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let handle = vm.arg_i64(ctx, 0) as u64;
    let obj = vm.arg_ref(ctx, 1);
    let id = binder_id_of(vm, obj);
    let r = super::parcel::with_store(|s| {
        s.table.with_parcel(handle, |p| p.write_strong_binder_opt(id))
    });
    r.map_err(|e| parcel_err_at("writeStrongBinder", e))
}

fn read_strong_binder(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let handle = vm.arg_i64(ctx, 0) as u64;
    let id = match super::parcel::with_store(|s| s.table.with_parcel(handle, Parcel::read_strong_binder))
    {
        Ok(Ok(id)) => id,
        Ok(Err(e)) | Err(e) => return Err(parcel_err_at("readStrongBinder", e)),
    };
    let obj = id.and_then(|id| binder_object(vm, id)).unwrap_or(NULL);
    vm.ret_ref(ctx, obj);
    Ok(())
}

fn mark_for_binder(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let handle = vm.arg_i64(ctx, 0) as u64;
    let obj = vm.arg_ref(ctx, 1);
    let id = binder_id_of(vm, obj);
    let r = super::parcel::with_store(|s| {
        s.table.with_parcel(handle, |p| {
            if let Some(id) = id {
                p.mark_for_binder(id);
            }
        })
    });
    r.map_err(|e| parcel_err_at("markForBinder", e))
}

fn parcel_err_at(what: &str, e: ParcelError) -> VmError {
    VmError::Internal(format!("binder parcel [{what}]: {e}"))
}

// ---------------------------------------------------------------------------
// `BinderProxy.transactNative` — the one outgoing native
// ---------------------------------------------------------------------------

/// Read `android.os.Parcel.mNativePtr` (the parcel-table handle) from a Java
/// `Parcel` object.
fn parcel_native_ptr(vm: &mut Vm, obj: Handle) -> Option<u64> {
    if obj == NULL {
        return None;
    }
    match vm.get_field(obj, "mNativePtr") {
        Ok(JavaValue::Long(v)) => Some(v as u64),
        Ok(JavaValue::Int(v)) => Some(v as u32 as u64),
        _ => None,
    }
}

fn transact_native(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let binder = match vm.get_field(this, "mNativeData") {
        Ok(JavaValue::Long(v)) => BinderId(v as u64),
        Ok(JavaValue::Int(v)) => BinderId(v as u32 as u64),
        _ => {
            return Err(vm.throw_new(
                "Ljava/lang/IllegalStateException;",
                Some("transactNative on a BinderProxy without mNativeData"),
            ))
        }
    };
    let code = vm.arg_i32(ctx, 0) as u32;
    // `transactNative` receives the Java `Parcel` objects; their `mNativePtr`
    // long is the handle into this thread's parcel table.
    let data_handle = parcel_native_ptr(vm, vm.arg_ref(ctx, 1)).unwrap_or(0);
    let reply_handle = parcel_native_ptr(vm, vm.arg_ref(ctx, 2)).unwrap_or(0);
    let flags = vm.arg_i32(ctx, 3) as u32;
    let oneway = flags & 1 != 0;

    let mut data = match super::parcel::with_store(|s| {
        s.table.with_parcel_shared(data_handle, Clone::clone)
    }) {
        Ok(p) => p,
        Err(e) => return Err(parcel_err_at("transactNative.data", e)),
    };
    // The Java writer leaves the position at the end; the dispatcher reads from
    // the start, exactly like libbinder (`transact` rewinds before the call).
    data.set_data_position(0);
    let mut reply = Parcel::new();
    let outcome = with_transport(|t| t.transact(vm, binder, code, &mut data, &mut reply, flags));
    let Some(outcome) = outcome else {
        // No transport: behave like a dead remote. A oneway call still reports
        // success (Binder drops the error), a synchronous call returns false so
        // the generated proxy raises a RemoteException.
        vm.ret_bool(ctx, oneway);
        return Ok(());
    };
    // The dispatcher writes an exception header into `reply` on failure; the
    // Java side's `readException()` is what raises it. So the parcel is written
    // back in both the success and the error case, and a failed synchronous
    // dispatch still returns `true` (the exception rides in the reply).
    let deliveries = outcome.unwrap_or_default();
    if !oneway {
        // The Java reader starts at position 0, exactly like libbinder's
        // `transact` (the reply is rewound after the handler writes it).
        reply.set_data_position(0);
        super::parcel::with_store(|s| {
            let _ = s.table.with_parcel(reply_handle, |p| *p = reply);
        });
    }
    for d in deliveries {
        deliver(vm, d)?;
    }
    vm.ret_bool(ctx, true);
    Ok(())
}

/// Run a `system -> app` delivery through the app's own `Binder.execTransact`.
///
/// The ACOS VM is cooperative green threads on one host thread; the current
/// green thread is the looper thread the outgoing call came from, so the
/// re-entrant `onTransact` runs on that looper. (A separate host-thread
/// delivery would need the eventfd wake path; that is not on the V1 path.)
///
/// `Binder.execTransact` is `private`; it is invoked directly, exactly as the
/// real JNI entry point does.
fn deliver(vm: &mut Vm, mut d: BinderDelivery) -> VmResult<()> {
    let Some(target) = binder_object(vm, BinderId(d.target)) else {
        return Ok(());
    };
    // The receiver reads from the start; the sender left the writer at the end.
    d.data.set_data_position(0);
    let data_handle = super::parcel::with_store(|s| s.table.create_with(d.data));
    let reply_handle = super::parcel::with_store(|s| s.table.create());
    let result = (|| -> VmResult<()> {
        let cid = vm
            .class_of_object(target)
            .ok_or_else(|| VmError::Internal("Binder.execTransact on a dead target".into()))?;
        let mid = vm.find_method(cid, "execTransact", "(IJJI)Z", crate::vm::InvokeKind::Direct)?;
        let words = [
            d.code as u64,
            data_handle,
            reply_handle,
            d.flags as u64,
        ];
        crate::interp::call_method(vm, mid, target, &words)?;
        Ok(())
    })();
    super::parcel::with_store(|s| {
        let _ = s.table.free(data_handle);
        let _ = s.table.free(reply_handle);
    });
    match result {
        Ok(()) => Ok(()),
        // Binder logs and drops exceptions thrown by a stub implementation.
        Err(VmError::JavaException(_)) => Ok(()),
        Err(e) => Err(e),
    }
}

// ---------------------------------------------------------------------------
// `Binder` natives
// ---------------------------------------------------------------------------

fn get_native_bbinder_holder(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let id = with_transport(|t| t.new_local_binder(vm)).unwrap_or_else(next_fallback_id);
    vm.ret_i64(ctx, id as i64);
    Ok(())
}

fn get_native_finalizer(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    // ACOS has no native finalizer; a zero handle is exactly what a no-op
    // NativeAllocationRegistry expects.
    vm.ret_i64(ctx, 0);
    Ok(())
}

fn get_calling_uid(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    vm.ret_i32(ctx, calling_uid() as i32);
    Ok(())
}

fn get_calling_pid(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    vm.ret_i32(ctx, calling_pid());
    Ok(())
}

fn get_calling_work_source_uid(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    vm.ret_i32(ctx, thread_work_source_uid());
    Ok(())
}

fn set_calling_work_source_uid(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let uid = vm.arg_i32(ctx, 0);
    let prev = thread_work_source_uid();
    set_thread_work_source_uid(uid);
    vm.ret_i64(ctx, prev as i64);
    Ok(())
}

fn clear_calling_work_source(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let prev = thread_work_source_uid();
    set_thread_work_source_uid(-1);
    vm.ret_i64(ctx, prev as i64);
    Ok(())
}

fn restore_calling_work_source(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let uid = vm.arg_i64(ctx, 0) as i32;
    set_thread_work_source_uid(uid);
    Ok(())
}

fn clear_calling_identity_native(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let token = clear_calling_identity();
    IDENTITY.with(|s| s.borrow_mut().push(token));
    // Android's return value is opaque; callers only hand it back to
    // restoreCallingIdentity.
    vm.ret_i64(ctx, 0);
    Ok(())
}

fn restore_calling_identity_native(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let _ = vm.arg_i64(ctx, 0);
    let token = IDENTITY.with(|s| s.borrow_mut().pop());
    if let Some(token) = token {
        let _ = restore_calling_identity(token);
    }
    Ok(())
}

fn get_thread_strict_mode_policy(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    vm.ret_i32(ctx, thread_strict_mode_policy());
    Ok(())
}

fn set_thread_strict_mode_policy_native(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    set_thread_strict_mode_policy(vm.arg_i32(ctx, 0));
    Ok(())
}

fn ret_false(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    vm.ret_bool(ctx, false);
    Ok(())
}

fn ret_null(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    vm.ret_ref(ctx, NULL);
    Ok(())
}

fn noop(_vm: &mut Vm, _ctx: NativeCtx) -> VmResult<()> {
    Ok(())
}

// ---------------------------------------------------------------------------
// `BinderProxy` natives
// ---------------------------------------------------------------------------

fn proxy_binder(vm: &mut Vm, ctx: NativeCtx) -> Option<BinderId> {
    let this = vm.ctx_this(ctx)?;
    match vm.get_field(this, "mNativeData") {
        Ok(JavaValue::Long(v)) => Some(BinderId(v as u64)),
        Ok(JavaValue::Int(v)) => Some(BinderId(v as u32 as u64)),
        _ => None,
    }
}

fn proxy_get_interface_descriptor(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let desc = proxy_binder(vm, ctx).and_then(|b| with_transport(|t| t.descriptor(b)).flatten());
    match desc {
        Some(d) => {
            let h = vm.new_string(&d)?;
            vm.ret_ref(ctx, h);
        }
        None => vm.ret_ref(ctx, NULL),
    }
    Ok(())
}

fn proxy_is_binder_alive(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let alive = proxy_binder(vm, ctx)
        .map(|b| with_transport(|t| t.is_alive(b)).unwrap_or(false))
        .unwrap_or(false);
    vm.ret_bool(ctx, alive);
    Ok(())
}

fn proxy_ping_binder(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    proxy_is_binder_alive(vm, ctx)
}

fn proxy_link_to_death(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let recipient = vm.arg_ref(ctx, 0);
    if let Some(b) = proxy_binder(vm, ctx) {
        let _ = with_transport(|t| t.link_to_death(b, recipient));
    }
    Ok(())
}

fn proxy_unlink_to_death(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let recipient = vm.arg_ref(ctx, 0);
    let ok = proxy_binder(vm, ctx)
        .map(|b| with_transport(|t| t.unlink_to_death(b, recipient)).unwrap_or(false))
        .unwrap_or(false);
    vm.ret_bool(ctx, ok);
    Ok(())
}

// ---------------------------------------------------------------------------
// `BinderInternal` natives
// ---------------------------------------------------------------------------

fn binder_internal_get_context_object(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let id = with_transport(|t| t.context_object(vm))
        .filter(|&v| v != 0)
        .map(BinderId);
    let obj = id.and_then(|id| binder_object(vm, id)).unwrap_or(NULL);
    vm.ret_ref(ctx, obj);
    Ok(())
}

fn binder_proxy_counts(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    vm.ret_i32(ctx, 0);
    Ok(())
}

// ---------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------

/// Register the Binder / BinderProxy / BinderInternal natives.
///
/// Must be called after `parcel::install` so the strong-binder overrides win.
pub(crate) fn install(vm: &mut Vm) -> VmResult<()> {
    // The strong-binder `Parcel` methods are re-registered here: the transport
    // must own the handle <-> BinderId mapping, and the read side must be able
    // to materialise a Java `BinderProxy`. `register_native` is a map insert, so
    // these replace the default implementations in `parcel.rs`.
    let parcel = "Landroid/os/Parcel;";
    vm.register_native(
        parcel,
        "nativeWriteStrongBinder",
        "(JLandroid/os/IBinder;)V",
        write_strong_binder,
    );
    vm.register_native(
        parcel,
        "nativeReadStrongBinder",
        "(J)Landroid/os/IBinder;",
        read_strong_binder,
    );
    vm.register_native(
        parcel,
        "nativeMarkForBinder",
        "(JLandroid/os/IBinder;)V",
        mark_for_binder,
    );

    let binder = "Landroid/os/Binder;";
    vm.register_native(binder, "getNativeBBinderHolder", "()J", get_native_bbinder_holder);
    vm.register_native(binder, "getNativeFinalizer", "()J", get_native_finalizer);
    vm.register_native(binder, "getCallingUid", "()I", get_calling_uid);
    vm.register_native(binder, "getCallingPid", "()I", get_calling_pid);
    vm.register_native(
        binder,
        "getCallingWorkSourceUid",
        "()I",
        get_calling_work_source_uid,
    );
    vm.register_native(
        binder,
        "setCallingWorkSourceUid",
        "(I)J",
        set_calling_work_source_uid,
    );
    vm.register_native(binder, "clearCallingWorkSource", "()J", clear_calling_work_source);
    vm.register_native(
        binder,
        "restoreCallingWorkSource",
        "(J)V",
        restore_calling_work_source,
    );
    vm.register_native(
        binder,
        "clearCallingIdentity",
        "()J",
        clear_calling_identity_native,
    );
    vm.register_native(
        binder,
        "restoreCallingIdentity",
        "(J)V",
        restore_calling_identity_native,
    );
    vm.register_native(
        binder,
        "getThreadStrictModePolicy",
        "()I",
        get_thread_strict_mode_policy,
    );
    vm.register_native(
        binder,
        "setThreadStrictModePolicy",
        "(I)V",
        set_thread_strict_mode_policy_native,
    );
    vm.register_native(binder, "flushPendingCommands", "()V", noop);
    vm.register_native(binder, "hasExplicitIdentity", "()Z", ret_false);
    vm.register_native(binder, "isDirectlyHandlingTransaction", "()Z", ret_false);
    vm.register_native(binder, "blockUntilThreadAvailable", "()V", noop);
    vm.register_native(binder, "forceDowngradeToSystemStability", "()V", noop);
    vm.register_native(binder, "markVintfStability", "()V", noop);
    vm.register_native(
        binder,
        "getExtension",
        "()Landroid/os/IBinder;",
        ret_null,
    );
    vm.register_native(
        binder,
        "setExtension",
        "(Landroid/os/IBinder;)V",
        noop,
    );

    let proxy = "Landroid/os/BinderProxy;";
    vm.register_native(
        proxy,
        "transactNative",
        "(ILandroid/os/Parcel;Landroid/os/Parcel;I)Z",
        transact_native,
    );
    vm.register_native(proxy, "getNativeFinalizer", "()J", get_native_finalizer);
    vm.register_native(
        proxy,
        "getInterfaceDescriptor",
        "()Ljava/lang/String;",
        proxy_get_interface_descriptor,
    );
    vm.register_native(proxy, "isBinderAlive", "()Z", proxy_is_binder_alive);
    vm.register_native(proxy, "pingBinder", "()Z", proxy_ping_binder);
    vm.register_native(
        proxy,
        "linkToDeath",
        "(Landroid/os/IBinder$DeathRecipient;I)V",
        proxy_link_to_death,
    );
    vm.register_native(
        proxy,
        "unlinkToDeath",
        "(Landroid/os/IBinder$DeathRecipient;I)Z",
        proxy_unlink_to_death,
    );
    vm.register_native(
        proxy,
        "getExtension",
        "()Landroid/os/IBinder;",
        ret_null,
    );

    let internal = "Lcom/android/internal/os/BinderInternal;";
    vm.register_native(
        internal,
        "getContextObject",
        "()Landroid/os/IBinder;",
        binder_internal_get_context_object,
    );
    vm.register_native(internal, "disableBackgroundScheduling", "(Z)V", noop);
    vm.register_native(internal, "handleGc", "()V", noop);
    vm.register_native(internal, "joinThreadPool", "()V", noop);
    vm.register_native(internal, "setMaxThreads", "(I)V", noop);
    vm.register_native(internal, "nGetBinderProxyCount", "(I)I", binder_proxy_counts);
    vm.register_native(
        internal,
        "nGetBinderProxyPerUidCounts",
        "()Landroid/util/SparseIntArray;",
        ret_null,
    );
    vm.register_native(internal, "nSetBinderProxyCountEnabled", "(Z)V", noop);
    vm.register_native(internal, "nSetBinderProxyCountWatermarks", "(II)V", noop);

    Ok(())
}

// ---------------------------------------------------------------------------
// tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fallback_sink_round_trips() {
        let mut sink = FallbackSink::default();
        let a = sink.binder_id(7).unwrap();
        let b = sink.binder_id(9).unwrap();
        assert_ne!(a, b);
        assert_eq!(sink.binder_id(7), Some(a));
        assert_eq!(sink.binder_object(a), Some(7));
        assert_eq!(sink.binder_id(NULL), None);
    }
}
