//! VM-internal object bridge.
//!
//! The real Android 14 core library treats a handful of fundamental classes
//! specially: `java.lang.String`'s characters and `java.lang.Class`'s metadata
//! live in runtime-internal storage, not in DEX-visible fields. ACOS keeps its
//! own representation for those (`Object::String`, `Object::Class`), so the
//! interpreter asks this module to synthesise the DEX-visible field values and
//! to finish green threads.
//!
//! Keeping the logic here (rather than in `interp.rs`) keeps the interpreter
//! hooks to one line per site and makes the VM-owned representation explicit.

use crate::class::{ClassId, MethodId};
use crate::heap::{ArrayData, ArrayObj, Handle, Object, NULL};
use crate::vm::{ThreadStatus, Vm};

/// Read a field of a fundamental object, synthesising VM-internal state.
///
/// Returns `None` when the field is not VM-internal (the interpreter then reads
/// the normal instance slots).
pub fn internal_field_get(vm: &mut Vm, obj: Handle, dex: u32, field_idx: u32) -> Option<u32> {
    let fr = vm.dexs[dex as usize].file.field_ref(field_idx).ok()?;
    let class = fr.class.to_string();
    let name = fr.name.to_string();
    if class.ends_with("java/lang/Object;") {
        if name == "shadow$_klass_" {
            let cid = vm.class_of_object(obj)?;
            return vm.class_object(cid).ok();
        }
        if name == "shadow$_monitor_" {
            return Some(0);
        }
        return None;
    }
    if class.ends_with("java/lang/Class;") {
        let id = vm.class_id_of_class_object(obj)?;
        return class_field(vm, id, &name);
    }
    if !class.ends_with("java/lang/String;") {
        return None;
    }
    let units = match vm.heap.get(obj) {
        Some(Object::String(u)) => u,
        _ => return None,
    };
    match name.as_str() {
        "count" => Some(((units.len() as u32) << 1) | coder_of(units)),
        "hash" => Some(vm.string_hashes.get(&obj).copied().unwrap_or(0) as u32),
        "coder" => Some(coder_of(units)),
        "value" => {
            // Materialise the hidden backing array the `StringLatin1` /
            // `StringUTF16` helpers operate on.
            let bytes: Vec<i8> = if coder_of(units) == 0 {
                units.iter().map(|&c| c as u8 as i8).collect()
            } else {
                let mut v = Vec::with_capacity(units.len() * 2);
                for &c in units.iter() {
                    v.push(c as u8 as i8);
                    v.push((c >> 8) as i8);
                }
                v
            };
            vm.alloc(Object::Array(ArrayObj {
                elem_desc: "B".to_string(),
                data: ArrayData::Byte(bytes),
            }))
            .ok()
        }
        _ => None,
    }
}

fn coder_of(units: &[u16]) -> u32 {
    if units.iter().all(|&c| c <= 0xFF) {
        0
    } else {
        1
    }
}

fn class_field(vm: &mut Vm, id: ClassId, name: &str) -> Option<u32> {
    match name {
        "accessFlags" => Some(vm.classes[id as usize].access),
        "primitiveType" => Some(if vm.classes[id as usize].is_primitive {
            1
        } else {
            0
        }),
        "classFlags" => Some(0),
        "classLoader" => Some(0),
        "componentType" => {
            let desc = vm.class_desc(id).to_string();
            if let Some(inner) = desc.strip_prefix('[') {
                let cid = vm.load_class(inner).ok()?;
                vm.class_object(cid).ok()
            } else {
                Some(0)
            }
        }
        "superClass" => match vm.classes[id as usize].super_class {
            Some(s) => vm.class_object(s).ok(),
            None => Some(0),
        },
        _ => None,
    }
}

/// Handle a write to a VM-internal field. Returns `true` when the interpreter
/// should skip the normal instance-slot write.
pub fn internal_field_put(vm: &mut Vm, obj: Handle, dex: u32, field_idx: u32) -> bool {
    if !matches!(vm.heap.get(obj), Some(Object::String(_))) {
        return false;
    }
    let (class, name) = match vm.dexs[dex as usize].file.field_ref(field_idx) {
        Ok(fr) => (fr.class.to_string(), fr.name.to_string()),
        Err(_) => return false,
    };
    if !class.ends_with("java/lang/String;") {
        return false;
    }
    // `String.hash` is a cache; recomputing it is always correct.
    name == "hash"
}

/// Mark a green thread finished, clear its Java liveness fields and wake
/// anything waiting on it (`Thread.join`).
pub fn finish_thread(vm: &mut Vm, t: usize) {
    if vm.threads[t].status == ThreadStatus::Finished {
        return;
    }
    vm.threads[t].status = ThreadStatus::Finished;
    let object = vm.threads[t].object;
    if let Some(Object::Builtin {
        data: crate::heap::BuiltinData::Thread(td),
        ..
    }) = vm.heap.get_mut(object)
    {
        td.alive = false;
    }
    // A real `java.lang.Thread`: clear `nativePeer` so `isAlive()` is false.
    vm.set_instance_field_by_name(object, "nativePeer", 0);
    vm.set_instance_field_by_name(object, "tid", 0);
    // Joiners registered by the builtin path.
    let joiners = vm.threads[t].joiners.clone();
    for j in joiners {
        if vm.threads.get(j).is_some() && vm.threads[j].status == ThreadStatus::Waiting {
            // A `Thread.join()` waiter released the thread monitor in `wait`;
            // mark it monitor-blocked so the monitor is re-acquired before it
            // resumes, then the scheduler runs it.
            vm.threads[j].status = match vm.threads[j].wait_monitor {
                Some(h) => ThreadStatus::BlockedMonitor(h),
                None => ThreadStatus::Runnable,
            };
            vm.threads[j].joiners.clear();
        }
    }
    // Real `Thread.join` waits on the target thread's `lock` object, so wake
    // waiters on both the thread object and its lock.
    let lock = vm
        .get_instance_field_by_name(object, "lock")
        .filter(|h| *h != NULL)
        .unwrap_or(NULL);
    for j in 0..vm.threads.len() {
        let wm = vm.threads[j].wait_monitor;
        if j != t
            && vm.threads[j].status == ThreadStatus::Waiting
            && (wm == Some(object) || (lock != NULL && wm == Some(lock)))
        {
            vm.threads[j].status = match wm {
                Some(h) => ThreadStatus::BlockedMonitor(h),
                None => ThreadStatus::Runnable,
            };
        }
    }
}

/// Record a native invocation for `--trace-natives`.
pub fn record_native(vm: &mut Vm, mid: MethodId) {
    if vm.native_trace.is_none() {
        return;
    }
    let m = &vm.methods[mid as usize];
    let desc = vm.class_desc(m.class).to_string();
    let name = m.name.clone();
    let sig = m.sig.clone();
    if let Some(tr) = vm.native_trace.as_mut() {
        tr.record(&desc, &name, &sig);
    }
}

/// Structured error for a `native` DEX method with no Rust implementation.
pub fn unbound_native_error(vm: &Vm, mid: MethodId, at: String) -> crate::error::VmError {
    let m = &vm.methods[mid as usize];
    let desc = vm.class_desc(m.class).to_string();
    crate::error::VmError::unsupported_api(format!("{desc}->{}{}", m.name, m.sig), at)
}

// Keep `NULL` referenced for future object materialisation helpers.
const _: Handle = NULL;
