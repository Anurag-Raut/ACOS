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
    // W128-A: SSE arguments are carried as their **raw bit patterns**, not as
    // `f64` values. A guest parameter declared `float` reads only the low 32
    // bits of its xmm register (`movss`), so widening an `f32` to `f64` and
    // loading it with `movsd` delivers the *upper* half of the double — for
    // `2.625f` that is exactly `0.0f`. `nativeSetViewportMetrics(long, float,
    // …)` therefore saw `devicePixelRatio = 0` and dropped every metrics
    // update. Keeping the raw pattern makes an `f32` argument arrive as its own
    // 4 bytes and an `f64` argument as its full 8.
    let mut floats: Vec<u64> = Vec::new();
    ints.push(env);
    ints.push(receiver as u64);
    for v in &call.args {
        match v {
            JniValue::Int(i) => ints.push(*i as u32 as u64),
            JniValue::Long(i) => ints.push(*i as u64),
            JniValue::Ref(h) => ints.push(*h as u64),
            JniValue::Float(f) => floats.push(u64::from(f.to_bits())),
            JniValue::Double(d) => floats.push(d.to_bits()),
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
        slot: "native ABI trampoline (x86_64 System V only; >8 float args unsupported)"
            .into(),
    })
}

#[cfg(target_arch = "x86_64")]
core::arch::global_asm!(
    // `extern "C" fn acos_jni_call_abi(f, ints, n_ints, floats, want_float) -> u64`
    //
    // Calls a guest native with the full System V x86_64 argument contract:
    // the first six integer/pointer arguments in rdi..r9, up to eight SSE
    // arguments in xmm0..xmm7, and **every remaining integer argument on the
    // stack**.
    //
    // Each SSE argument is moved as a raw 64-bit pattern (`movq`, not `movsd`),
    // so an `f32` argument keeps its own bits in the low half of the register
    // and an `f64` argument keeps all eight bytes. See the `floats` vector's
    // comment in `call_native_bridge`.  The previous implementation only spilled four words (enough for
    // 10 args); engine entry points such as `nativeSetViewportMetrics(long,
    // float, 15 ints, 3 arrays, 8 ints)` have 27 integer arguments, which hit
    // the "ABI trampoline" error before the frame could ever be scheduled.
    //
    // `ints` always has at least six elements (the Rust wrapper pads the short
    // case) and `floats` always has exactly eight.
    ".globl acos_jni_call_abi",
    ".hidden acos_jni_call_abi",
    ".type acos_jni_call_abi, @function",
    "acos_jni_call_abi:",
    "  push rbx",
    "  push r12",
    "  push r13",
    "  push r14",
    "  push r15",
    "  sub rsp, 16",
    "  mov r12, rdi",              // f
    "  mov r13, rsi",              // ints
    "  mov r14, rdx",              // n_ints
    "  mov r15, rcx",              // floats
    "  mov [rsp], r8",             // want_float
    // extra = max(0, n_ints - 6) * 8
    "  xor ebx, ebx",
    "  cmp r14, 6",
    "  jbe 1f",
    "  lea rbx, [r14 - 6]",
    "  shl rbx, 3",
    "1:",
    // Reserve the outgoing stack-argument area, 16-byte aligned.
    "  lea rdx, [rbx + 15]",
    "  and rdx, -16",
    "  sub rsp, rdx",
    // Copy the stack-passed integer arguments in order.
    "  xor rcx, rcx",
    "2:",
    "  cmp rcx, rbx",
    "  jae 3f",
    "  mov rdx, [r13 + rcx + 48]",
    "  mov [rsp + rcx], rdx",
    "  add rcx, 8",
    "  jmp 2b",
    "3:",
    // Register arguments.
    "  mov rdi, [r13 + 0]",
    "  mov rsi, [r13 + 8]",
    "  mov rdx, [r13 + 16]",
    "  mov rcx, [r13 + 24]",
    "  mov r8,  [r13 + 32]",
    "  mov r9,  [r13 + 40]",
    "  movq xmm0, qword ptr [r15 + 0]",
    "  movq xmm1, qword ptr [r15 + 8]",
    "  movq xmm2, qword ptr [r15 + 16]",
    "  movq xmm3, qword ptr [r15 + 24]",
    "  movq xmm4, qword ptr [r15 + 32]",
    "  movq xmm5, qword ptr [r15 + 40]",
    "  movq xmm6, qword ptr [r15 + 48]",
    "  movq xmm7, qword ptr [r15 + 56]",
    "  call r12",
    // Restore the argument area (never the callee-saved rbx).
    "  lea rdx, [rbx + 15]",
    "  and rdx, -16",
    "  add rsp, rdx",
    "  cmp qword ptr [rsp], 0",
    "  je 4f",
    "  movq rax, xmm0",
    "4:",
    "  add rsp, 16",
    "  pop r15",
    "  pop r14",
    "  pop r13",
    "  pop r12",
    "  pop rbx",
    "  ret",
    ".size acos_jni_call_abi, .-acos_jni_call_abi",
);

#[cfg(target_arch = "x86_64")]
extern "C" {
    fn acos_jni_call_abi(
        f: usize,
        ints: *const u64,
        n_ints: usize,
        floats: *const u64,
        want_float: usize,
    ) -> u64;
}

