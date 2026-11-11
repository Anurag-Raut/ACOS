//! The real `JNINativeInterface` and `JNIInvokeInterface` implementations.
//!
//! Every function has the exact C ABI of its JNI slot. The function table in
//! `table.rs` is generated from `jni_slots.txt` (NDK 27.1 `jni.h` order); slots
//! not implemented here point at `traps.rs`, which records a structured
//! `JNI_UNSUPPORTED` error and lets the enclosing native call abort cleanly.
#![allow(non_snake_case)]
#![allow(clippy::not_unsafe_ptr_arg_deref)]

use std::cell::RefCell;
use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_void};
use std::rc::Rc;

use acos_vm::class::{ClassId, MethodId};
use acos_vm::heap::{ArrayData, ArrayObj, Object, NULL};
use acos_vm::javabridge::{class_descriptor, JavaValue};
use acos_vm::vm::InvokeKind;
use acos_vm::{BridgeError, Handle, Vm};

use crate::error::JniError;
use crate::refs;
use crate::state::{
    runtime_state, Buffer, FieldInfo, MethodInfo, State, ThreadJni, JNI_ERR, JNI_OK,
    JNI_VERSION_1_6,
};

// ---------------------------------------------------------------------------
// Entry / error plumbing
// ---------------------------------------------------------------------------

struct Ctx {
    rt: Rc<RefCell<State>>,
    vm: *mut Vm,
    t: usize,
}

impl Ctx {
    /// The live VM for this attachment.
    ///
    /// The returned lifetime is not tied to `&self` so JNI code can interleave
    /// VM calls with reference-frame bookkeeping (`rt`). There is exactly one
    /// VM per host thread and this crate never creates two simultaneous `&mut`
    /// uses of it; see the SAFETY comment on `enter`.
    fn vm(&self) -> &'static mut Vm {
        // SAFETY: `enter` obtains the pointer from the active attachment; the
        // VM is not moved while a native/JNI call is in progress.
        unsafe { &mut *self.vm }
    }
}

fn enter(function: &'static str, exception_safe: bool) -> Option<Ctx> {
    let rt = runtime_state()?;
    let vm_ptr = crate::state::current_vm();
    if vm_ptr.is_null() {
        refs::set_error(
            &rt,
            0,
            JniError::NoVm {
                function: function.to_string(),
            },
        );
        return None;
    }
    // SAFETY: the pointer is set by `JniRuntime::attach` / native dispatch.
    // When the owner services a brokered worker request it forces the worker's
    // synthetic thread id so locals/pending land in that worker's frame.
    let t = crate::state::forced_thread()
        .unwrap_or_else(|| unsafe { (*vm_ptr).current_thread });
    let pending = refs::pending(&rt, t).is_some();
    if pending && !exception_safe {
        refs::set_error(
            &rt,
            t,
            JniError::PendingException {
                function: function.to_string(),
            },
        );
        return None;
    }
    {
        let mut st = rt.borrow_mut();
        st.last_thread = t;
        let th = st.threads.entry(t).or_default();
        if th.frames.is_empty() {
            th.frames.push(crate::state::LocalFrame::default());
        }
    }
    Some(Ctx { rt, vm: vm_ptr, t })
}

fn record(ctx: &Ctx, e: JniError) {
    refs::set_error(&ctx.rt, ctx.t, e);
}

fn unsupported_slot(slot: &str) -> JniError {
    JniError::Unsupported {
        slot: slot.to_string(),
    }
}

/// Called by the generated traps.
pub fn unsupported(slot: &'static str) {
    if let Some(rt) = runtime_state() {
        let vm_ptr = crate::state::current_vm();
        let t = if vm_ptr.is_null() {
            0
        } else {
            // SAFETY: see `enter`.
            unsafe { (*vm_ptr).current_thread }
        };
        refs::set_error(&rt, t, unsupported_slot(slot));
    } else {
        eprintln!("acos-jni: {slot} called without an attached runtime");
    }
}

fn cstr(p: *const c_char) -> String {
    if p.is_null() {
        return String::new();
    }
    // SAFETY: JNI passes NUL-terminated strings for the duration of the call.
    unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
}

fn local(ctx: &mut Ctx, h: Handle) -> Handle {
    let vm = ctx.vm();
    refs::local(vm, &ctx.rt, ctx.t, h)
}

fn throw_class(ctx: &mut Ctx, desc: &str, message: &str) -> i32 {
    let vm = ctx.vm();
    match vm.new_throwable(desc, Some(message)) {
        Ok(h) => {
            refs::set_pending(vm, &ctx.rt, ctx.t, h);
            0
        }
        Err(e) => {
            record(
                ctx,
                JniError::NativeFault {
                    message: format!("cannot construct {desc}: {e}"),
                },
            );
            JNI_ERR
        }
    }
}

/// Record a `BridgeError` from a VM class/field/method lookup. A pending Java
/// exception becomes a real JNI throw; anything else is a structured fault.
fn record_bridge(ctx: &mut Ctx, function: &str, e: BridgeError) {
    if let BridgeError::JavaException { class, message } = e {
        let desc = class_descriptor(&class);
        let _ = throw_class(ctx, &desc, &message);
    } else {
        record(ctx, bridge_error(function, e));
    }
}

/// JNI `GetStaticFieldID` / `GetStaticMethodID` must run the class `<clinit>`
/// before handing out an ID (JNI spec). Returns `false` (and leaves a pending
/// exception) when initialisation failed.
fn ensure_static_init(ctx: &mut Ctx, cid: ClassId, function: &str) -> bool {
    match ctx.vm().initialize_class_for_jni(cid) {
        Ok(()) => true,
        Err(e) => {
            record_bridge(ctx, function, e);
            false
        }
    }
}

fn live(ctx: &mut Ctx, h: Handle, what: &str, function: &str) -> Result<Handle, JniError> {
    if h == NULL {
        return Err(JniError::NullReference {
            function: function.to_string(),
            what: what.to_string(),
        });
    }
    let vm = ctx.vm();
    if !vm.heap.is_live(h) {
        return Err(JniError::BadReference {
            function: function.to_string(),
            what: what.to_string(),
            handle: h,
        });
    }
    Ok(h)
}

fn class_id(ctx: &mut Ctx, cls: Handle, function: &str) -> Result<ClassId, JniError> {
    let cls = live(ctx, cls, "jclass", function)?;
    let vm = ctx.vm();
    if let Some(id) = vm.class_id_of_class_object(cls) {
        return Ok(id);
    }
    // A few builtins hand out class objects without the `Object::Class`
    // payload; fall back to the object's runtime class.
    vm.class_of_object(cls).ok_or(JniError::BadReference {
        function: function.to_string(),
        what: "jclass".to_string(),
        handle: cls,
    })
}

macro_rules! guard {
    ($name:expr, $safe:expr, $default:expr) => {
        match enter($name, $safe) {
            Some(c) => c,
            None => return $default,
        }
    };
}

macro_rules! or_fail {
    ($ctx:expr, $default:expr, $expr:expr) => {
        match $expr {
            Ok(v) => v,
            Err(e) => {
                record(&$ctx, e);
                return $default;
            }
        }
    };
}

macro_rules! or_bail {
    ($ctx:expr, $expr:expr) => {
        match $expr {
            Ok(v) => v,
            Err(e) => {
                record(&$ctx, e);
                return;
            }
        }
    };
}

// ---------------------------------------------------------------------------
// Object model / class lookup
// ---------------------------------------------------------------------------

pub unsafe extern "C" fn jni_GetVersion(_env: usize) -> i32 {
    JNI_VERSION_1_6
}

pub unsafe extern "C" fn jni_FindClass(_env: usize, name: *const c_char) -> usize {
    if let Some(v) = crate::broker::worker_find_class(cstr(name).as_bytes()) {
        return v as usize;
    }
    let mut ctx = guard!("FindClass", false, 0);
    let name = cstr(name);
    let desc = if name.starts_with('[') || (name.starts_with('L') && name.ends_with(';')) {
        name.clone()
    } else {
        format!("L{};", name.replace('.', "/"))
    };
    let vm = ctx.vm();
    match vm.load_class(&desc) {
        Ok(cid) => match vm.class_object(cid) {
            Ok(h) => local(&mut ctx, h) as usize,
            Err(e) => {
                record(
                    &ctx,
                    JniError::ClassNotFound {
                        name: format!("{name} ({e})"),
                    },
                );
                0
            }
        },
        Err(_) => {
            let _ = throw_class(&mut ctx, "Ljava/lang/NoClassDefFoundError;", &name);
            0
        }
    }
}

pub unsafe extern "C" fn jni_GetSuperclass(_env: usize, cls: usize) -> usize {
    let mut ctx = guard!("GetSuperclass", false, 0);
    let cid = or_fail!(ctx, 0, class_id(&mut ctx, cls as Handle, "GetSuperclass"));
    let vm = ctx.vm();
    match vm.classes[cid as usize].super_class {
        Some(sup) => match vm.class_object(sup) {
            Ok(h) => local(&mut ctx, h) as usize,
            Err(_) => 0,
        },
        None => 0,
    }
}

pub unsafe extern "C" fn jni_IsAssignableFrom(_env: usize, a: usize, b: usize) -> u8 {
    let mut ctx = guard!("IsAssignableFrom", false, 0);
    let from = or_fail!(ctx, 0, class_id(&mut ctx, a as Handle, "IsAssignableFrom"));
    let to = or_fail!(ctx, 0, class_id(&mut ctx, b as Handle, "IsAssignableFrom"));
    let vm = ctx.vm();
    vm.is_assignable(from, to).unwrap_or(false) as u8
}

pub unsafe extern "C" fn jni_GetObjectClass(_env: usize, obj: usize) -> usize {
    if let Some(v) = crate::broker::worker_slot(crate::broker::Op::GetObjectClass { obj }, "GetObjectClass") {
        return v as usize;
    }
    let mut ctx = guard!("GetObjectClass", false, 0);
    let h = or_fail!(
        ctx,
        0,
        live(&mut ctx, obj as Handle, "jobject", "GetObjectClass")
    );
    let vm = ctx.vm();
    let Some(cid) = vm.class_of_object(h) else {
        record(
            &ctx,
            JniError::BadReference {
                function: "GetObjectClass".into(),
                what: "jobject".into(),
                handle: h,
            },
        );
        return 0;
    };
    match vm.class_object(cid) {
        Ok(c) => local(&mut ctx, c) as usize,
        Err(_) => 0,
    }
}

pub unsafe extern "C" fn jni_IsInstanceOf(_env: usize, obj: usize, cls: usize) -> u8 {
    let mut ctx = guard!("IsInstanceOf", false, 0);
    let h = obj as Handle;
    if h == NULL {
        return 0;
    }
    let Some(_) = live(&mut ctx, h, "jobject", "IsInstanceOf").ok() else {
        return 0;
    };
    let want = or_fail!(ctx, 0, class_id(&mut ctx, cls as Handle, "IsInstanceOf"));
    let vm = ctx.vm();
    match vm.class_of_object(h) {
        Some(got) => vm.is_assignable(got, want).unwrap_or(false) as u8,
        None => 0,
    }
}

pub unsafe extern "C" fn jni_AllocObject(_env: usize, cls: usize) -> usize {
    if let Some(v) = crate::broker::worker_slot(
        crate::broker::Op::AllocObject { class: cls },
        "AllocObject",
    ) {
        return v as usize;
    }
    let mut ctx = guard!("AllocObject", false, 0);
    let cid = or_fail!(ctx, 0, class_id(&mut ctx, cls as Handle, "AllocObject"));
    let vm = ctx.vm();
    let desc = vm.class_desc(cid).to_string();
    match vm.new_instance(&desc) {
        Ok(h) => local(&mut ctx, h) as usize,
        Err(e) => {
            record(
                &ctx,
                JniError::NativeFault {
                    message: e.to_string(),
                },
            );
            0
        }
    }
}

// ---------------------------------------------------------------------------
// Exceptions
// ---------------------------------------------------------------------------

pub unsafe extern "C" fn jni_Throw(_env: usize, obj: usize) -> i32 {
    if let Some(v) = crate::broker::worker_slot(crate::broker::Op::Throw { obj }, "Throw") {
        return v as i32;
    }
    let mut ctx = guard!("Throw", true, JNI_ERR);
    let h = match live(&mut ctx, obj as Handle, "throwable", "Throw") {
        Ok(h) => h,
        Err(e) => {
            record(&ctx, e);
            return JNI_ERR;
        }
    };
    let vm = ctx.vm();
    let throwable = vm.load_class("Ljava/lang/Throwable;").ok();
    let obj_class = vm.class_of_object(h);
    if let (Some(t), Some(c)) = (throwable, obj_class) {
        if !vm.is_assignable(c, t).unwrap_or(false) {
            record(
                &ctx,
                JniError::TypeMismatch {
                    function: "Throw".into(),
                    expected: "java.lang.Throwable".into(),
                    got: vm.class_desc(c).to_string(),
                },
            );
            return JNI_ERR;
        }
    }
    refs::set_pending(vm, &ctx.rt, ctx.t, h);
    JNI_OK
}

