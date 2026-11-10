//! The `acos-runtime` Binder transport: real framework `transact` ->
//! in-process [`ServiceHost`] dispatch, and the reverse `Binder.execTransact`
//! delivery.
//!
//! `android.os.BinderProxy.transactNative` calls
//! [`ServiceTransport::transact`] with the request `Parcel` bytes. This module
//! owns:
//!
//! * the `ServiceHost` (shared with the [`crate::Runtime`] so tests and the
//!   scenario runner can inspect/register services),
//! * the Java `IBinder` <-> `BinderId` mapping (a local `Binder` keeps its
//!   `mObject` id; a service id materialises as a real `android.os.BinderProxy`),
//! * the per-call caller identity pushed for `Binder.getCallingUid/Pid`,
//! * conversion of the host's [`PendingDelivery`]s into the
//!   [`BinderDelivery`]s the VM runs back through `Binder.execTransact`.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use acos_parcel::binder::BinderId;
use acos_parcel::registry::{pop_calling_identity, push_calling_identity};
use acos_parcel::Parcel;
use acos_services::ServiceHost;
use acos_vm::heap::{Handle, NULL};
use acos_vm::javabridge::JavaValue;
use acos_vm::natives::binder::{BinderDelivery, BinderTransport};
use acos_vm::Vm;

/// The ACOS Binder transport, backed by an in-process [`ServiceHost`].
pub struct ServiceTransport {
    host: Rc<RefCell<ServiceHost>>,
    /// The `IServiceManager` binder (`BinderInternal.getContextObject`).
    context: BinderId,
    /// Identity reported to `Binder.getCallingUid/Pid` inside a handler.
    uid: u32,
    pid: i32,
    /// App-side Java `Binder` handles, keyed by their `mObject` id.
    locals: HashMap<u64, Handle>,
    /// Materialised `BinderProxy` objects, so identity round-trips.
    proxies: HashMap<u64, Handle>,
}

impl core::fmt::Debug for ServiceTransport {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ServiceTransport")
            .field("context", &self.context)
            .field("uid", &self.uid)
            .field("pid", &self.pid)
            .field("locals", &self.locals.len())
            .field("proxies", &self.proxies.len())
            .finish()
    }
}

impl ServiceTransport {
    /// Build a transport around `host`; `context` is the `IServiceManager`
    /// binder id (`0` disables `getContextObject`).
    #[must_use]
    pub fn new(host: Rc<RefCell<ServiceHost>>, context: BinderId, uid: u32, pid: i32) -> Self {
        Self {
            host,
            context,
            uid,
            pid,
            locals: HashMap::new(),
            proxies: HashMap::new(),
        }
    }
}

impl BinderTransport for ServiceTransport {
    fn new_local_binder(&mut self, _vm: &mut Vm) -> u64 {
        // Bind it into the same registry the service binders use, so local and
        // service ids can never collide.
        self.host
            .borrow()
            .binder_registry()
            .register_local("android.os.Binder")
            .0
    }

    fn context_object(&mut self, _vm: &mut Vm) -> u64 {
        self.context.0
    }

    fn binder_id_of(&mut self, vm: &mut Vm, obj: Handle) -> Option<BinderId> {
        if obj == NULL {
            return None;
        }
        // A `BinderProxy` carries its handle in `mNativeData`.
        if let Ok(JavaValue::Long(v)) = vm.get_field(obj, "mNativeData") {
            return Some(BinderId(v as u64));
        }
        // A local `Binder` carries its `BBinder` id in `mObject`.
        if let Ok(JavaValue::Long(v)) = vm.get_field(obj, "mObject") {
            let id = BinderId(v as u64);
            self.locals.insert(id.0, obj);
            vm.root_handle(obj);
            return Some(id);
        }
        None
    }

    fn binder_object(&mut self, vm: &mut Vm, id: BinderId) -> Option<Handle> {
        if id.is_null() {
            return None;
        }
        if let Some(&h) = self.locals.get(&id.0) {
            return Some(h);
        }
        if let Some(&h) = self.proxies.get(&id.0) {
            return Some(h);
        }
        // A service binder becomes a real `android.os.BinderProxy` whose
        // `mNativeData` is the id, exactly like a proxy read from a real
        // binder driver. `updateTextServicesLocale` etc. then behave normally.
        let proxy = vm
            .new_instance_ctor(
                "android.os.BinderProxy",
                "(J)V",
                &[JavaValue::Long(id.0 as i64)],
            )
            .ok()?;
        self.proxies.insert(id.0, proxy);
        Some(proxy)
    }