/// Marshal one guest native call through the System V ABI trampoline.
///
/// # Safety
/// `f` must be a valid function pointer with the JNI C signature described by
/// `ints`/`floats`; the caller owns the validity of every reference handle in
/// `ints` for the duration of the call.
#[cfg(target_arch = "x86_64")]
unsafe fn call_guest_abi(
    f: usize,
    ints: &[u64],
    floats: &[u64],
    want_float: bool,
) -> Result<u64, CallFailure> {
    if floats.len() > 8 {
        return Err(abi_error());
    }
    // The trampoline always reads six integer registers, so pad short lists.
    let mut head = [0u64; 6];
    let n = ints.len();
    if n >= 6 {
        head.copy_from_slice(&ints[..6]);
    } else {
        head[..n].copy_from_slice(ints);
    }
    let base = if n >= 6 { ints.as_ptr() } else { head.as_ptr() };
    let mut x = [0u64; 8];
    x[..floats.len()].copy_from_slice(floats);
    // SAFETY: `f` is the validated guest export; `base` has `n` readable words
    // (padded to six) and `x` is exactly eight words, which the trampoline
    // reads unconditionally.
    Ok(unsafe { acos_jni_call_abi(f, base, n, x.as_ptr(), want_float as usize) })
}

#[cfg(target_arch = "x86_64")]
unsafe fn call_guest_int(f: usize, ints: &[u64], floats: &[u64]) -> Result<u64, CallFailure> {
    // SAFETY: forwarded from the caller's contract.
    unsafe { call_guest_abi(f, ints, floats, false) }
}

#[cfg(target_arch = "x86_64")]
unsafe fn call_guest_float(f: usize, ints: &[u64], floats: &[u64]) -> Result<u64, CallFailure> {
    // SAFETY: forwarded from the caller's contract.
    unsafe { call_guest_abi(f, ints, floats, true) }
}

#[cfg(not(target_arch = "x86_64"))]
unsafe fn call_guest_int(_f: usize, _ints: &[u64], _floats: &[u64]) -> Result<u64, CallFailure> {
    Err(abi_error())
}

#[cfg(not(target_arch = "x86_64"))]
unsafe fn call_guest_float(_f: usize, _ints: &[u64], _floats: &[u64]) -> Result<u64, CallFailure> {
    Err(abi_error())
}

/// Current VM pointer for diagnostics (never dereferenced in tests unless set).
pub fn current_vm_ptr() -> usize {
    current_vm() as usize
}

/// A live handle wrapper used by the table implementation.
pub type JObject = Handle;

#[cfg(all(test, target_arch = "x86_64"))]
mod abi_tests {
    //! W128-A: the guest ABI trampoline must deliver `float` and `double`
    //! arguments with their own bit patterns. Widening an `f32` to `f64` made
    //! every `float` parameter read the upper half of the double — `2.625f`
    //! arrived as `0.0f`, which is why `nativeSetViewportMetrics` dropped every
    //! metrics update (`Shell::OnPlatformViewSetViewportMetrics` returns early
    //! when `device_pixel_ratio <= 0`).

    use super::*;

    extern "C" fn take_f32_and_int(f: f32, i: i32) -> i32 {
        ((f * 1000.0) as i32) << 8 | (i & 0xff)
    }

    extern "C" fn take_f64_and_f32(d: f64, f: f32) -> f64 {
        d + f64::from(f)
    }

    #[allow(clippy::too_many_arguments)]
    extern "C" fn take_many_ints(
        a: i64,
        b: i64,
        c: i64,
        d: i64,
        e: i64,
        f: i64,
        g: i64,
        h: i64,
        i: i64,
        j: i64,
    ) -> i64 {
        a + b * 2 + c * 3 + d * 4 + e * 5 + f * 6 + g * 7 + h * 8 + i * 9 + j * 10
    }

    fn call(f: usize, ints: &[u64], floats: &[u64], want_float: bool) -> u64 {
        // SAFETY: `f` is one of the `extern "C"` helpers above and `ints` /
        // `floats` hold exactly the arguments each declares.
        unsafe { call_guest_abi(f, ints, floats, want_float) }.expect("trampoline")
    }

    #[test]
    fn float_argument_keeps_its_own_bits() {
        let floats = [u64::from(2.625f32.to_bits())];
        let got = call(take_f32_and_int as usize, &[7], &floats, false) as u32 as i32;
        assert_eq!(got, (2625 << 8) | 7, "f32 argument was not delivered");
    }

    #[test]
    fn double_and_float_arguments_do_not_collide() {
        let floats = [1.5f64.to_bits(), u64::from(0.25f32.to_bits())];
        let bits = call(take_f64_and_f32 as usize, &[], &floats, true);
        assert_eq!(f64::from_bits(bits), 1.75);
    }

    #[test]
    fn integer_arguments_past_the_sixth_use_the_stack() {
        let ints: Vec<u64> = (1..=10).collect();
        let got = call(take_many_ints as usize, &ints, &[], false) as i64;
        assert_eq!(got, 1 + 2 * 2 + 3 * 3 + 4 * 4 + 5 * 5 + 6 * 6 + 7 * 7 + 8 * 8 + 9 * 9 + 10 * 10);
    }
}