pub unsafe extern "C" fn jni_ThrowNew(_env: usize, cls: usize, msg: *const c_char) -> i32 {
    if let Some(v) = crate::broker::worker_slot(
        crate::broker::Op::ThrowNew { class: cls, message: cstr(msg).into_bytes() },
        "ThrowNew",
    ) {
        return v as i32;
    }
    let mut ctx = guard!("ThrowNew", true, JNI_ERR);
    let cid = match class_id(&mut ctx, cls as Handle, "ThrowNew") {
        Ok(c) => c,
        Err(e) => {
            record(&ctx, e);
            return JNI_ERR;
        }
    };
    let message = cstr(msg);
    let vm = ctx.vm();
    let desc = vm.class_desc(cid).to_string();
    match vm.new_throwable(&desc, Some(&message)) {
        Ok(h) => {
            refs::set_pending(vm, &ctx.rt, ctx.t, h);
            JNI_OK
        }
        Err(e) => {
            record(
                &ctx,
                JniError::NativeFault {
                    message: e.to_string(),
                },
            );
            JNI_ERR
        }
    }
}

pub unsafe extern "C" fn jni_ExceptionOccurred(_env: usize) -> usize {
    if let Some(v) = crate::broker::worker_slot(crate::broker::Op::ExceptionOccurred, "ExceptionOccurred") {
        return v as usize;
    }
    let mut ctx = guard!("ExceptionOccurred", true, 0);
    let Some(h) = refs::pending(&ctx.rt, ctx.t) else {
        return 0;
    };
    local(&mut ctx, h) as usize
}

pub unsafe extern "C" fn jni_ExceptionDescribe(_env: usize) {
    let Some(ctx) = enter("ExceptionDescribe", true) else {
        return;
    };
    if let Some(h) = refs::pending(&ctx.rt, ctx.t) {
        let vm = ctx.vm();
        let class = vm
            .class_of_object(h)
            .map(|c| vm.class_desc(c).to_string())
            .unwrap_or_else(|| "<unknown>".into());
        let msg = vm.throwable_message(h).unwrap_or_default();
        eprintln!("JNI exception: {class}: {msg}");
    }
}

pub unsafe extern "C" fn jni_ExceptionClear(_env: usize) {
    if crate::broker::worker_slot(crate::broker::Op::ExceptionClear, "ExceptionClear").is_some() {
        return;
    }
    let Some(ctx) = enter("ExceptionClear", true) else {
        return;
    };
    let vm = ctx.vm();
    refs::clear_pending(vm, &ctx.rt, ctx.t);
}

pub unsafe extern "C" fn jni_ExceptionCheck(_env: usize) -> u8 {
    if let Some(v) = crate::broker::worker_slot(crate::broker::Op::ExceptionCheck, "ExceptionCheck") {
        return v as u8;
    }
    guard!("ExceptionCheck", true, 0);
    let rt = runtime_state().unwrap();
    let vm_ptr = crate::state::current_vm();
    let t = crate::state::forced_thread().unwrap_or_else(|| {
        if vm_ptr.is_null() {
            0
        } else {
            unsafe { (*vm_ptr).current_thread }
        }
    });
    refs::pending(&rt, t).is_some() as u8
}

// ---------------------------------------------------------------------------
// Local / global references
// ---------------------------------------------------------------------------

pub unsafe extern "C" fn jni_PushLocalFrame(_env: usize, capacity: i32) -> i32 {
    if let Some(v) = crate::broker::worker_slot(
        crate::broker::Op::PushLocalFrame { cap: capacity },
        "PushLocalFrame",
    ) {
        return v as i32;
    }
    let mut ctx = guard!("PushLocalFrame", false, JNI_ERR);
    if !(0..=crate::state::MAX_LOCAL_CAPACITY).contains(&capacity) {
        let _ = throw_class(
            &mut ctx,
            "Ljava/lang/OutOfMemoryError;",
            "PushLocalFrame: unsatisfiable local reference capacity",
        );
        return JNI_ERR;
    }
    refs::push_frame(&ctx.rt, ctx.t, capacity as usize);
    JNI_OK
}

pub unsafe extern "C" fn jni_PopLocalFrame(_env: usize, result: usize) -> usize {
    if let Some(v) = crate::broker::worker_slot(
        crate::broker::Op::PopLocalFrame { result },
        "PopLocalFrame",
    ) {
        return v as usize;
    }
    let ctx = guard!("PopLocalFrame", true, 0);
    let vm = ctx.vm();
    refs::pop_frame(vm, &ctx.rt, ctx.t, result as Handle) as usize
}

pub unsafe extern "C" fn jni_NewGlobalRef(_env: usize, obj: usize) -> usize {
    if let Some(v) = crate::broker::worker_slot(
        crate::broker::Op::NewGlobalRef { obj },
        "NewGlobalRef",
    ) {
        return v as usize;
    }
    let mut ctx = guard!("NewGlobalRef", false, 0);
    let h = if obj == 0 {
        NULL
    } else {
        or_fail!(
            ctx,
            0,
            live(&mut ctx, obj as Handle, "jobject", "NewGlobalRef")
        )
    };
    let vm = ctx.vm();
    refs::global(vm, &ctx.rt, h) as usize
}

pub unsafe extern "C" fn jni_DeleteGlobalRef(_env: usize, obj: usize) {
    if crate::broker::worker_slot(
        crate::broker::Op::DeleteGlobalRef { obj },
        "DeleteGlobalRef",
    )
    .is_some()
    {
        return;
    }
    let Some(ctx) = enter("DeleteGlobalRef", true) else {
        return;
    };
    let vm = ctx.vm();
    refs::delete_global(vm, &ctx.rt, obj as Handle);
}

pub unsafe extern "C" fn jni_DeleteLocalRef(_env: usize, obj: usize) {
    if crate::broker::worker_slot(
        crate::broker::Op::DeleteLocalRef { obj },
        "DeleteLocalRef",
    )
    .is_some()
    {
        return;
    }
    let Some(ctx) = enter("DeleteLocalRef", true) else {
        return;
    };
    let vm = ctx.vm();
    refs::delete_local(vm, &ctx.rt, ctx.t, obj as Handle);
}

pub unsafe extern "C" fn jni_IsSameObject(_env: usize, a: usize, b: usize) -> u8 {
    if let Some(v) = crate::broker::worker_slot(
        crate::broker::Op::IsSameObject { a, b },
        "IsSameObject",
    ) {
        return v as u8;
    }
    let ctx = guard!("IsSameObject", false, 0);
    // A cleared weak global reference is treated as `NULL`; this is the JNI
    // idiom `IsSameObject(env, weak, NULL)` for testing for collection.
    let rt = ctx.rt.clone();
    let vm: &Vm = ctx.vm();
    let resolve = |h: usize| -> Option<Handle> {
        let h = h as Handle;
        if h == NULL {
            return None;
        }
        if refs::is_weak_global(&rt, h) {
            if refs::weak_is_live(vm, &rt, h) {
                Some(h)
            } else {
                None
            }
        } else {
            Some(h)
        }
    };
    (resolve(a) == resolve(b)) as u8
}

pub unsafe extern "C" fn jni_NewLocalRef(_env: usize, obj: usize) -> usize {
    let mut ctx = guard!("NewLocalRef", false, 0);
    if obj == 0 {
        return 0;
    }
    let h = or_fail!(
        ctx,
        0,
        live(&mut ctx, obj as Handle, "jobject", "NewLocalRef")
    );
    local(&mut ctx, h) as usize
}

pub unsafe extern "C" fn jni_NewWeakGlobalRef(_env: usize, obj: usize) -> usize {
    let mut ctx = guard!("NewWeakGlobalRef", false, 0);
    if obj == 0 {
        return 0;
    }
    let h = or_fail!(
        ctx,
        0,
        live(&mut ctx, obj as Handle, "jobject", "NewWeakGlobalRef")
    );
    refs::weak_global(&ctx.rt, h) as usize
}

pub unsafe extern "C" fn jni_DeleteWeakGlobalRef(_env: usize, obj: usize) {
    let Some(ctx) = enter("DeleteWeakGlobalRef", true) else {
        return;
    };
    refs::delete_weak_global(&ctx.rt, obj as Handle);
}

pub unsafe extern "C" fn jni_EnsureLocalCapacity(_env: usize, capacity: i32) -> i32 {
    let mut ctx = guard!("EnsureLocalCapacity", false, JNI_ERR);
    if !(0..=crate::state::MAX_LOCAL_CAPACITY).contains(&capacity) {
        let _ = throw_class(
            &mut ctx,
            "Ljava/lang/OutOfMemoryError;",
            "EnsureLocalCapacity: unsatisfiable local reference capacity",
        );
        return JNI_ERR;
    }
    JNI_OK
}

// ---------------------------------------------------------------------------
// Methods / fields IDs
// ---------------------------------------------------------------------------

fn method_info(ctx: &mut Ctx, mid: usize) -> Option<MethodInfo> {
    let rt = ctx.rt.borrow();
    rt.methods
        .iter()
        .find(|m| &***m as *const MethodInfo as usize == mid)
        .map(|m| (**m).clone())
}

fn valid_descriptor(d: &str) -> bool {
    match d.as_bytes().first().copied() {
        Some(b'Z') | Some(b'B') | Some(b'C') | Some(b'S') | Some(b'I') | Some(b'J')
        | Some(b'F') | Some(b'D') => d.len() == 1,
        Some(b'L') => d.len() > 2 && d.ends_with(';'),
        Some(b'[') => valid_descriptor(&d[1..]),
        _ => false,
    }
}

fn arg_codes(params: &[String]) -> CString {
    let mut codes = Vec::with_capacity(params.len());
    for p in params {
        let c = match p.as_bytes().first().copied() {
            Some(b'L') | Some(b'[') => b'L',
            Some(b'J') => b'J',
            Some(b'F') => b'F',
            Some(b'D') => b'D',
            Some(b'Z') => b'Z',
            Some(b'B') => b'B',
            Some(b'C') => b'C',
            Some(b'S') => b'S',
            _ => b'I',
        };
        codes.push(c);
    }
    CString::new(codes).unwrap_or_default()
}

fn make_method_id(
    ctx: &mut Ctx,
    cid: ClassId,
    name: &str,
    sig: &str,
    is_static: bool,
) -> Option<usize> {
    let (params, ret) = match acos_vm::class::parse_signature(sig) {
        Ok(v)
            if v.0.iter().all(|p| valid_descriptor(p))
                && (v.1 == "V" || valid_descriptor(&v.1)) =>
        {
            v
        }
        _ => {
            record(
                ctx,
                JniError::BadArgument {
                    function: "GetMethodID".into(),
                    message: format!("bad signature {sig}"),
                },
            );
            return None;
        }
    };
    let vm = ctx.vm();
    let class = vm.class_desc(cid).to_string();
    let kind = if is_static {
        InvokeKind::Static
    } else {
        InvokeKind::Virtual
    };
    let found = vm.find_method(cid, name, sig, kind).or_else(|_| {
        if is_static {
            Err(())
        } else {
            vm.find_method(cid, name, sig, InvokeKind::Direct)
                .map_err(|_| ())
        }
    });
    let _mid: MethodId = match found {
        Ok(m) => m,
        Err(_) => {
            let _ = throw_class(
                &mut *ctx,
                "Ljava/lang/NoSuchMethodError;",
                &format!("{class}->{name}{sig}"),
            );
            return None;
        }
    };
    let info = MethodInfo {
        class,
        name: name.to_string(),
        sig: sig.to_string(),
        is_static,
        arg_codes: arg_codes(&params),
        param_descs: params,
        ret_desc: ret,
    };
    let mut st = ctx.rt.borrow_mut();
    st.methods.push(Box::new(info));
    let idx = st.methods.len() - 1;
    Some(&*st.methods[idx] as *const MethodInfo as usize)
}

pub unsafe extern "C" fn jni_GetMethodID(
    _env: usize,
    cls: usize,
    name: *const c_char,
    sig: *const c_char,
) -> usize {
    if let Some(v) = crate::broker::worker_get_method_id(cls, cstr(name).as_bytes(), cstr(sig).as_bytes(), false) {
        return v as usize;
    }
    let mut ctx = guard!("GetMethodID", false, 0);
    let cid = or_fail!(ctx, 0, class_id(&mut ctx, cls as Handle, "GetMethodID"));
    let name = cstr(name);
    let sig = cstr(sig);
    make_method_id(&mut ctx, cid, &name, &sig, false).unwrap_or(0)
}

pub unsafe extern "C" fn jni_GetStaticMethodID(
    _env: usize,
    cls: usize,
    name: *const c_char,
    sig: *const c_char,
) -> usize {
    if let Some(v) = crate::broker::worker_get_method_id(cls, cstr(name).as_bytes(), cstr(sig).as_bytes(), true) {
        return v as usize;
    }
    let mut ctx = guard!("GetStaticMethodID", false, 0);
    let cid = or_fail!(
        ctx,
        0,
        class_id(&mut ctx, cls as Handle, "GetStaticMethodID")
    );
    if !ensure_static_init(&mut ctx, cid, "GetStaticMethodID") {
        return 0;
    }
    let name = cstr(name);
    let sig = cstr(sig);
    make_method_id(&mut ctx, cid, &name, &sig, true).unwrap_or(0)
}

fn find_field(
    vm: &Vm,
    cid: ClassId,
    name: &str,
    desc: &str,
    is_static: bool,
) -> Option<(ClassId, String)> {
    let mut cur = Some(cid);
    while let Some(c) = cur {
        let cl = &vm.classes[c as usize];
        let list = if is_static {
            &cl.declared_static_fields
        } else {
            &cl.declared_instance_fields
        };
        if list.iter().any(|f| f.name == name && f.desc == desc) {
            return Some((c, cl.descriptor.clone()));
        }
        cur = cl.super_class;
    }
    None
}

