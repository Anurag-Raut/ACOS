//! True natives for `java.lang.*` and `dalvik.system.VMStack`.
//!
//! The VM keeps `java.lang.String` as an internal UTF-16 payload ([`Object::String`])
//! and `java.lang.Class` as [`Object::Class`]; the real DEX bytecode reads the
//! `count`/`hash` fields through the interpreter's shadow-field path. Every
//! entry here is a method declared `native` in the real Android 14 core library.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::class::{MethodImpl, NativeCtx};
use crate::error::VmResult;
use crate::heap::{ArrayData, ArrayObj, Handle, Object, NULL};
use crate::vm::{descriptor_to_dotted, ThreadStatus, Vm};

pub(crate) fn install(vm: &mut Vm) -> VmResult<()> {
    object(vm);
    string(vm);
    string_factory(vm);
    system(vm);
    thread(vm);
    throwable(vm);
    reference(vm);
    vmstack(vm);
    Ok(())
}

// ---------------------------------------------------------------------------
// java.lang.Object
// ---------------------------------------------------------------------------

fn object(vm: &mut Vm) {
    let c = "Ljava/lang/Object;";
    vm.register_native(
        c,
        "identityHashCodeNative",
        "(Ljava/lang/Object;)I",
        obj_identity_hash,
    );
    vm.register_native(
        c,
        "internalClone",
        "()Ljava/lang/Object;",
        obj_internal_clone,
    );
    vm.register_native(c, "notify", "()V", obj_notify);
    vm.register_native(c, "notifyAll", "()V", obj_notify_all);
    vm.register_native(c, "wait", "(JI)V", obj_wait);
}

fn obj_identity_hash(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let o = vm.arg_ref(ctx, 0);
    vm.ret_i32(ctx, o as i32);
    Ok(())
}

fn obj_internal_clone(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    if this == NULL {
        return Err(vm.throw_new("Ljava/lang/NullPointerException;", None));
    }
    let h = crate::corelib::lang::clone_object(vm, this)?;
    vm.ret_ref(ctx, h);
    Ok(())
}

/// `Object.wait(long,int)` — timeout-bounded waits park the green thread; the
/// scheduler resumes it. A zero timeout parks it until notified.
fn obj_wait(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let ms = vm.arg_i64(ctx, 0);
    let nanos = vm.arg_i32(ctx, 1);
    let t = vm.current_thread;
    // Java requires the caller to own the monitor and `wait` must release it
    // for the whole wait so another thread can enter the `synchronized` block
    // and `notify`. Without the release `HandlerThread.getLooper()` holds its
    // own monitor while waiting and `HandlerThread.run()` deadlocks on the
    // `synchronized (this) { mLooper = ...; notifyAll(); }` block (W133-A).
    let saved = vm
        .monitors
        .get(&this)
        .filter(|(owner, _)| *owner == t)
        .map(|(_, count)| *count)
        .unwrap_or(1);
    vm.monitors.remove(&this);
    vm.threads[t].wait_saved_count = saved;
    vm.threads[t].wait_monitor = Some(this);
    if ms > 0 || nanos > 0 {
        let total = ms.max(0) as u64 * 1_000_000 + nanos.max(0) as u64;
        vm.threads[t].status = ThreadStatus::Sleeping;
        vm.threads[t].wake_at = Some(Instant::now() + Duration::from_nanos(total));
    } else {
        vm.threads[t].status = ThreadStatus::Waiting;
    }
    Ok(())
}

fn obj_notify(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    wake_waiters(vm, this, 1);
    Ok(())
}

fn obj_notify_all(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    wake_waiters(vm, this, usize::MAX);
    Ok(())
}

fn wake_waiters(vm: &mut Vm, monitor: Handle, limit: usize) {
    let mut n = 0;
    for t in vm.threads.iter_mut() {
        if n >= limit {
            break;
        }
        if t.status == ThreadStatus::Waiting && t.wait_monitor == Some(monitor) {
            // The woken thread must re-acquire the monitor it released in
            // `wait` before it resumes; park it as monitor-blocked and leave
            // `wait_monitor` set so `pick_runnable` restores the ownership.
            t.status = ThreadStatus::BlockedMonitor(monitor);
            n += 1;
        }
    }
}

// ---------------------------------------------------------------------------
// java.lang.String
// ---------------------------------------------------------------------------

fn string(vm: &mut Vm) {
    let c = "Ljava/lang/String;";
    // The real `String` storage is VM-internal, so its constructors are
    // implemented here (the DEX bodies are `throw new
    // UnsupportedOperationException("Use StringFactory instead.")`).
    for sig in [
        "()V",
        "(Ljava/lang/String;)V",
        "(Ljava/lang/StringBuffer;)V",
        "(Ljava/lang/StringBuilder;)V",
        "([B)V",
        "([BB)V",
        "([BI)V",
        "([BII)V",
        "([BIII)V",
        "([BIILjava/lang/String;)V",
        "([BIILjava/nio/charset/Charset;)V",
        "([BLjava/lang/String;)V",
        "([BLjava/nio/charset/Charset;)V",
        "([C)V",
        "([CII)V",
        "([III)V",
        "(II[C)V",
    ] {
        vm.register_native(c, "<init>", sig, str_init);
    }
    // Content methods are overridden: the real DEX routes them through
    // `StringLatin1`/`StringUTF16`, which ACOS's internal string payload cannot
    // back reliably. The VM implements them directly instead.
    vm.register_native(c, "substring", "(I)Ljava/lang/String;", str_substring_from);
    vm.register_native(c, "substring", "(II)Ljava/lang/String;", str_substring);
    vm.register_native(c, "indexOf", "(I)I", str_index_of_char);
    vm.register_native(c, "indexOf", "(II)I", str_index_of_char_from);
    vm.register_native(c, "indexOf", "(Ljava/lang/String;)I", str_index_of_str);
    vm.register_native(c, "lastIndexOf", "(I)I", str_last_index_of_char);
    vm.register_native(
        c,
        "lastIndexOf",
        "(Ljava/lang/String;)I",
        str_last_index_of_str,
    );
    vm.register_native(c, "startsWith", "(Ljava/lang/String;)Z", str_starts_with);
    vm.register_native(
        c,
        "startsWith",
        "(Ljava/lang/String;I)Z",
        str_starts_with_from,
    );
    vm.register_native(c, "endsWith", "(Ljava/lang/String;)Z", str_ends_with);
    vm.register_native(c, "contains", "(Ljava/lang/CharSequence;)Z", str_contains);
    vm.register_native(c, "trim", "()Ljava/lang/String;", str_trim);
    vm.register_native(c, "toUpperCase", "()Ljava/lang/String;", str_to_upper);
    vm.register_native(c, "toLowerCase", "()Ljava/lang/String;", str_to_lower);
    vm.register_native(c, "replace", "(CC)Ljava/lang/String;", str_do_replace);
    vm.register_native(c, "isEmpty", "()Z", str_is_empty);
    vm.register_native(
        c,
        "equalsIgnoreCase",
        "(Ljava/lang/String;)Z",
        str_equals_ignore_case,
    );
    vm.register_native(c, "charAt", "(I)C", str_char_at);
    vm.register_native(c, "getCharsNoCheck", "(II[CI)V", str_get_chars_no_check);
    vm.register_native(c, "fillBytesLatin1", "([BI)V", str_fill_bytes_latin1);
    vm.register_native(c, "fillBytesUTF16", "([BI)V", str_fill_bytes_utf16);
    vm.register_native(
        c,
        "fastSubstring",
        "(II)Ljava/lang/String;",
        str_fast_substring,
    );
    vm.register_native(c, "intern", "()Ljava/lang/String;", str_intern);
    vm.register_native(c, "toCharArray", "()[C", str_to_char_array);
    vm.register_native(c, "compareTo", "(Ljava/lang/String;)I", str_compare_to);
    vm.register_native(
        c,
        "concat",
        "(Ljava/lang/String;)Ljava/lang/String;",
        str_concat,
    );
    vm.register_native(c, "doReplace", "(CC)Ljava/lang/String;", str_do_replace);
    vm.register_native(c, "doRepeat", "(I)Ljava/lang/String;", str_do_repeat);
}

