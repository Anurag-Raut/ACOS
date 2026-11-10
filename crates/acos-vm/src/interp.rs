//! The bytecode interpreter and thread scheduler.
//!
//! Execution is fully iterative: no Java invocation recurses on the host Rust
//! stack. Frames live in per-thread VM stacks and the scheduler picks a
//! runnable thread for each step.

use crate::class::{ClassId, FieldSign, Insn, MethodCode, MethodImpl, MethodId, NativeCtx};
use crate::error::{VmError, VmResult};
use crate::frame::Frame;
use crate::heap::{ArrayData, Handle, Object, NULL};
use crate::vm::{InvokeKind, ThreadStatus, Vm};

thread_local! {
    /// Innermost synchronous Rust→Java call base (`call_method`,
    /// `initialize_class`, native→Java callbacks). Exception unwinding must not
    /// pop below this depth: it is converted into a VM-level pending exception
    /// and returned to the Rust caller as `Err(JavaException)`, which rethrows
    /// it into its own Java frames at the right handler. `0` is the top-level
    /// scheduler run (and any base at depth 0), where unwinding may reach an
    /// empty stack.
    static EXCEPTION_FLOOR: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Borrow the decoded body of a DEX method. Frames no longer hold an
/// `Arc<MethodCode>`: the body is owned by the method table and looked up by
/// `MethodId`, so a call no longer pays an atomic clone/drop on push/pop.
#[inline]
fn code_of(vm: &Vm, method: MethodId) -> Option<&MethodCode> {
    match &vm.methods[method as usize].imp {
        MethodImpl::Dex { code: Some(c), .. } => Some(c),
        _ => None,
    }
}

fn acquire_frame(
    vm: &mut Vm,
    t: usize,
    method: MethodId,
    class: ClassId,
    regs_size: usize,
) -> Frame {
    let mut frame = match vm.threads[t].frame_pool.pop() {
        Some(mut f) => {
            // Keep the lowered IR program when the pooled frame is reused for
            // the same method body (the common recursive/looping case), so the
            // process-global cache is probed at most once per distinct body per
            // pooled slot.
            if f.method != method {
                f.ir = None;
                f.ir_checked = false;
            }
            // Set the length to exactly the method's register size. Native
            // handlers (`ctx_this`/`param_off`) derive the frame base from
            // `regs.len()`, so an oversized pooled buffer would corrupt them.
            // `truncate`/`resize` keep the allocation, so reuse still avoids
            // the two per-call heap allocations.
            if f.regs.len() < regs_size {
                f.regs.resize(regs_size, 0);
            } else {
                f.regs.truncate(regs_size);
            }
            if f.refs.len() < regs_size {
                f.refs.resize(regs_size, false);
            } else {
                f.refs.truncate(regs_size);
            }
            f.regs.fill(0);
            f.refs.fill(false);
            f
        }
        None => Frame::new(method, class, regs_size),
    };
    frame.method = method;
    frame.class = class;
    frame.pc = 0;
    frame.insn_index = 0;
    frame.pending_result = 0;
    frame.pending_result_is_ref = false;
    frame.has_pending_result = false;
    frame.pending_exception = NULL;
    frame.monitor = None;
    frame.advance_on_return = false;
    frame.advance_size = 0;
    frame
}

/// Return a popped frame's register buffers to the pool (bounded).
#[inline]
fn release_frame(vm: &mut Vm, t: usize, f: Frame) {
    const POOL_CAP: usize = 128;
    if vm.threads[t].frame_pool.len() < POOL_CAP {
        // Keep the body reference; the slot is overwritten on reuse and the
        // pool is capped, so at most POOL_CAP bodies are retained.
        vm.threads[t].frame_pool.push(f);
    }
}

/// Top-level scheduler: runs all runnable green threads to completion.
pub fn run(vm: &mut Vm) -> VmResult<()> {
    let mut next_yield: u64 = 8192;
    loop {
        // Host safe point: let an installed broker service requests queued by
        // native OS threads. It runs here, on the owner thread, between green
        // thread batches so the VM is not aliased from two threads.
        crate::natives::safe_point::pump(vm);
        // Host RSS / watchdog check once per scheduler turn (cheap; the RSS
        // read is throttled). This bounds a stuck green thread even when it
        // never reaches an allocation.
        vm.host.check()?;
        if vm.pending_exception != NULL {
            break;
        }
        let t = vm.current_thread;
        if vm.threads.get(t).is_none() {
            break;
        }
        if vm.threads[t].frames.is_empty() {
            crate::bridge::finish_thread(vm, t);
            if !pick_runnable(vm) {
                break;
            }
            continue;
        }
        if !matches!(vm.threads[t].status, ThreadStatus::Runnable | ThreadStatus::Running) {
            if !pick_runnable(vm) {
                break;
            }
            continue;
        }
        vm.threads[t].status = ThreadStatus::Running;
        let n = run_batch(vm, t, 0, true)?;
        vm.metrics.instructions += n;
        if vm.threads.len() > 1 && vm.metrics.instructions >= next_yield {
            // Pre-empt the running thread for scheduling fairness, but never
            // resurrect a thread that blocked/parked during the batch. Race:
            // resetting a `Parked` looper thread to `Runnable` here made the
            // scheduler re-run it forever, so `pump` was never reached and the
            // host burned 100% CPU (vm-perf merge regression, W12).
            if vm.threads[t].status == ThreadStatus::Running {
                vm.threads[t].status = ThreadStatus::Runnable;
            }
            let _ = pick_runnable(vm);
            next_yield = vm.metrics.instructions + 8192;
        }
    }
    Ok(())
}

/// Run the current thread as a batch: repeated runs of frame-local opcodes
/// (through [`crate::dispatch::fast_run`]) interleaved with single general-path
/// instructions (invoke, return, native, field resolution, allocation, ...).
///
/// Staying inside one call instead of bouncing out to the scheduler for every
/// invoke/return removed most of the per-call overhead in call-heavy code.
/// Stops at the batch cap, at `stop_depth`, when the thread blocks/finishes, or
/// when an uncaught exception is pending. Returns the instructions executed.
fn run_batch(vm: &mut Vm, t: usize, stop_depth: usize, yield_on_block: bool) -> VmResult<u64> {
    // Opcode histograms need the general path so every opcode is counted.
    if vm.count_opcodes {
        exec_one(vm, t)?;
        return Ok(1);
    }
    let cap = crate::dispatch::FAST_CAP;
    let use_ir = crate::exec::enabled();
    let mut count = 0u64;
    while count < cap {
        if vm.threads[t].frames.len() <= stop_depth {
            break;
        }
        if vm.pending_exception != NULL {
            break;
        }
        // The scheduler must yield when a thread blocks/goes to sleep. A
        // synchronous nested Java call (`call_method` from a native) must not:
        // it has to run to completion on the host stack, exactly as the old
        // `step`-based `run_current_until` did.
        if yield_on_block
            && matches!(
                vm.threads[t].status,
                ThreadStatus::Sleeping
                    | ThreadStatus::Waiting
                    | ThreadStatus::BlockedMonitor(_)
                    | ThreadStatus::Parked
                    | ThreadStatus::Finished
            )
        {
            break;
        }
        let fi = vm.threads[t].frames.len() - 1;
        let is_dex = matches!(
            &vm.methods[vm.threads[t].frames[fi].method as usize].imp,
            MethodImpl::Dex { code: Some(_), .. }
        );
        if is_dex {
            // Fast opcodes never push/pop frames, allocate, or resolve
            // references, so `fi` and the frame pointer stay valid for the
            // whole run and the frame lookup is paid once.
            if use_ir {
                // Typed-IR tier (default on; `ACOS_EXEC=interp` forces the
                // general interpreter). The lowered program is cached on the
                // frame (the process-global cache is only probed on first
                // execution) and immutable; on any opcode it does not handle it
                // stops before the instruction and the general path below runs
                // it, so semantics are unchanged. Only loop bodies use the tier:
                // straight-line methods get no fused-dispatch win and would pay
                // the per-segment entry cost.
                if !vm.threads[t].frames[fi].ir_checked {
                    vm.threads[t].frames[fi].ir_checked = true;
                    let method = vm.threads[t].frames[fi].method;
                    if let MethodImpl::Dex { code: Some(code), .. } =
                        &vm.methods[method as usize].imp
                    {
                        let code = code.clone();
                        if crate::exec::is_loop_body(&code) {
                            vm.threads[t].frames[fi].ir = Some(crate::exec::get_or_lower(&code));
                        }
                    }
                }
                if vm.threads[t].frames[fi].ir.is_some() {
                    let prog = vm.threads[t].frames[fi].ir.take().expect("ir cached");
                    // Resolve any field ops this VM has not quickened yet; this
                    // is off the per-instruction path. Failures become
                    // `Fallback` and are reported by the general path.
                    crate::exec::quicken(vm, t, fi, &prog);
                    let depth = vm.threads[t].frames.len();
                    let handled = match crate::exec::run_ir(vm, t, fi, &prog, cap - count) {
                        Ok((n, handled)) => {
                            count += n;
                            handled
                        }
                        Err(VmError::JavaException(h)) => {
                            // A slow op (allocation/string/invoke) threw: route
                            // it through the catch-table machinery exactly as
                            // the general path would.
                            handle_exception(vm, h)?;
                            count += 1;
                            true
                        }
                        Err(e) => return Err(e),
                    };
                    // Restore the cached program on the (surviving) frame.
                    if fi < vm.threads[t].frames.len() {
                        vm.threads[t].frames[fi].ir = Some(prog);
                    }
                    // `run_ir` handled the current op (invoke/allocation) or
                    // pushed a callee frame: hand control back to the
                    // scheduler loop so it picks up the new top frame.
                    if handled || vm.threads[t].frames.len() != depth {
                        continue;
                    }
                } else {
                    let Vm {
                        threads,
                        heap,
                        methods,
                        ..
                    } = &mut *vm;
                    let method = threads[t].frames[fi].method;
                    let code = match &methods[method as usize].imp {
                        MethodImpl::Dex { code: Some(c), .. } => c.as_ref(),
                        _ => unreachable!(),
                    };
                    let f = &mut threads[t].frames[fi];
                    count += crate::dispatch::fast_run(f, code, heap, cap - count);
                }
            } else {
                let Vm {
                    threads,
                    heap,
                    methods,
                    ..
                } = &mut *vm;
                let method = threads[t].frames[fi].method;
                let code = match &methods[method as usize].imp {
                    MethodImpl::Dex { code: Some(c), .. } => c.as_ref(),
                    _ => unreachable!(),
                };
                let f = &mut threads[t].frames[fi];
                count += crate::dispatch::fast_run(f, code, heap, cap - count);
            }
        }
        if count >= cap || vm.threads[t].frames.len() <= stop_depth {
            break;
        }
        // Fast path for a `return`: `exec_one`'s prologue (method lookup, impl
        // match and instruction fetch) is pure overhead for the opcode that
        // ends every call. The register/result transfer and frame pop are the
        // same as the general path.
        if is_dex {
            let insn = {
                let method = vm.threads[t].frames[fi].method;
                match &vm.methods[method as usize].imp {
                    MethodImpl::Dex { code: Some(code), .. } => {
                        code.insns.get(vm.threads[t].frames[fi].insn_index).copied()
                    }
                    _ => None,
                }
            };
            if let Some(insn) = insn {
                match insn.op {
                    // `return*`
                    0x0e..=0x11 => {
                        exec_return(vm, t, fi, &insn);
                        count += 1;
                        continue;
                    }
                    // `invoke-*`: skip the `exec_one` prologue, but keep the
                    // same Java-exception routing as the general path.
                    0x6e..=0x72 | 0x74..=0x78 => {
                        match invoke(vm, t, fi, &insn) {
                            Ok(()) => {}
                            Err(VmError::JavaException(h)) => handle_exception(vm, h)?,
                            Err(e) => return Err(e),
                        }
                        count += 1;
                        continue;
                    }
                    _ => {}
                }
            }
        }
        // Exactly one general-path instruction: native, invoke, return,
        // field/class resolution, allocation, monitor, throw, ...
        exec_one(vm, t)?;
        count += 1;
    }
    Ok(count)
}

/// Fast `return`/`return-void`/`return-wide`/`return-object`: store the result
/// into the frame and pop it, exactly as `execute`'s return arms do.
#[inline]
fn exec_return(vm: &mut Vm, t: usize, fi: usize, insn: &Insn) {
    let r0 = insn.regs[0];
    let f = &mut vm.threads[t].frames[fi];
    match insn.op {
        0x0e => {
            f.pending_result = 0;
            f.pending_result_is_ref = false;
        }
        0x0f => {
            f.pending_result = f.regs[r0 as usize] as u64;
            f.pending_result_is_ref = false;
        }
        0x10 => {
            f.pending_result = f.reg_wide(r0);
            f.pending_result_is_ref = false;
        }
        0x11 => {
            f.pending_result = f.regs[r0 as usize] as u64;
            f.pending_result_is_ref = true;
        }
        _ => {}
    }
    pop_frame(vm, t);
}

fn pick_runnable(vm: &mut Vm) -> bool {
    // Wake sleepers whose deadline passed, and monitor-blocked threads whose
    // lock is now free.
    let now = std::time::Instant::now();
    for i in 0..vm.threads.len() {
        match vm.threads[i].status {
            ThreadStatus::Sleeping => {
                if vm.threads[i].wake_at.map(|w| w <= now).unwrap_or(true) {
                    vm.threads[i].status = ThreadStatus::Runnable;
                }
            }
            ThreadStatus::BlockedMonitor(h) => {
                if monitor_owner(vm, h).map(|o| o == i).unwrap_or(true) {
                    vm.threads[i].status = ThreadStatus::Runnable;
                }
            }
            _ => {}
        }
    }
    let n = vm.threads.len();
    for k in 1..=n {
        let i = (vm.current_thread + k) % n;
        if vm.threads[i].status == ThreadStatus::Runnable && !vm.threads[i].frames.is_empty() {
            vm.current_thread = i;
            return true;
        }
    }
    // Nothing runnable; if someone is sleeping, busy-advance time.
    if vm
        .threads
        .iter()
        .any(|t| t.status == ThreadStatus::Sleeping)
    {
        std::thread::sleep(std::time::Duration::from_millis(1));
        for i in 0..vm.threads.len() {
            if vm.threads[i].status == ThreadStatus::Sleeping {
                vm.threads[i].status = ThreadStatus::Runnable;
                vm.current_thread = i;
                return true;
            }
        }
    }
    // Deadlocked or finished.
    false
}

fn monitor_owner(vm: &Vm, h: Handle) -> Option<usize> {
    vm.monitors.get(&h).map(|(o, _)| *o)
}

/// Execute exactly one instruction on the current thread (general path).
fn exec_one(vm: &mut Vm, t: usize) -> VmResult<()> {
    if vm.threads[t].frames.is_empty() {
        return Ok(());
    }
    let fi = vm.threads[t].frames.len() - 1;
    let method = vm.threads[t].frames[fi].method;

    // Copy the native function pointer out (if any) instead of cloning the
    // whole `MethodImpl` for every single instruction.
    let native = match &vm.methods[method as usize].imp {
        MethodImpl::Native(f) => Some(*f),
        _ => None,
    };
    if let Some(f) = native {
        vm.metrics.invokes += 1;
        crate::bridge::record_native(vm, method);
        let ctx = NativeCtx { thread: t, frame: fi };
        return match f(vm, ctx) {
            Ok(()) => {
                pop_frame(vm, t);
                Ok(())
            }
            Err(VmError::JavaException(h)) => {
                pop_frame_quiet(vm, t);
                handle_exception(vm, h)
            }
            Err(e) => Err(e),
        };
    }

    if !matches!(
        &vm.methods[method as usize].imp,
        MethodImpl::Dex { code: Some(_), .. }
    ) {
        // A native method declared without a registered implementation is an
        // unbound platform API, not an abstract method.
        if vm.methods[method as usize].is_native() {
            let at = format!("{} pc={}", vm.frames_label(t, fi), vm.threads[t].frames[fi].pc);
            return Err(crate::bridge::unbound_native_error(vm, method, at));
        }
        let desc = vm.class_desc(vm.methods[method as usize].class).to_string();
        let name = vm.methods[method as usize].name.clone();
        let sig = vm.methods[method as usize].sig.clone();
        let e = vm.throw_new(
            "Ljava/lang/AbstractMethodError;",
            Some(&format!("{desc}.{name}{sig}")),
        );
        return Err(e);
    }

    let insn = {
        let f = &vm.threads[t].frames[fi];
        let code = match code_of(vm, method) {
            Some(c) => c,
            None => unreachable!(),
        };
        match code.insns.get(f.insn_index) {
            Some(i) => *i,
            None => {
                return Err(VmError::Internal(format!(
                    "pc out of range in {}",
                    vm.methods[method as usize].name
                )))
            }
        }
    };
    match execute(vm, t, fi, &insn) {
        Ok(()) => Ok(()),
        Err(VmError::JavaException(h)) => {
            handle_exception(vm, h)?;
            Ok(())
        }
        Err(e) => Err(e),
    }
}

#[inline]
fn pop_frame(vm: &mut Vm, t: usize) {
    let f = vm.threads[t].frames.pop().unwrap();
    if let Some(m) = f.monitor {
        exit_monitor(vm, m);
    }
    // The caller's next instruction size was recorded on its frame by
    // `invoke`, so no per-return lookup of the caller's body is needed.
    let adv_size = if f.advance_on_return {
        vm.threads[t]
            .frames
            .last()
            .map(|caller| caller.advance_size as u32)
            .unwrap_or(0)
    } else {
        0
    };
    if let Some(caller) = vm.threads[t].frames.last_mut() {
        caller.pending_result = f.pending_result;
        caller.pending_result_is_ref = f.pending_result_is_ref;
        caller.has_pending_result = true;
        if f.advance_on_return {
            caller.pc += adv_size;
            caller.insn_index += 1;
        }
    }
    release_frame(vm, t, f);
}

/// Pop a frame without transferring a result (used when an exception is
/// propagating out of a native call).
fn pop_frame_quiet(vm: &mut Vm, t: usize) {
    if let Some(f) = vm.threads[t].frames.pop() {
        if let Some(m) = f.monitor {
            exit_monitor(vm, m);
        }
        release_frame(vm, t, f);
    }
}

fn handle_exception(vm: &mut Vm, h: Handle) -> VmResult<()> {
    vm.native_roots.push(h);
    let result = handle_exception_inner(vm, h);
    vm.native_roots.pop();
    result
}

fn handle_exception_inner(vm: &mut Vm, h: Handle) -> VmResult<()> {
    let t = vm.current_thread;
    let floor = EXCEPTION_FLOOR.with(|f| f.get());
    loop {
        if vm.threads[t].frames.is_empty() {
            vm.pending_exception = h;
            return Ok(());
        }
        // Stop at the synchronous Rust→Java call base. The frame below the
        // floor belongs to the Rust caller, not to the unwinding Java stack;
        // the caller receives the exception as `Err(JavaException)` and
        // rethrows it into its own frames. Without this the unwinder pops the
        // native/`call_method` frame and the caller then continues with a stale
        // frame index (`set_ref` panic, `frames.len()=3 fi=13`).
        if vm.threads[t].frames.len() <= floor {
            vm.pending_exception = h;
            return Ok(());
        }
        let fi = vm.threads[t].frames.len() - 1;
        let (pc, method) = {
            let f = &vm.threads[t].frames[fi];
            (f.pc, f.method)
        };
        // Native frames have no body and no handler; pop and keep unwinding.
        // The `Arc` clone here is on the cold exception path only.
        let code = match &vm.methods[method as usize].imp {
            MethodImpl::Dex { code: Some(c), .. } => c.clone(),
            _ => {
                pop_frame_quiet(vm, t);
                continue;
            }
        };
        if let Some(addr) = find_handler(vm, &code, pc, h)? {
            let idx = code.pc_to_idx[addr as usize] as usize;
            let f = &mut vm.threads[t].frames[fi];
            f.pc = addr;
            f.insn_index = idx;
            f.pending_exception = h;
            f.has_pending_result = false;
            return Ok(());
        }
        pop_frame_quiet(vm, t);
    }
}

fn find_handler(
    vm: &mut Vm,
    code: &MethodCode,
    pc: u32,
    exception: Handle,
) -> VmResult<Option<u32>> {
    let exc_class = vm
        .class_of_object(exception)
        .ok_or_else(|| VmError::Internal("exception has no class".into()))?;
    for tr in &code.tries {
        if pc < tr.start || pc >= tr.end {
            continue;
        }
        for (desc, addr) in &tr.typed {
            let target = vm.load_class(desc)?;
            if vm.is_assignable(exc_class, target)? {
                return Ok(Some(*addr));
            }
        }
        if let Some(addr) = tr.catch_all {
            return Ok(Some(addr));
        }
    }
    Ok(None)
}

// ---------------------------------------------------------------------------
// Register helpers
// ---------------------------------------------------------------------------

#[inline]
fn reg(vm: &Vm, t: usize, fi: usize, r: u16) -> u32 {
    vm.threads[t].frames[fi].regs[r as usize]
}
#[inline]
fn set_reg(vm: &mut Vm, t: usize, fi: usize, r: u16, v: u32) {
    let f = &mut vm.threads[t].frames[fi];
    f.regs[r as usize] = v;
    f.refs[r as usize] = false;
}
#[inline]
fn set_ref(vm: &mut Vm, t: usize, fi: usize, r: u16, v: Handle) {
    let f = &mut vm.threads[t].frames[fi];
    f.regs[r as usize] = v;
    f.refs[r as usize] = true;
}
#[inline]
fn set_wide(vm: &mut Vm, t: usize, fi: usize, r: u16, v: u64) {
    let f = &mut vm.threads[t].frames[fi];
    f.regs[r as usize] = v as u32;
    f.regs[r as usize + 1] = (v >> 32) as u32;
    f.refs[r as usize] = false;
    f.refs[r as usize + 1] = false;
}
#[inline]
fn get_wide(vm: &Vm, t: usize, fi: usize, r: u16) -> u64 {
    let f = &vm.threads[t].frames[fi];
    (f.regs[r as usize] as u64) | ((f.regs[r as usize + 1] as u64) << 32)
}

/// Advance the program counter past the current instruction.
#[inline]
fn advance(vm: &mut Vm, t: usize, fi: usize, size: u8) {
    let f = &mut vm.threads[t].frames[fi];
    f.pc += size as u32;
    f.insn_index += 1;
}

#[inline]
fn branch_to(vm: &mut Vm, t: usize, fi: usize, target_pc: u32) -> VmResult<()> {
    let idx = {
        let f = &vm.threads[t].frames[fi];
        code_of(vm, f.method).and_then(|c| c.pc_to_idx.get(target_pc as usize).copied())
    };
    let idx = idx.and_then(|i| if i == u32::MAX { None } else { Some(i as usize) });
    let idx = match idx {
        Some(i) => i,
        None => {
            let src = {
                let f = &vm.threads[t].frames[fi];
                code_of(vm, f.method)
                    .and_then(|c| c.source_file.clone())
                    .unwrap_or_default()
            };
            return Err(VmError::Internal(format!(
                "branch to non-instruction pc {target_pc} in {src}"
            )))
        }
    };
    let f = &mut vm.threads[t].frames[fi];
    f.pc = target_pc;
    f.insn_index = idx;
    Ok(())
}

fn rel_target(pc: u32, target: i32) -> u32 {
    (pc as i64 + target as i64) as u32
}

/// Allocate a `new-instance` object. Shared by the general path and the typed-IR
/// slow-op trampoline; runs `<clinit>` first, exactly as before.
pub(crate) fn exec_new_instance(
    vm: &mut Vm,
    t: usize,
    fi: usize,
    dst: u16,
    type_index: u32,
) -> VmResult<()> {
    let dex = frame_dex(vm, t, fi);
    let cid = cached_class(vm, dex, type_index)?;
    if !matches!(vm.classes[cid as usize].init, crate::class::InitState::Init) {
        vm.initialize_class(cid)?;
    }
    // DEX-defined classes skip the builtin-allocator `HashMap` probe (which
    // `builtin_instance` would otherwise run for every `new-instance` miss).
    // `c_string` must still go through `builtin_instance`: it is special-cased
    // to `Object::String` even in real-corelib mode, where the class is
    // DEX-loaded and therefore not flagged `is_builtin`.
    let h = if vm.classes[cid as usize].is_builtin || cid == vm.c_string {
        vm.builtin_instance(cid)?
    } else {
        vm.alloc_instance(cid)?
    };
    set_ref(vm, t, fi, dst, h);
    Ok(())
}

/// Allocate a `new-array`. Shared by the general path and the typed-IR slow-op
/// trampoline; `len` is the already-read length register.
pub(crate) fn exec_new_array(
    vm: &mut Vm,
    t: usize,
    fi: usize,
    dst: u16,
    type_index: u32,
    len: i32,
) -> VmResult<()> {
    let dex = frame_dex(vm, t, fi);
    if len < 0 {
        return Err(vm.throw_new(
            "Ljava/lang/NegativeArraySizeException;",
            Some(&len.to_string()),
        ));
    }
    // Fast path: borrow the DEX component descriptor directly and build the
    // backing store in place. `ArrayObj` already owns a copy of the element
    // descriptor, so the previous unconditional `to_string()` here was a
    // throwaway `String` heap allocation on every `new-array`. It is rebuilt
    // only on the (cold) bounds-check error path, which needs the general
    // `check_array_len` OOM throw.
    let built = {
        let desc = vm.dexs[dex as usize].file.type_descriptor(type_index)?;
        let elem = desc.strip_prefix('[').unwrap_or(desc);
        if crate::heap::array_len_fits(elem, len as usize, vm.heap.max_bytes) {
            Some(crate::vm::simple_array_object(elem, len as usize)?)
        } else {
            None
        }
    };
    let obj = match built {
        Some(obj) => obj,
        None => {
            let desc = vm.dexs[dex as usize]
                .file
                .type_descriptor(type_index)?
                .to_string();
            let elem = desc.strip_prefix('[').unwrap_or(&desc);
            // Same predicate as `array_len_fits`, so this always errs; it is
            // the only place the Java `OutOfMemoryError` message is built.
            return Err(vm.check_array_len(elem, len as usize).unwrap_err());
        }
    };
    let h = vm.alloc(obj)?;
    set_ref(vm, t, fi, dst, h);
    Ok(())
}

/// Invoke the `invoke-*` at `insn_index` on the current frame. Shared by the
/// general path and the typed-IR slow-op trampoline; the caller must have the
/// frame positioned at the instruction.
pub(crate) fn exec_invoke_at(
    vm: &mut Vm,
    t: usize,
    fi: usize,
    insn_index: usize,
) -> VmResult<()> {
    let method = vm.threads[t].frames[fi].method;
    let insn = match &vm.methods[method as usize].imp {
        MethodImpl::Dex { code: Some(code), .. } => match code.insns.get(insn_index) {
            Some(i) => *i,
            None => {
                return Err(VmError::Internal(format!(
                    "invoke pc out of range in {}",
                    vm.methods[method as usize].name
                )))
            }
        },
        _ => return Err(VmError::Internal("invoke on non-Dex frame".into())),
    };
    invoke(vm, t, fi, &insn)
}

/// Resolve/intern a `const-string`. Shared by the general path and the typed-IR
/// slow-op trampoline.
pub(crate) fn exec_const_string(
    vm: &mut Vm,
    t: usize,
    fi: usize,
    dst: u16,
    string_index: u32,
) -> VmResult<()> {
    let dex = frame_dex(vm, t, fi);
    let h = const_string(vm, dex, string_index)?;
    set_ref(vm, t, fi, dst, h);
    Ok(())
}

// ---------------------------------------------------------------------------
// The dispatch table
// ---------------------------------------------------------------------------

fn execute(vm: &mut Vm, t: usize, fi: usize, insn: &Insn) -> VmResult<()> {
    let op = insn.op;
    if vm.count_opcodes {
        vm.opcode_counts[op as usize] += 1;
    }
    let r0 = insn.regs[0];
    let r1 = insn.regs[1];
    let r2 = insn.regs[2];
    let pc = vm.threads[t].frames[fi].pc;

    match op {
        // ---- moves ----
        0x01 | 0x07 => {
            // move / move-object
            let v = reg(vm, t, fi, r1);
            if op == 0x07 {
                set_ref(vm, t, fi, r0, v);
            } else {
                let is_ref = vm.threads[t].frames[fi].refs[r1 as usize];
                set_reg(vm, t, fi, r0, v);
                vm.threads[t].frames[fi].refs[r0 as usize] = is_ref;
            }
            advance(vm, t, fi, insn.size);
        }
        0x02 | 0x08 => {
            let v = reg(vm, t, fi, r1);
            if op == 0x08 {
                set_ref(vm, t, fi, r0, v);
            } else {
                let is_ref = vm.threads[t].frames[fi].refs[r1 as usize];
                set_reg(vm, t, fi, r0, v);
                vm.threads[t].frames[fi].refs[r0 as usize] = is_ref;
            }
            advance(vm, t, fi, insn.size);
        }
        0x03 | 0x09 => {
            let v = reg(vm, t, fi, r1);
            if op == 0x09 {
                set_ref(vm, t, fi, r0, v);
            } else {
                set_reg(vm, t, fi, r0, v);
            }
            advance(vm, t, fi, insn.size);
        }
        0x04..=0x06 => {
            let v = get_wide(vm, t, fi, r1);
            set_wide(vm, t, fi, r0, v);
            advance(vm, t, fi, insn.size);
        }
        0x0a => {
            let v = vm.threads[t].frames[fi].pending_result as u32;
            set_reg(vm, t, fi, r0, v);
            vm.threads[t].frames[fi].has_pending_result = false;
            advance(vm, t, fi, insn.size);
        }
        0x0b => {
            let v = vm.threads[t].frames[fi].pending_result;
            set_wide(vm, t, fi, r0, v);
            vm.threads[t].frames[fi].has_pending_result = false;
            advance(vm, t, fi, insn.size);
        }
        0x0c => {
            let v = vm.threads[t].frames[fi].pending_result as u32;
            set_ref(vm, t, fi, r0, v);
            vm.threads[t].frames[fi].has_pending_result = false;
            advance(vm, t, fi, insn.size);
        }
        0x0d => {
            let e = vm.threads[t].frames[fi].pending_exception;
            set_ref(vm, t, fi, r0, e);
            vm.threads[t].frames[fi].pending_exception = NULL;
            advance(vm, t, fi, insn.size);
        }

        // ---- returns ----
        0x0e => {
            let f = &mut vm.threads[t].frames[fi];
            f.pending_result = 0;
            f.pending_result_is_ref = false;
            pop_frame(vm, t);
        }
        0x0f => {
            let v = reg(vm, t, fi, r0);
            let f = &mut vm.threads[t].frames[fi];
            f.pending_result = v as u64;
            f.pending_result_is_ref = false;
            pop_frame(vm, t);
        }
        0x10 => {
            let v = get_wide(vm, t, fi, r0);
            let f = &mut vm.threads[t].frames[fi];
            f.pending_result = v;
            f.pending_result_is_ref = false;
            pop_frame(vm, t);
        }
        0x11 => {
            let v = reg(vm, t, fi, r0);
            let f = &mut vm.threads[t].frames[fi];
            f.pending_result = v as u64;
            f.pending_result_is_ref = true;
            pop_frame(vm, t);
        }

        // ---- constants ----
        0x12..=0x15 => {
            set_reg(vm, t, fi, r0, insn.literal as u32);
            advance(vm, t, fi, insn.size);
        }
        0x16..=0x19 => {
            set_wide(vm, t, fi, r0, insn.literal as u64);
            advance(vm, t, fi, insn.size);
        }
        0x1a..=0x1b => {
            exec_const_string(vm, t, fi, r0, insn.index)?;
            advance(vm, t, fi, insn.size);
        }
        0x1c => {
            let dex = frame_dex(vm, t, fi);
            let cid = cached_class(vm, dex, insn.index)?;
            let h = vm.class_object(cid)?;
            set_ref(vm, t, fi, r0, h);
            advance(vm, t, fi, insn.size);
        }

        // ---- monitors ----
        0x1d => {
            let h = reg(vm, t, fi, r0);
            if h == NULL {
                return Err(vm.throw_new(
                    "Ljava/lang/NullPointerException;",
                    Some("monitor-enter on null"),
                ));
            }
            if try_enter_monitor(vm, h, t) {
                advance(vm, t, fi, insn.size);
            } else {
                vm.threads[t].status = ThreadStatus::BlockedMonitor(h);
            }
        }
        0x1e => {
            let h = reg(vm, t, fi, r0);
            if h == NULL {
                return Err(vm.throw_new(
                    "Ljava/lang/NullPointerException;",
                    Some("monitor-exit on null"),
                ));
            }
            exit_monitor(vm, h);
            advance(vm, t, fi, insn.size);
        }

        // ---- casts / type checks ----
        0x1f => {
            let v = reg(vm, t, fi, r0);
            if v != NULL {
                let dex = frame_dex(vm, t, fi);
                let target = cached_class(vm, dex, insn.index)?;
                let from = vm
                    .class_of_object(v)
                    .ok_or_else(|| VmError::Internal("check-cast non-object".into()))?;
                if !vm.is_assignable(from, target)? {
                    let desc = vm.dexs[dex as usize]
                        .file
                        .type_descriptor(insn.index)?
                        .to_string();
                    let name = crate::vm::descriptor_to_dotted(&desc);
                    let msg = format!("class {} cannot be cast to class {}", vm.class_desc_of_id(from), name);
                    return Err(vm.throw_new("Ljava/lang/ClassCastException;", Some(&msg)));
                }
            }
            advance(vm, t, fi, insn.size);
        }
        0x20 => {
            let v = reg(vm, t, fi, r1);
            let result = if v == NULL {
                0
            } else {
                let dex = frame_dex(vm, t, fi);
                let target = cached_class(vm, dex, insn.index)?;
                let from = vm
                    .class_of_object(v)
                    .ok_or_else(|| VmError::Internal("instance-of non-object".into()))?;
                vm.is_assignable(from, target)? as u32
            };
            set_reg(vm, t, fi, r0, result);
            advance(vm, t, fi, insn.size);
        }

        // ---- arrays ----
        0x21 => {
            let h = reg(vm, t, fi, r1);
            let len = match vm.heap.get(h) {
                Some(Object::Array(a)) => a.len() as u32,
                Some(_) => {
                    return Err(vm.throw_new(
                        "Ljava/lang/IllegalArgumentException;",
                        Some("array-length on non-array"),
                    ))
                }
                None => return Err(vm.throw_new("Ljava/lang/NullPointerException;", None)),
            };
            set_reg(vm, t, fi, r0, len);
            advance(vm, t, fi, insn.size);
        }
        0x22 => {
            exec_new_instance(vm, t, fi, r0, insn.index)?;
            advance(vm, t, fi, insn.size);
        }
        0x23 => {
            let len = reg(vm, t, fi, r1) as i32;
            exec_new_array(vm, t, fi, r0, insn.index, len)?;
            advance(vm, t, fi, insn.size);
        }
        0x24..=0x25 => {
            let h = filled_new_array(vm, t, fi, insn)?;
            // `filled-new-array` leaves the result for the following
            // `move-result-object`, exactly like an invoke.
            vm.threads[t].frames[fi].pending_result = h as u64;
            vm.threads[t].frames[fi].pending_result_is_ref = true;
            vm.threads[t].frames[fi].has_pending_result = true;
            advance(vm, t, fi, insn.size);
        }
        0x26 => {
            let arr = reg(vm, t, fi, r0);
            let payload_pc = rel_target(pc, insn.target);
            let payload = {
                let f = &vm.threads[t].frames[fi];
                let code = code_of(vm, f.method)
                    .ok_or_else(|| VmError::Internal("no code for fill-array-data".into()))?;
                acos_dex::decode_payload(&code.raw, payload_pc as usize)
            }
            .map_err(|e| VmError::Internal(format!("fill-array-data payload: {e}")))?;
            if let acos_dex::Payload::FillArrayData(fad) = payload {
                fill_array_data(vm, arr, &fad)?;
            }
            advance(vm, t, fi, insn.size);
        }
        0x27 => {
            let h = reg(vm, t, fi, r0);
            if h == NULL {
                return Err(vm.throw_new("Ljava/lang/NullPointerException;", None));
            }
            vm.capture_stack(h, t);
            return Err(VmError::JavaException(h));
        }
        0x28..=0x2a => {
            let target = rel_target(pc, insn.target);
            branch_to(vm, t, fi, target)?;
        }
        0x2b..=0x2c => {
            let value = reg(vm, t, fi, r0) as i32;
            let payload_pc = rel_target(pc, insn.target);
            let payload = {
                let f = &vm.threads[t].frames[fi];
                let code = code_of(vm, f.method)
                    .ok_or_else(|| VmError::Internal("no code for switch".into()))?;
                acos_dex::decode_payload(&code.raw, payload_pc as usize)
            }
            .map_err(|e| VmError::Internal(format!("switch payload: {e}")))?;
            let target = match payload {
                acos_dex::Payload::PackedSwitch(sw) => {
                    let base = sw.keys.first().copied().unwrap_or(0) as i64;
                    let idx = value as i64 - base;
                    if idx >= 0 && (idx as usize) < sw.keys.len() {
                        Some(pc as i64 + sw.targets[idx as usize] as i64)
                    } else {
                        None
                    }
                }
                acos_dex::Payload::SparseSwitch(sw) => sw
                    .keys
                    .iter()
                    .position(|k| *k == value)
                    .map(|i| pc as i64 + sw.targets[i] as i64),
                _ => None,
            };
            match target {
                Some(tp) => branch_to(vm, t, fi, tp as u32)?,
                None => advance(vm, t, fi, insn.size),
            }
        }

        // ---- compare ----
        0x2d..=0x2e => {
            let a = f32::from_bits(reg(vm, t, fi, r1));
            let b = f32::from_bits(reg(vm, t, fi, r2));
            let r: i32 = if a.is_nan() || b.is_nan() {
                if op == 0x2d {
                    -1
                } else {
                    1
                }
            } else if a < b {
                -1
            } else if a > b {
                1
            } else {
                0
            };
            set_reg(vm, t, fi, r0, r as u32);
            advance(vm, t, fi, insn.size);
        }
        0x2f..=0x30 => {
            let a = f64::from_bits(get_wide(vm, t, fi, r1));
            let b = f64::from_bits(get_wide(vm, t, fi, r2));
            let r: i32 = if a.is_nan() || b.is_nan() {
                if op == 0x2f {
                    -1
                } else {
                    1
                }
            } else if a < b {
                -1
            } else if a > b {
                1
            } else {
                0
            };
            set_reg(vm, t, fi, r0, r as u32);
            advance(vm, t, fi, insn.size);
        }
        0x31 => {
            let a = get_wide(vm, t, fi, r1) as i64;
            let b = get_wide(vm, t, fi, r2) as i64;
            let r = if a < b {
                -1i32
            } else if a > b {
                1
            } else {
                0
            };
            set_reg(vm, t, fi, r0, r as u32);
            advance(vm, t, fi, insn.size);
        }

        // ---- if ----
        0x32..=0x37 => {
            let a = reg(vm, t, fi, r0);
            let b = reg(vm, t, fi, r1);
            let take = match op {
                0x32 => a == b,
                0x33 => a != b,
                0x34 => (a as i32) < (b as i32),
                0x35 => (a as i32) >= (b as i32),
                0x36 => (a as i32) > (b as i32),
                _ => (a as i32) <= (b as i32),
            };
            if take {
                let target = rel_target(pc, insn.target);
                branch_to(vm, t, fi, target)?;
            } else {
                advance(vm, t, fi, insn.size);
            }
        }
        0x38..=0x3d => {
            let a = reg(vm, t, fi, r0) as i32;
            let take = match op {
                0x38 => a == 0,
                0x39 => a != 0,
                0x3a => a < 0,
                0x3b => a >= 0,
                0x3c => a > 0,
                _ => a <= 0,
            };
            if take {
                let target = rel_target(pc, insn.target);
                branch_to(vm, t, fi, target)?;
            } else {
                advance(vm, t, fi, insn.size);
            }
        }

        // ---- array get ----
        0x44..=0x4a => {
            let arr = reg(vm, t, fi, r1);
            let idx = reg(vm, t, fi, r2) as i32;
            array_get(vm, t, fi, r0, arr, idx, op)?;
            advance(vm, t, fi, insn.size);
        }
        // ---- array put ----
        0x4b..=0x51 => {
            let arr = reg(vm, t, fi, r1);
            let idx = reg(vm, t, fi, r2) as i32;
            array_put(vm, t, fi, r0, arr, idx, op)?;
            advance(vm, t, fi, insn.size);
        }

        // ---- instance fields ----
        0x52..=0x58 => {
            let obj = reg(vm, t, fi, r1);
            if obj == NULL {
                return Err(vm.throw_new("Ljava/lang/NullPointerException;", None));
            }
            let dex = frame_dex(vm, t, fi);
            let f = vm.resolve_field(dex, insn.index)?;
            let off = f.offset as usize;
            // VM-internal shadow fields (`String.count`/`hash`, `Class`, ...)
            // take precedence; `Some` means the hook handled the read and the
            // normal instance-field path must be skipped. The hook materialises
            // descriptor `String`s and probes them, so only reach it when the
            // declaring class is one of the three bridged classes: for ordinary
            // user fields this skips two allocations and the whole probe.
            let internal = f.class == vm.c_object || f.class == vm.c_class || f.class == vm.c_string;
            let handled = if internal {
                crate::bridge::internal_field_get(vm, obj, dex, insn.index)
            } else {
                None
            };
            if let Some(v) = handled {
                if op == 0x54 {
                    set_ref(vm, t, fi, r0, v);
                } else {
                    set_reg(vm, t, fi, r0, apply_sign(v, f.sign));
                }
            } else if f.width == 2 {
                let (lo, hi) = match vm.heap.get(obj) {
                    Some(Object::Instance { fields, .. }) => (fields[off], fields[off + 1]),
                    _ => (0, 0),
                };
                set_wide(vm, t, fi, r0, (lo as u64) | ((hi as u64) << 32));
            } else if op == 0x54 {
                let v = match vm.heap.get(obj) {
                    Some(Object::Instance { fields, .. }) => fields[off],
                    _ => 0,
                };
                set_ref(vm, t, fi, r0, v);
            } else {
                let v = match vm.heap.get(obj) {
                    Some(Object::Instance { fields, .. }) => fields[off],
                    _ => 0,
                };
                set_reg(vm, t, fi, r0, apply_sign(v, f.sign));
            }
            advance(vm, t, fi, insn.size);
        }
        0x59..=0x5f => {
            let obj = reg(vm, t, fi, r1);
            if obj == NULL {
                return Err(vm.throw_new("Ljava/lang/NullPointerException;", None));
            }
            let dex = frame_dex(vm, t, fi);
            let f = vm.resolve_field(dex, insn.index)?;
            let off = f.offset as usize;
            let internal = f.class == vm.c_object || f.class == vm.c_class || f.class == vm.c_string;
            if internal && crate::bridge::internal_field_put(vm, obj, dex, insn.index) {
                // Shadow String fields (`hash`) are cached in a side table.
            } else if f.width == 2 {
                let val = get_wide(vm, t, fi, r0);
                if let Some(Object::Instance { fields, .. }) = vm.heap.get_mut(obj) {
                    fields[off] = val as u32;
                    fields[off + 1] = (val >> 32) as u32;
                }
            } else if op == 0x5b {
                let val = reg(vm, t, fi, r0);
                if let Some(Object::Instance { fields, .. }) = vm.heap.get_mut(obj) {
                    fields[off] = val;
                }
            } else {
                let val = reg(vm, t, fi, r0);
                if let Some(Object::Instance { fields, .. }) = vm.heap.get_mut(obj) {
                    fields[off] = apply_sign(val, f.sign);
                }
            }
            advance(vm, t, fi, insn.size);
        }

        // ---- static fields ----
        0x60..=0x66 => {
            let dex = frame_dex(vm, t, fi);
            let f = vm.resolve_field(dex, insn.index)?;
            vm.initialize_class(f.class)?;
            let off = f.offset as usize;
            let val = if f.width == 2 {
                let c = &vm.classes[f.class as usize];
                (c.statics[off] as u64) | ((c.statics[off + 1] as u64) << 32)
            } else {
                vm.classes[f.class as usize].statics[off] as u64
            };
            if f.width == 2 {
                set_wide(vm, t, fi, r0, val);
            } else if op == 0x62 {
                set_ref(vm, t, fi, r0, val as u32);
            } else {
                set_reg(vm, t, fi, r0, apply_sign(val as u32, f.sign));
            }
            advance(vm, t, fi, insn.size);
        }
        0x67..=0x6d => {
            let dex = frame_dex(vm, t, fi);
            let f = vm.resolve_field(dex, insn.index)?;
            vm.initialize_class(f.class)?;
            let off = f.offset as usize;
            if f.width == 2 {
                let val = get_wide(vm, t, fi, r0);
                let c = &mut vm.classes[f.class as usize];
                c.statics[off] = val as u32;
                c.statics[off + 1] = (val >> 32) as u32;
            } else {
                let val = reg(vm, t, fi, r0);
                let c = &mut vm.classes[f.class as usize];
                c.statics[off] = if op == 0x69 {
                    val
                } else {
                    apply_sign(val, f.sign)
                };
            }
            advance(vm, t, fi, insn.size);
        }

        // ---- invokes ----
        0x6e..=0x72 | 0x74..=0x78 => {
            invoke(vm, t, fi, insn)?;
        }

        // ---- unary ----
        0x7b => {
            let a = reg(vm, t, fi, r1) as i32;
            set_reg(vm, t, fi, r0, a.wrapping_neg() as u32);
            advance(vm, t, fi, insn.size);
        }
        0x7c => {
            let a = reg(vm, t, fi, r1) as i32;
            set_reg(vm, t, fi, r0, (!a) as u32);
            advance(vm, t, fi, insn.size);
        }
        0x7d => {
            let a = get_wide(vm, t, fi, r1) as i64;
            set_wide(vm, t, fi, r0, a.wrapping_neg() as u64);
            advance(vm, t, fi, insn.size);
        }
        0x7e => {
            let a = get_wide(vm, t, fi, r1) as i64;
            set_wide(vm, t, fi, r0, (!a) as u64);
            advance(vm, t, fi, insn.size);
        }
        0x7f => {
            let a = f32::from_bits(reg(vm, t, fi, r1));
            set_reg(vm, t, fi, r0, (-a).to_bits());
            advance(vm, t, fi, insn.size);
        }
        0x80 => {
            let a = f64::from_bits(get_wide(vm, t, fi, r1));
            set_wide(vm, t, fi, r0, (-a).to_bits());
            advance(vm, t, fi, insn.size);
        }
        0x81..=0x8f => {
            convert(vm, t, fi, insn);
            advance(vm, t, fi, insn.size);
        }

        // ---- 3-register int/long/float/double arithmetic ----
        0x90..=0xaf => {
            arith3(vm, t, fi, insn)?;
            advance(vm, t, fi, insn.size);
        }
        // ---- /2addr arithmetic ----
        0xb0..=0xcf => {
            arith2addr(vm, t, fi, insn)?;
            advance(vm, t, fi, insn.size);
        }
        // ---- /lit16 and /lit8 ----
        0xd0..=0xd7 => {
            arith_lit16(vm, t, fi, insn)?;
            advance(vm, t, fi, insn.size);
        }
        0xd8..=0xe2 => {
            arith_lit8(vm, t, fi, insn)?;
            advance(vm, t, fi, insn.size);
        }

        0xfa..=0xfd => {
            return Err(VmError::unsupported_bytecode(
                format!("{} (invoke-polymorphic/custom)", insn.mnemonic()),
                format!("{} pc={pc}", vm.frames_label(t, fi)),
            ));
        }
        0xfe..=0xff => {
            return Err(VmError::unsupported_bytecode(
                insn.mnemonic(),
                format!("{} pc={pc}", vm.frames_label(t, fi)),
            ));
        }

        0x00 => {
            // nop (including payload pseudo-instructions encountered as nop)
            advance(vm, t, fi, insn.size);
        }

        other => {
            return Err(VmError::unsupported_bytecode(
                format!("0x{other:02x}"),
                format!("{} pc={pc}", vm.frames_label(t, fi)),
            ));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Field descriptor helpers
// ---------------------------------------------------------------------------

/// Narrow/widen a stored field word to the register representation. `sign` is
/// cached in the resolved-field entry, so no descriptor string is built.
#[inline]
pub(crate) fn apply_sign(v: u32, sign: FieldSign) -> u32 {
    match sign {
        FieldSign::Byte => (v as i8) as i32 as u32,
        FieldSign::Short => (v as i16) as i32 as u32,
        FieldSign::Char => (v as u16) as u32,
        FieldSign::Bool => (v != 0) as u32,
        FieldSign::None => v,
    }
}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

fn const_string(vm: &mut Vm, dex: u32, index: u32) -> VmResult<Handle> {
    vm.dex_string(dex, index)
}

/// Resolve a DEX type index to a class id, cached per DEX. `new-instance`,
/// `const-class`, `check-cast` and `instance-of` used to build a descriptor
/// `String` and run a global `HashMap` lookup on every execution even though
/// `DexCtx::class_cache` existed for exactly this.
fn cached_class(vm: &mut Vm, dex: u32, type_idx: u32) -> VmResult<ClassId> {
    if let Some(cid) = vm.dexs[dex as usize]
        .class_cache
        .get(type_idx as usize)
        .copied()
        .flatten()
    {
        return Ok(cid);
    }
    let desc = vm.dexs[dex as usize].file.type_descriptor(type_idx)?.to_string();
    let cid = vm.load_class(&desc)?;
    if let Some(slot) = vm.dexs[dex as usize].class_cache.get_mut(type_idx as usize) {
        *slot = Some(cid);
    }
    Ok(cid)
}

// ---------------------------------------------------------------------------
// Arrays
// ---------------------------------------------------------------------------

fn array_get(vm: &mut Vm, t: usize, fi: usize, dst: u16, arr: Handle, idx: i32, op: u8) -> VmResult<()> {
    let len = match vm.heap.get(arr) {
        Some(Object::Array(a)) => a.len(),
        None => return Err(vm.throw_new("Ljava/lang/NullPointerException;", None)),
        _ => return Err(VmError::Internal("aget on non-array".into())),
    };
    if idx < 0 || idx as usize >= len {
        let msg = format!("Index {idx} out of bounds for length {len}");
        return Err(vm.throw_new("Ljava/lang/ArrayIndexOutOfBoundsException;", Some(&msg)));
    }
    match vm.heap.get(arr) {
        Some(Object::Array(a)) => match &a.data {
            ArrayData::Bool(v) => set_reg(vm, t, fi, dst, v[idx as usize] as u32),
            ArrayData::Byte(v) => set_reg(vm, t, fi, dst, v[idx as usize] as i32 as u32),
            ArrayData::Char(v) => set_reg(vm, t, fi, dst, v[idx as usize] as u32),
            ArrayData::Short(v) => set_reg(vm, t, fi, dst, v[idx as usize] as i32 as u32),
            ArrayData::Int(v) => set_reg(vm, t, fi, dst, v[idx as usize] as u32),
            ArrayData::Long(v) => set_wide(vm, t, fi, dst, v[idx as usize] as u64),
            ArrayData::Float(v) => set_reg(vm, t, fi, dst, v[idx as usize].to_bits()),
            ArrayData::Double(v) => set_wide(vm, t, fi, dst, v[idx as usize].to_bits()),
            ArrayData::Ref(v) => {
                if op == 0x46 {
                    set_ref(vm, t, fi, dst, v[idx as usize])
                } else {
                    set_reg(vm, t, fi, dst, v[idx as usize])
                }
            }
        },
        _ => unreachable!(),
    }
    Ok(())
}

fn array_put(vm: &mut Vm, t: usize, fi: usize, src: u16, arr: Handle, idx: i32, op: u8) -> VmResult<()> {
    let (len, elem_desc) = match vm.heap.get(arr) {
        Some(Object::Array(a)) => (a.len(), a.elem_desc.clone()),
        None => return Err(vm.throw_new("Ljava/lang/NullPointerException;", None)),
        _ => return Err(VmError::Internal("aput on non-array".into())),
    };
    if idx < 0 || idx as usize >= len {
        let msg = format!("Index {idx} out of bounds for length {len}");
        return Err(vm.throw_new("Ljava/lang/ArrayIndexOutOfBoundsException;", Some(&msg)));
    }
    let rv = reg(vm, t, fi, src);
    let rv_wide = if op == 0x4c {
        get_wide(vm, t, fi, src)
    } else {
        0
    };
    if op == 0x4d {
        // aput-object: check ArrayStoreException
        if rv != NULL {
            let comp_desc = elem_desc.clone();
            let comp = vm.load_class(&comp_desc)?;
            let from = vm
                .class_of_object(rv)
                .ok_or_else(|| VmError::Internal("aput-object non-object".into()))?;
            if !vm.is_assignable(from, comp)? {
                let msg = format!(
                    "array element type mismatch: cannot store {} into {}",
                    vm.class_desc_of_id(from),
                    comp_desc
                );
                return Err(vm.throw_new("Ljava/lang/ArrayStoreException;", Some(&msg)));
            }
        }
    }
    match vm.heap.get_mut(arr) {
        Some(Object::Array(a)) => match &mut a.data {
            ArrayData::Bool(v) => v[idx as usize] = rv != 0,
            ArrayData::Byte(v) => v[idx as usize] = rv as i8,
            ArrayData::Char(v) => v[idx as usize] = rv as u16,
            ArrayData::Short(v) => v[idx as usize] = rv as i16,
            ArrayData::Int(v) => v[idx as usize] = rv as i32,
            ArrayData::Long(v) => v[idx as usize] = rv_wide as i64,
            ArrayData::Float(v) => v[idx as usize] = f32::from_bits(rv),
            ArrayData::Double(v) => v[idx as usize] = f64::from_bits(rv_wide),
            ArrayData::Ref(v) => v[idx as usize] = rv,
        },
        _ => unreachable!(),
    }
    Ok(())
}

fn filled_new_array(vm: &mut Vm, t: usize, fi: usize, insn: &Insn) -> VmResult<Handle> {
    let dex = frame_dex(vm, t, fi);
    let arr_desc = vm.dexs[dex as usize].file.type_descriptor(insn.index)?.to_string();
    let elem = arr_desc.strip_prefix('[').unwrap_or(&arr_desc).to_string();
    let count = insn.reg_count as usize;
    // Non-range: regs[0..count] are the argument registers.
    // Range: regs[0] is the base and the arguments are consecutive.
    let starts: Vec<u16> = if insn.op == 0x25 {
        (0..count).map(|i| insn.regs[0] + i as u16).collect()
    } else {
        insn.regs[..count].to_vec()
    };
    let obj = crate::vm::simple_array_object(&elem, count)?;
    let h = vm.alloc(obj)?;
    vm.native_roots.push(h);
    let mut vals: Vec<u64> = Vec::with_capacity(count);
    for &s in &starts {
        vals.push(reg(vm, t, fi, s) as u64);
    }
    fill_array_from_regs(vm, h, &vals)?;
    vm.native_roots.pop();
    Ok(h)
}

fn fill_array_from_regs(vm: &mut Vm, h: Handle, vals: &[u64]) -> VmResult<()> {
    if let Some(Object::Array(a)) = vm.heap.get_mut(h) {
        match &mut a.data {
            ArrayData::Bool(v) => {
                for (i, val) in vals.iter().enumerate() {
                    v[i] = *val != 0;
                }
            }
            ArrayData::Byte(v) => {
                for (i, val) in vals.iter().enumerate() {
                    v[i] = *val as i8;
                }
            }
            ArrayData::Char(v) => {
                for (i, val) in vals.iter().enumerate() {
                    v[i] = *val as u16;
                }
            }
            ArrayData::Short(v) => {
                for (i, val) in vals.iter().enumerate() {
                    v[i] = *val as i16;
                }
            }
            ArrayData::Int(v) => {
                for (i, val) in vals.iter().enumerate() {
                    v[i] = *val as i32;
                }
            }
            ArrayData::Float(v) => {
                for (i, val) in vals.iter().enumerate() {
                    v[i] = f32::from_bits(*val as u32);
                }
            }
            ArrayData::Ref(v) => {
                for (i, val) in vals.iter().enumerate() {
                    v[i] = *val as u32;
                }
            }
            ArrayData::Long(v) => {
                for (i, pair) in vals.chunks(2).enumerate() {
                    if i >= v.len() {
                        break;
                    }
                    v[i] = (pair[0] | (pair.get(1).copied().unwrap_or(0) << 32)) as i64;
                }
            }
            ArrayData::Double(v) => {
                for (i, pair) in vals.chunks(2).enumerate() {
                    if i >= v.len() {
                        break;
                    }
                    let bits = pair[0] | (pair.get(1).copied().unwrap_or(0) << 32);
                    v[i] = f64::from_bits(bits);
                }
            }
        }
    }
    Ok(())
}

fn fill_array_data(vm: &mut Vm, arr: Handle, fad: &acos_dex::FillArrayDataPayload) -> VmResult<()> {
    if let Some(Object::Array(a)) = vm.heap.get_mut(arr) {
        let w = fad.element_width as usize;
        let n = fad.size as usize;
        for i in 0..n {
            let off = i * w;
            if off + w > fad.data.len() {
                break;
            }
            let bytes = &fad.data[off..off + w];
            let mut raw: u64 = 0;
            for (j, &b) in bytes.iter().enumerate() {
                raw |= (b as u64) << (8 * j);
            }
            match &mut a.data {
                ArrayData::Bool(v) => v[i] = raw != 0,
                ArrayData::Byte(v) => v[i] = raw as i8,
                ArrayData::Char(v) => v[i] = raw as u16,
                ArrayData::Short(v) => v[i] = raw as i16,
                ArrayData::Int(v) => v[i] = raw as i32,
                ArrayData::Long(v) => v[i] = raw as i64,
                ArrayData::Float(v) => v[i] = f32::from_bits(raw as u32),
                ArrayData::Double(v) => v[i] = f64::from_bits(raw),
                ArrayData::Ref(_) => {}
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Conversions
// ---------------------------------------------------------------------------

fn convert(vm: &mut Vm, t: usize, fi: usize, insn: &Insn) {
    let r0 = insn.regs[0];
    let r1 = insn.regs[1];
    match insn.op {
        0x81 => {
            let v = reg(vm, t, fi, r1) as i32 as i64;
            set_wide(vm, t, fi, r0, v as u64);
        }
        0x82 => {
            let v = reg(vm, t, fi, r1) as i32 as f32;
            set_reg(vm, t, fi, r0, v.to_bits());
        }
        0x83 => {
            let v = reg(vm, t, fi, r1) as i32 as f64;
            set_wide(vm, t, fi, r0, v.to_bits());
        }
        0x84 => {
            let v = get_wide(vm, t, fi, r1) as i64 as i32;
            set_reg(vm, t, fi, r0, v as u32);
        }
        0x85 => {
            let v = get_wide(vm, t, fi, r1) as i64 as f32;
            set_reg(vm, t, fi, r0, v.to_bits());
        }
        0x86 => {
            let v = get_wide(vm, t, fi, r1) as i64 as f64;
            set_wide(vm, t, fi, r0, v.to_bits());
        }
        0x87 => {
            let v = f32::from_bits(reg(vm, t, fi, r1)) as i32;
            set_reg(vm, t, fi, r0, v as u32);
        }
        0x88 => {
            let v = f32::from_bits(reg(vm, t, fi, r1)) as i64;
            set_wide(vm, t, fi, r0, v as u64);
        }
        0x89 => {
            let v = f32::from_bits(reg(vm, t, fi, r1)) as f64;
            set_wide(vm, t, fi, r0, v.to_bits());
        }
        0x8a => {
            let v = f64::from_bits(get_wide(vm, t, fi, r1)) as i32;
            set_reg(vm, t, fi, r0, v as u32);
        }
        0x8b => {
            let v = f64::from_bits(get_wide(vm, t, fi, r1)) as i64;
            set_wide(vm, t, fi, r0, v as u64);
        }
        0x8c => {
            let v = f64::from_bits(get_wide(vm, t, fi, r1)) as f32;
            set_reg(vm, t, fi, r0, v.to_bits());
        }
        0x8d => {
            let v = reg(vm, t, fi, r1) as i32 as i8 as i32;
            set_reg(vm, t, fi, r0, v as u32);
        }
        0x8e => {
            let v = reg(vm, t, fi, r1) as u16;
            set_reg(vm, t, fi, r0, v as u32);
        }
        0x8f => {
            let v = reg(vm, t, fi, r1) as i32 as i16 as i32;
            set_reg(vm, t, fi, r0, v as u32);
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Arithmetic
// ---------------------------------------------------------------------------

pub(crate) fn div_int(a: i32, b: i32) -> Result<i32, ()> {
    if b == 0 {
        Err(())
    } else if a == i32::MIN && b == -1 {
        Ok(i32::MIN)
    } else {
        Ok(a / b)
    }
}

pub(crate) fn rem_int(a: i32, b: i32) -> Result<i32, ()> {
    if b == 0 {
        Err(())
    } else if a == i32::MIN && b == -1 {
        Ok(0)
    } else {
        Ok(a % b)
    }
}

pub(crate) fn div_long(a: i64, b: i64) -> Result<i64, ()> {
    if b == 0 {
        Err(())
    } else if a == i64::MIN && b == -1 {
        Ok(i64::MIN)
    } else {
        Ok(a / b)
    }
}

pub(crate) fn rem_long(a: i64, b: i64) -> Result<i64, ()> {
    if b == 0 {
        Err(())
    } else if a == i64::MIN && b == -1 {
        Ok(0)
    } else {
        Ok(a % b)
    }
}

fn arith3(vm: &mut Vm, t: usize, fi: usize, insn: &Insn) -> VmResult<()> {
    let r0 = insn.regs[0];
    let r1 = insn.regs[1];
    let r2 = insn.regs[2];
    match insn.op {
        0x90 => {
            let (a, b) = (reg(vm, t, fi, r1) as i32, reg(vm, t, fi, r2) as i32);
            set_reg(vm, t, fi, r0, a.wrapping_add(b) as u32)
        }
        0x91 => {
            let (a, b) = (reg(vm, t, fi, r1) as i32, reg(vm, t, fi, r2) as i32);
            set_reg(vm, t, fi, r0, a.wrapping_sub(b) as u32)
        }
        0x92 => {
            let (a, b) = (reg(vm, t, fi, r1) as i32, reg(vm, t, fi, r2) as i32);
            set_reg(vm, t, fi, r0, a.wrapping_mul(b) as u32)
        }
        0x93 => {
            let (a, b) = (reg(vm, t, fi, r1) as i32, reg(vm, t, fi, r2) as i32);
            match div_int(a, b) {
                Ok(v) => set_reg(vm, t, fi, r0, v as u32),
                Err(()) => return Err(vm.throw_new("Ljava/lang/ArithmeticException;", Some("/ by zero"))),
            }
        }
        0x94 => {
            let (a, b) = (reg(vm, t, fi, r1) as i32, reg(vm, t, fi, r2) as i32);
            match rem_int(a, b) {
                Ok(v) => set_reg(vm, t, fi, r0, v as u32),
                Err(()) => return Err(vm.throw_new("Ljava/lang/ArithmeticException;", Some("/ by zero"))),
            }
        }
        0x95 => {
            let (a, b) = (reg(vm, t, fi, r1), reg(vm, t, fi, r2));
            set_reg(vm, t, fi, r0, a & b)
        }
        0x96 => {
            let (a, b) = (reg(vm, t, fi, r1), reg(vm, t, fi, r2));
            set_reg(vm, t, fi, r0, a | b)
        }
        0x97 => {
            let (a, b) = (reg(vm, t, fi, r1), reg(vm, t, fi, r2));
            set_reg(vm, t, fi, r0, a ^ b)
        }
        0x98 => {
            let (a, b) = (reg(vm, t, fi, r1) as i32, reg(vm, t, fi, r2));
            set_reg(vm, t, fi, r0, a.wrapping_shl(b & 0x1f) as u32)
        }
        0x99 => {
            let (a, b) = (reg(vm, t, fi, r1) as i32, reg(vm, t, fi, r2));
            set_reg(vm, t, fi, r0, a.wrapping_shr(b & 0x1f) as u32)
        }
        0x9a => {
            let (a, b) = (reg(vm, t, fi, r1), reg(vm, t, fi, r2));
            set_reg(vm, t, fi, r0, a.wrapping_shr(b & 0x1f))
        }
        0x9b..=0xa5 => {
            let a = get_wide(vm, t, fi, r1) as i64;
            // `shl-long`/`shr-long`/`ushr-long` take an *int* shift amount in
            // `vCC`; the other long binary ops take a wide `vCC`.
            let v: i64 = if (0xa3..=0xa5).contains(&insn.op) {
                let sh = (reg(vm, t, fi, r2) as i32 & 0x3f) as u32;
                match insn.op {
                    0xa3 => (a as u64).wrapping_shl(sh) as i64,
                    0xa4 => a.wrapping_shr(sh),
                    _ => ((a as u64).wrapping_shr(sh)) as i64,
                }
            } else {
                let b = get_wide(vm, t, fi, r2) as i64;
                match insn.op {
                    0x9b => a.wrapping_add(b),
                    0x9c => a.wrapping_sub(b),
                    0x9d => a.wrapping_mul(b),
                    0x9e => match div_long(a, b) {
                        Ok(v) => v,
                        Err(()) => return Err(vm.throw_new("Ljava/lang/ArithmeticException;", Some("/ by zero"))),
                    },
                    0x9f => match rem_long(a, b) {
                        Ok(v) => v,
                        Err(()) => return Err(vm.throw_new("Ljava/lang/ArithmeticException;", Some("/ by zero"))),
                    },
                    0xa0 => a & b,
                    0xa1 => a | b,
                    _ => a ^ b,
                }
            };
            set_wide(vm, t, fi, r0, v as u64);
        }
        0xa6..=0xaf => {
            let is_double = insn.op >= 0xab;
            if is_double {
                let a = f64::from_bits(get_wide(vm, t, fi, r1));
                let b = f64::from_bits(get_wide(vm, t, fi, r2));
                let v = match insn.op {
                    0xab => a + b,
                    0xac => a - b,
                    0xad => a * b,
                    0xae => a / b,
                    _ => a % b,
                };
                set_wide(vm, t, fi, r0, v.to_bits());
            } else {
                let a = f32::from_bits(reg(vm, t, fi, r1));
                let b = f32::from_bits(reg(vm, t, fi, r2));
                let v = match insn.op {
                    0xa6 => a + b,
                    0xa7 => a - b,
                    0xa8 => a * b,
                    0xa9 => a / b,
                    _ => a % b,
                };
                set_reg(vm, t, fi, r0, v.to_bits());
            }
        }
        _ => {}
    }
    Ok(())
}

fn arith2addr(vm: &mut Vm, t: usize, fi: usize, insn: &Insn) -> VmResult<()> {
    let r0 = insn.regs[0];
    let r1 = insn.regs[1];
    match insn.op {
        0xb0..=0xba => {
            let a = reg(vm, t, fi, r0) as i32;
            let b = reg(vm, t, fi, r1);
            let v: i32 = match insn.op {
                0xb0 => a.wrapping_add(b as i32),
                0xb1 => a.wrapping_sub(b as i32),
                0xb2 => a.wrapping_mul(b as i32),
                0xb3 => match div_int(a, b as i32) {
                    Ok(v) => v,
                    Err(()) => return Err(vm.throw_new("Ljava/lang/ArithmeticException;", Some("/ by zero"))),
                },
                0xb4 => match rem_int(a, b as i32) {
                    Ok(v) => v,
                    Err(()) => return Err(vm.throw_new("Ljava/lang/ArithmeticException;", Some("/ by zero"))),
                },
                0xb5 => a & b as i32,
                0xb6 => a | b as i32,
                0xb7 => a ^ b as i32,
                0xb8 => a.wrapping_shl(b & 0x1f),
                0xb9 => a.wrapping_shr(b & 0x1f),
                _ => (a as u32).wrapping_shr(b & 0x1f) as i32,
            };
            set_reg(vm, t, fi, r0, v as u32);
        }
        0xbb..=0xc5 => {
            let a = get_wide(vm, t, fi, r0) as i64;
            // Long shift /2addr forms take an int shift amount in `vB`.
            let v: i64 = if (0xc3..=0xc5).contains(&insn.op) {
                let sh = (reg(vm, t, fi, r1) as i32 & 0x3f) as u32;
                match insn.op {
                    0xc3 => ((a as u64).wrapping_shl(sh)) as i64,
                    0xc4 => a.wrapping_shr(sh),
                    _ => ((a as u64).wrapping_shr(sh)) as i64,
                }
            } else {
                let b = get_wide(vm, t, fi, r1) as i64;
                match insn.op {
                    0xbb => a.wrapping_add(b),
                    0xbc => a.wrapping_sub(b),
                    0xbd => a.wrapping_mul(b),
                    0xbe => match div_long(a, b) {
                        Ok(v) => v,
                        Err(()) => return Err(vm.throw_new("Ljava/lang/ArithmeticException;", Some("/ by zero"))),
                    },
                    0xbf => match rem_long(a, b) {
                        Ok(v) => v,
                        Err(()) => return Err(vm.throw_new("Ljava/lang/ArithmeticException;", Some("/ by zero"))),
                    },
                    0xc0 => a & b,
                    0xc1 => a | b,
                    _ => a ^ b,
                }
            };
            set_wide(vm, t, fi, r0, v as u64);
        }
        0xc6..=0xcf => {
            if insn.op >= 0xcb {
                let a = f64::from_bits(get_wide(vm, t, fi, r0));
                let b = f64::from_bits(get_wide(vm, t, fi, r1));
                let v = match insn.op {
                    0xcb => a + b,
                    0xcc => a - b,
                    0xcd => a * b,
                    0xce => a / b,
                    _ => a % b,
                };
                set_wide(vm, t, fi, r0, v.to_bits());
            } else {
                let a = f32::from_bits(reg(vm, t, fi, r0));
                let b = f32::from_bits(reg(vm, t, fi, r1));
                let v = match insn.op {
                    0xc6 => a + b,
                    0xc7 => a - b,
                    0xc8 => a * b,
                    0xc9 => a / b,
                    _ => a % b,
                };
                set_reg(vm, t, fi, r0, v.to_bits());
            }
        }
        _ => {}
    }
    Ok(())
}

fn arith_lit16(vm: &mut Vm, t: usize, fi: usize, insn: &Insn) -> VmResult<()> {
    let r0 = insn.regs[0];
    let r1 = insn.regs[1];
    let lit = insn.literal as i32;
    let a = reg(vm, t, fi, r1) as i32;
    let v = match insn.op {
        0xd0 => a.wrapping_add(lit),
        0xd1 => lit.wrapping_sub(a),
        0xd2 => a.wrapping_mul(lit),
        0xd3 => match div_int(a, lit) {
            Ok(v) => v,
            Err(()) => return Err(vm.throw_new("Ljava/lang/ArithmeticException;", Some("/ by zero"))),
        },
        0xd4 => match rem_int(a, lit) {
            Ok(v) => v,
            Err(()) => return Err(vm.throw_new("Ljava/lang/ArithmeticException;", Some("/ by zero"))),
        },
        0xd5 => a & lit,
        0xd6 => a | lit,
        _ => a ^ lit,
    };
    set_reg(vm, t, fi, r0, v as u32);
    Ok(())
}

fn arith_lit8(vm: &mut Vm, t: usize, fi: usize, insn: &Insn) -> VmResult<()> {
    let r0 = insn.regs[0];
    let r1 = insn.regs[1];
    let lit = insn.literal as i32;
    let a = reg(vm, t, fi, r1) as i32;
    let v = match insn.op {
        0xd8 => a.wrapping_add(lit),
        0xd9 => lit.wrapping_sub(a),
        0xda => a.wrapping_mul(lit),
        0xdb => match div_int(a, lit) {
            Ok(v) => v,
            Err(()) => return Err(vm.throw_new("Ljava/lang/ArithmeticException;", Some("/ by zero"))),
        },
        0xdc => match rem_int(a, lit) {
            Ok(v) => v,
            Err(()) => return Err(vm.throw_new("Ljava/lang/ArithmeticException;", Some("/ by zero"))),
        },
        0xdd => a & lit,
        0xde => a | lit,
        0xdf => a ^ lit,
        0xe0 => a.wrapping_shl(lit as u32 & 0x1f),
        0xe1 => a.wrapping_shr(lit as u32 & 0x1f),
        _ => (a as u32).wrapping_shr(lit as u32 & 0x1f) as i32,
    };
    set_reg(vm, t, fi, r0, v as u32);
    Ok(())
}

// ---------------------------------------------------------------------------
// Invocation
// ---------------------------------------------------------------------------

fn invoke(vm: &mut Vm, t: usize, fi: usize, insn: &Insn) -> VmResult<()> {
    let (kind, range) = invoke_kind(insn.op);
    // Inlined `frame_dex`: this runs on every call.
    let dex = match &vm.methods[vm.threads[t].frames[fi].method as usize].imp {
        MethodImpl::Dex { dex, .. } => *dex,
        _ => 0,
    };
    let count = insn.reg_count as usize;
    // Argument word registers: a 35c invoke lists one register per argument
    // *word* (a wide argument contributes two entries); a range invoke uses
    // `count` consecutive registers starting at regs[0].
    let base = insn.regs[0];

    let method = match kind {
        InvokeKind::Virtual | InvokeKind::Interface => {
            let recv_reg = if range { base } else { insn.regs[0] };
            let receiver = reg(vm, t, fi, recv_reg);
            if receiver == NULL {
                return Err(vm.throw_new("Ljava/lang/NullPointerException;", None));
            }
            // Inline cache keyed by the receiver's dynamic class. The mapping
            // is a pure function of the declared signature and the receiver
            // class, so it never needs invalidation; a differently-typed
            // receiver probes a different key and resolves normally. This
            // removes the per-call name/signature allocations and the
            // `HashMap<String,_>` probes from the hot virtual/interface path.
            let mut rclass = vm.class_of_object(receiver);
            if rclass.is_none() {
                rclass = vm.array_class(receiver);
            }
            match rclass {
                Some(rc) => {
                    // Probe the one-entry inline cache first; a hit skips the
                    // tuple-keyed `HashMap` probe entirely.
                    let hit = match vm.dexs[dex as usize]
                        .virtual_cache
                        .get(insn.index as usize)
                        .copied()
                        .flatten()
                    {
                        Some((c, m)) if c == rc => Some(m),
                        _ => None,
                    };
                    match hit {
                        Some(m) => m,
                        None => {
                            let m = match vm.dispatch_cache.get(&(dex, insn.index, rc)) {
                                Some(&m) => m,
                                None => {
                                    let m =
                                        dispatch_uncached(vm, dex, insn.index, receiver, t, fi)?;
                                    vm.dispatch_cache.insert((dex, insn.index, rc), m);
                                    m
                                }
                            };
                            if let Some(slot) =
                                vm.dexs[dex as usize].virtual_cache.get_mut(insn.index as usize)
                            {
                                *slot = Some((rc, m));
                            }
                            m
                        }
                    }
                }
                None => dispatch_uncached(vm, dex, insn.index, receiver, t, fi)?,
            }
        }
        // Static/direct/super targets are resolved through the DEX method
        // cache. Probe it inline (the common case is a cached hit) and only
        // fall back to the general resolver on a miss.
        _ => {
            let cached = vm.dexs[dex as usize]
                .method_cache
                .get(insn.index as usize)
                .copied()
                .flatten();
            match cached {
                Some(m) => m,
                None => match vm.resolve_method_ref(dex, insn.index, kind) {
                    Ok(m) => m,
                    Err(_) => return Err(unsupported_call_site(vm, dex, insn.index, t, fi)),
                },
            }
        }
    };

    let (ins_size, is_static, is_synchronized, mclass) = {
        let m = &vm.methods[method as usize];
        (m.ins_size, m.is_static(), m.is_synchronized(), m.class)
    };
    // Class initialization for static calls; the inlined `Init` check avoids
    // a function call on every static invoke after the class is initialised.
    if is_static && !matches!(vm.classes[mclass as usize].init, crate::class::InitState::Init) {
        vm.initialize_class(mclass)?;
        // A static initializer may throw and an outer frame may catch the
        // exception, unwinding this call site's frame (or resuming it at a
        // handler). In that case the invoke must be abandoned: `fi` is stale
        // and building a callee frame would read a different method's
        // registers.
        if vm.threads[t].frames.len() <= fi
            || vm.threads[t].frames[fi].pending_exception != NULL
        {
            return Ok(());
        }
    }

    let list = if range { &insn.regs[..0] } else { &insn.regs[..count] };
    let mut frame = build_frame(vm, method, range, base, list, count)?;
    // Synchronized method: acquire the monitor, blocking if another thread
    // holds it. On block we leave the pc at the invoke so it is retried.
    if is_synchronized {
        let target = if is_static {
            vm.class_object(mclass)?
        } else {
            frame.regs[frame.regs.len() - ins_size as usize]
        };
        if !try_enter_monitor(vm, target, t) {
            vm.threads[t].status = ThreadStatus::BlockedMonitor(target);
            return Ok(());
        }
        frame.monitor = Some(target);
    }
    vm.threads[t].frames[fi].advance_size = insn.size;
    vm.threads[t].frames.push(frame);
    vm.metrics.invokes += 1;
    Ok(())
}

/// Slow path for an `invoke-virtual`/`invoke-interface` inline-cache miss:
/// decode the declared name/signature and dispatch on the dynamic class.
fn dispatch_uncached(
    vm: &mut Vm,
    dex: u32,
    method_idx: u32,
    receiver: Handle,
    t: usize,
    fi: usize,
) -> VmResult<MethodId> {
    let r = vm.dexs[dex as usize].file.method_ref(method_idx)?;
    let name = r.name.to_string();
    let sig = format!("({}){}", r.parameters.concat(), r.return_type);
    match vm.dispatch(receiver, &name, &sig) {
        Ok(m) => Ok(m),
        Err(_) => Err(unsupported_call_site(vm, dex, method_idx, t, fi)),
    }
}

/// Build the structured `UNSUPPORTED_JAVA_API` error naming the exact call
/// site, preserving the pre-optimisation reporting contract.
fn unsupported_call_site(vm: &mut Vm, dex: u32, method_idx: u32, t: usize, fi: usize) -> VmError {
    let (ref_class, name, sig) = match vm.dexs[dex as usize]
        .file
        .method_ref(method_idx)
        .map_err(VmError::from)
    {
        Ok(r) => (
            r.class.to_string(),
            r.name.to_string(),
            format!("({}){}", r.parameters.concat(), r.return_type),
        ),
        Err(e) => return e,
    };
    let at = format!("{} pc={}", vm.frames_label(t, fi), vm.threads[t].frames[fi].pc);
    VmError::unsupported_api(format!("{ref_class}->{name}{sig}"), at)
}

fn invoke_kind(op: u8) -> (InvokeKind, bool) {
    match op {
        0x6e => (InvokeKind::Virtual, false),
        0x6f => (InvokeKind::Super, false),
        0x70 => (InvokeKind::Direct, false),
        0x71 => (InvokeKind::Static, false),
        0x72 => (InvokeKind::Interface, false),
        0x74 => (InvokeKind::Virtual, true),
        0x75 => (InvokeKind::Super, true),
        0x76 => (InvokeKind::Direct, true),
        0x77 => (InvokeKind::Static, true),
        0x78 => (InvokeKind::Interface, true),
        _ => (InvokeKind::Virtual, false),
    }
}

/// Build a callee frame by copying argument *words* from the caller.
///
/// `range` selects the consecutive-register form, otherwise `list` holds the
/// explicit argument words. `count` is the number of argument words. No
/// `Method` clone and no per-call `Vec` are allocated here (both used to be on
/// the hot call path).
#[inline]
fn build_frame(
    vm: &mut Vm,
    method: MethodId,
    range: bool,
    base: u16,
    list: &[u16],
    count: usize,
) -> VmResult<Frame> {
    if vm.threads[vm.current_thread].frames.len() >= vm.max_frames
        && vm.stack_overflow_object != NULL
    {
        return Err(VmError::JavaException(vm.stack_overflow_object));
    }
    let (regs_size, ins_size, class_id) = {
        let m = &vm.methods[method as usize];
        match &m.imp {
            MethodImpl::Dex { code: Some(c), .. } => {
                (c.registers_size as usize, m.ins_size as usize, m.class)
            }
            _ => (m.ins_size as usize, m.ins_size as usize, m.class),
        }
    };
    let t = vm.current_thread;
    let base_slot = regs_size - ins_size;
    let n = ins_size.min(count);
    let mut frame = acquire_frame(vm, t, method, class_id, regs_size);
    frame.advance_on_return = true;
    {
        let caller_regs = &vm.threads[t].frames.last().unwrap().regs;
        if range {
            for i in 0..n {
                let r = base.wrapping_add(i as u16);
                frame.regs[base_slot + i] = caller_regs[r as usize];
            }
        } else {
            for (i, &r) in list.iter().take(n).enumerate() {
                frame.regs[base_slot + i] = caller_regs[r as usize];
            }
        }
    }
    // Reference map for incoming slots (used by the GC).
    {
        let m = &vm.methods[method as usize];
        for (i, &is_ref) in m.ins_ref.iter().enumerate() {
            if base_slot + i < frame.refs.len() {
                frame.refs[base_slot + i] = is_ref;
            }
        }
    }
    Ok(frame)
}

/// Push a frame directly from raw parameter values (used by native callbacks
/// and the top-level `main`).
pub fn push_frame_values(
    vm: &mut Vm,
    method: MethodId,
    this: Handle,
    args: &[u64],
) -> VmResult<()> {
    if vm.threads[vm.current_thread].frames.len() >= vm.max_frames
        && vm.stack_overflow_object != NULL
    {
        return Err(VmError::JavaException(vm.stack_overflow_object));
    }
    let m = vm.methods[method as usize].clone();
    let regs_size = match &m.imp {
        MethodImpl::Dex { code: Some(c), .. } => c.registers_size as usize,
        _ => m.ins_size as usize,
    };
    let t = vm.current_thread;
    let mut frame = acquire_frame(vm, t, method, m.class, regs_size);
    let base = regs_size - m.ins_size as usize;
    let mut pos = base;
    if !m.is_static() {
        frame.regs[pos] = this;
        pos += 1;
    }
    for (i, &a) in args.iter().enumerate() {
        let w = match m.params.get(i) {
            Some(&p) => crate::class::type_width(vm.type_kind(p)),
            None => 1,
        };
        frame.regs[pos] = a as u32;
        if w == 2 {
            frame.regs[pos + 1] = (a >> 32) as u32;
        }
        pos += w as usize;
    }
    for (i, &is_ref) in m.ins_ref.iter().enumerate() {
        frame.refs[base + i] = is_ref;
    }
    vm.threads[vm.current_thread].frames.push(frame);
    vm.metrics.invokes += 1;
    Ok(())
}

/// Run the current thread until its frame depth returns to `base_depth`.
pub fn run_current_until(vm: &mut Vm, base_depth: usize) -> VmResult<()> {
    let t = vm.current_thread;
    // Set the exception floor for the duration of this synchronous call. A
    // nested synchronous call installs a deeper floor and restores this one on
    // return, so the innermost base always wins.
    let prev_floor = EXCEPTION_FLOOR.with(|f| f.replace(base_depth));
    let result = run_current_until_body(vm, t, base_depth);
    EXCEPTION_FLOOR.with(|f| f.set(prev_floor));
    result
}

fn run_current_until_body(vm: &mut Vm, t: usize, base_depth: usize) -> VmResult<()> {
    // A pending exception installed by an earlier top-level run blocks the
    // interpreter: `run_batch` returns immediately without popping the frame
    // this call just pushed, so the loop below would spin forever on a frame
    // that can never pop. Unwind down to this call's base and surface it, the
    // same way `handle_exception` unwinds to `EXCEPTION_FLOOR`.
    if vm.pending_exception != NULL {
        while vm.threads[t].frames.len() > base_depth {
            pop_frame_quiet(vm, t);
        }
        let h = vm.pending_exception;
        vm.pending_exception = NULL;
        return Err(VmError::JavaException(h));
    }
    while vm.threads[t].frames.len() > base_depth {
        let n = run_batch(vm, t, base_depth, false)?;
        vm.metrics.instructions += n;
    }
    if vm.pending_exception != NULL {
        let h = vm.pending_exception;
        vm.pending_exception = NULL;
        return Err(VmError::JavaException(h));
    }
    Ok(())
}

/// Call a Java method synchronously and return its raw 64-bit result.
pub fn call_method(
    vm: &mut Vm,
    method: MethodId,
    this: Handle,
    args: &[u64],
) -> VmResult<u64> {
    let t = vm.current_thread;
    let base = vm.threads[t].frames.len();
    push_frame_values(vm, method, this, args)?;
    run_current_until(vm, base)?;
    let caller = vm.threads[t].frames.last().unwrap();
    Ok(caller.pending_result)
}

// ---------------------------------------------------------------------------
// Monitors
// ---------------------------------------------------------------------------

fn try_enter_monitor(vm: &mut Vm, h: Handle, t: usize) -> bool {
    match vm.monitors.get_mut(&h) {
        Some((owner, count)) => {
            if *owner == t {
                *count += 1;
                true
            } else {
                false
            }
        }
        None => {
            vm.monitors.insert(h, (t, 1));
            true
        }
    }
}

fn exit_monitor(vm: &mut Vm, h: Handle) {
    if let Some((_, count)) = vm.monitors.get_mut(&h) {
        if *count > 0 {
            *count -= 1;
        }
        if *count == 0 {
            vm.monitors.remove(&h);
        }
    }
}

// ---------------------------------------------------------------------------
// Class initialization
// ---------------------------------------------------------------------------

impl Vm {
    pub fn initialize_class(&mut self, id: ClassId) -> VmResult<()> {
        use crate::class::InitState;
        match self.classes[id as usize].init {
            InitState::Init => return Ok(()),
            InitState::InProgress => return Ok(()),
            InitState::Failed => {
                let desc = self.class_desc(id).to_string();
                return Err(self.throw_new("Ljava/lang/NoClassDefFoundError;", Some(&desc)));
            }
            InitState::NotInit => {}
        }
        self.classes[id as usize].init = InitState::InProgress;
        if !self.classes[id as usize].is_interface {
            if let Some(sup) = self.classes[id as usize].super_class {
                if let Err(e) = self.initialize_class(sup) {
                    self.classes[id as usize].init = InitState::Failed;
                    return Err(e);
                }
            }
        }
        let clinit = self.classes[id as usize]
            .direct_map
            .get(&("<clinit>".to_string(), "()V".to_string()))
            .copied();
        if let Some(mid) = clinit {
            self.note_clinit();
            let t = self.current_thread;
            let base = self.threads[t].frames.len();
            push_frame_values(self, mid, NULL, &[])?;
            if let Err(e) = run_current_until(self, base) {
                self.classes[id as usize].init = InitState::Failed;
                return Err(self.wrap_in_exception_in_initializer(e));
            }
        }
        self.classes[id as usize].init = InitState::Init;
        Ok(())
    }

    /// Wrap a `<clinit>` failure in `ExceptionInInitializerError`, exactly as
    /// ART does the first time a class initialisation fails. A later access to
    /// the now-erroneous class throws `NoClassDefFoundError` (the
    /// `InitState::Failed` arm of `initialize_class`).
    fn wrap_in_exception_in_initializer(&mut self, e: VmError) -> VmError {
        let VmError::JavaException(cause) = e else {
            return e;
        };
        // Root the cause across the allocation below (the pending-exception
        // root was already released by the time we get here).
        self.native_roots.push(cause);
        let result = (|| -> VmResult<Handle> {
            if let Some(cid) = self.class_of_object(cause) {
                if self.class_desc_of_id(cid) == "java.lang.ExceptionInInitializerError" {
                    return Ok(cause);
                }
            }
            let cid = self.load_class("Ljava/lang/ExceptionInInitializerError;")?;
            let h = self.builtin_instance(cid)?;
            self.native_roots.push(h);
            // API 34 `ExceptionInInitializerError` overrides `getCause()` to
            // read its own `exception` field; the bootstrap corelib has no such
            // field and inherits `Throwable.getCause()` (the `cause` field).
            // `set_instance_field_by_name` is a no-op when the field is absent.
            self.set_instance_field_by_name(h, "exception", cause);
            self.set_instance_field_by_name(h, "cause", cause);
            self.native_roots.pop();
            Ok(h)
        })();
        self.native_roots.pop();
        match result {
            Ok(h) => VmError::JavaException(h),
            Err(e) => e,
        }
    }
}

/// Resolve the DEX index of the currently executing frame (DEX frames only).
#[inline]
fn frame_dex(vm: &Vm, t: usize, fi: usize) -> u32 {
    let method = vm.threads[t].frames[fi].method;
    match &vm.methods[method as usize].imp {
        MethodImpl::Dex { dex, .. } => *dex,
        _ => 0,
    }
}

// Small helper used by the interpreter to name the current frame.
impl Vm {
    pub fn frames_label(&self, t: usize, fi: usize) -> String {
        if let Some(f) = self.threads.get(t).and_then(|th| th.frames.get(fi)) {
            let desc = self.class_desc(f.class);
            let name = self
                .methods
                .get(f.method as usize)
                .map(|m| m.name.as_str())
                .unwrap_or("?");
            format!("{desc}.{name}")
        } else {
            "<unknown>".to_string()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integer_division_edge_semantics() {
        assert_eq!(div_int(7, 2), Ok(3));
        assert_eq!(div_int(-7, 2), Ok(-3));
        assert_eq!(div_int(7, -2), Ok(-3));
        assert_eq!(div_int(i32::MIN, -1), Ok(i32::MIN));
        assert_eq!(div_int(1, 0), Err(()));
        assert_eq!(rem_int(i32::MIN, -1), Ok(0));
        assert_eq!(rem_int(-7, 3), Ok(-1));
        assert_eq!(rem_int(1, 0), Err(()));
        assert_eq!(div_long(i64::MIN, -1), Ok(i64::MIN));
        assert_eq!(rem_long(i64::MIN, -1), Ok(0));
        assert_eq!(div_long(1, 0), Err(()));
    }

    #[test]
    fn shift_masking_matches_java() {
        // Java masks the shift distance to 5 bits for int and 6 for long.
        let a: i32 = 1;
        assert_eq!(a.wrapping_shl(32 & 0x1f), 1);
        assert_eq!(a.wrapping_shl(33 & 0x1f), 2);
        let b: i64 = 1;
        assert_eq!(b.wrapping_shl(64 & 0x3f), 1);
        assert_eq!(b.wrapping_shl(65 & 0x3f), 2);
    }
}