fn make_field_id(
    ctx: &mut Ctx,
    cid: ClassId,
    name: &str,
    desc: &str,
    is_static: bool,
) -> Option<usize> {
    let vm = ctx.vm();
    let Some((_, class)) = find_field(vm, cid, name, desc, is_static) else {
        let start = vm.class_desc(cid).to_string();
        let _ = throw_class(
            &mut *ctx,
            "Ljava/lang/NoSuchFieldError;",
            &format!("{start}.{name}:{desc}"),
        );
        return None;
    };
    let info = FieldInfo {
        class,
        name: name.to_string(),
        desc: desc.to_string(),
        is_static,
    };
    let mut st = ctx.rt.borrow_mut();
    st.fields.push(Box::new(info));
    let idx = st.fields.len() - 1;
    Some(&*st.fields[idx] as *const FieldInfo as usize)
}

pub unsafe extern "C" fn jni_GetFieldID(
    _env: usize,
    cls: usize,
    name: *const c_char,
    sig: *const c_char,
) -> usize {
    if let Some(v) = crate::broker::worker_get_field_id(cls, cstr(name).as_bytes(), cstr(sig).as_bytes(), false) {
        return v as usize;
    }
    let mut ctx = guard!("GetFieldID", false, 0);
    let cid = or_fail!(ctx, 0, class_id(&mut ctx, cls as Handle, "GetFieldID"));
    let name = cstr(name);
    let sig = cstr(sig);
    make_field_id(&mut ctx, cid, &name, &sig, false).unwrap_or(0)
}

pub unsafe extern "C" fn jni_GetStaticFieldID(
    _env: usize,
    cls: usize,
    name: *const c_char,
    sig: *const c_char,
) -> usize {
    if let Some(v) = crate::broker::worker_get_field_id(cls, cstr(name).as_bytes(), cstr(sig).as_bytes(), true) {
        return v as usize;
    }
    let mut ctx = guard!("GetStaticFieldID", false, 0);
    let cid = or_fail!(
        ctx,
        0,
        class_id(&mut ctx, cls as Handle, "GetStaticFieldID")
    );
    if !ensure_static_init(&mut ctx, cid, "GetStaticFieldID") {
        return 0;
    }
    let name = cstr(name);
    let sig = cstr(sig);
    make_field_id(&mut ctx, cid, &name, &sig, true).unwrap_or(0)
}

/// `FromReflectedMethod(jobject method)` — a `java.lang.reflect.Method` mirror
/// back to its `jmethodID`.
pub unsafe extern "C" fn jni_FromReflectedMethod(_env: usize, method: usize) -> usize {
    let mut ctx = guard!("FromReflectedMethod", false, 0);
    let h = or_fail!(
        ctx,
        0,
        live(&mut ctx, method as Handle, "java.lang.reflect.Method", "FromReflectedMethod")
    );
    let vm = ctx.vm();
    let Some(mid) = vm.reflected_method_id(h) else {
        let _ = throw_class(
            &mut ctx,
            "Ljava/lang/IllegalArgumentException;",
            "FromReflectedMethod: not a Method/Constructor mirror",
        );
        return 0;
    };
    let (cid, name, sig, is_static) = {
        let m = &vm.methods[mid as usize];
        (m.class, m.name.clone(), m.sig.clone(), m.is_static())
    };
    make_method_id(&mut ctx, cid, &name, &sig, is_static).unwrap_or(0)
}

/// `FromReflectedField(jobject field)` — a `java.lang.reflect.Field` mirror
/// back to its `jfieldID`.
pub unsafe extern "C" fn jni_FromReflectedField(_env: usize, field: usize) -> usize {
    let mut ctx = guard!("FromReflectedField", false, 0);
    let h = or_fail!(
        ctx,
        0,
        live(&mut ctx, field as Handle, "java.lang.reflect.Field", "FromReflectedField")
    );
    let Some((class, name, desc, is_static)) = ctx.vm().reflected_field_parts(h) else {
        let _ = throw_class(
            &mut ctx,
            "Ljava/lang/IllegalArgumentException;",
            "FromReflectedField: not a Field mirror",
        );
        return 0;
    };
    let cid = match ctx.vm().load_class(&class) {
        Ok(cid) => cid,
        Err(e) => {
            record_bridge(&mut ctx, "FromReflectedField", BridgeError::Vm(e));
            return 0;
        }
    };
    make_field_id(&mut ctx, cid, &name, &desc, is_static).unwrap_or(0)
}

/// `ToReflectedMethod(jclass cls, jmethodID methodID, jboolean isStatic)` — a
/// real `java.lang.reflect.Method` mirror for a resolved method ID.
pub unsafe extern "C" fn jni_ToReflectedMethod(
    _env: usize,
    cls: usize,
    mid: usize,
    is_static: u8,
) -> usize {
    let mut ctx = guard!("ToReflectedMethod", false, 0);
    let cid = or_fail!(
        ctx,
        0,
        class_id(&mut ctx, cls as Handle, "ToReflectedMethod")
    );
    let Some(info) = method_info(&mut ctx, mid) else {
        let _ = throw_class(
            &mut ctx,
            "Ljava/lang/IllegalArgumentException;",
            "ToReflectedMethod: bad jmethodID",
        );
        return 0;
    };
    let kind = if is_static != 0 {
        InvokeKind::Static
    } else {
        InvokeKind::Virtual
    };
    let resolved = {
        let vm = ctx.vm();
        vm.find_method(cid, &info.name, &info.sig, kind)
            .or_else(|_| vm.find_method(cid, &info.name, &info.sig, InvokeKind::Direct))
    };
    match resolved {
        Ok(mid) => match ctx.vm().make_reflected_method(cid, mid) {
            Ok(h) => local(&mut ctx, h) as usize,
            Err(e) => {
                record(&ctx, bridge_error("ToReflectedMethod", e));
                0
            }
        },
        Err(_) => {
            let _ = throw_class(
                &mut ctx,
                "Ljava/lang/NoSuchMethodError;",
                &format!("{}{}", info.name, info.sig),
            );
            0
        }
    }
}

/// `ToReflectedField(jclass cls, jfieldID fieldID, jboolean isStatic)` — a real
/// `java.lang.reflect.Field` mirror for a resolved field ID.
pub unsafe extern "C" fn jni_ToReflectedField(
    _env: usize,
    cls: usize,
    fid: usize,
    _is_static: u8,
) -> usize {
    let mut ctx = guard!("ToReflectedField", false, 0);
    let _ = or_fail!(
        ctx,
        0,
        class_id(&mut ctx, cls as Handle, "ToReflectedField")
    );
    let Some(info) = field_info(&mut ctx, fid) else {
        let _ = throw_class(
            &mut ctx,
            "Ljava/lang/IllegalArgumentException;",
            "ToReflectedField: bad jfieldID",
        );
        return 0;
    };
    match ctx.vm().make_reflected_field(&info.class, &info.name, info.is_static) {
        Ok(h) => local(&mut ctx, h) as usize,
        Err(e) => {
            record(&ctx, bridge_error("ToReflectedField", e));
            0
        }
    }
}

fn field_info(ctx: &mut Ctx, fid: usize) -> Option<FieldInfo> {
    let rt = ctx.rt.borrow();
    rt.fields
        .iter()
        .find(|f| &***f as *const FieldInfo as usize == fid)
        .map(|f| (**f).clone())
}

// ---------------------------------------------------------------------------
// Java values ↔ JNI words
// ---------------------------------------------------------------------------

fn value_bits(
    ctx: &mut Ctx,
    value: JavaValue,
    desc: &str,
    function: &str,
) -> Result<u64, JniError> {
    let want = desc.as_bytes().first().copied();
    let got = value.kind();
    let mismatch = || JniError::TypeMismatch {
        function: function.to_string(),
        expected: desc.to_string(),
        got: got.to_string(),
    };
    Ok(match (want, value) {
        (Some(b'Z'), JavaValue::Boolean(v)) => v as u64,
        (Some(b'B'), JavaValue::Byte(v)) => v as u8 as u64,
        (Some(b'C'), JavaValue::Char(v)) => v as u64,
        (Some(b'S'), JavaValue::Short(v)) => v as u16 as u64,
        (Some(b'I'), JavaValue::Int(v)) => v as u32 as u64,
        (Some(b'J'), JavaValue::Long(v)) => v as u64,
        (Some(b'F'), JavaValue::Float(v)) => v.to_bits() as u64,
        (Some(b'D'), JavaValue::Double(v)) => v.to_bits(),
        (Some(b'L') | Some(b'['), JavaValue::Null) => NULL as u64,
        (Some(b'L') | Some(b'['), JavaValue::Object(h)) => {
            let _ = ctx.vm();
            h as u64
        }
        _ => return Err(mismatch()),
    })
}

fn bits_value(ctx: &mut Ctx, desc: &str, bits: u64, function: &str) -> Result<JavaValue, JniError> {
    let vm = ctx.vm();
    Ok(match desc.as_bytes().first().copied() {
        Some(b'Z') => JavaValue::Boolean(bits != 0),
        Some(b'B') => JavaValue::Byte(bits as u8 as i8),
        Some(b'C') => JavaValue::Char(bits as u16),
        Some(b'S') => JavaValue::Short(bits as u16 as i16),
        Some(b'I') => JavaValue::Int(bits as u32 as i32),
        Some(b'J') => JavaValue::Long(bits as i64),
        Some(b'F') => JavaValue::Float(f32::from_bits(bits as u32)),
        Some(b'D') => JavaValue::Double(f64::from_bits(bits)),
        Some(b'L') | Some(b'[') => {
            let h = bits as u32;
            if h != NULL && !vm.heap.is_live(h) {
                return Err(JniError::BadReference {
                    function: function.to_string(),
                    what: "jobject".into(),
                    handle: h,
                });
            }
            if h == NULL {
                JavaValue::Null
            } else {
                JavaValue::Object(h)
            }
        }
        _ => {
            return Err(JniError::TypeMismatch {
                function: function.to_string(),
                expected: "field descriptor".into(),
                got: desc.to_string(),
            })
        }
    })
}

// ---------------------------------------------------------------------------
// Field access
// ---------------------------------------------------------------------------

fn get_field_impl(ctx: &mut Ctx, obj: usize, fid: usize, function: &str) -> Result<u64, JniError> {
    let fi = field_info(ctx, fid).ok_or(JniError::BadReference {
        function: function.to_string(),
        what: "jfieldID".into(),
        handle: fid as u32,
    })?;
    if fi.is_static {
        ctx.vm()
            .get_static_field(&fi.class, &fi.name)
            .map_err(|e| bridge_error(function, e))
            .and_then(|v| value_bits(ctx, v, &fi.desc, function))
    } else {
        let h = live(ctx, obj as Handle, "jobject", function)?;
        ctx.vm()
            .get_field(h, &fi.name)
            .map_err(|e| bridge_error(function, e))
            .and_then(|v| {
                let _ = h;
                value_bits(ctx, v, &fi.desc, function)
            })
    }
}

fn set_field_impl(
    ctx: &mut Ctx,
    obj: usize,
    fid: usize,
    bits: u64,
    function: &str,
) -> Result<(), JniError> {
    let fi = field_info(ctx, fid).ok_or(JniError::BadReference {
        function: function.to_string(),
        what: "jfieldID".into(),
        handle: fid as u32,
    })?;
    let value = bits_value(ctx, &fi.desc, bits, function)?;
    if fi.is_static {
        ctx.vm()
            .set_static_field(&fi.class, &fi.name, value)
            .map_err(|e| bridge_error(function, e))
    } else {
        let h = live(ctx, obj as Handle, "jobject", function)?;
        ctx.vm()
            .set_field(h, &fi.name, value)
            .map_err(|e| bridge_error(function, e))
    }
}

/// Field access for a brokered worker request. Runs on the owner thread with
/// the worker's id forced by [`crate::broker`].
pub(crate) fn broker_get_field(obj: usize, fid: usize) -> u64 {
    let Some(mut ctx) = enter("GetField", false) else {
        return 0;
    };
    match get_field_impl(&mut ctx, obj, fid, "GetField") {
        Ok(bits) => bits,
        Err(e) => {
            record(&ctx, e);
            0
        }
    }
}

/// Field write for a brokered worker request.
pub(crate) fn broker_set_field(obj: usize, fid: usize, bits: u64) {
    let Some(mut ctx) = enter("SetField", false) else {
        return;
    };
    if let Err(e) = set_field_impl(&mut ctx, obj, fid, bits, "SetField") {
        record(&ctx, e);
    }
}

fn bridge_error(function: &str, e: BridgeError) -> JniError {
    match e {
        BridgeError::JavaException { class, message } => JniError::NativeFault {
            message: format!("{function}: pending Java exception {class}: {message}"),
        },
        BridgeError::FieldTypeMismatch {
            declared, provided, ..
        } => JniError::TypeMismatch {
            function: function.to_string(),
            expected: declared,
            got: provided,
        },
        other => JniError::NativeFault {
            message: format!("{function}: {other}"),
        },
    }
}

macro_rules! get_field_slot {
    ($name:ident, $ret:ty, $conv:expr) => {
        pub unsafe extern "C" fn $name(_env: usize, obj: usize, fid: usize) -> $ret {
            // Native OS threads route the read through the owner broker.
            if let Some(bits) = crate::broker::worker_get_field(obj, fid, stringify!($name)) {
                #[allow(clippy::redundant_closure_call)]
                return ($conv)(bits);
            }
            let mut ctx = guard!(stringify!($name), false, 0 as $ret);
            let bits = or_fail!(
                ctx,
                0 as $ret,
                get_field_impl(&mut ctx, obj, fid, stringify!($name))
            );
            #[allow(clippy::redundant_closure_call)]
            let out: $ret = ($conv)(bits);
            out
        }
    };
}