fn units_of(vm: &Vm, h: Handle) -> Option<Vec<u16>> {
    vm.str_units(h).map(|u| u.to_vec())
}

fn set_string_units(vm: &mut Vm, h: Handle, units: Vec<u16>) {
    if let Some(Object::String(u)) = vm.heap.get_mut(h) {
        *u = units;
    }
}

fn int_array(vm: &Vm, h: Handle) -> Vec<i32> {
    match vm.heap.get(h) {
        Some(Object::Array(ArrayObj {
            data: ArrayData::Int(v),
            ..
        })) => v.clone(),
        _ => Vec::new(),
    }
}

fn str_init(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let mid = vm.ctx_method_id(ctx);
    let sig = vm.methods[mid as usize].sig.clone();
    let units: Vec<u16> = match sig.as_str() {
        "()V" => Vec::new(),
        "(Ljava/lang/String;)V" => units_of(vm, vm.arg_ref(ctx, 0)).unwrap_or_default(),
        "(Ljava/lang/StringBuffer;)V" | "(Ljava/lang/StringBuilder;)V" => {
            let obj = vm.arg_ref(ctx, 0);
            let m = vm.dispatch(obj, "toString", "()Ljava/lang/String;")?;
            let s = crate::interp::call_method(vm, m, obj, &[])? as u32;
            units_of(vm, s).unwrap_or_default()
        }
        "([B)V" => decode_bytes(vm, vm.arg_ref(ctx, 0), 0, 0, usize::MAX, "latin1"),
        "([BB)V" => {
            let b = vm.arg_ref(ctx, 0);
            let coder = vm.arg_i32(ctx, 1);
            decode_bytes_coder(vm, b, 0, usize::MAX, coder)
        }
        "([BI)V" => decode_bytes(
            vm,
            vm.arg_ref(ctx, 0),
            vm.arg_i32(ctx, 1),
            0,
            usize::MAX,
            "latin1",
        ),
        "([BII)V" => {
            // `String(byte[] bytes, int offset, int length)` (latin1).
            let b = vm.arg_ref(ctx, 0);
            let offset = vm.arg_i32(ctx, 1).max(0) as usize;
            let count = vm.arg_i32(ctx, 2).max(0) as usize;
            decode_bytes(vm, b, 0, offset, count, "latin1")
        }
        "([BIII)V" => {
            let b = vm.arg_ref(ctx, 0);
            let hibyte = vm.arg_i32(ctx, 1);
            let offset = vm.arg_i32(ctx, 2).max(0) as usize;
            let count = vm.arg_i32(ctx, 3).max(0) as usize;
            decode_bytes_hibyte(vm, b, hibyte, offset, count)
        }
        "([BIILjava/lang/String;)V" => {
            let b = vm.arg_ref(ctx, 0);
            let offset = vm.arg_i32(ctx, 1).max(0) as usize;
            let count = vm.arg_i32(ctx, 2).max(0) as usize;
            let name = vm.string_to_rust(vm.arg_ref(ctx, 3)).unwrap_or_default();
            decode_bytes(vm, b, 0, offset, count, &name)
        }
        "([BIILjava/nio/charset/Charset;)V" => {
            let b = vm.arg_ref(ctx, 0);
            let offset = vm.arg_i32(ctx, 1).max(0) as usize;
            let count = vm.arg_i32(ctx, 2).max(0) as usize;
            decode_bytes(vm, b, 0, offset, count, "UTF-8")
        }
        "([BLjava/lang/String;)V" => {
            let b = vm.arg_ref(ctx, 0);
            let name = vm.string_to_rust(vm.arg_ref(ctx, 1)).unwrap_or_default();
            decode_bytes(vm, b, 0, 0, usize::MAX, &name)
        }
        "([BLjava/nio/charset/Charset;)V" => {
            decode_bytes(vm, vm.arg_ref(ctx, 0), 0, 0, usize::MAX, "UTF-8")
        }
        "([C)V" => char_array(vm, vm.arg_ref(ctx, 0)).unwrap_or_default(),
        "([CII)V" => {
            let v = char_array(vm, vm.arg_ref(ctx, 0)).unwrap_or_default();
            let offset = vm.arg_i32(ctx, 1).max(0) as usize;
            let count = vm.arg_i32(ctx, 2).max(0) as usize;
            let end = (offset + count).min(v.len());
            v[offset.min(end)..end].to_vec()
        }
        "([III)V" => {
            let v = int_array(vm, vm.arg_ref(ctx, 0));
            let offset = vm.arg_i32(ctx, 1).max(0) as usize;
            let count = vm.arg_i32(ctx, 2).max(0) as usize;
            let end = (offset + count).min(v.len());
            v[offset.min(end)..end]
                .iter()
                .filter_map(|&c| char::from_u32(c as u32))
                .map(|c| c as u32 as u16)
                .collect()
        }
        "(II[C)V" => {
            let offset = vm.arg_i32(ctx, 0).max(0) as usize;
            let count = vm.arg_i32(ctx, 1).max(0) as usize;
            let v = char_array(vm, vm.arg_ref(ctx, 2)).unwrap_or_default();
            let end = (offset + count).min(v.len());
            v[offset.min(end)..end].to_vec()
        }
        _ => Vec::new(),
    };
    set_string_units(vm, this, units);
    Ok(())
}

fn decode_bytes(
    vm: &Vm,
    h: Handle,
    _hibyte: i32,
    offset: usize,
    count: usize,
    charset: &str,
) -> Vec<u16> {
    let bytes = byte_array(vm, h).unwrap_or_default();
    let end = if count == usize::MAX {
        bytes.len()
    } else {
        (offset + count).min(bytes.len())
    };
    let slice: Vec<u8> = bytes[offset.min(end)..end]
        .iter()
        .map(|&b| b as u8)
        .collect();
    let up = charset.to_ascii_uppercase();
    if up.contains("UTF-8") || up.contains("UTF8") {
        String::from_utf8_lossy(&slice).encode_utf16().collect()
    } else if up.contains("UTF-16") && !up.contains("BE") {
        slice
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect()
    } else {
        slice.iter().map(|&b| b as u16).collect()
    }
}

