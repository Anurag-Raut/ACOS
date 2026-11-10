//! Java → native dispatch: build the native C ABI call from a DEX frame.
//!
//! The guest function is an arbitrary C function pointer, so the argument
//! registers are filled by a small x86_64 System V trampoline (`asm!`). The
//! trampoline is the only place in this crate that executes guest code; every
//! pointer is validated on the Rust side before it gets here.

use acos_vm::heap::NULL;
use acos_vm::natives::jni_glue::{CallFailure, JniReturn, JniValue, NativeCall};
use acos_vm::{Handle, Vm};

use crate::error::JniError;
use crate::load::{cache_resolved, resolve_native};
use crate::refs;
use crate::state::{current_vm, runtime_state, set_vm};

fn structured(e: JniError) -> CallFailure {
    CallFailure::Structured {
        code: e.code().to_string(),
        message: e.to_string(),
    }
}

fn throw(vm: &mut Vm, desc: &str, message: &str) -> CallFailure {
    match vm.new_throwable(desc, Some(message)) {
        Ok(h) => CallFailure::JavaException(h),
        Err(e) => structured(JniError::NativeFault {
            message: format!("cannot construct {desc}: {e}"),
        }),
    }
}

/// Host callback: invoke a guest native for one DEX frame.
pub(crate) fn host_call_native(
    vm_ptr: *mut Vm,
    call: NativeCall,
) -> Result<JniReturn, CallFailure> {
    if vm_ptr.is_null() {
        return Err(structured(JniError::NoVm {
            function: "nativeCall".into(),
        }));
    }
    // SAFETY: the interpreter passes the live VM for this native call.
    let vm = unsafe { &mut *vm_ptr };
    // SAFETY: same pointer, scoped to this call.
    unsafe { set_vm(vm_ptr) };

    let rt = runtime_state().ok_or_else(|| {
        structured(JniError::NoVm {
            function: "nativeCall".into(),
        })
    })?;
    let t = vm.current_thread;

    // Release the previous call's return roots; by now their values live in a
    // Java frame (or were dropped by the caller).
    {
        let previous = {
            let mut st = rt.borrow_mut();
            std::mem::take(&mut st.threads.entry(t).or_default().return_roots)
        };
        for h in previous {
            let _ = vm.release_bridge_root(h);
        }
    }
    refs::push_frame(&rt, t, 0);

    let result = invoke_guest(vm, &rt, t, &call);

    let error = refs::take_error(&rt, t);
    let pending = refs::take_pending(&mut *vm, &rt, t);
    let _ = refs::pop_frame(vm, &rt, t, NULL);

    if let Some(e) = error {
        return Err(structured(e));
    }
    if let Some(h) = pending {
        return Err(CallFailure::JavaException(h));
    }
    result
}

fn invoke_guest(
    vm: &mut Vm,
    rt: &std::rc::Rc<std::cell::RefCell<crate::state::State>>,
    t: usize,
    call: &NativeCall,
) -> Result<JniReturn, CallFailure> {
    // Resolve the guest function.
    let f = {
        let st = rt.borrow();
        resolve_native(&st, &call.class, &call.name, &call.sig)
    };
    let f = match f {
        Some(f) => f,
        None => {
            let symbol = crate::load::symbol_names(&call.class, &call.name, &call.sig)
                .into_iter()
                .next()
                .unwrap_or_default();
            return Err(throw(
                vm,
                "Ljava/lang/UnsatisfiedLinkError;",
                &format!(
                    "No implementation found for {}->{}{} (tried {symbol})",
                    call.class, call.name, call.sig
                ),
            ));
        }
    };
    {
        let mut st = rt.borrow_mut();
        cache_resolved(&mut st, &call.class, &call.name, &call.sig, f);
    }

    // `jclass` for static natives; `jobject` for instance natives.
    let receiver = if call.this != NULL {
        if !vm.heap.is_live(call.this) {
            return Err(structured(JniError::BadReference {
                function: "nativeCall".into(),
                what: "this".into(),
                handle: call.this,
            }));
        }
        call.this
    } else {
        let cid = vm.load_class(&call.class).map_err(|e| {
            structured(JniError::NativeFault {
                message: format!("static native receiver class {}: {e}", call.class),
            })
        })?;
        vm.class_object(cid).map_err(|e| {
            structured(JniError::NativeFault {
                message: format!("class object {}: {e}", call.class),
            })
        })?
    };

    let env = rt.borrow().env_slot as u64;
    let mut ints: Vec<u64> = Vec::with_capacity(call.args.len() + 2);
    let mut floats: Vec<f64> = Vec::new();
    ints.push(env);
    ints.push(receiver as u64);
    for v in &call.args {
        match v {
            JniValue::Int(i) => ints.push(*i as u32 as u64),
            JniValue::Long(i) => ints.push(*i as u64),
            JniValue::Ref(h) => ints.push(*h as u64),
            JniValue::Float(f) => floats.push(*f as f64),
            JniValue::Double(d) => floats.push(*d),
        }
    }

    let is_float_ret = matches!(
        call.ret.as_bytes().first().copied(),
        Some(b'F') | Some(b'D')
    );
    // SAFETY: `f` was resolved from a loaded ELF export and validated above.
    let bits = unsafe {
        if is_float_ret {
            call_guest_float(f, &ints, &floats)?
        } else {
            call_guest_int(f, &ints, &floats)?
        }
    };

    let _ = t;
    decode_return(vm, &call.ret, bits)
}