macro_rules! set_field_slot {
    ($name:ident, $arg:ty, $bits:expr) => {
        pub unsafe extern "C" fn $name(_env: usize, obj: usize, fid: usize, value: $arg) -> () {
            let bits: u64 = ($bits)(value);
            // Native OS threads route the write through the owner broker.
            if crate::broker::is_worker() {
                crate::broker::worker_set_field(obj, fid, bits, stringify!($name));
                return;
            }
            let mut ctx = guard!(stringify!($name), false, ());
            if let Err(e) = set_field_impl(&mut ctx, obj, fid, bits, stringify!($name)) {
                record(&ctx, e);
            }
        }
    };
}

get_field_slot!(jni_GetObjectField, usize, |b: u64| b as usize);
get_field_slot!(jni_GetBooleanField, u8, |b: u64| b as u8);
get_field_slot!(jni_GetByteField, i8, |b: u64| b as i8);
get_field_slot!(jni_GetCharField, u16, |b: u64| b as u16);
get_field_slot!(jni_GetShortField, i16, |b: u64| b as i16);
get_field_slot!(jni_GetIntField, i32, |b: u64| b as i32);
get_field_slot!(jni_GetLongField, i64, |b: u64| b as i64);
get_field_slot!(jni_GetFloatField, f32, |b: u64| f32::from_bits(b as u32));
get_field_slot!(jni_GetDoubleField, f64, |b: u64| f64::from_bits(b));

set_field_slot!(jni_SetObjectField, usize, |v: usize| v as u64);
set_field_slot!(jni_SetBooleanField, u8, |v: u8| v as u64);
set_field_slot!(jni_SetByteField, i8, |v: i8| v as u8 as u64);
set_field_slot!(jni_SetCharField, u16, |v: u16| v as u64);
set_field_slot!(jni_SetShortField, i16, |v: i16| v as u16 as u64);
set_field_slot!(jni_SetIntField, i32, |v: i32| v as u32 as u64);
set_field_slot!(jni_SetLongField, i64, |v: i64| v as u64);
set_field_slot!(jni_SetFloatField, f32, |v: f32| v.to_bits() as u64);
set_field_slot!(jni_SetDoubleField, f64, |v: f64| v.to_bits());

// Static variants share the implementation; the `is_static` flag on the
// `jfieldID` decides which storage is used.
get_field_slot!(jni_GetStaticObjectField, usize, |b: u64| b as usize);
get_field_slot!(jni_GetStaticBooleanField, u8, |b: u64| b as u8);
get_field_slot!(jni_GetStaticByteField, i8, |b: u64| b as i8);
get_field_slot!(jni_GetStaticCharField, u16, |b: u64| b as u16);
get_field_slot!(jni_GetStaticShortField, i16, |b: u64| b as i16);
get_field_slot!(jni_GetStaticIntField, i32, |b: u64| b as i32);
get_field_slot!(jni_GetStaticLongField, i64, |b: u64| b as i64);
get_field_slot!(jni_GetStaticFloatField, f32, |b: u64| f32::from_bits(
    b as u32
));
get_field_slot!(jni_GetStaticDoubleField, f64, |b: u64| f64::from_bits(b));

set_field_slot!(jni_SetStaticObjectField, usize, |v: usize| v as u64);
set_field_slot!(jni_SetStaticBooleanField, u8, |v: u8| v as u64);
set_field_slot!(jni_SetStaticByteField, i8, |v: i8| v as u8 as u64);
set_field_slot!(jni_SetStaticCharField, u16, |v: u16| v as u64);
set_field_slot!(jni_SetStaticShortField, i16, |v: i16| v as u16 as u64);
set_field_slot!(jni_SetStaticIntField, i32, |v: i32| v as u32 as u64);
set_field_slot!(jni_SetStaticLongField, i64, |v: i64| v as u64);
set_field_slot!(jni_SetStaticFloatField, f32, |v: f32| v.to_bits() as u64);
set_field_slot!(jni_SetStaticDoubleField, f64, |v: f64| v.to_bits());

// ---------------------------------------------------------------------------
// Strings (modified UTF-8)
// ---------------------------------------------------------------------------

fn decode_mutf8(bytes: &[u8]) -> Vec<u16> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b & 0x80 == 0 {
            out.push(b as u16);
            i += 1;
        } else if b & 0xE0 == 0xC0 && i + 1 < bytes.len() {
            let c = ((b as u16 & 0x1F) << 6) | (bytes[i + 1] as u16 & 0x3F);
            out.push(c);
            i += 2;
        } else if b & 0xF0 == 0xE0 && i + 2 < bytes.len() {
            let c = ((b as u16 & 0x0F) << 12)
                | ((bytes[i + 1] as u16 & 0x3F) << 6)
                | (bytes[i + 2] as u16 & 0x3F);
            out.push(c);
            i += 3;
        } else if b & 0xF8 == 0xF0 && i + 3 < bytes.len() {
            // Lenient: accept standard 4-byte UTF-8 and convert to a surrogate
            // pair. Java's modified UTF-8 never uses this form.
            let cp = (((b as u32 & 0x07) << 18)
                | ((bytes[i + 1] as u32 & 0x3F) << 12)
                | ((bytes[i + 2] as u32 & 0x3F) << 6)
                | (bytes[i + 3] as u32 & 0x3F))
                .saturating_sub(0x1_0000);
            out.push(0xD800 | ((cp >> 10) as u16));
            out.push(0xDC00 | ((cp & 0x3FF) as u16));
            i += 4;
        } else {
            out.push(0xFFFD);
            i += 1;
        }
    }
    out
}

fn encode_mutf8(units: &[u16]) -> Vec<u8> {
    let mut out = Vec::with_capacity(units.len());
    for &c in units {
        if c == 0 {
            out.extend_from_slice(&[0xC0, 0x80]);
        } else if c < 0x80 {
            out.push(c as u8);
        } else if c < 0x800 {
            out.push(0xC0 | (c >> 6) as u8);
            out.push(0x80 | (c & 0x3F) as u8);
        } else {
            out.push(0xE0 | (c >> 12) as u8);
            out.push(0x80 | ((c >> 6) & 0x3F) as u8);
            out.push(0x80 | (c & 0x3F) as u8);
        }
    }
    out
}

fn string_units(ctx: &mut Ctx, h: Handle, function: &str) -> Result<Vec<u16>, JniError> {
    let h = live(ctx, h, "jstring", function)?;
    let vm = ctx.vm();
    vm.str_units(h)
        .map(|u| u.to_vec())
        .ok_or(JniError::TypeMismatch {
            function: function.to_string(),
            expected: "java.lang.String".into(),
            got: "non-string object".into(),
        })
}

fn store_buffer(ctx: &mut Ctx, ptr: usize, buffer: Buffer) {
    let mut st = ctx.rt.borrow_mut();
    st.threads
        .entry(ctx.t)
        .or_default()
        .buffers
        .insert(ptr, buffer);
}

fn take_buffer(ctx: &mut Ctx, ptr: usize) -> Option<Buffer> {
    let mut st = ctx.rt.borrow_mut();
    st.threads
        .get_mut(&ctx.t)
        .and_then(|th| th.buffers.remove(&ptr))
}

pub unsafe extern "C" fn jni_NewString(_env: usize, chars: *const u16, len: i32) -> usize {
    if !chars.is_null() && len >= 0 {
        // SAFETY: the caller supplies `len` UTF-16 code units.
        let units = unsafe { std::slice::from_raw_parts(chars, len as usize) };
        if let Some(v) = crate::broker::worker_new_string(units) {
            return v as usize;
        }
    }
    let mut ctx = guard!("NewString", false, 0);
    if chars.is_null() || len < 0 {
        record(
            &ctx,
            JniError::BadArgument {
                function: "NewString".into(),
                message: "null chars or negative length".into(),
            },
        );
        return 0;
    }
    // SAFETY: the caller supplies `len` UTF-16 code units.
    let units = unsafe { std::slice::from_raw_parts(chars, len as usize) }.to_vec();
    let vm = ctx.vm();
    match vm.new_string_utf16(units) {
        Ok(h) => local(&mut ctx, h) as usize,
        Err(e) => {
            record(
                &ctx,
                JniError::NativeFault {
                    message: e.to_string(),
                },
            );
            0
        }
    }
}

pub unsafe extern "C" fn jni_NewStringUTF(_env: usize, bytes: *const c_char) -> usize {
    if !bytes.is_null() {
        // SAFETY: JNI passes a NUL-terminated modified-UTF-8 string.
        let raw = unsafe { CStr::from_ptr(bytes) }.to_bytes();
        if let Some(v) = crate::broker::worker_new_string_utf(raw) {
            return v as usize;
        }
    }
    let mut ctx = guard!("NewStringUTF", false, 0);
    if bytes.is_null() {
        return 0;
    }
    // SAFETY: JNI passes a NUL-terminated modified-UTF-8 string.
    let raw = unsafe { CStr::from_ptr(bytes) }.to_bytes();
    let units = decode_mutf8(raw);
    let vm = ctx.vm();
    match vm.new_string_utf16(units) {
        Ok(h) => local(&mut ctx, h) as usize,
        Err(e) => {
            record(
                &ctx,
                JniError::NativeFault {
                    message: e.to_string(),
                },
            );
            0
        }
    }
}

pub unsafe extern "C" fn jni_GetStringLength(_env: usize, s: usize) -> i32 {
    if let Some(v) = crate::broker::worker_slot(
        crate::broker::Op::GetStringLength { s },
        "GetStringLength",
    ) {
        return v as i32;
    }
    let mut ctx = guard!("GetStringLength", false, 0);
    let units = or_fail!(
        ctx,
        0,
        string_units(&mut ctx, s as Handle, "GetStringLength")
    );
    units.len() as i32
}

pub unsafe extern "C" fn jni_GetStringChars(_env: usize, s: usize, is_copy: *mut u8) -> *const u16 {
    let mut ctx = guard!("GetStringChars", false, std::ptr::null());
    if !is_copy.is_null() {
        unsafe { *is_copy = 1 };
    }
    let units = or_fail!(
        ctx,
        std::ptr::null(),
        string_units(&mut ctx, s as Handle, "GetStringChars")
    );
    let boxed: Box<[u16]> = units.into_boxed_slice();
    let ptr = boxed.as_ptr();
    store_buffer(&mut ctx, ptr as usize, Buffer::Utf16(boxed));
    ptr
}

pub unsafe extern "C" fn jni_ReleaseStringChars(_env: usize, _s: usize, chars: *const u16) {
    let Some(mut ctx) = enter("ReleaseStringChars", true) else {
        return;
    };
    let _ = take_buffer(&mut ctx, chars as usize);
}

pub unsafe extern "C" fn jni_GetStringCritical(
    _env: usize,
    s: usize,
    is_copy: *mut u8,
) -> *const u16 {
    let mut ctx = guard!("GetStringCritical", false, std::ptr::null());
    if !is_copy.is_null() {
        // ACOS always materialises a copy; JNI_GetStringCritical may report
        // either, and a copy is the safe choice for a synthetic heap.
        unsafe { *is_copy = 1 };
    }
    let units = or_fail!(
        ctx,
        std::ptr::null(),
        string_units(&mut ctx, s as Handle, "GetStringCritical")
    );
    let boxed: Box<[u16]> = units.into_boxed_slice();
    let ptr = boxed.as_ptr();
    store_buffer(&mut ctx, ptr as usize, Buffer::Utf16(boxed));
    ptr
}

pub unsafe extern "C" fn jni_ReleaseStringCritical(_env: usize, _s: usize, chars: *const u16) {
    let Some(mut ctx) = enter("ReleaseStringCritical", true) else {
        return;
    };
    let _ = take_buffer(&mut ctx, chars as usize);
}

pub unsafe extern "C" fn jni_GetStringUTFChars(
    _env: usize,
    s: usize,
    is_copy: *mut u8,
) -> *const c_char {
    if let Some(v) = crate::broker::worker_slot(
        crate::broker::Op::GetStringUtfChars { s },
        "GetStringUTFChars",
    ) {
        if !is_copy.is_null() {
            // SAFETY: JNI requires a writable `jboolean*`.
            unsafe { *is_copy = 1 };
        }
        return v as *const c_char;
    }
    let mut ctx = guard!("GetStringUTFChars", false, std::ptr::null());
    if !is_copy.is_null() {
        unsafe { *is_copy = 1 };
    }
    let units = or_fail!(
        ctx,
        std::ptr::null(),
        string_units(&mut ctx, s as Handle, "GetStringUTFChars")
    );
    let mut bytes = encode_mutf8(&units);
    bytes.push(0);
    let boxed: Box<[u8]> = bytes.into_boxed_slice();
    let ptr = boxed.as_ptr() as *const c_char;
    store_buffer(&mut ctx, ptr as usize, Buffer::Utf8(boxed));
    ptr
}

pub unsafe extern "C" fn jni_GetStringUTFLength(_env: usize, s: usize) -> i32 {
    let mut ctx = guard!("GetStringUTFLength", false, 0);
    let units = or_fail!(
        ctx,
        0,
        string_units(&mut ctx, s as Handle, "GetStringUTFLength")
    );
    encode_mutf8(&units).len() as i32
}

pub unsafe extern "C" fn jni_ReleaseStringUTFChars(_env: usize, s: usize, chars: *const c_char) {
    if crate::broker::worker_slot(
        crate::broker::Op::ReleaseStringUtfChars { s, chars: chars as usize },
        "ReleaseStringUTFChars",
    )
    .is_some()
    {
        return;
    }
    let Some(mut ctx) = enter("ReleaseStringUTFChars", true) else {
        return;
    };
    let _ = take_buffer(&mut ctx, chars as usize);
}