fn decode_bytes_coder(vm: &Vm, h: Handle, offset: usize, count: usize, coder: i32) -> Vec<u16> {
    let bytes = byte_array(vm, h).unwrap_or_default();
    let end = if count == usize::MAX {
        bytes.len()
    } else {
        (offset + count).min(bytes.len())
    };
    let slice: Vec<u8> = bytes[offset.min(end)..end]
        .iter()
        .map(|&b| b as u8)
        .collect();
    if coder == 1 {
        slice
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect()
    } else {
        slice.iter().map(|&b| b as u16).collect()
    }
}

fn decode_bytes_hibyte(vm: &Vm, h: Handle, hibyte: i32, offset: usize, count: usize) -> Vec<u16> {
    let bytes = byte_array(vm, h).unwrap_or_default();
    let end = (offset + count).min(bytes.len());
    bytes[offset.min(end)..end]
        .iter()
        .map(|&b| {
            let lo = (b as u8) as u16;
            if hibyte == 0 {
                lo
            } else {
                ((hibyte as u16 & 0xFF) << 8) | lo
            }
        })
        .collect()
}

fn str_char_at(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let i = vm.arg_i32(ctx, 0);
    let u = units_of(vm, this).unwrap_or_default();
    let c = u.get(i as usize).copied().unwrap_or(0);
    vm.ret_i32(ctx, c as i32);
    Ok(())
}

fn str_get_chars_no_check(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let src_begin = vm.arg_i32(ctx, 0).max(0) as usize;
    let src_end = vm.arg_i32(ctx, 1).max(0) as usize;
    let dst = vm.arg_ref(ctx, 2);
    let dst_begin = vm.arg_i32(ctx, 3).max(0) as usize;
    let u = units_of(vm, this).unwrap_or_default();
    let end = src_end.min(u.len());
    let begin = src_begin.min(end);
    if let Some(Object::Array(ArrayObj {
        data: ArrayData::Char(v),
        ..
    })) = vm.heap.get_mut(dst)
    {
        for (k, &ch) in u[begin..end].iter().enumerate() {
            if dst_begin + k < v.len() {
                v[dst_begin + k] = ch;
            }
        }
    }
    Ok(())
}

fn str_fill_bytes_latin1(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let dst = vm.arg_ref(ctx, 0);
    let offset = vm.arg_i32(ctx, 1).max(0) as usize;
    let u = units_of(vm, this).unwrap_or_default();
    if let Some(Object::Array(ArrayObj {
        data: ArrayData::Byte(v),
        ..
    })) = vm.heap.get_mut(dst)
    {
        for (k, &ch) in u.iter().enumerate() {
            if offset + k < v.len() {
                v[offset + k] = ch as i8;
            }
        }
    }
    Ok(())
}

fn str_fill_bytes_utf16(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let dst = vm.arg_ref(ctx, 0);
    let offset = vm.arg_i32(ctx, 1).max(0) as usize;
    let u = units_of(vm, this).unwrap_or_default();
    if let Some(Object::Array(ArrayObj {
        data: ArrayData::Byte(v),
        ..
    })) = vm.heap.get_mut(dst)
    {
        for (k, &ch) in u.iter().enumerate() {
            let lo = offset + k * 2;
            if lo + 1 < v.len() {
                v[lo] = ch as i8;
                v[lo + 1] = (ch >> 8) as i8;
            }
        }
    }
    Ok(())
}

fn str_fast_substring(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let begin = vm.arg_i32(ctx, 0).max(0) as usize;
    let end = vm.arg_i32(ctx, 1).max(0) as usize;
    let u = units_of(vm, this).unwrap_or_default();
    let end = end.min(u.len());
    let begin = begin.min(end);
    let h = vm.new_string_utf16(u[begin..end].to_vec())?;
    vm.ret_ref(ctx, h);
    Ok(())
}

fn str_intern(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let s = vm.string_to_rust(this).unwrap_or_default();
    let h = vm.intern_str(&s)?;
    vm.ret_ref(ctx, h);
    Ok(())
}

fn str_to_char_array(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let u = units_of(vm, this).unwrap_or_default();
    let h = vm.alloc(Object::Array(ArrayObj {
        elem_desc: "C".to_string(),
        data: ArrayData::Char(u),
    }))?;
    vm.ret_ref(ctx, h);
    Ok(())
}

fn str_compare_to(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let other = vm.arg_ref(ctx, 0);
    let a = units_of(vm, this).unwrap_or_default();
    let b = units_of(vm, other).unwrap_or_default();
    let n = a.len().min(b.len());
    let mut r = 0i32;
    for i in 0..n {
        if a[i] != b[i] {
            r = a[i] as i32 - b[i] as i32;
            break;
        }
    }
    if r == 0 {
        r = a.len() as i32 - b.len() as i32;
    }
    vm.ret_i32(ctx, r);
    Ok(())
}

fn str_concat(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let other = vm.arg_ref(ctx, 0);
    let mut a = units_of(vm, this).unwrap_or_default();
    a.extend(units_of(vm, other).unwrap_or_default());
    let h = vm.new_string_utf16(a)?;
    vm.ret_ref(ctx, h);
    Ok(())
}

fn str_do_replace(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let old = vm.arg_i32(ctx, 0) as u16;
    let new = vm.arg_i32(ctx, 1) as u16;
    let u: Vec<u16> = units_of(vm, this)
        .unwrap_or_default()
        .into_iter()
        .map(|c| if c == old { new } else { c })
        .collect();
    let h = vm.new_string_utf16(u)?;
    vm.ret_ref(ctx, h);
    Ok(())
}

fn str_do_repeat(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let n = vm.arg_i32(ctx, 0);
    if n < 0 {
        return Err(vm.throw_new(
            "Ljava/lang/IllegalArgumentException;",
            Some("count is negative"),
        ));
    }
    let n = n as usize;
    let base = units_of(vm, this).unwrap_or_default();
    // Checked multiply so a guest `int` cannot overflow into an unbounded
    // host `Vec` (HONESTY_AUDIT.md #9). Bound the result against the heap cap.
    let Some(total) = base.len().checked_mul(n) else {
        return Err(vm.throw_new(
            "Ljava/lang/OutOfMemoryError;",
            Some("String.repeat length overflow"),
        ));
    };
    vm.check_array_len("C", total)?;
    let mut out = Vec::with_capacity(total);
    for _ in 0..n {
        out.extend_from_slice(&base);
    }
    let h = vm.new_string_utf16(out)?;
    vm.ret_ref(ctx, h);
    Ok(())
}

fn find_sub(hay: &[u16], pat: &[u16], from: usize) -> Option<usize> {
    if pat.is_empty() {
        return Some(from.min(hay.len()));
    }
    if from >= hay.len() || pat.len() > hay.len() {
        return None;
    }
    (from..=hay.len() - pat.len()).find(|&i| hay[i..i + pat.len()] == *pat)
}

fn str_substring_from(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let u = units_of(vm, this).unwrap_or_default();
    let from = vm.arg_i32(ctx, 0).max(0) as usize;
    let from = from.min(u.len());
    let h = vm.new_string_utf16(u[from..].to_vec())?;
    vm.ret_ref(ctx, h);
    Ok(())
}

fn str_substring(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let u = units_of(vm, this).unwrap_or_default();
    let begin = (vm.arg_i32(ctx, 0).max(0) as usize).min(u.len());
    let end = vm.arg_i32(ctx, 1).max(0) as usize;
    let end = end.clamp(begin, u.len());
    let h = vm.new_string_utf16(u[begin..end].to_vec())?;
    vm.ret_ref(ctx, h);
    Ok(())
}