fn decode_return(vm: &mut Vm, ret: &str, bits: u64) -> Result<JniReturn, CallFailure> {
    Ok(match ret.as_bytes().first().copied() {
        Some(b'V') => JniReturn::Void,
        Some(b'Z') | Some(b'B') | Some(b'C') | Some(b'S') | Some(b'I') => {
            JniReturn::Int(bits as u32 as i32)
        }
        Some(b'J') => JniReturn::Long(bits as i64),
        Some(b'F') => JniReturn::Float(f32::from_bits(bits as u32)),
        Some(b'D') => JniReturn::Double(f64::from_bits(bits)),
        Some(b'L') | Some(b'[') => {
            let h = bits as u32;
            if h != NULL && !vm.heap.is_live(h) {
                return Err(structured(JniError::BadReference {
                    function: "nativeReturn".into(),
                    what: "object".into(),
                    handle: h,
                }));
            }
            if h != NULL {
                // Keep the value alive until the Java frame owns it.
                vm.native_roots.push(h);
                // The caller's thread state is not passed here; record it in
                // the runtime's thread entry via the current thread id.
                if let Some(rt) = runtime_state() {
                    let t = vm.current_thread;
                    rt.borrow_mut()
                        .threads
                        .entry(t)
                        .or_default()
                        .return_roots
                        .push(h);
                }
            }
            JniReturn::Ref(h)
        }
        _ => {
            return Err(structured(JniError::NativeFault {
                message: format!("invalid return descriptor {ret:?}"),
            }))
        }
    })
}

/// Whether the guest ABI trampoline is available on this host.
pub fn host_abi_supported() -> bool {
    cfg!(target_arch = "x86_64")
}

fn abi_error() -> CallFailure {
    structured(JniError::Unsupported {
        slot: "native ABI trampoline (only x86_64 System V is implemented)".into(),
    })
}

#[cfg(target_arch = "x86_64")]
unsafe fn call_guest_int(f: usize, ints: &[u64], floats: &[f64]) -> Result<u64, CallFailure> {
    use std::arch::asm;
    if floats.len() > 8 || ints.len() > 10 {
        return Err(abi_error());
    }
    let mut a = [0u64; 10];
    a[..ints.len()].copy_from_slice(ints);
    let mut x = [0f64; 8];
    x[..floats.len()].copy_from_slice(floats);
    let mut ret: u64;
    if ints.len() <= 6 {
        // SAFETY: the function pointer came from a validated ELF export; the
        // System V register assignment matches the JNI C ABI and all extra
        // registers are marked clobbered.
        unsafe {
            asm!(
                "call {f}",
                f = in(reg) f,
                inout("rdi") a[0] => _,
                inout("rsi") a[1] => _,
                inout("rdx") a[2] => _,
                inout("rcx") a[3] => _,
                inout("r8") a[4] => _,
                inout("r9") a[5] => _,
                inout("xmm0") x[0] => _,
                inout("xmm1") x[1] => _,
                inout("xmm2") x[2] => _,
                inout("xmm3") x[3] => _,
                inout("xmm4") x[4] => _,
                inout("xmm5") x[5] => _,
                inout("xmm6") x[6] => _,
                inout("xmm7") x[7] => _,
                out("r10") _, out("r11") _,
                out("xmm8") _, out("xmm9") _, out("xmm10") _, out("xmm11") _,
                out("xmm12") _, out("xmm13") _, out("xmm14") _, out("xmm15") _,
                out("rax") ret,
            );
        }
    } else {
        // Stack arguments. `sub rsp, 32` keeps the mandated 16-byte call
        // alignment; unused slots are ignored by the callee.
        let e0 = a[6];
        let e1 = a[7];
        let e2 = a[8];
        let e3 = a[9];
        // SAFETY: as above, plus the four extra words are written to the
        // outgoing argument area before the call.
        unsafe {
            asm!(
                "sub rsp, 32",
                "mov [rsp + 0], {e0}",
                "mov [rsp + 8], {e1}",
                "mov [rsp + 16], {e2}",
                "mov [rsp + 24], {e3}",
                "call {f}",
                "add rsp, 32",
                f = in(reg) f,
                e0 = in(reg) e0,
                e1 = in(reg) e1,
                e2 = in(reg) e2,
                e3 = in(reg) e3,
                inout("rdi") a[0] => _,
                inout("rsi") a[1] => _,
                inout("rdx") a[2] => _,
                inout("rcx") a[3] => _,
                inout("r8") a[4] => _,
                inout("r9") a[5] => _,
                inout("xmm0") x[0] => _,
                inout("xmm1") x[1] => _,
                inout("xmm2") x[2] => _,
                inout("xmm3") x[3] => _,
                inout("xmm4") x[4] => _,
                inout("xmm5") x[5] => _,
                inout("xmm6") x[6] => _,
                inout("xmm7") x[7] => _,
                out("r10") _, out("r11") _,
                out("xmm8") _, out("xmm9") _, out("xmm10") _, out("xmm11") _,
                out("xmm12") _, out("xmm13") _, out("xmm14") _, out("xmm15") _,
                out("rax") ret,
            );
        }
    }
    Ok(ret)
}