pub unsafe extern "C" fn jni_GetStringRegion(
    _env: usize,
    s: usize,
    start: i32,
    len: i32,
    buf: *mut u16,
) {
    let Some(mut ctx) = enter("GetStringRegion", false) else {
        return;
    };
    let units = or_bail!(ctx, string_units(&mut ctx, s as Handle, "GetStringRegion"));
    if start < 0 || len < 0 || (start + len) as usize > units.len() {
        let _ = throw_class(
            &mut ctx,
            "Ljava/lang/StringIndexOutOfBoundsException;",
            "GetStringRegion",
        );
        return;
    }
    if buf.is_null() || len == 0 {
        return;
    }
    // SAFETY: the caller supplies a buffer of `len` code units.
    unsafe { std::ptr::copy_nonoverlapping(units[start as usize..].as_ptr(), buf, len as usize) };
}

pub unsafe extern "C" fn jni_GetStringUTFRegion(
    _env: usize,
    s: usize,
    start: i32,
    len: i32,
    buf: *mut c_char,
) {
    let Some(mut ctx) = enter("GetStringUTFRegion", false) else {
        return;
    };
    let units = or_bail!(
        ctx,
        string_units(&mut ctx, s as Handle, "GetStringUTFRegion")
    );
    if start < 0 || len < 0 || (start + len) as usize > units.len() {
        let _ = throw_class(
            &mut ctx,
            "Ljava/lang/StringIndexOutOfBoundsException;",
            "GetStringUTFRegion",
        );
        return;
    }
    if buf.is_null() || len == 0 {
        return;
    }
    let bytes = encode_mutf8(&units[start as usize..(start + len) as usize]);
    // SAFETY: the caller supplies a buffer large enough for the region.
    unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr() as *const c_char, buf, bytes.len()) };
}

// ---------------------------------------------------------------------------
// Arrays
// ---------------------------------------------------------------------------

fn array_desc(ctx: &mut Ctx, array: Handle, function: &str) -> Result<String, JniError> {
    let array = live(ctx, array, "jarray", function)?;
    let vm = ctx.vm();
    match vm.heap.get(array) {
        Some(Object::Array(a)) => Ok(a.elem_desc.clone()),
        _ => Err(JniError::BadReference {
            function: function.to_string(),
            what: "jarray".into(),
            handle: array,
        }),
    }
}

fn read_region(
    ctx: &mut Ctx,
    array: Handle,
    start: usize,
    len: usize,
    desc: &str,
    function: &str,
) -> Result<Vec<u64>, JniError> {
    let vm = ctx.vm();
    let alen = vm
        .array_length(array)
        .map_err(|e| bridge_error(function, e))?;
    if start > alen || len > alen - start {
        return Err(JniError::BadArgument {
            function: function.to_string(),
            message: format!("region {start}..{} of length {alen}", start + len),
        });
    }
    let mut out = Vec::with_capacity(len);
    for i in start..start + len {
        let v = vm
            .get_array_element(array, i)
            .map_err(|e| bridge_error(function, e))?;
        out.push(value_bits(ctx, v, desc, function)?);
    }
    Ok(out)
}

fn write_region(
    ctx: &mut Ctx,
    array: Handle,
    start: usize,
    words: &[u64],
    desc: &str,
    function: &str,
) -> Result<(), JniError> {
    let vm = ctx.vm();
    let alen = vm
        .array_length(array)
        .map_err(|e| bridge_error(function, e))?;
    if start > alen || words.len() > alen - start {
        return Err(JniError::BadArgument {
            function: function.to_string(),
            message: format!("region {start}..{} of length {alen}", start + words.len()),
        });
    }
    for (i, w) in words.iter().enumerate() {
        let value = bits_value(ctx, desc, *w, function)?;
        ctx.vm()
            .set_array_element(array, start + i, value)
            .map_err(|e| bridge_error(function, e))?;
    }
    Ok(())
}

pub unsafe extern "C" fn jni_GetArrayLength(_env: usize, array: usize) -> i32 {
    let mut ctx = guard!("GetArrayLength", false, 0);
    let array = or_fail!(
        ctx,
        0,
        live(&mut ctx, array as Handle, "jarray", "GetArrayLength")
    );
    let vm = ctx.vm();
    vm.array_length(array)
        .map(|n| n as i32)
        .unwrap_or_else(|e| {
            record(&ctx, bridge_error("GetArrayLength", e));
            0
        })
}

fn new_array(ctx: &mut Ctx, elem: &str, len: i32, function: &str) -> usize {
    if len < 0 {
        let _ = throw_class(ctx, "Ljava/lang/NegativeArraySizeException;", function);
        return 0;
    }
    let vm = ctx.vm();
    match vm.new_array_empty(elem, len as usize) {
        Ok(h) => local(ctx, h) as usize,
        Err(e) => {
            record(
                ctx,
                JniError::NativeFault {
                    message: e.to_string(),
                },
            );
            0
        }
    }
}

macro_rules! new_array_slot {
    ($name:ident, $elem:literal) => {
        pub unsafe extern "C" fn $name(_env: usize, len: i32) -> usize {
            let mut ctx = guard!(stringify!($name), false, 0);
            new_array(&mut ctx, $elem, len, stringify!($name))
        }
    };
}

new_array_slot!(jni_NewBooleanArray, "Z");
new_array_slot!(jni_NewByteArray, "B");
new_array_slot!(jni_NewCharArray, "C");
new_array_slot!(jni_NewShortArray, "S");
new_array_slot!(jni_NewIntArray, "I");
new_array_slot!(jni_NewLongArray, "J");
new_array_slot!(jni_NewFloatArray, "F");
new_array_slot!(jni_NewDoubleArray, "D");

pub unsafe extern "C" fn jni_NewObjectArray(
    _env: usize,
    len: i32,
    cls: usize,
    init: usize,
) -> usize {
    let mut ctx = guard!("NewObjectArray", false, 0);
    let cid = or_fail!(ctx, 0, class_id(&mut ctx, cls as Handle, "NewObjectArray"));
    let elem = ctx.vm().class_desc(cid).to_string();
    let array = new_array(&mut ctx, &elem, len, "NewObjectArray");
    if array != 0 && init != 0 {
        let h = init as Handle;
        for i in 0..len as usize {
            let value = match bits_value(&mut ctx, &elem, h as u64, "NewObjectArray") {
                Ok(v) => v,
                Err(e) => {
                    record(&ctx, e);
                    return array;
                }
            };
            if let Err(e) = ctx.vm().set_array_element(array as Handle, i, value) {
                record(&ctx, bridge_error("NewObjectArray", e));
                break;
            }
        }
    }
    array
}

fn array_elements(ctx: &mut Ctx, array: usize, kind: &str) -> usize {
    let function = "GetArrayElements";
    let h = match live(ctx, array as Handle, "jarray", function) {
        Ok(h) => h,
        Err(e) => {
            record(ctx, e);
            return 0;
        }
    };
    let desc = match array_desc(ctx, h, function) {
        Ok(d) => d,
        Err(e) => {
            record(ctx, e);
            return 0;
        }
    };
    let len = match ctx.vm().array_length(h) {
        Ok(n) => n,
        Err(e) => {
            record(ctx, bridge_error(function, e));
            return 0;
        }
    };
    let words = match read_region(ctx, h, 0, len, &desc, function) {
        Ok(w) => w,
        Err(e) => {
            record(ctx, e);
            return 0;
        }
    };
    let (buffer, ptr) = match kind {
        "Z" => {
            let v: Vec<u8> = words.iter().map(|w| *w as u8).collect();
            let ptr = v.as_ptr() as usize;
            (Buffer::Bool(v), ptr)
        }
        "B" => {
            let v: Vec<i8> = words.iter().map(|w| *w as i8).collect();
            let ptr = v.as_ptr() as usize;
            (Buffer::Byte(v), ptr)
        }
        "C" => {
            let v: Vec<u16> = words.iter().map(|w| *w as u16).collect();
            let ptr = v.as_ptr() as usize;
            (Buffer::Char(v), ptr)
        }
        "S" => {
            let v: Vec<i16> = words.iter().map(|w| *w as i16).collect();
            let ptr = v.as_ptr() as usize;
            (Buffer::Short(v), ptr)
        }
        "I" => {
            let v: Vec<i32> = words.iter().map(|w| *w as i32).collect();
            let ptr = v.as_ptr() as usize;
            (Buffer::Int(v), ptr)
        }
        "J" => {
            let v: Vec<i64> = words.iter().map(|w| *w as i64).collect();
            let ptr = v.as_ptr() as usize;
            (Buffer::Long(v), ptr)
        }
        "F" => {
            let v: Vec<f32> = words.iter().map(|w| f32::from_bits(*w as u32)).collect();
            let ptr = v.as_ptr() as usize;
            (Buffer::Float(v), ptr)
        }
        _ => {
            let v: Vec<f64> = words.iter().map(|w| f64::from_bits(*w)).collect();
            let ptr = v.as_ptr() as usize;
            (Buffer::Double(v), ptr)
        }
    };
    store_buffer(ctx, ptr, buffer);
    ptr
}

macro_rules! elements_slot {
    ($name:ident, $kind:literal, $ret:ty) => {
        pub unsafe extern "C" fn $name(_env: usize, array: usize, is_copy: *mut u8) -> *mut $ret {
            let mut ctx = guard!(stringify!($name), false, std::ptr::null_mut());
            if !is_copy.is_null() {
                unsafe { *is_copy = 1 };
            }
            array_elements(&mut ctx, array, $kind) as *mut $ret
        }
    };
}

elements_slot!(jni_GetBooleanArrayElements, "Z", u8);
elements_slot!(jni_GetByteArrayElements, "B", i8);
elements_slot!(jni_GetCharArrayElements, "C", u16);
elements_slot!(jni_GetShortArrayElements, "S", i16);
elements_slot!(jni_GetIntArrayElements, "I", i32);
elements_slot!(jni_GetLongArrayElements, "J", i64);
elements_slot!(jni_GetFloatArrayElements, "F", f32);
elements_slot!(jni_GetDoubleArrayElements, "D", f64);

fn release_elements(ctx: &mut Ctx, array: usize, elems: usize, mode: i32, kind: &str) {
    let Some(buf) = take_buffer(ctx, elems) else {
        return;
    };
    let h = array as Handle;
    if mode != 2 {
        // Copy back. `JNI_ABORT` (2) discards, `0` commits and frees,
        // `JNI_COMMIT` (1) commits but leaves the buffer valid.
        let words: Vec<u64> = match &buf {
            Buffer::Bool(v) => v.iter().map(|x| *x as u64).collect(),
            Buffer::Byte(v) => v.iter().map(|x| *x as u8 as u64).collect(),
            Buffer::Char(v) => v.iter().map(|x| *x as u64).collect(),
            Buffer::Short(v) => v.iter().map(|x| *x as u16 as u64).collect(),
            Buffer::Int(v) => v.iter().map(|x| *x as u32 as u64).collect(),
            Buffer::Long(v) => v.iter().map(|x| *x as u64).collect(),
            Buffer::Float(v) => v.iter().map(|x| x.to_bits() as u64).collect(),
            Buffer::Double(v) => v.iter().map(|x| x.to_bits()).collect(),
            other => {
                record(
                    ctx,
                    JniError::TypeMismatch {
                        function: "ReleaseArrayElements".into(),
                        expected: kind.to_string(),
                        got: other.kind().to_string(),
                    },
                );
                return;
            }
        };
        if let Err(e) = write_region(ctx, h, 0, &words, kind, "ReleaseArrayElements") {
            record(ctx, e);
        }
    }
    if mode == 1 {
        store_buffer(ctx, elems, buf);
    }
}

macro_rules! release_elements_slot {
    ($name:ident, $kind:literal, $ptr:ty) => {
        pub unsafe extern "C" fn $name(_env: usize, array: usize, elems: *mut $ptr, mode: i32) {
            let mut ctx = guard!(stringify!($name), true, ());
            release_elements(&mut ctx, array, elems as usize, mode, $kind);
        }
    };
}

release_elements_slot!(jni_ReleaseBooleanArrayElements, "Z", u8);
release_elements_slot!(jni_ReleaseByteArrayElements, "B", i8);
release_elements_slot!(jni_ReleaseCharArrayElements, "C", u16);
release_elements_slot!(jni_ReleaseShortArrayElements, "S", i16);
release_elements_slot!(jni_ReleaseIntArrayElements, "I", i32);
release_elements_slot!(jni_ReleaseLongArrayElements, "J", i64);
release_elements_slot!(jni_ReleaseFloatArrayElements, "F", f32);
release_elements_slot!(jni_ReleaseDoubleArrayElements, "D", f64);

fn region_in_bounds(ctx: &mut Ctx, array: Handle, start: i32, len: i32, function: &str) -> bool {
    let vm = ctx.vm();
    match vm.array_length(array) {
        Ok(n)
            if start >= 0
                && len >= 0
                && (start as usize)
                    .checked_add(len as usize)
                    .map(|end| end <= n)
                    .unwrap_or(false) =>
        {
            true
        }
        _ => {
            // JNI specifies `ArrayIndexOutOfBoundsException` for an
            // out-of-range `Get/Set<Type>ArrayRegion`.
            let _ = throw_class(ctx, "Ljava/lang/ArrayIndexOutOfBoundsException;", function);
            false
        }
    }
}