fn str_index_of_char(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let c = vm.arg_i32(ctx, 0) as u16;
    let u = units_of(vm, this).unwrap_or_default();
    let i = u
        .iter()
        .position(|&x| x == c)
        .map(|i| i as i32)
        .unwrap_or(-1);
    vm.ret_i32(ctx, i);
    Ok(())
}

fn str_index_of_char_from(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let c = vm.arg_i32(ctx, 0) as u16;
    let from = vm.arg_i32(ctx, 1).max(0) as usize;
    let u = units_of(vm, this).unwrap_or_default();
    let i = u
        .iter()
        .enumerate()
        .skip(from.min(u.len()))
        .find(|(_, &x)| x == c)
        .map(|(i, _)| i as i32)
        .unwrap_or(-1);
    vm.ret_i32(ctx, i);
    Ok(())
}

fn str_index_of_str(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let hay = units_of(vm, this).unwrap_or_default();
    let pat = units_of(vm, vm.arg_ref(ctx, 0)).unwrap_or_default();
    let i = find_sub(&hay, &pat, 0).map(|i| i as i32).unwrap_or(-1);
    vm.ret_i32(ctx, i);
    Ok(())
}

fn str_last_index_of_char(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let c = vm.arg_i32(ctx, 0) as u16;
    let u = units_of(vm, this).unwrap_or_default();
    let i = u
        .iter()
        .rposition(|&x| x == c)
        .map(|i| i as i32)
        .unwrap_or(-1);
    vm.ret_i32(ctx, i);
    Ok(())
}

fn str_last_index_of_str(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let hay = units_of(vm, this).unwrap_or_default();
    let pat = units_of(vm, vm.arg_ref(ctx, 0)).unwrap_or_default();
    let i = if pat.len() > hay.len() {
        -1
    } else {
        (0..=hay.len() - pat.len())
            .rev()
            .find(|&i| hay[i..i + pat.len()] == pat[..])
            .map(|i| i as i32)
            .unwrap_or(-1)
    };
    vm.ret_i32(ctx, i);
    Ok(())
}

fn str_starts_with(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let hay = units_of(vm, this).unwrap_or_default();
    let pat = units_of(vm, vm.arg_ref(ctx, 0)).unwrap_or_default();
    vm.ret_bool(ctx, hay.starts_with(&pat));
    Ok(())
}

fn str_starts_with_from(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let hay = units_of(vm, this).unwrap_or_default();
    let pat = units_of(vm, vm.arg_ref(ctx, 0)).unwrap_or_default();
    let from = vm.arg_i32(ctx, 1).max(0) as usize;
    let ok = from <= hay.len() && hay[from..].starts_with(&pat);
    vm.ret_bool(ctx, ok);
    Ok(())
}

fn str_ends_with(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let hay = units_of(vm, this).unwrap_or_default();
    let pat = units_of(vm, vm.arg_ref(ctx, 0)).unwrap_or_default();
    vm.ret_bool(ctx, hay.ends_with(&pat));
    Ok(())
}

fn str_contains(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let hay = units_of(vm, this).unwrap_or_default();
    let arg = vm.arg_ref(ctx, 0);
    let s = vm.string_to_rust(arg).unwrap_or_default();
    let pat: Vec<u16> = s.encode_utf16().collect();
    vm.ret_bool(ctx, find_sub(&hay, &pat, 0).is_some());
    Ok(())
}

fn str_trim(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let u = units_of(vm, this).unwrap_or_default();
    let begin = u.iter().position(|&c| c > 0x20).unwrap_or(u.len());
    let end = u
        .iter()
        .rposition(|&c| c > 0x20)
        .map(|i| i + 1)
        .unwrap_or(begin);
    let h = vm.new_string_utf16(u[begin..end].to_vec())?;
    vm.ret_ref(ctx, h);
    Ok(())
}

fn str_to_upper(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let u = units_of(vm, this).unwrap_or_default();
    let out: Vec<u16> = String::from_utf16_lossy(&u)
        .to_uppercase()
        .encode_utf16()
        .collect();
    let h = vm.new_string_utf16(out)?;
    vm.ret_ref(ctx, h);
    Ok(())
}

fn str_to_lower(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let u = units_of(vm, this).unwrap_or_default();
    let out: Vec<u16> = String::from_utf16_lossy(&u)
        .to_lowercase()
        .encode_utf16()
        .collect();
    let h = vm.new_string_utf16(out)?;
    vm.ret_ref(ctx, h);
    Ok(())
}

fn str_is_empty(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let empty = units_of(vm, this).map(|u| u.is_empty()).unwrap_or(true);
    vm.ret_bool(ctx, empty);
    Ok(())
}

fn str_equals_ignore_case(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let other = vm.arg_ref(ctx, 0);
    if other == NULL {
        vm.ret_bool(ctx, false);
        return Ok(());
    }
    let a = units_of(vm, this).unwrap_or_default();
    let b = units_of(vm, other).unwrap_or_default();
    let sa = String::from_utf16_lossy(&a).to_lowercase();
    let sb = String::from_utf16_lossy(&b).to_lowercase();
    vm.ret_bool(ctx, sa == sb);
    Ok(())
}

// ---------------------------------------------------------------------------
// java.lang.StringFactory
// ---------------------------------------------------------------------------

fn string_factory(vm: &mut Vm) {
    let c = "Ljava/lang/StringFactory;";
    vm.register_native(
        c,
        "newStringFromBytes",
        "([BIII)Ljava/lang/String;",
        sf_from_bytes,
    );
    vm.register_native(
        c,
        "newStringFromChars",
        "(II[C)Ljava/lang/String;",
        sf_from_chars,
    );
    vm.register_native(
        c,
        "newStringFromString",
        "(Ljava/lang/String;)Ljava/lang/String;",
        sf_from_string,
    );
    vm.register_native(
        c,
        "newStringFromUtf8Bytes",
        "([BII)Ljava/lang/String;",
        sf_from_utf8,
    );
    vm.register_native(
        c,
        "newStringFromUtf16Bytes",
        "([BII)Ljava/lang/String;",
        sf_from_utf16,
    );
}

fn sf_from_bytes(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    // (byte[] data, int high, int offset, int byteCount)
    let data = vm.arg_ref(ctx, 0);
    let high = vm.arg_i32(ctx, 1);
    let offset = vm.arg_i32(ctx, 2).max(0) as usize;
    let count = vm.arg_i32(ctx, 3).max(0) as usize;
    let bytes = byte_array(vm, data).unwrap_or_default();
    let units: Vec<u16> = if high == 0 {
        let end = (offset + count).min(bytes.len());
        bytes[offset.min(end)..end]
            .iter()
            .map(|&b| b as u16)
            .collect()
    } else {
        let end = (offset + count * 2).min(bytes.len());
        bytes[offset.min(end)..end]
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0] as u8, c[1] as u8]))
            .collect()
    };
    let h = vm.new_string_utf16(units)?;
    vm.ret_ref(ctx, h);
    Ok(())
}

