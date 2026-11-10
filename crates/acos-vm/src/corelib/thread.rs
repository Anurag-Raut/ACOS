//! `java.lang.Thread` builtins (cooperative green threads).

use std::time::{Duration, Instant};

use acos_dex::access;

use crate::class::NativeCtx;
use crate::corelib::Vm;
use crate::error::VmResult;
use crate::heap::{BuiltinData, Handle, Object, NULL};
use crate::vm::{ThreadRT, ThreadStatus};

pub(crate) fn install(vm: &mut Vm) -> VmResult<()> {
    let id = vm.def_class(
        "Ljava/lang/Thread;",
        access::PUBLIC,
        Some("Ljava/lang/Object;"),
        &["Ljava/lang/Runnable;"],
    )?;
    vm.c_thread = id;
    let p = access::PUBLIC;
    vm.add_native(id, "<init>", "()V", p, thread_init);
    vm.add_native(id, "<init>", "(Ljava/lang/Runnable;)V", p, thread_init_runnable);
    vm.add_native(id, "<init>", "(Ljava/lang/String;)V", p, thread_init_name);
    vm.add_native(id, "start", "()V", p, thread_start);
    vm.add_native(id, "run", "()V", p, thread_run);
    vm.add_native(id, "join", "()V", p, thread_join);
    vm.add_native(id, "isAlive", "()Z", p, thread_is_alive);
    vm.add_native(id, "getName", "()Ljava/lang/String;", p, thread_get_name);
    vm.add_native(id, "setName", "(Ljava/lang/String;)V", p, thread_set_name);
    vm.add_native(id, "getId", "()J", p, thread_get_id);
    vm.add_native(id, "setDaemon", "(Z)V", p, thread_set_daemon);
    vm.add_native(id, "currentThread", "()Ljava/lang/Thread;", access::PUBLIC | access::STATIC, thread_current);
    vm.add_native(id, "sleep", "(J)V", access::PUBLIC | access::STATIC, thread_sleep);
    vm.add_native(id, "yield", "()V", access::PUBLIC | access::STATIC, thread_yield);
    vm.link_class(id)?;
    Ok(())
}

fn thread_data(vm: &Vm, this: Handle) -> Option<&crate::heap::ThreadData> {
    match vm.heap.get(this) {
        Some(Object::Builtin {
            data: BuiltinData::Thread(t),
            ..
        }) => Some(t),
        _ => None,
    }
}

fn thread_data_mut(vm: &mut Vm, this: Handle) -> Option<&mut crate::heap::ThreadData> {
    match vm.heap.get_mut(this) {
        Some(Object::Builtin {
            data: BuiltinData::Thread(t),
            ..
        }) => Some(t),
        _ => None,
    }
}

fn thread_init(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    if let Some(t) = thread_data_mut(vm, this) {
        t.target = NULL;
    }
    Ok(())
}

fn thread_init_runnable(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let target = vm.arg_ref(ctx, 0);
    if let Some(t) = thread_data_mut(vm, this) {
        t.target = target;
    }
    Ok(())
}

fn thread_init_name(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let name = vm.arg_ref(ctx, 0);
    if let Some(t) = thread_data_mut(vm, this) {
        t.name = name;
    }
    Ok(())
}

fn thread_start(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let method = vm.dispatch(this, "run", "()V")?;
    let mut rt = ThreadRT::new(this);
    rt.status = ThreadStatus::Runnable;
    vm.threads.push(rt);
    let new_index = vm.threads.len() - 1;
    // Push the `run()` frame onto the new thread.
    let saved = vm.current_thread;
    vm.current_thread = new_index;
    let r = crate::interp::push_frame_values(vm, method, this, &[]);
    vm.current_thread = saved;
    r?;
    Ok(())
}

fn thread_run(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let target = thread_data(vm, this).map(|t| t.target).unwrap_or(NULL);
    if target != NULL {
        let m = vm.dispatch(target, "run", "()V")?;
        let r = crate::interp::call_method(vm, m, target, &[])?;
        let _ = r;
    }
    Ok(())
}

fn thread_join(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let cur = vm.current_thread;
    let target = vm.threads.iter().position(|t| t.object == this);
    let target = match target {
        Some(t) if t != cur => t,
        _ => return Ok(()),
    };
    let alive = matches!(
        vm.threads[target].status,
        ThreadStatus::Runnable | ThreadStatus::Running | ThreadStatus::BlockedMonitor(_) | ThreadStatus::Sleeping | ThreadStatus::Waiting
    ) && !vm.threads[target].frames.is_empty();
    if alive {
        vm.threads[cur].status = ThreadStatus::Waiting;
        vm.threads[target].joiners.push(cur);
    }
    Ok(())
}

fn thread_is_alive(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let alive = thread_data(vm, this).map(|t| t.alive).unwrap_or(false);
    vm.ret_bool(ctx, alive);
    Ok(())
}

fn thread_get_name(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let name = thread_data(vm, this).map(|t| t.name).unwrap_or(NULL);
    vm.ret_ref(ctx, name);
    Ok(())
}

fn thread_set_name(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let name = vm.arg_ref(ctx, 0);
    if let Some(t) = thread_data_mut(vm, this) {
        t.name = name;
    }
    Ok(())
}

fn thread_get_id(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let id = thread_data(vm, this).map(|t| t.id).unwrap_or(0);
    vm.ret_i64(ctx, id as i64);
    Ok(())
}

fn thread_set_daemon(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let this = vm.ctx_this(ctx).unwrap_or(NULL);
    let d = vm.arg_bool(ctx, 0);
    if let Some(t) = thread_data_mut(vm, this) {
        t.daemon = d;
    }
    Ok(())
}

fn thread_current(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let cur = vm.current_thread;
    let obj = vm.threads[cur].object;
    vm.ret_ref(ctx, obj);
    Ok(())
}

fn thread_sleep(vm: &mut Vm, ctx: NativeCtx) -> VmResult<()> {
    let ms = vm.arg_i64(ctx, 0);
    let cur = vm.current_thread;
    vm.threads[cur].status = ThreadStatus::Sleeping;
    vm.threads[cur].wake_at = Some(Instant::now() + Duration::from_millis(ms.max(0) as u64));
    Ok(())
}

fn thread_yield(vm: &mut Vm, _ctx: NativeCtx) -> VmResult<()> {
    let cur = vm.current_thread;
    vm.threads[cur].status = ThreadStatus::Runnable;
    Ok(())
}