macro_rules! region_get_slot {
    ($name:ident, $kind:literal, $ptr:ty, $conv:expr) => {
        pub unsafe extern "C" fn $name(
            _env: usize,
            array: usize,
            start: i32,
            len: i32,
            buf: *mut $ptr,
        ) {
            let mut ctx = guard!(stringify!($name), false, ());
            let h = or_bail!(
                ctx,
                live(&mut ctx, array as Handle, "jarray", stringify!($name))
            );
            if !region_in_bounds(&mut ctx, h, start, len, stringify!($name)) {
                return;
            }
            let words = or_bail!(
                ctx,
                read_region(
                    &mut ctx,
                    h,
                    start as usize,
                    len as usize,
                    $kind,
                    stringify!($name)
                )
            );
            if buf.is_null() || len == 0 {
                return;
            }
            for (i, w) in words.iter().enumerate() {
                let v: $ptr = ($conv)(*w);
                // SAFETY: the caller provides `len` elements.
                unsafe { *buf.add(i) = v };
            }
        }
    };
}

region_get_slot!(jni_GetBooleanArrayRegion, "Z", u8, |b: u64| b as u8);
region_get_slot!(jni_GetByteArrayRegion, "B", i8, |b: u64| b as i8);
region_get_slot!(jni_GetCharArrayRegion, "C", u16, |b: u64| b as u16);
region_get_slot!(jni_GetShortArrayRegion, "S", i16, |b: u64| b as i16);
region_get_slot!(jni_GetIntArrayRegion, "I", i32, |b: u64| b as i32);
region_get_slot!(jni_GetLongArrayRegion, "J", i64, |b: u64| b as i64);
region_get_slot!(jni_GetFloatArrayRegion, "F", f32, |b: u64| f32::from_bits(
    b as u32
));
region_get_slot!(jni_GetDoubleArrayRegion, "D", f64, |b: u64| f64::from_bits(
    b
));

macro_rules! region_set_slot {
    ($name:ident, $kind:literal, $ptr:ty, $conv:expr) => {
        pub unsafe extern "C" fn $name(
            _env: usize,
            array: usize,
            start: i32,
            len: i32,
            buf: *const $ptr,
        ) {
            let mut ctx = guard!(stringify!($name), false, ());
            let h = or_bail!(
                ctx,
                live(&mut ctx, array as Handle, "jarray", stringify!($name))
            );
            if !region_in_bounds(&mut ctx, h, start, len, stringify!($name)) {
                return;
            }
            if buf.is_null() && len > 0 {
                record(
                    &ctx,
                    JniError::NullReference {
                        function: stringify!($name).into(),
                        what: "buffer".into(),
                    },
                );
                return;
            }
            let mut words = Vec::with_capacity(len as usize);
            for i in 0..len as usize {
                // SAFETY: the caller provides `len` elements.
                let v = unsafe { *buf.add(i) };
                words.push(($conv)(v));
            }
            if let Err(e) = write_region(
                &mut ctx,
                h,
                start as usize,
                &words,
                $kind,
                stringify!($name),
            ) {
                record(&ctx, e);
            }
        }
    };
}

region_set_slot!(jni_SetBooleanArrayRegion, "Z", u8, |v: u8| v as u64);
region_set_slot!(jni_SetByteArrayRegion, "B", i8, |v: i8| v as u8 as u64);
region_set_slot!(jni_SetCharArrayRegion, "C", u16, |v: u16| v as u64);
region_set_slot!(jni_SetShortArrayRegion, "S", i16, |v: i16| v as u16 as u64);
region_set_slot!(jni_SetIntArrayRegion, "I", i32, |v: i32| v as u32 as u64);
region_set_slot!(jni_SetLongArrayRegion, "J", i64, |v: i64| v as u64);
region_set_slot!(jni_SetFloatArrayRegion, "F", f32, |v: f32| v.to_bits()
    as u64);
region_set_slot!(jni_SetDoubleArrayRegion, "D", f64, |v: f64| v.to_bits());

pub unsafe extern "C" fn jni_GetObjectArrayElement(_env: usize, array: usize, index: i32) -> usize {
    let mut ctx = guard!("GetObjectArrayElement", false, 0);
    let h = or_fail!(
        ctx,
        0,
        live(&mut ctx, array as Handle, "jarray", "GetObjectArrayElement")
    );
    if index < 0 {
        let _ = throw_class(
            &mut ctx,
            "Ljava/lang/ArrayIndexOutOfBoundsException;",
            "GetObjectArrayElement",
        );
        return 0;
    }
    let vm = ctx.vm();
    match vm.get_array_element(h, index as usize) {
        Ok(JavaValue::Object(v)) => local(&mut ctx, v) as usize,
        Ok(JavaValue::Null) => 0,
        Ok(other) => {
            record(
                &ctx,
                JniError::TypeMismatch {
                    function: "GetObjectArrayElement".into(),
                    expected: "object[]".into(),
                    got: other.kind().to_string(),
                },
            );
            0
        }
        Err(e) => {
            record(&ctx, bridge_error("GetObjectArrayElement", e));
            0
        }
    }
}

pub unsafe extern "C" fn jni_SetObjectArrayElement(
    _env: usize,
    array: usize,
    index: i32,
    value: usize,
) {
    let Some(mut ctx) = enter("SetObjectArrayElement", false) else {
        return;
    };
    let h = or_bail!(
        ctx,
        live(&mut ctx, array as Handle, "jarray", "SetObjectArrayElement")
    );
    if index < 0 {
        let _ = throw_class(
            &mut ctx,
            "Ljava/lang/ArrayIndexOutOfBoundsException;",
            "SetObjectArrayElement",
        );
        return;
    }
    let v = if value == 0 {
        JavaValue::Null
    } else {
        match live(
            &mut ctx,
            value as Handle,
            "jobject",
            "SetObjectArrayElement",
        ) {
            Ok(x) => JavaValue::Object(x),
            Err(e) => {
                record(&ctx, e);
                return;
            }
        }
    };
    if let Err(e) = ctx.vm().set_array_element(h, index as usize, v) {
        record(&ctx, bridge_error("SetObjectArrayElement", e));
    }
}

pub unsafe extern "C" fn jni_GetPrimitiveArrayCritical(
    _env: usize,
    array: usize,
    is_copy: *mut u8,
) -> *mut c_void {
    let mut ctx = guard!("GetPrimitiveArrayCritical", false, std::ptr::null_mut());
    if !is_copy.is_null() {
        unsafe { *is_copy = 1 };
    }
    let h = or_fail!(
        ctx,
        std::ptr::null_mut(),
        live(
            &mut ctx,
            array as Handle,
            "jarray",
            "GetPrimitiveArrayCritical"
        )
    );
    let desc = or_fail!(
        ctx,
        std::ptr::null_mut(),
        array_desc(&mut ctx, h, "GetPrimitiveArrayCritical")
    );
    let len = or_fail!(
        ctx,
        std::ptr::null_mut(),
        ctx.vm()
            .array_length(h)
            .map_err(|e| bridge_error("GetPrimitiveArrayCritical", e))
    );
    let words = or_fail!(
        ctx,
        std::ptr::null_mut(),
        read_region(&mut ctx, h, 0, len, &desc, "GetPrimitiveArrayCritical")
    );
    let kind = desc.as_bytes().first().copied().unwrap_or(b'I');
    array_elements_from_words(&mut ctx, kind, words)
}

fn array_elements_from_words(ctx: &mut Ctx, kind: u8, words: Vec<u64>) -> *mut c_void {
    let (buffer, ptr) = match kind {
        b'Z' => {
            let v: Vec<u8> = words.iter().map(|w| *w as u8).collect();
            let p = v.as_ptr() as usize;
            (Buffer::Bool(v), p)
        }
        b'B' => {
            let v: Vec<i8> = words.iter().map(|w| *w as i8).collect();
            let p = v.as_ptr() as usize;
            (Buffer::Byte(v), p)
        }
        b'C' => {
            let v: Vec<u16> = words.iter().map(|w| *w as u16).collect();
            let p = v.as_ptr() as usize;
            (Buffer::Char(v), p)
        }
        b'S' => {
            let v: Vec<i16> = words.iter().map(|w| *w as i16).collect();
            let p = v.as_ptr() as usize;
            (Buffer::Short(v), p)
        }
        b'J' => {
            let v: Vec<i64> = words.iter().map(|w| *w as i64).collect();
            let p = v.as_ptr() as usize;
            (Buffer::Long(v), p)
        }
        b'F' => {
            let v: Vec<f32> = words.iter().map(|w| f32::from_bits(*w as u32)).collect();
            let p = v.as_ptr() as usize;
            (Buffer::Float(v), p)
        }
        b'D' => {
            let v: Vec<f64> = words.iter().map(|w| f64::from_bits(*w)).collect();
            let p = v.as_ptr() as usize;
            (Buffer::Double(v), p)
        }
        _ => {
            let v: Vec<i32> = words.iter().map(|w| *w as i32).collect();
            let p = v.as_ptr() as usize;
            (Buffer::Int(v), p)
        }
    };
    store_buffer(ctx, ptr, buffer);
    ptr as *mut c_void
}

pub unsafe extern "C" fn jni_ReleasePrimitiveArrayCritical(
    _env: usize,
    array: usize,
    carray: *mut c_void,
    mode: i32,
) {
    let Some(mut ctx) = enter("ReleasePrimitiveArrayCritical", true) else {
        return;
    };
    let h = array as Handle;
    let desc = match array_desc(&mut ctx, h, "ReleasePrimitiveArrayCritical") {
        Ok(d) => d,
        Err(e) => {
            record(&ctx, e);
            return;
        }
    };
    let kind = desc.as_bytes().first().copied().unwrap_or(b'I');
    release_elements(
        &mut ctx,
        array,
        carray as usize,
        mode,
        match kind {
            b'Z' => "Z",
            b'B' => "B",
            b'C' => "C",
            b'S' => "S",
            b'I' => "I",
            b'J' => "J",
            b'F' => "F",
            _ => "D",
        },
    );
}

// ---------------------------------------------------------------------------
// RegisterNatives / monitors / VM
// ---------------------------------------------------------------------------

#[repr(C)]
#[derive(Debug)]
pub struct JNINativeMethod {
    pub name: *const c_char,
    pub signature: *const c_char,
    pub fn_ptr: *mut c_void,
}

pub unsafe extern "C" fn jni_RegisterNatives(
    _env: usize,
    cls: usize,
    methods: *const JNINativeMethod,
    n_methods: i32,
) -> i32 {
    let mut ctx = guard!("RegisterNatives", false, JNI_ERR);
    let cid = or_fail!(
        ctx,
        JNI_ERR,
        class_id(&mut ctx, cls as Handle, "RegisterNatives")
    );
    let class = ctx.vm().class_desc(cid).to_string();
    if (methods.is_null() && n_methods > 0) || n_methods < 0 {
        record(
            &ctx,
            JniError::BadArgument {
                function: "RegisterNatives".into(),
                message: "null method array or negative count".into(),
            },
        );
        return JNI_ERR;
    }
    // SAFETY: the guest passes `n_methods` JNINativeMethod entries. A zero
    // count is a valid no-op even with a null array.
    let list: &[JNINativeMethod] = if n_methods == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(methods, n_methods as usize) }
    };
    let mut to_insert = Vec::with_capacity(list.len());
    for m in list {
        let name = cstr(m.name);
        let sig = cstr(m.signature);
        let f = m.fn_ptr as usize;
        if f == 0 {
            record(
                &ctx,
                JniError::BadArgument {
                    function: "RegisterNatives".into(),
                    message: format!("{class}->{name}{sig}: null function pointer"),
                },
            );
            return JNI_ERR;
        }
        let vm = ctx.vm();
        let found = vm
            .find_method(cid, &name, &sig, InvokeKind::Virtual)
            .or_else(|_| vm.find_method(cid, &name, &sig, InvokeKind::Direct))
            .or_else(|_| vm.find_method(cid, &name, &sig, InvokeKind::Static));
        if found.is_err() {
            let _ = throw_class(
                &mut ctx,
                "Ljava/lang/NoSuchMethodError;",
                &format!("{class}->{name}{sig}"),
            );
            return JNI_ERR;
        }
        to_insert.push(((class.clone(), name, sig), f));
    }
    let mut st = ctx.rt.borrow_mut();
    for (key, f) in to_insert {
        st.natives.insert(key, f);
    }
    JNI_OK
}

pub unsafe extern "C" fn jni_UnregisterNatives(_env: usize, cls: usize) -> i32 {
    let mut ctx = guard!("UnregisterNatives", false, JNI_ERR);
    let cid = or_fail!(
        ctx,
        JNI_ERR,
        class_id(&mut ctx, cls as Handle, "UnregisterNatives")
    );
    let class = ctx.vm().class_desc(cid).to_string();
    let mut st = ctx.rt.borrow_mut();
    st.natives.retain(|(c, _, _), _| c != &class);
    JNI_OK
}

pub unsafe extern "C" fn jni_GetJavaVM(_env: usize, out: *mut usize) -> i32 {
    let ctx = guard!("GetJavaVM", false, JNI_ERR);
    if out.is_null() {
        record(
            &ctx,
            JniError::NullReference {
                function: "GetJavaVM".into(),
                what: "JavaVM**".into(),
            },
        );
        return JNI_ERR;
    }
    let slot = ctx.rt.borrow().vm_slot;
    // SAFETY: JNI requires a writable `JavaVM*`.
    unsafe { *out = slot };
    JNI_OK
}