fn sf_from_chars(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let offset = vm.arg_i32(ctx, 0).max(0) as usize;
    let count = vm.arg_i32(ctx, 1).max(0) as usize;
    let chars = vm.arg_ref(ctx, 2);
    let v = char_array(vm, chars).unwrap_or_default();
    let end = (offset + count).min(v.len());
    let h = vm.new_string_utf16(v[offset.min(end)..end].to_vec())?;
    vm.ret_ref(ctx, h);
    Ok(())
}

fn sf_from_string(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let s = vm.arg_ref(ctx, 0);
    let u = units_of(vm, s).unwrap_or_default();
    let h = vm.new_string_utf16(u)?;
    vm.ret_ref(ctx, h);
    Ok(())
}

fn sf_from_utf8(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let data = vm.arg_ref(ctx, 0);
    let offset = vm.arg_i32(ctx, 1).max(0) as usize;
    let count = vm.arg_i32(ctx, 2).max(0) as usize;
    let bytes = byte_array(vm, data).unwrap_or_default();
    let end = (offset + count).min(bytes.len());
    let slice: Vec<u8> = bytes[offset.min(end)..end]
        .iter()
        .map(|&b| b as u8)
        .collect();
    let h = vm.new_string_utf16(String::from_utf8_lossy(&slice).encode_utf16().collect())?;
    vm.ret_ref(ctx, h);
    Ok(())
}

fn sf_from_utf16(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let data = vm.arg_ref(ctx, 0);
    let offset = vm.arg_i32(ctx, 1).max(0) as usize;
    let count = vm.arg_i32(ctx, 2).max(0) as usize;
    let bytes = byte_array(vm, data).unwrap_or_default();
    let end = (offset + count * 2).min(bytes.len());
    let units: Vec<u16> = bytes[offset.min(end)..end]
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0] as u8, c[1] as u8]))
        .collect();
    let h = vm.new_string_utf16(units)?;
    vm.ret_ref(ctx, h);
    Ok(())
}

// ---------------------------------------------------------------------------
// java.lang.System
// ---------------------------------------------------------------------------

fn system(vm: &mut Vm) {
    let c = "Ljava/lang/System;";
    vm.register_native(
        c,
        "arraycopy",
        "(Ljava/lang/Object;ILjava/lang/Object;II)V",
        sys_arraycopy,
    );
    vm.register_native(c, "arraycopyBooleanUnchecked", "([ZI[ZII)V", sys_arraycopy);
    vm.register_native(c, "arraycopyByteUnchecked", "([BI[BII)V", sys_arraycopy);
    vm.register_native(c, "arraycopyCharUnchecked", "([CI[CII)V", sys_arraycopy);
    vm.register_native(c, "arraycopyShortUnchecked", "([SI[SII)V", sys_arraycopy);
    vm.register_native(c, "arraycopyIntUnchecked", "([II[III)V", sys_arraycopy);
    vm.register_native(c, "arraycopyLongUnchecked", "([JI[JII)V", sys_arraycopy);
    vm.register_native(c, "arraycopyFloatUnchecked", "([FI[FII)V", sys_arraycopy);
    vm.register_native(c, "arraycopyDoubleUnchecked", "([DI[DII)V", sys_arraycopy);
    vm.register_native(c, "currentTimeMillis", "()J", sys_current_time_millis);
    vm.register_native(c, "nanoTime", "()J", sys_nano_time);
    vm.register_native(c, "setOut0", "(Ljava/io/PrintStream;)V", sys_set_out);
    vm.register_native(c, "setErr0", "(Ljava/io/PrintStream;)V", sys_set_err);
    vm.register_native(c, "setIn0", "(Ljava/io/InputStream;)V", sys_set_in);
    vm.register_native(
        c,
        "specialProperties",
        "()[Ljava/lang/String;",
        sys_special_properties,
    );
    vm.register_native(
        c,
        "mapLibraryName",
        "(Ljava/lang/String;)Ljava/lang/String;",
        sys_map_library_name,
    );
    // `System.log` is the core-library warning sink; ACOS has no log sink here.
    vm.register_native(
        c,
        "log",
        "(CLjava/lang/String;Ljava/lang/Throwable;)V",
        sys_log,
    );
    vm.register_native(c, "log", "(CLjava/lang/String;)V", sys_log);
}

fn sys_log(_vm: &mut Vm, _ctx: NativeCtx) -> VmResult<()> {
    Ok(())
}

fn sys_arraycopy(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let src = vm.arg_ref(ctx, 0);
    let src_pos = vm.arg_i32(ctx, 1);
    let dst = vm.arg_ref(ctx, 2);
    let dst_pos = vm.arg_i32(ctx, 3);
    let len = vm.arg_i32(ctx, 4);
    if src == NULL || dst == NULL {
        return Err(vm.throw_new("Ljava/lang/NullPointerException;", None));
    }
    if len < 0 || src_pos < 0 || dst_pos < 0 {
        return Err(vm.throw_new("Ljava/lang/ArrayIndexOutOfBoundsException;", None));
    }
    array_copy(
        vm,
        src,
        src_pos as usize,
        dst,
        dst_pos as usize,
        len as usize,
    )
}

fn array_copy(
    vm: &mut Vm,
    src: Handle,
    src_pos: usize,
    dst: Handle,
    dst_pos: usize,
    len: usize,
) -> VmResult<()> {
    let src_data = match vm.heap.get(src) {
        Some(Object::Array(a)) => a.data.clone(),
        _ => return Err(vm.throw_new("Ljava/lang/ArrayStoreException;", None)),
    };
    let dst_is_ref = matches!(
        vm.heap.get(dst),
        Some(Object::Array(ArrayObj {
            data: ArrayData::Ref(_),
            ..
        }))
    );
    let src_is_ref = matches!(src_data, ArrayData::Ref(_));
    if src_is_ref != dst_is_ref {
        return Err(vm.throw_new("Ljava/lang/ArrayStoreException;", None));
    }
    macro_rules! copy {
        ($variant:ident, $v:expr, $dv:expr) => {{
            if src_pos + len > $v.len() || dst_pos + len > $dv.len() {
                return Err(vm.throw_new("Ljava/lang/ArrayIndexOutOfBoundsException;", None));
            }
            let tmp: Vec<_> = $v[src_pos..src_pos + len].to_vec();
            $dv[dst_pos..dst_pos + len].copy_from_slice(&tmp);
        }};
    }
    if let Some(Object::Array(ArrayObj { data, .. })) = vm.heap.get_mut(dst) {
        match (src_data, data) {
            (ArrayData::Bool(s), ArrayData::Bool(d)) => copy!(Bool, s, d),
            (ArrayData::Byte(s), ArrayData::Byte(d)) => {
                let s: Vec<u8> = s.iter().map(|&b| b as u8).collect();
                if dst_pos + len > d.len() {
                    return Err(vm.throw_new("Ljava/lang/ArrayIndexOutOfBoundsException;", None));
                }
                let tmp = s[src_pos..src_pos + len].to_vec();
                for (i, b) in tmp.iter().enumerate() {
                    d[dst_pos + i] = *b as i8;
                }
            }
            (ArrayData::Char(s), ArrayData::Char(d)) => copy!(Char, s, d),
            (ArrayData::Short(s), ArrayData::Short(d)) => copy!(Short, s, d),
            (ArrayData::Int(s), ArrayData::Int(d)) => copy!(Int, s, d),
            (ArrayData::Long(s), ArrayData::Long(d)) => copy!(Long, s, d),
            (ArrayData::Float(s), ArrayData::Float(d)) => copy!(Float, s, d),
            (ArrayData::Double(s), ArrayData::Double(d)) => copy!(Double, s, d),
            (ArrayData::Ref(s), ArrayData::Ref(d)) => copy!(Ref, s, d),
            _ => return Err(vm.throw_new("Ljava/lang/ArrayStoreException;", None)),
        }
    }
    Ok(())
}