#[cfg(target_arch = "x86_64")]
unsafe fn call_guest_float(f: usize, ints: &[u64], floats: &[f64]) -> Result<u64, CallFailure> {
    use std::arch::asm;
    if floats.len() > 8 || ints.len() > 10 {
        return Err(abi_error());
    }
    let mut a = [0u64; 10];
    a[..ints.len()].copy_from_slice(ints);
    let mut x = [0f64; 8];
    x[..floats.len()].copy_from_slice(floats);
    let ret: f64;
    if ints.len() <= 6 {
        // SAFETY: see `call_guest_int`; the float result arrives in xmm0.
        unsafe {
            asm!(
                "call {f}",
                f = in(reg) f,
                inout("rdi") a[0] => _,
                inout("rsi") a[1] => _,
                inout("rdx") a[2] => _,
                inout("rcx") a[3] => _,
                inout("r8") a[4] => _,
                inout("r9") a[5] => _,
                inout("xmm0") x[0] => ret,
                inout("xmm1") x[1] => _,
                inout("xmm2") x[2] => _,
                inout("xmm3") x[3] => _,
                inout("xmm4") x[4] => _,
                inout("xmm5") x[5] => _,
                inout("xmm6") x[6] => _,
                inout("xmm7") x[7] => _,
                out("r10") _, out("r11") _,
                out("xmm8") _, out("xmm9") _, out("xmm10") _, out("xmm11") _,
                out("xmm12") _, out("xmm13") _, out("xmm14") _, out("xmm15") _,
                out("rax") _,
            );
        }
    } else {
        let e0 = a[6];
        let e1 = a[7];
        let e2 = a[8];
        let e3 = a[9];
        // SAFETY: see `call_guest_int`.
        unsafe {
            asm!(
                "sub rsp, 32",
                "mov [rsp + 0], {e0}",
                "mov [rsp + 8], {e1}",
                "mov [rsp + 16], {e2}",
                "mov [rsp + 24], {e3}",
                "call {f}",
                "add rsp, 32",
                f = in(reg) f,
                e0 = in(reg) e0,
                e1 = in(reg) e1,
                e2 = in(reg) e2,
                e3 = in(reg) e3,
                inout("rdi") a[0] => _,
                inout("rsi") a[1] => _,
                inout("rdx") a[2] => _,
                inout("rcx") a[3] => _,
                inout("r8") a[4] => _,
                inout("r9") a[5] => _,
                inout("xmm0") x[0] => ret,
                inout("xmm1") x[1] => _,
                inout("xmm2") x[2] => _,
                inout("xmm3") x[3] => _,
                inout("xmm4") x[4] => _,
                inout("xmm5") x[5] => _,
                inout("xmm6") x[6] => _,
                inout("xmm7") x[7] => _,
                out("r10") _, out("r11") _,
                out("xmm8") _, out("xmm9") _, out("xmm10") _, out("xmm11") _,
                out("xmm12") _, out("xmm13") _, out("xmm14") _, out("xmm15") _,
                out("rax") _,
            );
        }
    }
    Ok(ret.to_bits())
}

#[cfg(not(target_arch = "x86_64"))]
unsafe fn call_guest_int(_f: usize, _ints: &[u64], _floats: &[f64]) -> Result<u64, CallFailure> {
    Err(abi_error())
}

#[cfg(not(target_arch = "x86_64"))]
unsafe fn call_guest_float(_f: usize, _ints: &[u64], _floats: &[f64]) -> Result<u64, CallFailure> {
    Err(abi_error())
}

/// Current VM pointer for diagnostics (never dereferenced in tests unless set).
pub fn current_vm_ptr() -> usize {
    current_vm() as usize
}

/// A live handle wrapper used by the table implementation.
pub type JObject = Handle;