pub unsafe extern "C" fn jni_MonitorEnter(_env: usize, obj: usize) -> i32 {
    let mut ctx = guard!("MonitorEnter", false, JNI_ERR);
    let h = or_fail!(
        ctx,
        JNI_ERR,
        live(&mut ctx, obj as Handle, "jobject", "MonitorEnter")
    );
    let t = ctx.t;
    let vm = ctx.vm();
    match vm.monitors.get_mut(&h) {
        Some((owner, count)) if *owner == t => *count += 1,
        Some(_) => {
            record(
                &ctx,
                JniError::NativeFault {
                    message: "MonitorEnter: monitor held by another green thread".into(),
                },
            );
            return JNI_ERR;
        }
        None => {
            vm.monitors.insert(h, (t, 1));
        }
    }
    JNI_OK
}

pub unsafe extern "C" fn jni_MonitorExit(_env: usize, obj: usize) -> i32 {
    let mut ctx = guard!("MonitorExit", false, JNI_ERR);
    let h = or_fail!(
        ctx,
        JNI_ERR,
        live(&mut ctx, obj as Handle, "jobject", "MonitorExit")
    );
    let t = ctx.t;
    let vm = ctx.vm();
    match vm.monitors.get_mut(&h) {
        Some((owner, count)) if *owner == t => {
            if *count > 1 {
                *count -= 1;
            } else {
                vm.monitors.remove(&h);
            }
            JNI_OK
        }
        _ => {
            record(
                &ctx,
                JniError::NativeFault {
                    message: "MonitorExit: monitor not owned by this thread".into(),
                },
            );
            JNI_ERR
        }
    }
}

pub unsafe extern "C" fn jni_GetObjectRefType(_env: usize, obj: usize) -> i32 {
    if let Some(v) = crate::broker::worker_slot(
        crate::broker::Op::GetObjectRefType { obj },
        "GetObjectRefType",
    ) {
        return v as i32;
    }
    let ctx = guard!(
        "GetObjectRefType",
        false,
        crate::state::JNI_INVALID_REF_TYPE
    );
    let h = obj as Handle;
    if h == NULL {
        return crate::state::JNI_INVALID_REF_TYPE;
    }
    if refs::is_global(&ctx.rt, h) {
        crate::state::JNI_GLOBAL_REF_TYPE
    } else if refs::is_weak_global(&ctx.rt, h) {
        crate::state::JNI_WEAK_GLOBAL_REF_TYPE
    } else if refs::is_local(&ctx.rt, ctx.t, h) {
        crate::state::JNI_LOCAL_REF_TYPE
    } else {
        crate::state::JNI_INVALID_REF_TYPE
    }
}

// ---------------------------------------------------------------------------
// Direct byte buffers
// ---------------------------------------------------------------------------

/// `java.nio.DirectByteBuffer` — the real carrier object the JNI contract
/// requires (a marker of any other type fails Java-side assignability).
const DIRECT_BYTE_BUFFER: &str = "Ljava/nio/DirectByteBuffer;";
/// Its `MemoryRef` holds the synthetic address/offset the buffer reads through.
const DIRECT_BYTE_BUFFER_MEMORY_REF: &str = "Ljava/nio/DirectByteBuffer$MemoryRef;";

/// Allocate a real `java.nio.DirectByteBuffer` over `address`/`capacity`.
///
/// ART's `NewDirectByteBuffer` builds the same pair
/// (`MemoryRef(long addr, Object originalBufferObject)` +
/// `DirectByteBuffer(int cap, MemoryRef)`); the resulting object answers
/// `ByteBuffer.get`/`position`/`limit` and is assignable to every Java API that
/// declares a `ByteBuffer` parameter (e.g. `FlutterJNI.handlePlatformMessage`).
/// Only the JNI side table carries the raw address; Java reads through the
/// usual `libcore.io.Memory`/`Unsafe` natives, which bounds-check the region.
fn new_direct_buffer_object(
    ctx: &mut Ctx,
    address: i64,
    capacity: i64,
) -> Result<Handle, JniError> {
    let cap = i32::try_from(capacity).map_err(|_| JniError::BadArgument {
        function: "NewDirectByteBuffer".into(),
        message: format!("capacity {capacity} exceeds the ByteBuffer int limit"),
    })?;
    let vm = ctx.vm();
    let mem = match vm.new_instance_ctor(
        DIRECT_BYTE_BUFFER_MEMORY_REF,
        "(JLjava/lang/Object;)V",
        &[
            acos_vm::JavaValue::Long(address),
            acos_vm::JavaValue::Null,
        ],
    ) {
        Ok(mem) => mem,
        // A bootstrap/partial corelib (no `java.nio`) cannot declare a
        // `ByteBuffer` parameter at all, so a real carrier is impossible. Keep
        // the historic opaque marker: the JNI side table still owns the
        // address/capacity pair, so `GetDirectBufferAddress`/`Capacity` answer
        // exactly as before. Real corelibs (all application VMs) take the
        // branch above and get a genuine `java.nio.DirectByteBuffer`.
        Err(acos_vm::BridgeError::ClassNotFound(_)) => {
            return vm
                .new_string_utf16(Vec::new())
                .map_err(|e| JniError::NativeFault {
                    message: format!("NewDirectByteBuffer: marker: {e}"),
                });
        }
        Err(e) => {
            return Err(JniError::NativeFault {
                message: format!("NewDirectByteBuffer: MemoryRef: {e}"),
            })
        }
    };
    let buffer = vm.new_instance_ctor(
        DIRECT_BYTE_BUFFER,
        "(ILjava/nio/DirectByteBuffer$MemoryRef;)V",
        &[
            acos_vm::JavaValue::Int(cap),
            acos_vm::JavaValue::Object(mem),
        ],
    );
    // The buffer keeps `memoryRef` reachable; the bridge root is not needed.
    vm.release_bridge_root(mem);
    buffer.map_err(|e| JniError::NativeFault {
        message: format!("NewDirectByteBuffer: DirectByteBuffer: {e}"),
    })
}

pub unsafe extern "C" fn jni_NewDirectByteBuffer(
    _env: usize,
    address: usize,
    capacity: i64,
) -> usize {
    let mut ctx = guard!("NewDirectByteBuffer", false, 0);
    if capacity < 0 {
        record(
            &ctx,
            JniError::BadArgument {
                function: "NewDirectByteBuffer".into(),
                message: format!("negative capacity {capacity}"),
            },
        );
        return 0;
    }
    // A zero address means "allocate the backing store"; a non-zero address is
    // a caller-owned region. ACOS reads Java `ByteBuffer`s through its own
    // bounded native-memory substrate (`Unsafe`/`libcore.io.Memory` reject any
    // address that is not a live synthetic region), so a caller-owned region is
    // copied into a fresh synthetic region. `GetDirectBufferAddress` still
    // returns the original pointer so the native owner can keep using it (the
    // Flutter engine wraps its `updateSemantics` buffer this way).
    let (addr, host) = if address == 0 {
        match acos_vm::natives::jni_glue::native_mem_alloc(capacity as usize) {
            Ok(a) => (a, 0i64),
            Err(m) => {
                let _ = throw_class(
                    &mut ctx,
                    "Ljava/lang/OutOfMemoryError;",
                    &format!("NewDirectByteBuffer: {m}"),
                );
                return 0;
            }
        }
    } else if acos_vm::natives::jni_glue::native_mem_check(address as i64, capacity as usize) {
        // Already a synthetic region (a native caller passed back an address it
        // obtained from `GetDirectBufferAddress`).
        (address as i64, 0i64)
    } else {
        let synthetic = match acos_vm::natives::jni_glue::native_mem_alloc(capacity as usize) {
            Ok(a) => a,
            Err(m) => {
                let _ = throw_class(
                    &mut ctx,
                    "Ljava/lang/OutOfMemoryError;",
                    &format!("NewDirectByteBuffer: {m}"),
                );
                return 0;
            }
        };
        if capacity > 0 {
            // SAFETY: the JNI contract says `address` points at `capacity`
            // readable bytes owned by the caller for the duration of this call.
            let bytes =
                unsafe { std::slice::from_raw_parts(address as *const u8, capacity as usize) };
            if let Err(e) = acos_vm::natives::jni_glue::native_mem_write(synthetic, bytes) {
                let _ = throw_class(
                    &mut ctx,
                    "Ljava/lang/IllegalStateException;",
                    &format!("NewDirectByteBuffer: {e}"),
                );
                return 0;
            }
        }
        (synthetic, address as i64)
    };
    let h = match new_direct_buffer_object(&mut ctx, addr, capacity) {
        Ok(h) => h,
        Err(e) => {
            record(&ctx, e);
            return 0;
        }
    };
    let lh = local(&mut ctx, h);
    ctx.rt
        .borrow_mut()
        .direct_buffers
        .insert(lh, (addr, capacity, host));
    lh as usize
}

/// `GetDirectBufferAddress`: the address a JNI-created direct buffer wraps.
///
/// The JNI side table is authoritative for buffers this slot created (the
/// address may be a synthetic `native_mem` region, which Java cannot compute).
/// For a buffer Java allocated itself (`ByteBuffer.allocateDirect`/`map`), read
/// the real `DirectByteBuffer.address` field, exactly like ART — the same
/// representation the `MemoryRef` ctor stores.
///
/// Returns NULL (0) for a non-direct buffer, a freed buffer, or a buffer whose
/// recorded capacity is negative; callers must treat NULL as "not direct".
pub unsafe extern "C" fn jni_GetDirectBufferAddress(_env: usize, obj: usize) -> usize {
    let ctx = guard!("GetDirectBufferAddress", false, 0);
    let h = obj as Handle;
    if h == NULL {
        return 0;
    }
    if let Some((addr, cap, host)) = ctx.rt.borrow().direct_buffers.get(&h).copied() {
        if cap < 0 {
            return 0;
        }
        // For a buffer wrapped over caller-owned native memory, hand the
        // original pointer back; Java reads through the synthetic copy.
        return if host != 0 { host as usize } else { addr as usize };
    }
    // Java-created direct buffer: `DirectByteBuffer.address` is the base the
    // `MemoryRef` computed. `get_field` searches the class hierarchy.
    let vm = ctx.vm();
    if vm.heap.is_live(h) {
        if let Ok(acos_vm::JavaValue::Long(addr)) = vm.get_field(h, "address") {
            if addr != 0 {
                // ACOS stores `VMRuntime.addressOf` — a synthetic array alias —
                // in that field. Native callers dereference the value (the
                // Flutter engine `memcpy`s the pointer packet straight out of
                // it), so resolve the alias to its host backing store. Fall back
                // to the raw value when it is not an ACOS synthetic address
                // (e.g. a buffer over caller-owned memory, which is already a
                // real pointer).
                let capacity = match vm.get_field(h, "capacity") {
                    Ok(acos_vm::JavaValue::Int(cap)) if cap >= 0 => cap as usize,
                    _ => 0,
                };
                if let Some(ptr) =
                    acos_vm::natives::jni_glue::native_mem_host_pointer(vm, addr, capacity)
                {
                    return ptr;
                }
                return addr as usize;
            }
        }
    }
    0
}

/// `GetDirectBufferCapacity`: the buffer's capacity, or `-1` when `obj` is not
/// a direct buffer. Mirrors `GetDirectBufferAddress` (JNI table first, then the
/// Java `capacity` field) so the pair always describes the same buffer.
pub unsafe extern "C" fn jni_GetDirectBufferCapacity(_env: usize, obj: usize) -> i64 {
    let ctx = guard!("GetDirectBufferCapacity", false, -1);
    let h = obj as Handle;
    if h == NULL {
        return -1;
    }
    if let Some((_, cap, _)) = ctx.rt.borrow().direct_buffers.get(&h).copied() {
        return cap;
    }
    let vm = ctx.vm();
    if vm.heap.is_live(h) {
        if let Ok(acos_vm::JavaValue::Int(cap)) = vm.get_field(h, "capacity") {
            return cap as i64;
        }
    }
    -1
}

// ---------------------------------------------------------------------------
// JavaVM invoke interface
// ---------------------------------------------------------------------------

pub unsafe extern "C" fn vm_DestroyJavaVM(_vm: usize) -> i32 {
    JNI_OK
}

pub unsafe extern "C" fn vm_AttachCurrentThread(
    _vm: usize,
    out: *mut usize,
    _args: *mut c_void,
) -> i32 {
    attach_env(out)
}

pub unsafe extern "C" fn vm_AttachCurrentThreadAsDaemon(
    _vm: usize,
    out: *mut usize,
    _args: *mut c_void,
) -> i32 {
    attach_env(out)
}

fn attach_env(out: *mut usize) -> i32 {
    if out.is_null() {
        return JNI_ERR;
    }
    // The owning host thread already has a thread-local runtime.
    if let Some(rt) = runtime_state() {
        let slot = rt.borrow().env_slot;
        // SAFETY: JNI requires a writable `JNIEnv*`.
        unsafe { *out = slot };
        return JNI_OK;
    }
    // A native OS thread (`pthread_create`) attaches through the
    // process-global slots published by `JniRuntime::new`.
    match crate::state::attach_current_os_thread() {
        Some(slot) => {
            // SAFETY: JNI requires a writable `JNIEnv*`.
            unsafe { *out = slot };
            JNI_OK
        }
        None => crate::state::JNI_EDETACHED,
    }
}

pub unsafe extern "C" fn vm_DetachCurrentThread(_vm: usize) -> i32 {
    // The owner thread's attachment is owned by `JniRuntime`; a native thread
    // that attached through the global slots is recorded per OS thread. A
    // brokered worker also asks the owner to release its reference frames.
    if runtime_state().is_none() {
        crate::broker::detach_worker();
        crate::state::detach_current_os_thread();
    }
    JNI_OK
}