fn sys_current_time_millis(_vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    _vm.ret_i64(ctx, ms);
    Ok(())
}

fn sys_nano_time(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    // `System.nanoTime()` is CLOCK_MONOTONIC on Android: nanoseconds since
    // boot, in the *same* timebase as `SystemClock.uptimeMillis()`/
    // `uptimeNanos()`. Returning epoch time here made `Choreographer` see a
    // vsync timestamp ~56 years "in the past", take the jitter branch in
    // `doFrame`, and replace the frame time with `System.nanoTime()`.
    vm.ret_i64(ctx, crate::natives::os_clock::uptime_nanos());
    Ok(())
}

fn sys_set_out(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    set_static_ref(vm, "Ljava/lang/System;", "out", vm.arg_ref(ctx, 0))
}

fn sys_set_err(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    set_static_ref(vm, "Ljava/lang/System;", "err", vm.arg_ref(ctx, 0))
}

fn sys_set_in(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    set_static_ref(vm, "Ljava/lang/System;", "in", vm.arg_ref(ctx, 0))
}

fn sys_special_properties(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let h = vm.alloc(Object::Array(ArrayObj {
        elem_desc: "Ljava/lang/String;".to_string(),
        data: ArrayData::Ref(Vec::new()),
    }))?;
    vm.ret_ref(ctx, h);
    Ok(())
}

fn sys_map_library_name(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let name = vm.arg_ref(ctx, 0);
    let s = vm.string_to_rust(name).unwrap_or_default();
    let h = vm.new_string(&format!("lib{s}.so"))?;
    vm.ret_ref(ctx, h);
    Ok(())
}

// ---------------------------------------------------------------------------
// java.lang.Thread
// ---------------------------------------------------------------------------

fn thread(vm: &mut Vm) {
    let c = "Ljava/lang/Thread;";
    vm.register_native(c, "currentThread", "()Ljava/lang/Thread;", th_current);
    vm.register_native(c, "interrupt0", "()V", th_interrupt0);
    vm.register_native(c, "isInterrupted", "()Z", th_is_interrupted);
    vm.register_native(c, "interrupted", "()Z", th_interrupted);
    vm.register_native(c, "holdsLock", "(Ljava/lang/Object;)Z", th_holds_lock);
    vm.register_native(c, "sleep", "(Ljava/lang/Object;JI)V", th_sleep);
    // The real `sleep(long,int)` acquires `Thread.currentThread().lock` via
    // `monitor-enter`; the VM parks the green thread directly instead.
    vm.register_native(c, "sleep", "(JI)V", th_sleep_ji);
    vm.register_native(c, "yield", "()V", th_yield);
    vm.register_native(
        c,
        "nativeCreate",
        "(Ljava/lang/Thread;JZ)V",
        th_native_create,
    );
    vm.register_native(c, "nativeGetStatus", "(Z)I", th_native_get_status);
    vm.register_native(
        c,
        "setNativeName",
        "(Ljava/lang/String;)V",
        th_set_native_name,
    );
    vm.register_native(c, "setPriority0", "(I)V", th_set_priority0);

    // ThreadGroup bookkeeping is maintained by the VM's green-thread runtime,
    // so the Java-side add/remove hooks are no-ops.
    let g = "Ljava/lang/ThreadGroup;";
    vm.register_native(g, "addUnstarted", "()V", noop_method);
    vm.register_native(g, "add", "(Ljava/lang/Thread;)V", noop_method);
    vm.register_native(g, "remove", "(Ljava/lang/Thread;)V", noop_method);
    vm.register_native(g, "threadTerminated", "(Ljava/lang/Thread;)V", noop_method);
}

fn noop_method(_vm: &mut Vm, _ctx: NativeCtx) -> VmResult<()> {
    Ok(())
}

fn th_current(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let t = vm.current_thread;
    let obj = vm.threads[t].object;
    vm.ret_ref(ctx, obj);
    Ok(())
}

fn th_interrupt0(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    for t in &mut vm.threads {
        // Interrupting a sleeping thread wakes it early; interrupting a
        // `LockSupport.park`ed thread unparks it (JDK `Parker` semantics).
        if t.object == this && matches!(t.status, ThreadStatus::Sleeping | ThreadStatus::Parked) {
            t.status = ThreadStatus::Runnable;
            t.wake_at = None;
        }
    }
    Ok(())
}

fn th_is_interrupted(vm: &mut Vm, _ctx: NativeCtx) -> VmResult<()> {
    let _ = vm;
    Ok(())
}

fn th_interrupted(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    vm.ret_bool(ctx, false);
    Ok(())
}

fn th_holds_lock(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let obj = vm.arg_ref(ctx, 0);
    let t = vm.current_thread;
    let held = vm.monitors.get(&obj).map(|(o, _)| *o == t).unwrap_or(false);
    vm.ret_bool(ctx, held);
    Ok(())
}

fn th_sleep(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let ms = vm.arg_i64(ctx, 1).max(0) as u64;
    let nanos = vm.arg_i32(ctx, 2).max(0) as u64;
    let t = vm.current_thread;
    vm.threads[t].status = ThreadStatus::Sleeping;
    vm.threads[t].wake_at = Some(Instant::now() + Duration::from_nanos(ms * 1_000_000 + nanos));
    Ok(())
}

fn th_sleep_ji(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let ms = vm.arg_i64(ctx, 0).max(0) as u64;
    let nanos = vm.arg_i32(ctx, 1).max(0) as u64;
    let t = vm.current_thread;
    vm.threads[t].status = ThreadStatus::Sleeping;
    vm.threads[t].wake_at = Some(Instant::now() + Duration::from_nanos(ms * 1_000_000 + nanos));
    Ok(())
}

fn th_yield(vm: &mut Vm, _ctx: NativeCtx) -> VmResult<()> {
    let t = vm.current_thread;
    vm.threads[t].status = ThreadStatus::Runnable;
    Ok(())
}

/// Register a started thread with the green-thread runtime and schedule its
/// `run()` on a new VM thread.
fn th_native_create(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.arg_ref(ctx, 0);
    if this == NULL {
        return Ok(());
    }
    let run = vm.dispatch(this, "run", "()V")?;
    let mut rt = crate::vm::ThreadRT::new(this);
    rt.status = ThreadStatus::Runnable;
    // Give the Java object a thread id if it does not have one yet.
    if vm
        .get_instance_field_by_name(this, "tid")
        .map(|v| v as i64)
        .unwrap_or(0)
        == 0
    {
        let id = vm.next_thread_id as i64;
        vm.next_thread_id += 1;
        vm.set_instance_field_by_name(this, "tid", id as u32);
    }
    // `Thread.isAlive` reads `nativePeer` (a long); mark it live.
    vm.set_instance_field_by_name(this, "nativePeer", 1);
    vm.set_instance_field_by_name(this, "started", 1);
    vm.threads.push(rt);
    let idx = vm.threads.len() - 1;
    let saved = vm.current_thread;
    vm.current_thread = idx;
    let r = crate::interp::push_frame_values(vm, run, this, &[]);
    vm.current_thread = saved;
    r?;
    Ok(())
}