    fn transact(
        &mut self,
        vm: &mut Vm,
        binder: BinderId,
        code: u32,
        data: &mut Parcel,
        reply: &mut Parcel,
        flags: u32,
    ) -> Result<Vec<BinderDelivery>, String> {
        // The framework asks `Binder.getCallingUid/Pid` from inside `onTransact`;
        // report the app as the caller for the duration of the dispatch.
        push_calling_identity(self.uid, self.pid);
        // The interface token is needed after the handler runs, to choose the
        // reply encoder; grab it before the dispatcher consumes it.
        let descriptor = data.peek_interface_token().ok().flatten();

        // WMS (T-29) is routed here: it owns the `acos-wm` policy state and the
        // DEX-derived parcelable codecs, which the generic `Value` codec cannot
        // express (`MergedConfiguration` has three inline `Configuration`s).
        let (result, deliveries): (Result<(), String>, Vec<BinderDelivery>) =
            if crate::wms_java::owns(descriptor.as_deref()) {
                let result = crate::wms_java::transact(binder, code, data, reply, flags);
                // T-37: the outgoing `IWindow` callbacks queued by this
                // transaction are the system -> app oneway deliveries.
                let deliveries = if result.is_ok() {
                    crate::wms_java::take_deliveries().unwrap_or_default()
                } else {
                    Vec::new()
                };
                (result, deliveries)
            } else if crate::ime_java::owns(descriptor.as_deref())
                && code_owns_method(descriptor.as_deref(), code, crate::ime_java::owns_method)
            {
                // `startInputOrWindowGainedFocus` returns a real
                // `InputBindResult` (T-38 follow-up): the request carries a real
                // `EditorInfo` and the reply is a framework parcelable the
                // generic codec cannot express.
                let result = crate::ime_java::transact(vm, binder, code, data, reply, flags)
                    .map(|_| ());
                (result, Vec::new())
            } else if crate::clipboard_java::owns(descriptor.as_deref())
                && code_owns_method(descriptor.as_deref(), code, crate::clipboard_java::owns_method)
            {
                // `setPrimaryClip`/`getPrimaryClip`/`getPrimaryClipDescription`
                // carry/return a real `ClipData`; `setPrimaryClip` also emits
                // the `IOnPrimaryClipChangedListener` oneway deliveries.
                match crate::clipboard_java::transact(vm, binder, code, data, reply, flags) {
                    Ok(d) => (Ok(()), d),
                    Err(e) => (Err(e), Vec::new()),
                }
            } else if crate::user_java::owns(descriptor.as_deref())
                && code_owns_method(descriptor.as_deref(), code, crate::user_java::owns_method)
            {
                // `getUserInfo`/`getUsers`/... return a real `UserInfo`.
                let result = crate::user_java::transact(vm, binder, code, data, reply, flags)
                    .map(|_| ());
                (result, Vec::new())
            } else {
                let mut host = self.host.borrow_mut();
                let result = host
                    .transact(binder, code, data, reply, flags)
                    .map_err(|e| e.to_string());
                let pending = host.take_deliveries();
                let deliveries = pending
                    .into_iter()
                    .map(|d| BinderDelivery {
                        target: d.target.0,
                        code: d.code,
                        flags: 0,
                        data: d.data,
                    })
                    .collect();
                (result, deliveries)
            };
        pop_calling_identity();

        // PMS replies are real framework Java objects: the framework marshals
        // its own `ApplicationInfo`/`PackageInfo`/`ActivityInfo` through the
        // Java bridge, overriding the generic prefix codec.
        if result.is_ok() && flags & 1 == 0 {
            if let Some(descriptor) = descriptor.as_deref() {
                if descriptor == "android.content.IContentProvider" {
                    // The framework instantiates the generation `MemoryIntArray`
                    // through its static `CREATOR`; make sure `<clinit>` ran.
                    crate::settings_java::prepare_settings_reply(vm)
                        .map_err(|e| format!("settings_java: {e}"))?;
                }
                if descriptor == "android.content.pm.IPackageManager" {
                    if let Some(method) = acos_aidl::lookup(descriptor, code) {
                        if crate::pms_java::owns_method(&method.name) {
                            crate::pms_java::rewrite_reply(vm, &method.name, reply)
                                .map_err(|e| format!("pms_java: {e}"))?;
                        }
                    }
                }
                // `IDisplayManager` replies are real framework parcelables too
                // (`DisplayInfo.writeToParcel` is 49 fields long); the Java
                // object marshals its own bytes so `DisplayInfo.CREATOR` reads
                // exactly what was written.
                if descriptor == "android.hardware.display.IDisplayManager" {
                    if let Some(method) = acos_aidl::lookup(descriptor, code) {
                        if crate::display_java::owns_method(&method.name) {
                            crate::display_java::rewrite_reply(vm, &method.name, reply)
                                .map_err(|e| format!("display_java: {e}"))?;
                        }
                    }
                }
            }
        }

        result.map(|()| deliveries)
    }

    fn descriptor(&mut self, id: BinderId) -> Option<String> {
        if let Some(d) = self.host.borrow().descriptor_of(id) {
            return Some(d.to_string());
        }
        if self.locals.contains_key(&id.0) {
            return Some("android.os.Binder".to_string());
        }
        None
    }

    fn is_alive(&mut self, id: BinderId) -> bool {
        id == self.context
            || self.locals.contains_key(&id.0)
            || self.host.borrow().descriptor_of(id).is_some()
    }

    fn link_to_death(&mut self, _id: BinderId, _recipient: Handle) -> bool {
        true
    }

    fn unlink_to_death(&mut self, _id: BinderId, _recipient: Handle) -> bool {
        true
    }
}

/// True when the decoded method for `(descriptor, code)` is owned by the
/// marshaller `owns_method` predicate.
fn code_owns_method(
    descriptor: Option<&str>,
    code: u32,
    owns_method: fn(&str) -> bool,
) -> bool {
    descriptor
        .and_then(|d| acos_aidl::lookup(d, code))
        .is_some_and(|m| owns_method(&m.name))
}