pub unsafe extern "C" fn vm_GetEnv(_vm: usize, out: *mut usize, version: i32) -> i32 {
    // A JNI 1.6 VM supports every earlier 1.x request. `jni_lib_merge` in
    // `libreactnative.so` asks for `JNI_VERSION_1_2`; rejecting it made the
    // merged `JNI_OnLoad` return `JNI_ERR`. Accept 1.1 through 1.6.
    let v = version as u32;
    let major = (v >> 16) & 0xffff;
    let minor = v & 0xffff;
    // `minor` is the low 16 bits; compare it against the *minor* of 1.6, not
    // the whole packed constant (0x0001_0006), which would accept any 1.x.
    if major != 1 || minor > (JNI_VERSION_1_6 as u32 & 0xffff) {
        return crate::state::JNI_EVERSION;
    }
    if out.is_null() {
        return JNI_ERR;
    }
    // `GetEnv` never attaches: it reports the current attachment.
    if let Some(rt) = runtime_state() {
        let slot = rt.borrow().env_slot;
        // SAFETY: JNI requires a writable `JNIEnv*`.
        unsafe { *out = slot };
        return JNI_OK;
    }
    if crate::state::current_thread_attached() {
        // SAFETY: JNI requires a writable `JNIEnv*`.
        unsafe { *out = crate::state::global_env_slot() };
        JNI_OK
    } else {
        crate::state::JNI_EDETACHED
    }
}

// ---------------------------------------------------------------------------
// Rust side of the C varargs shims
// ---------------------------------------------------------------------------

/// Descriptor-derived argument codes for `mid` (one byte per parameter).
#[no_mangle]
pub extern "C" fn acos_jni_method_codes(mid: usize) -> *const c_char {
    // A native OS thread cannot read the owner's method table: ask the owner
    // for the stable `arg_codes` pointer it minted with the method ID.
    if crate::broker::is_worker() {
        return crate::broker::worker_slot(
            crate::broker::Op::MethodCodes { mid },
            "Call<Type>Method",
        )
        .unwrap_or(0) as *const c_char;
    }
    let Some(rt) = runtime_state() else {
        return std::ptr::null();
    };
    let st = rt.borrow();
    st.methods
        .iter()
        .find(|m| &***m as *const MethodInfo as usize == mid)
        .map(|m| m.arg_codes.as_ptr())
        .unwrap_or(std::ptr::null())
}

#[repr(C)]
#[derive(Debug)]
pub struct JniCallResult {
    pub bits: u64,
    pub kind: u32,
}

fn zero_result() -> JniCallResult {
    JniCallResult { bits: 0, kind: 0 }
}

fn value_result(value: JavaValue) -> JniCallResult {
    match value {
        JavaValue::Void => zero_result(),
        JavaValue::Boolean(v) => JniCallResult {
            bits: v as u64,
            kind: 0,
        },
        JavaValue::Byte(v) => JniCallResult {
            bits: v as u8 as u64,
            kind: 0,
        },
        JavaValue::Char(v) => JniCallResult {
            bits: v as u64,
            kind: 0,
        },
        JavaValue::Short(v) => JniCallResult {
            bits: v as u16 as u64,
            kind: 0,
        },
        JavaValue::Int(v) => JniCallResult {
            bits: v as u32 as u64,
            kind: 0,
        },
        JavaValue::Long(v) => JniCallResult {
            bits: v as u64,
            kind: 0,
        },
        JavaValue::Float(v) => JniCallResult {
            bits: v.to_bits() as u64,
            kind: 1,
        },
        JavaValue::Double(v) => JniCallResult {
            bits: v.to_bits(),
            kind: 2,
        },
        JavaValue::Object(h) => JniCallResult {
            bits: h as u64,
            kind: 0,
        },
        JavaValue::Null => zero_result(),
    }
}

/// C varargs shim target.
#[no_mangle]
pub unsafe extern "C" fn acos_jni_va_invoke(
    _env: usize,
    target: usize,
    mid: usize,
    mode: i32,
    words: *const u64,
    n: i32,
) -> JniCallResult {
    if crate::broker::is_worker() {
        let raw: Vec<u64> = if n <= 0 || words.is_null() {
            Vec::new()
        } else {
            // SAFETY: the worker shim supplies `n` argument words.
            unsafe { std::slice::from_raw_parts(words, n as usize) }.to_vec()
        };
        return match crate::broker::worker_op(
            crate::broker::Op::CallMethod { target, mid, mode, words: raw },
            "Call<Type>Method",
        ) {
            Some(Ok(bits)) => JniCallResult { bits, kind: 0 },
            Some(Err(e)) => {
                crate::broker::set_worker_error(e);
                zero_result()
            }
            None => JniCallResult { bits: 0, kind: 0 },
        };
    }

    let Some(mut ctx) = enter("Call<Type>Method", false) else {
        return zero_result();
    };
    let Some(info) = method_info(&mut ctx, mid) else {
        record(
            &ctx,
            JniError::BadReference {
                function: "Call<Type>Method".into(),
                what: "jmethodID".into(),
                handle: mid as u32,
            },
        );
        return zero_result();
    };

    if n < 0 || n as usize != info.param_descs.len() {
        record(
            &ctx,
            JniError::BadArgument {
                function: "Call<Type>Method".into(),
                message: format!(
                    "{} arguments supplied for {} params of {}{}",
                    n,
                    info.param_descs.len(),
                    info.name,
                    info.sig
                ),
            },
        );
        return zero_result();
    }
    let raw: Vec<u64> = if n == 0 || words.is_null() {
        Vec::new()
    } else {
        // SAFETY: the C shim passes exactly `n` words.
        unsafe { std::slice::from_raw_parts(words, n as usize) }.to_vec()
    };
    let mut args = Vec::with_capacity(raw.len());
    for (desc, w) in info.param_descs.iter().zip(raw.iter()) {
        match bits_value(&mut ctx, desc, *w, "Call<Type>Method") {
            Ok(v) => args.push(v),
            Err(e) => {
                record(&ctx, e);
                return zero_result();
            }
        }
    }

    let vm = ctx.vm();
    let result = match mode {
        0 => {
            let h = target as Handle;
            if h == NULL || !vm.heap.is_live(h) {
                record(
                    &ctx,
                    JniError::BadReference {
                        function: "Call<Type>Method".into(),
                        what: "jobject".into(),
                        handle: h,
                    },
                );
                return zero_result();
            }
            // JNI `Call<Type>Method` resolves the method by ID; a private
            // method (e.g. `FlutterJNI.updateSemantics`, invoked by the engine
            // to deliver the accessibility tree) is only in the class's
            // direct map, so virtual dispatch misses it. Fall back to direct
            // resolution, exactly like the `GetMethodID` lookup above.
            vm.call_method(h, &info.name, &info.sig, &args)
                .or_else(|e| match e {
                    BridgeError::MethodNotFound { .. } => {
                        vm.call_method_direct(h, &info.name, &info.sig, &args)
                    }
                    other => Err(other),
                })
        }
        1 => vm.call_static(&info.class, &info.name, &info.sig, &args),
        2 => vm
            .new_instance_ctor(&info.class, &info.sig, &args)
            .map(JavaValue::Object),
        _ => {
            record(
                &ctx,
                JniError::BadArgument {
                    function: "Call<Type>Method".into(),
                    message: format!("bad call mode {mode}"),
                },
            );
            return zero_result();
        }
    };

    match result {
        Ok(JavaValue::Object(h)) if mode == 2 => {
            let h = local(&mut ctx, h);
            value_result(JavaValue::Object(h))
        }
        Ok(v) => value_result(v),
        Err(BridgeError::JavaException { class, message }) => {
            let vm = ctx.vm();
            match vm.new_throwable(&class_descriptor(&class), Some(&message)) {
                Ok(h) => {
                    refs::set_pending(vm, &ctx.rt, ctx.t, h);
                }
                Err(e) => record(
                    &ctx,
                    JniError::NativeFault {
                        message: format!("cannot materialise pending exception {class}: {e}"),
                    },
                ),
            }
            zero_result()
        }
        Err(e) => {
            record(&ctx, bridge_error("Call<Type>Method", e));
            zero_result()
        }
    }
}

/// C varargs shim target for `CallNonvirtual<Type>Method{,V,A}`.
///
/// `obj` is the receiver, `cls` is the `jclass` the method ID was derived
/// from and `mid` identifies the method. The method is resolved on the class
/// it was declared in (`info.class`) and invoked without virtual dispatch.
#[no_mangle]
pub unsafe extern "C" fn acos_jni_va_invoke_nonvirtual(
    _env: usize,
    obj: usize,
    cls: usize,
    mid: usize,
    _mode: i32,
    words: *const u64,
    n: i32,
) -> JniCallResult {
    if crate::broker::is_worker() {
        let raw: Vec<u64> = if n <= 0 || words.is_null() {
            Vec::new()
        } else {
            // SAFETY: the worker shim supplies `n` argument words.
            unsafe { std::slice::from_raw_parts(words, n as usize) }.to_vec()
        };
        return match crate::broker::worker_op(
            crate::broker::Op::CallNonvirtual { obj, cls, mid, words: raw },
            "CallNonvirtual<Type>Method",
        ) {
            Some(Ok(bits)) => JniCallResult { bits, kind: 0 },
            Some(Err(e)) => {
                crate::broker::set_worker_error(e);
                zero_result()
            }
            None => JniCallResult { bits: 0, kind: 0 },
        };
    }

    const FN: &str = "CallNonvirtual<Type>Method";
    let Some(mut ctx) = enter(FN, false) else {
        return zero_result();
    };
    let Some(info) = method_info(&mut ctx, mid) else {
        record(
            &ctx,
            JniError::BadReference {
                function: FN.into(),
                what: "jmethodID".into(),
                handle: mid as u32,
            },
        );
        return zero_result();
    };
    if n < 0 || n as usize != info.param_descs.len() {
        record(
            &ctx,
            JniError::BadArgument {
                function: FN.into(),
                message: format!(
                    "{} arguments supplied for {} params of {}{}",
                    n,
                    info.param_descs.len(),
                    info.name,
                    info.sig
                ),
            },
        );
        return zero_result();
    }
    // The `jclass` must be a live class; the method ID carries the declaring
    // class descriptor used for non-virtual resolution.
    if let Err(e) = class_id(&mut ctx, cls as Handle, FN) {
        record(&ctx, e);
        return zero_result();
    }
    let raw: Vec<u64> = if n == 0 || words.is_null() {
        Vec::new()
    } else {
        // SAFETY: the C shim passes exactly `n` words.
        unsafe { std::slice::from_raw_parts(words, n as usize) }.to_vec()
    };
    let mut args = Vec::with_capacity(raw.len());
    for (desc, w) in info.param_descs.iter().zip(raw.iter()) {
        match bits_value(&mut ctx, desc, *w, FN) {
            Ok(v) => args.push(v),
            Err(e) => {
                record(&ctx, e);
                return zero_result();
            }
        }
    }

    let vm = ctx.vm();
    let h = obj as Handle;
    if h == NULL || !vm.heap.is_live(h) {
        record(
            &ctx,
            JniError::BadReference {
                function: FN.into(),
                what: "jobject".into(),
                handle: h,
            },
        );
        return zero_result();
    }
    let result = vm.call_nonvirtual(h, &info.class, &info.name, &info.sig, &args);
    match result {
        Ok(v) => value_result(v),
        Err(BridgeError::JavaException { class, message }) => {
            let vm = ctx.vm();
            match vm.new_throwable(&class_descriptor(&class), Some(&message)) {
                Ok(h) => refs::set_pending(vm, &ctx.rt, ctx.t, h),
                Err(e) => record(
                    &ctx,
                    JniError::NativeFault {
                        message: format!("cannot materialise pending exception {class}: {e}"),
                    },
                ),
            }
            zero_result()
        }
        Err(e) => {
            record(&ctx, bridge_error(FN, e));
            zero_result()
        }
    }
}

/// Kept for API completeness: the table exposes every slot, including the
/// direct-buffer ones the VM does not have yet.
pub fn implemented_slot_count() -> usize {
    crate::table::JNI_SLOT_NAMES.len()
}

#[allow(dead_code)]
fn _assert_array_types(_: ArrayData, _: ArrayObj, _: ThreadJni) {}

/// Re-export for tests that need the exact class descriptor form.
pub fn descriptor_of(name: &str) -> String {
    class_descriptor(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_has_the_jni_shape() {
        assert_eq!(crate::table::JNI_SLOT_COUNT, 233);
        assert_eq!(crate::table::JNI_SLOT_NAMES[4], "GetVersion");
        assert_eq!(crate::table::JNI_SLOT_NAMES[5], "DefineClass");
        assert_eq!(
            crate::table::JNI_SLOT_NAMES[crate::table::JNI_SLOT_COUNT - 1],
            "GetObjectRefType"
        );
    }

    #[test]
    fn descriptors_are_validated() {
        assert!(valid_descriptor("I"));
        assert!(valid_descriptor("[Ljava/lang/String;"));
        assert!(!valid_descriptor("V"));
        assert!(!valid_descriptor("!"));
    }

    #[test]
    fn mutf8_round_trips_bmp_and_supplementary() {
        let units = vec![b'h' as u16, 0, 0x00E9, 0xD83D, 0xDE00];
        let bytes = encode_mutf8(&units);
        assert_eq!(decode_mutf8(&bytes), units);
        assert_eq!(&bytes[1..3], &[0xC0, 0x80], "NUL uses the two-byte form");
    }
}