fn th_native_get_status(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let started = vm.arg_bool(ctx, 0);
    if !started {
        vm.ret_i32(ctx, 0); // NEW
        return Ok(());
    }
    let alive = vm
        .get_instance_field_by_name(this, "nativePeer")
        .map(|v| v != 0)
        .unwrap_or(false);
    // Java `Thread.State`: RUNNABLE=1, TERMINATED=5.
    vm.ret_i32(ctx, if alive { 1 } else { 5 });
    Ok(())
}

fn th_set_native_name(vm: &mut Vm, _ctx: NativeCtx) -> VmResult<()> {
    let _ = vm;
    Ok(())
}

fn th_set_priority0(vm: &mut Vm, _ctx: NativeCtx) -> VmResult<()> {
    let _ = vm;
    Ok(())
}

// ---------------------------------------------------------------------------
// java.lang.Throwable
// ---------------------------------------------------------------------------

fn throwable(vm: &mut Vm) {
    let c = "Ljava/lang/Throwable;";
    vm.register_native(
        c,
        "nativeFillInStackTrace",
        "()Ljava/lang/Object;",
        thr_fill_in,
    );
    vm.register_native(
        c,
        "nativeGetStackTrace",
        "(Ljava/lang/Object;)[Ljava/lang/StackTraceElement;",
        thr_get_stack_trace,
    );
}

/// One frame captured at `Throwable` construction time. Names are resolved
/// eagerly so a later `getStackTrace()` does not depend on the stack that has
/// since unwound.
struct CapturedFrame {
    class_desc: String,
    method_name: String,
    file_name: Option<String>,
    line: i32,
}

thread_local! {
    /// Live `nativeFillInStackTrace` captures, keyed by the `long[]` token
    /// handed to the `Throwable` as its `backtrace`.
    static BACKTRACES: RefCell<HashMap<u64, Vec<CapturedFrame>>> =
        RefCell::new(HashMap::new());
    static BACKTRACE_IDS: Cell<u64> = const { Cell::new(1) };
}

/// Snapshot the Java (non-native) frames of `thread`, innermost first.
fn thread_frames(vm: &Vm, thread: usize) -> Vec<CapturedFrame> {
    let mut out = Vec::new();
    let Some(t) = vm.threads.get(thread) else {
        return out;
    };
    for fr in t.frames.iter().rev() {
        let m = &vm.methods[fr.method as usize];
        // `VMStack`/`Thread` runtime frames are not part of a Java stack trace.
        if matches!(m.imp, MethodImpl::Native(_)) {
            continue;
        }
        let (file_name, line) = match &m.imp {
            MethodImpl::Dex {
                code: Some(code), ..
            } => (code.source_file.clone(), code.line_for_pc(fr.pc) as i32),
            _ => (vm.classes[fr.class as usize].source_file.clone(), -1),
        };
        out.push(CapturedFrame {
            class_desc: vm.class_desc(fr.class).to_string(),
            method_name: m.name.clone(),
            file_name,
            line,
        });
    }
    out
}

/// Materialise captured frames as a real `java.lang.StackTraceElement[]`.
fn build_stack_elements(vm: &mut Vm, frames: &[CapturedFrame]) -> VmResult<Handle> {
    let ste = vm.load_class("Ljava/lang/StackTraceElement;")?;
    let roots_base = vm.native_roots.len();
    let mut elems: Vec<Handle> = Vec::with_capacity(frames.len());
    let built = (|| -> VmResult<()> {
        for f in frames {
            let class_h = vm.new_string(&descriptor_to_dotted(&f.class_desc))?;
            vm.native_roots.push(class_h);
            let method_h = vm.new_string(&f.method_name)?;
            vm.native_roots.push(method_h);
            let file_h = match &f.file_name {
                Some(s) => {
                    let h = vm.new_string(s)?;
                    vm.native_roots.push(h);
                    h
                }
                None => NULL,
            };
            let obj = vm.alloc_instance(ste)?;
            vm.native_roots.push(obj);
            vm.set_instance_field_by_name(obj, "declaringClass", class_h);
            vm.set_instance_field_by_name(obj, "methodName", method_h);
            vm.set_instance_field_by_name(obj, "fileName", file_h);
            vm.set_instance_field_by_name(obj, "lineNumber", f.line as u32);
            elems.push(obj);
        }
        Ok(())
    })();
    if let Err(e) = built {
        vm.native_roots.truncate(roots_base);
        return Err(e);
    }
    let arr = vm.alloc(Object::Array(ArrayObj {
        elem_desc: "Ljava/lang/StackTraceElement;".to_string(),
        data: ArrayData::Ref(elems),
    }))?;
    vm.native_roots.truncate(roots_base);
    Ok(arr)
}

/// Capture `thread`'s Java frames and intern them, returning the opaque
/// `long[]` token a real `Throwable.backtrace` holds. Shared by
/// `nativeFillInStackTrace` (Java construction) and `Vm::new_throwable`
/// (VM-constructed exceptions, which never run the Java constructor).
pub(crate) fn capture_backtrace(vm: &mut Vm, thread: usize) -> VmResult<Handle> {
    let frames = thread_frames(vm, thread);
    let id = BACKTRACE_IDS.with(|c| {
        let v = c.get();
        c.set(v + 1);
        v
    });
    BACKTRACES.with(|m| m.borrow_mut().insert(id, frames));
    // `Throwable.backtrace` is opaque to Java; a real `long[]` matches ART.
    vm.alloc(Object::Array(ArrayObj {
        elem_desc: "J".to_string(),
        data: ArrayData::Long(vec![id as i64]),
    }))
}

fn thr_fill_in(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    // Capture the real VM stack now: `getOurStackTrace` may be called long
    // after the stack unwound (Kotlin's `Intrinsics.sanitizeStackTrace`).
    let h = capture_backtrace(vm, ctx.thread)?;
    vm.ret_ref(ctx, h);
    Ok(())
}

fn thr_get_stack_trace(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let token = vm.arg_ref(ctx, 0);
    let id = match vm.heap.get(token) {
        Some(Object::Array(ArrayObj {
            data: ArrayData::Long(v),
            ..
        })) => v.first().copied().unwrap_or(0) as u64,
        _ => 0,
    };
    let frames = BACKTRACES
        .with(|m| m.borrow_mut().remove(&id))
        .unwrap_or_default();
    let arr = build_stack_elements(vm, &frames)?;
    vm.ret_ref(ctx, arr);
    Ok(())
}

// ---------------------------------------------------------------------------
// java.lang.ref.Reference
// ---------------------------------------------------------------------------

fn reference(vm: &mut Vm) {
    let c = "Ljava/lang/ref/Reference;";
    vm.register_native(c, "refersTo0", "(Ljava/lang/Object;)Z", ref_refers_to0);
    vm.register_native(c, "getReferent", "()Ljava/lang/Object;", ref_get_referent);
    vm.register_native(c, "clearReferent", "()V", ref_clear_referent);
    let f = "Ljava/lang/ref/FinalizerReference;";
    vm.register_native(f, "getReferent", "()Ljava/lang/Object;", ref_get_referent);
    vm.register_native(
        f,
        "makeCircularListIfUnenqueued",
        "()Z",
        ref_finalizer_circular,
    );
}

fn ref_refers_to0(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let other = vm.arg_ref(ctx, 0);
    let referent = vm
        .get_instance_field_by_name(this, "referent")
        .unwrap_or(NULL);
    vm.ret_bool(ctx, referent == other);
    Ok(())
}

fn ref_get_referent(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let referent = vm
        .get_instance_field_by_name(this, "referent")
        .unwrap_or(NULL);
    vm.ret_ref(ctx, referent);
    Ok(())
}

fn ref_clear_referent(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    vm.set_instance_field_by_name(this, "referent", NULL);
    Ok(())
}

fn ref_finalizer_circular(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    vm.ret_bool(ctx, false);
    Ok(())
}

// ---------------------------------------------------------------------------
// dalvik.system.VMStack
// ---------------------------------------------------------------------------

fn vmstack(vm: &mut Vm) {
    let c = "Ldalvik/system/VMStack;";
    vm.register_native(
        c,
        "getStackClass2",
        "()Ljava/lang/Class;",
        vms_get_stack_class,
    );
    vm.register_native(
        c,
        "getCallingClassLoader",
        "()Ljava/lang/ClassLoader;",
        vms_calling_class_loader,
    );
    vm.register_native(
        c,
        "getClosestUserClassLoader",
        "()Ljava/lang/ClassLoader;",
        vms_calling_class_loader,
    );
    vm.register_native(
        c,
        "fillStackTraceElements",
        "(Ljava/lang/Thread;[Ljava/lang/StackTraceElement;)I",
        vms_fill_stack,
    );
    vm.register_native(
        c,
        "getThreadStackTrace",
        "(Ljava/lang/Thread;)[Ljava/lang/StackTraceElement;",
        vms_thread_stack,
    );
}

/// Whether a method is `@CallerSensitive` (transparent to `getCallerClass`).
///
/// ACOS does not parse annotation metadata, but the defining property is
/// checkable statically: a `@CallerSensitive` method is one that calls
/// `Reflection.getCallerClass()` (directly or through `VMStack.getStackClass2`).
fn method_is_caller_sensitive(vm: &Vm, mid: crate::class::MethodId) -> bool {
    let m = &vm.methods[mid as usize];
    let (dex, code) = match &m.imp {
        crate::class::MethodImpl::Dex {
            dex,
            code: Some(code),
            ..
        } => (*dex, code.clone()),
        _ => return false,
    };
    for insn in &code.insns {
        if !(0x6e..=0x78).contains(&insn.op) {
            continue;
        }
        if let Ok(r) = vm.dexs[dex as usize].file.method_ref(insn.index) {
            let name = r.name.to_string();
            if name == "getCallerClass" || name == "getStackClass2" {
                return true;
            }
        }
    }
    false
}

fn vms_get_stack_class(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    // `getStackClass2` backs `Reflection.getCallerClass()`: return the class of
    // the first frame above the `@CallerSensitive` chain.
    let mut caller_class = None;
    let mut i = ctx.frame;
    while i > 0 {
        i -= 1;
        let mid = vm.threads[ctx.thread].frames[i].method;
        let c = vm.threads[ctx.thread].frames[i].class;
        if method_is_caller_sensitive(vm, mid) {
            continue;
        }
        caller_class = Some(c);
        break;
    }
    match caller_class {
        Some(c) => {
            let obj = vm.class_object(c)?;
            vm.ret_ref(ctx, obj);
        }
        None => vm.ret_ref(ctx, NULL),
    }
    Ok(())
}

fn vms_calling_class_loader(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    vm.ret_ref(ctx, NULL);
    Ok(())
}

/// Map a live `java.lang.Thread` object to its VM thread slot.
fn thread_index_of(vm: &Vm, thread_obj: Handle) -> Option<usize> {
    vm.threads.iter().position(|t| t.object == thread_obj)
}

/// `VMStack.fillStackTraceElements(Thread, StackTraceElement[])`: fill the
/// caller-supplied array with as many frames as fit and return the count.
fn vms_fill_stack(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let thread_obj = vm.arg_ref(ctx, 0);
    let arr = vm.arg_ref(ctx, 1);
    let cap = match vm.heap.get(arr) {
        Some(Object::Array(a)) if matches!(a.data, ArrayData::Ref(_)) => a.len(),
        _ => {
            vm.ret_i32(ctx, 0);
            return Ok(());
        }
    };
    let idx = thread_index_of(vm, thread_obj).unwrap_or(vm.current_thread);
    let frames = thread_frames(vm, idx);
    let take = frames.len().min(cap);
    let source = build_stack_elements(vm, &frames[..take])?;
    let elems = match vm.heap.get(source) {
        Some(Object::Array(ArrayObj {
            data: ArrayData::Ref(v),
            ..
        })) => v.clone(),
        _ => Vec::new(),
    };
    if let Some(Object::Array(ArrayObj {
        data: ArrayData::Ref(dst),
        ..
    })) = vm.heap.get_mut(arr)
    {
        for (i, h) in elems.into_iter().enumerate() {
            if i < dst.len() {
                dst[i] = h;
            }
        }
    }
    vm.ret_i32(ctx, take as i32);
    Ok(())
}

/// `VMStack.getThreadStackTrace(Thread)`: the real `Thread.getStackTrace()`
/// implementation. Returning an empty array here made Kotlin's
/// `Intrinsics.createParameterIsNullExceptionMessage` index `[0]` of a
/// zero-length trace and throw a spurious `ArrayIndexOutOfBoundsException`
/// (W88-A blocker 4).
fn vms_thread_stack(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let thread_obj = vm.arg_ref(ctx, 0);
    let idx = thread_index_of(vm, thread_obj).unwrap_or(vm.current_thread);
    let frames = thread_frames(vm, idx);
    let arr = build_stack_elements(vm, &frames)?;
    vm.ret_ref(ctx, arr);
    Ok(())
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

fn byte_array(vm: &Vm, h: Handle) -> Option<Vec<i8>> {
    match vm.heap.get(h) {
        Some(Object::Array(ArrayObj {
            data: ArrayData::Byte(v),
            ..
        })) => Some(v.clone()),
        _ => None,
    }
}

fn char_array(vm: &Vm, h: Handle) -> Option<Vec<u16>> {
    match vm.heap.get(h) {
        Some(Object::Array(ArrayObj {
            data: ArrayData::Char(v),
            ..
        })) => Some(v.clone()),
        _ => None,
    }
}

fn set_static_ref(vm: &mut Vm, class_desc: &str, field: &str, value: Handle) -> VmResult<()> {
    let cid = vm.load_class(class_desc)?;
    let off = vm.classes[cid as usize]
        .declared_static_fields
        .iter()
        .find(|f| f.name == field)
        .map(|f| f.offset as usize);
    if let Some(off) = off {
        vm.classes[cid as usize].statics[off] = value;
    }
    Ok(())
}
