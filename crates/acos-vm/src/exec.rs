//! Typed-IR execution tier ("exec").
//!
//! This is an **opt-in** alternative to [`crate::dispatch::fast_run`]: a method
//! body is decoded once into a compact [`Op`] array (operands pre-resolved,
//! branch targets as IR indices, hot pairs fused into superinstructions) and
//! executed by a tight `match` loop over `&[Op]` using the same register file
//! as the interpreter.
//!
//! Scope and safety:
//!
//! * The tier only ever executes register/heap-local opcodes and quickened
//!   field accesses with the exact semantics of the existing fast path.
//!   Anything else (invoke, return, allocation, monitors, strings, switches,
//!   every opcode that can throw) lowers to [`Op::Fallback`], at which the loop
//!   stops *before* the instruction and hands control back to the general
//!   interpreter. Catch-table semantics are therefore unchanged: every
//!   instruction that can throw is executed by the general path, which already
//!   routes exceptions through `interp::handle_exception`. Field ops that can
//!   throw (null receiver, uninitialised static owner, resolution failure) stay
//!   `Fallback` for the same reason.
//! * Frames keep the same `regs`/`refs` layout, so GC root semantics are
//!   identical (the tier never introduces a value that the interpreter would
//!   not also have in a register/ref slot).
//! * Enabled by default; set `ACOS_EXEC=interp` (or `off`/`0`), or call
//!   [`set_enabled`], to force the general interpreter.
//!
//! Lowered programs are immutable and cached process-wide, keyed by the shared
//! `Arc<MethodCode>` pointer (the cache pins the `Arc`, so the address cannot be
//! reused while the entry lives). Field resolution is **per-VM quickened**: the
//! program stores only the DEX instruction and a process-unique quick slot, and
//! each VM resolves that slot into its own side vector (`Vm::ir_fields`) the
//! first time the body runs. Nothing resolved is shared across VMs, so a
//! snapshot restore (a fresh `Vm`) re-resolves safely and
//! `Vm::invalidate_ir_quickening` drops stale offsets on a class change.

use std::collections::HashMap;
use std::sync::atomic::{AtomicI8, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use crate::class::{Class, ClassId, FieldSign, InitState, Insn, MethodCode};
use crate::error::VmResult;
use crate::frame::Frame;
use crate::heap::{ArrayData, Heap, Object, NULL};
use crate::interp::{apply_sign, div_int, div_long, rem_int, rem_long};
use crate::vm::Vm;

// ---------------------------------------------------------------------------
// Switch
// ---------------------------------------------------------------------------

static MODE: AtomicI8 = AtomicI8::new(-1);

/// Force-enable/disable the tier, or `None` to fall back to `ACOS_EXEC=ir`.
pub fn set_enabled(v: Option<bool>) {
    let mode = match v {
        Some(true) => 1,
        Some(false) => 0,
        None => -1,
    };
    MODE.store(mode, Ordering::Relaxed);
}

/// Whether the IR tier should be used. Defaults to **on**; set
/// `ACOS_EXEC=interp` (or `off`/`0`) to force the general interpreter.
pub fn enabled() -> bool {
    match MODE.load(Ordering::Relaxed) {
        1 => true,
        0 => false,
        _ => {
            let on = !matches!(
                std::env::var("ACOS_EXEC").as_deref(),
                Ok("interp") | Ok("off") | Ok("0")
            );
            MODE.store(on as i8, Ordering::Relaxed);
            on
        }
    }
}

// ---------------------------------------------------------------------------
// Per-VM field quickening
// ---------------------------------------------------------------------------

/// Resolved state of one typed-IR field instruction in one VM.
///
/// Lowering assigns every field instruction a process-unique *quick slot*; each
/// VM keeps its own side vector (`Vm::ir_fields`) indexed by that slot. This is
/// the typed-IR analogue of Dalvik/ART quickening: the descriptor lookup and
/// class-resolution cost is paid once per VM, not once per execution, and the
/// resolved offset is never shared across VMs (so a snapshot restore, which
/// builds a fresh `Vm`, starts with an empty table and re-resolves safely).
#[derive(Clone, Copy, Debug)]
pub enum FieldQuick {
    /// Not resolved yet (or quickening was invalidated). The tier must not
    /// execute the op until the general path has run it once.
    Unresolved,
    /// Must stay on the general path: a VM-internal shadow field of
    /// `Object`/`Class`/`String`, or a field whose resolution failed and whose
    /// exact error the general path must report at the real instruction.
    Fallback,
    Ready(QuickField),
}

/// A resolved field access that the tier can execute directly.
#[derive(Clone, Copy, Debug)]
pub struct QuickField {
    /// Declaring class: the static storage owner for `sget`/`sput`.
    pub class: ClassId,
    pub offset: u32,
    pub width: u32,
    pub is_ref: bool,
    pub is_static: bool,
    pub sign: FieldSign,
}

/// One field instruction of a lowered program, paired with its quick slot.
/// `quicken` walks this to resolve the slots of a VM lazily.
#[derive(Clone, Copy, Debug)]
pub struct FieldRef {
    pub slot: u32,
    /// Index into `MethodCode::insns` of the field instruction.
    pub insn_index: u32,
    /// The instruction's DEX field reference index.
    pub field_index: u32,
}

static NEXT_QSLOT: AtomicU32 = AtomicU32::new(0);

fn next_qslot() -> u32 {
    NEXT_QSLOT.fetch_add(1, Ordering::Relaxed)
}

// ---------------------------------------------------------------------------
// IR
// ---------------------------------------------------------------------------

/// A lowered instruction. Operands are pre-resolved; branch targets are IR
/// indices (not DEX pcs); fused ops consume two DEX instructions.
#[derive(Clone, Copy, Debug)]
pub enum Op {
    /// Needs the general interpreter; the loop stops before it.
    Fallback,
    Nop,
    /// `kind`: 0 = copy ref flag, 1 = plain (clears ref), 2 = object (sets ref),
    /// 3 = wide.
    Move { kind: u8, dst: u16, src: u16 },
    /// `kind`: 0 = move-result, 1 = move-result-wide, 2 = move-result-object.
    MoveRes { kind: u8, dst: u16 },
    MoveException { dst: u16 },
    Const { dst: u16, val: i32 },
    ConstWide { dst: u16, val: i64 },
    /// `const-string`/`const-string/jumbo`: intern string `string_index` into `dst`.
    ConstString { dst: u16, string_index: u32 },
    /// `new-instance`: allocate `type_index`, store the handle in `dst`.
    NewInstance { dst: u16, type_index: u32 },
    /// `new-array`: allocate `type_index` with length from register `len`.
    NewArray { dst: u16, type_index: u32, len: u16 },
    ArrayLen { dst: u16, arr: u16 },
    ArrayGet { op: u8, dst: u16, arr: u16, idx: u16 },
    ArrayPut { op: u8, src: u16, arr: u16, idx: u16 },
    Goto { target: u32 },
    /// `op` is the original `if-*` opcode; `IfZ` ignores `b`.
    If { op: u8, a: u16, b: u16, target: u32 },
    Cmp { op: u8, dst: u16, a: u16, b: u16 },
    Unary { op: u8, dst: u16, src: u16 },
    Convert { op: u8, dst: u16, src: u16 },
    Arith3 { op: u8, dst: u16, a: u16, b: u16 },
    Arith2 { op: u8, dst: u16, b: u16 },
    ArithLit { op: u8, dst: u16, src: u16, lit: i32 },
    /// Fused `<arith>/lit` + conditional branch (the loop-increment idiom).
    ArithLitIf {
        aop: u8,
        bop: u8,
        dst: u16,
        src: u16,
        lit: i32,
        a: u16,
        b: u16,
        target: u32,
    },
    /// `invoke-*`: the trampoline pushes the callee frame; the loop then
    /// returns to the scheduler so the callee body runs.
    Invoke,
    /// `iget*`/`iget-object`; `slot` indexes the per-VM quickening table.
    InstanceGet { op: u8, dst: u16, obj: u16, slot: u32 },
    /// `iput*`/`iput-object`; `slot` indexes the per-VM quickening table.
    InstancePut { op: u8, src: u16, obj: u16, slot: u32 },
    /// `sget*`/`sget-object`; `slot` indexes the per-VM quickening table.
    StaticGet { op: u8, dst: u16, slot: u32 },
    /// `sput*`/`sput-object`; `slot` indexes the per-VM quickening table.
    StaticPut { op: u8, src: u16, slot: u32 },
}

/// A lowered method body plus the reverse maps needed for re-entry.
#[derive(Debug)]
pub struct Program {
    pub ops: Vec<Op>,
    /// Source code-unit pc for each op.
    pub op_pc: Vec<u32>,
    /// Source DEX instruction index for each op.
    pub op_insn: Vec<u32>,
    /// DEX instructions consumed by each op (1, or 2 for a fused op).
    pub op_len: Vec<u8>,
    /// DEX instruction index -> op index (`u32::MAX` for a fused second half).
    pub insn_to_op: Vec<u32>,
    /// End-of-code pc and instruction index (used when the loop runs off the
    /// end of the op array).
    pub end_pc: u32,
    pub end_insn: u32,
    /// True when every lowered op is directly executable (no `Fallback`).
    pub full: bool,
    /// True when the body contains a backward branch (a loop). The tier is only
    /// used for these: straight-line bodies get no benefit from the fused
    /// dispatch and would pay the per-segment entry cost on a call-heavy path.
    pub has_loop: bool,
    /// Every field instruction in the body, with the quick slot it uses.
    pub field_refs: Vec<FieldRef>,
}

/// Cache value: the lowered program *and* a strong reference to the body it was
/// lowered from. Holding the `Arc<MethodCode>` pins the address used as the key,
/// so it can never be reused by a different body while the entry is resident.
type CacheEntry = (Arc<MethodCode>, Arc<Program>);

static CACHE: OnceLock<Mutex<HashMap<usize, CacheEntry>>> = OnceLock::new();

fn cache() -> &'static Mutex<HashMap<usize, CacheEntry>> {
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Lower (or fetch) the program for a shared method body. The key is the
/// `Arc<MethodCode>` address.
pub(crate) fn get_or_lower(code: &Arc<MethodCode>) -> Arc<Program> {
    let key = Arc::as_ptr(code) as usize;
    if let Some((_, p)) = cache()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&key)
        .cloned()
    {
        return p;
    }
    let program = Arc::new(lower(code));
    let mut map = cache().lock().unwrap_or_else(|e| e.into_inner());
    map.entry(key)
        .or_insert_with(|| (code.clone(), program))
        .1
        .clone()
}

// ---------------------------------------------------------------------------
// Lowering
// ---------------------------------------------------------------------------

fn cond_branch(op: u8) -> bool {
    (0x32..=0x3d).contains(&op)
}

fn arith_lit(op: u8) -> bool {
    (0xd0..=0xe2).contains(&op)
}

/// Cheap scan (no lowering, no cache, no lock) for a backward branch: only loop
/// bodies are worth the typed-IR entry cost.
pub(crate) fn is_loop_body(code: &MethodCode) -> bool {
    let mut pc = 0u32;
    for insn in &code.insns {
        match insn.op {
            0x28..=0x2a | 0x32..=0x3d => {
                let target = (pc as i64 + insn.target as i64) as u32;
                if target <= pc {
                    return true;
                }
            }
            0x2b | 0x2c => {
                let payload_pc = (pc as i64 + insn.target as i64) as u32;
                if let Ok(payload) = acos_dex::decode_payload(&code.raw, payload_pc as usize) {
                    let targets: &[i32] = match &payload {
                        acos_dex::Payload::PackedSwitch(sw) => &sw.targets,
                        acos_dex::Payload::SparseSwitch(sw) => &sw.targets,
                        _ => &[],
                    };
                    if targets.iter().any(|&t| ((pc as i64 + t as i64) as u32) <= pc) {
                        return true;
                    }
                }
            }
            _ => {}
        }
        pc += insn.size as u32;
    }
    false
}

/// Reconstruct the code-unit pc of every decoded instruction.
fn insn_pcs(code: &MethodCode) -> Vec<u32> {
    let mut pcs = Vec::with_capacity(code.insns.len() + 1);
    let mut pc = 0u32;
    for i in &code.insns {
        pcs.push(pc);
        pc += i.size as u32;
    }
    pcs.push(pc);
    pcs
}

fn mark_target(code: &MethodCode, insn: &Insn, pcs_at: u32, is_target: &mut [bool]) {
    let mark = |is_target: &mut [bool], target_pc: u32| {
        if let Some(&k) = code.pc_to_idx.get(target_pc as usize) {
            if k != u32::MAX && (k as usize) < is_target.len() {
                is_target[k as usize] = true;
            }
        }
    };
    match insn.op {
        0x28..=0x2a | 0x32..=0x3d => {
            mark(is_target, (pcs_at as i64 + insn.target as i64) as u32);
        }
        0x2b | 0x2c => {
            let payload_pc = (pcs_at as i64 + insn.target as i64) as u32;
            if let Ok(payload) = acos_dex::decode_payload(&code.raw, payload_pc as usize) {
                match payload {
                    acos_dex::Payload::PackedSwitch(sw) => {
                        for &t in &sw.targets {
                            mark(is_target, (pcs_at as i64 + t as i64) as u32);
                        }
                    }
                    acos_dex::Payload::SparseSwitch(sw) => {
                        for &t in &sw.targets {
                            mark(is_target, (pcs_at as i64 + t as i64) as u32);
                        }
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }
}

/// Lower one decoded instruction. Field instructions register a quick slot in
/// `field_refs`; the slot is resolved per-VM at execution time.
fn lower_op(code: &MethodCode, pcs: &[u32], i: usize, field_refs: &mut Vec<FieldRef>) -> Op {
    let insn = &code.insns[i];
    let (r0, r1, r2) = (insn.regs[0], insn.regs[1], insn.regs[2]);
    let op = insn.op;
    let mut field_slot = || {
        let slot = next_qslot();
        field_refs.push(FieldRef {
            slot,
            insn_index: i as u32,
            field_index: insn.index,
        });
        slot
    };
    match op {
        0x00 => Op::Nop,
        0x01 | 0x02 => Op::Move { kind: 0, dst: r0, src: r1 },
        0x03 => Op::Move { kind: 1, dst: r0, src: r1 },
        0x04..=0x06 => Op::Move { kind: 3, dst: r0, src: r1 },
        0x07..=0x09 => Op::Move { kind: 2, dst: r0, src: r1 },
        0x0a => Op::MoveRes { kind: 0, dst: r0 },
        0x0b => Op::MoveRes { kind: 1, dst: r0 },
        0x0c => Op::MoveRes { kind: 2, dst: r0 },
        0x0d => Op::MoveException { dst: r0 },
        0x12..=0x15 => Op::Const { dst: r0, val: insn.literal as i32 },
        0x16..=0x19 => Op::ConstWide { dst: r0, val: insn.literal },
        0x1a..=0x1b => Op::ConstString {
            dst: r0,
            string_index: insn.index,
        },
        0x22 => Op::NewInstance {
            dst: r0,
            type_index: insn.index,
        },
        0x23 => Op::NewArray {
            dst: r0,
            type_index: insn.index,
            len: r1,
        },
        0x21 => Op::ArrayLen { dst: r0, arr: r1 },
        0x44..=0x4a => Op::ArrayGet { op, dst: r0, arr: r1, idx: r2 },
        // `aput-object` needs the ArrayStoreException check: fall back.
        0x4b..=0x4c | 0x4e..=0x51 => Op::ArrayPut { op, src: r0, arr: r1, idx: r2 },
        // Instance field get/put. Null checks, internal shadow fields, and
        // resolution failures stay `Fallback` via the `FieldQuick` state.
        0x52..=0x58 => Op::InstanceGet {
            op,
            dst: r0,
            obj: r1,
            slot: field_slot(),
        },
        0x59..=0x5f => Op::InstancePut {
            op,
            src: r0,
            obj: r1,
            slot: field_slot(),
        },
        0x60..=0x66 => Op::StaticGet {
            op,
            dst: r0,
            slot: field_slot(),
        },
        0x67..=0x6d => Op::StaticPut {
            op,
            src: r0,
            slot: field_slot(),
        },
        0x6e..=0x72 | 0x74..=0x78 => Op::Invoke,
        0x28..=0x2a => Op::Goto { target: insn_target_insn(code, pcs, i) },
        0x32..=0x3d => Op::If {
            op,
            a: r0,
            b: r1,
            target: insn_target_insn(code, pcs, i),
        },
        0x2d..=0x31 => Op::Cmp { op, dst: r0, a: r1, b: r2 },
        0x7b..=0x80 => Op::Unary { op, dst: r0, src: r1 },
        0x81..=0x8f => Op::Convert { op, dst: r0, src: r1 },
        0x90..=0xaf => Op::Arith3 { op, dst: r0, a: r1, b: r2 },
        0xb0..=0xcf => Op::Arith2 { op, dst: r0, b: r1 },
        0xd0..=0xe2 => Op::ArithLit { op, dst: r0, src: r1, lit: insn.literal as i32 },
        _ => Op::Fallback,
    }
}

/// DEX insn index of a relative branch target, or `u32::MAX` if invalid.
fn insn_target_insn(code: &MethodCode, pcs: &[u32], i: usize) -> u32 {
    let insn = &code.insns[i];
    let target_pc = (pcs[i] as i64 + insn.target as i64) as u32;
    match code.pc_to_idx.get(target_pc as usize) {
        Some(&k) if k != u32::MAX => k,
        _ => u32::MAX,
    }
}

/// Lower a whole method body.
pub(crate) fn lower(code: &MethodCode) -> Program {
    let n = code.insns.len();
    let pcs = insn_pcs(code);
    let mut is_target = vec![false; n + 1];
    if n > 0 {
        is_target[0] = true;
    }
    for (i, insn) in code.insns.iter().enumerate() {
        mark_target(code, insn, pcs[i], &mut is_target);
    }
    // Exception handler entries are legal re-entry points.
    for tr in &code.tries {
        for (_, addr) in &tr.typed {
            if let Some(&k) = code.pc_to_idx.get(*addr as usize) {
                if k != u32::MAX {
                    is_target[k as usize] = true;
                }
            }
        }
        if let Some(addr) = tr.catch_all {
            if let Some(&k) = code.pc_to_idx.get(addr as usize) {
                if k != u32::MAX {
                    is_target[k as usize] = true;
                }
            }
        }
    }

    let mut ops: Vec<Op> = Vec::with_capacity(n);
    let mut op_pc: Vec<u32> = Vec::with_capacity(n);
    let mut op_insn: Vec<u32> = Vec::with_capacity(n);
    let mut op_len: Vec<u8> = Vec::with_capacity(n);
    let mut insn_to_op = vec![u32::MAX; n + 1];
    let mut field_refs: Vec<FieldRef> = Vec::new();

    let mut i = 0usize;
    while i < n {
        let insn = &code.insns[i];
        // Fuse `<arith>/lit` + conditional branch when the branch is not a
        // re-entry point (so it can never be entered on its own).
        let can_fuse = arith_lit(insn.op)
            && i + 1 < n
            && cond_branch(code.insns[i + 1].op)
            && !is_target[i + 1];
        let (op, len) = if can_fuse {
            let b = &code.insns[i + 1];
            let fused = Op::ArithLitIf {
                aop: insn.op,
                bop: b.op,
                dst: insn.regs[0],
                src: insn.regs[1],
                lit: insn.literal as i32,
                a: b.regs[0],
                b: b.regs[1],
                target: insn_target_insn(code, &pcs, i + 1),
            };
            (fused, 2u8)
        } else {
            (lower_op(code, &pcs, i, &mut field_refs), 1u8)
        };
        let idx = ops.len() as u32;
        ops.push(op);
        op_pc.push(pcs[i]);
        op_insn.push(i as u32);
        op_len.push(len);
        insn_to_op[i] = idx;
        if len == 2 {
            insn_to_op[i + 1] = idx;
        }
        i += len as usize;
    }
    if n > 0 {
        insn_to_op[n] = u32::MAX;
    }

    // Resolve temporary target insn-indices to IR op indices.
    for op in &mut ops {
        let t = match op {
            Op::Goto { target } => Some(target),
            Op::If { target, .. } => Some(target),
            Op::ArithLitIf { target, .. } => Some(target),
            _ => None,
        };
        if let Some(t) = t {
            let resolved = if *t == u32::MAX {
                u32::MAX
            } else {
                *insn_to_op.get(*t as usize).unwrap_or(&u32::MAX)
            };
            match op {
                Op::Goto { target } | Op::If { target, .. } | Op::ArithLitIf { target, .. } => {
                    *target = resolved;
                }
                _ => {}
            }
        }
    }

    let full = ops.iter().all(|o| !matches!(o, Op::Fallback));
    // A backward branch marks a loop; only loops are worth the typed-IR entry
    // cost (straight-line bodies are already well served by `fast_run`).
    let has_loop = ops.iter().enumerate().any(|(idx, op)| {
        let t = match op {
            Op::Goto { target } => *target,
            Op::If { target, .. } => *target,
            Op::ArithLitIf { target, .. } => *target,
            _ => return false,
        };
        t != u32::MAX && (t as usize) <= idx
    });
    Program {
        ops,
        op_pc,
        op_insn,
        op_len,
        insn_to_op,
        end_pc: *pcs.last().unwrap_or(&0),
        end_insn: n as u32,
        full,
        has_loop,
        field_refs,
    }
}

// ---------------------------------------------------------------------------
// Execution
// ---------------------------------------------------------------------------

/// Resolve, for this VM, the field instructions of `prog` that have not been
/// quickened yet. Called once per IR segment, before [`run_ir`], so the
/// descriptor lookup is off the per-instruction path.
///
/// Resolution failures and VM-internal shadow fields are recorded as
/// [`FieldQuick::Fallback`]: the general interpreter then handles that
/// instruction exactly as before (including the exact error and its pc).
/// Because the table lives on the `Vm`, restore (which builds a fresh `Vm`)
/// starts empty and [`Vm::invalidate_ir_quickening`] drops it on a class layout
/// change, so a resolved offset is never used against a different layout.
pub(crate) fn quicken(vm: &mut Vm, t: usize, fi: usize, prog: &Program) {
    if prog.field_refs.is_empty() {
        return;
    }
    // Every field op in a body shares the body's DEX image.
    let dex = match &vm.methods[vm.threads[t].frames[fi].method as usize].imp {
        crate::class::MethodImpl::Dex { dex, .. } => *dex,
        _ => return,
    };
    for fr in &prog.field_refs {
        let slot = fr.slot as usize;
        if slot >= vm.ir_fields.len() {
            vm.ir_fields.resize(slot + 1, FieldQuick::Unresolved);
        }
        if !matches!(vm.ir_fields[slot], FieldQuick::Unresolved) {
            continue;
        }
        let quick = match vm.resolve_field(dex, fr.field_index) {
            Ok(rf) => {
                let internal =
                    rf.class == vm.c_object || rf.class == vm.c_class || rf.class == vm.c_string;
                if internal && !rf.is_static {
                    FieldQuick::Fallback
                } else {
                    FieldQuick::Ready(QuickField {
                        class: rf.class,
                        offset: rf.offset,
                        width: rf.width,
                        is_ref: rf.is_ref,
                        is_static: rf.is_static,
                        sign: rf.sign,
                    })
                }
            }
            Err(_) => FieldQuick::Fallback,
        };
        vm.ir_fields[slot] = quick;
    }
}

#[inline(always)]
fn cond_take(op: u8, x: u32, y: u32) -> bool {
    match op {
        0x32 => x == y,
        0x33 => x != y,
        0x34 => (x as i32) < (y as i32),
        0x35 => (x as i32) >= (y as i32),
        0x36 => (x as i32) > (y as i32),
        0x37 => (x as i32) <= (y as i32),
        0x38 => (x as i32) == 0,
        0x39 => (x as i32) != 0,
        0x3a => (x as i32) < 0,
        0x3b => (x as i32) >= 0,
        0x3c => (x as i32) > 0,
        0x3d => (x as i32) <= 0,
        _ => false,
    }
}

#[inline(always)]
fn exec_arith3(f: &mut Frame, op: u8, dst: u16, ra: u16, rb: u16) -> bool {
    match op {
        0x90 => {
            let (a, b) = (f.reg(ra) as i32, f.reg(rb) as i32);
            f.set_reg(dst, a.wrapping_add(b) as u32);
        }
        0x91 => {
            let (a, b) = (f.reg(ra) as i32, f.reg(rb) as i32);
            f.set_reg(dst, a.wrapping_sub(b) as u32);
        }
        0x92 => {
            let (a, b) = (f.reg(ra) as i32, f.reg(rb) as i32);
            f.set_reg(dst, a.wrapping_mul(b) as u32);
        }
        0x93 => {
            let (a, b) = (f.reg(ra) as i32, f.reg(rb) as i32);
            if b == 0 {
                return false;
            }
            f.set_reg(dst, div_int(a, b).unwrap() as u32);
        }
        0x94 => {
            let (a, b) = (f.reg(ra) as i32, f.reg(rb) as i32);
            if b == 0 {
                return false;
            }
            f.set_reg(dst, rem_int(a, b).unwrap() as u32);
        }
        0x95 => {
            let (a, b) = (f.reg(ra), f.reg(rb));
            f.set_reg(dst, a & b);
        }
        0x96 => {
            let (a, b) = (f.reg(ra), f.reg(rb));
            f.set_reg(dst, a | b);
        }
        0x97 => {
            let (a, b) = (f.reg(ra), f.reg(rb));
            f.set_reg(dst, a ^ b);
        }
        0x98 => {
            let (a, b) = (f.reg(ra) as i32, f.reg(rb));
            f.set_reg(dst, a.wrapping_shl(b & 0x1f) as u32);
        }
        0x99 => {
            let (a, b) = (f.reg(ra) as i32, f.reg(rb));
            f.set_reg(dst, a.wrapping_shr(b & 0x1f) as u32);
        }
        0x9a => {
            let (a, b) = (f.reg(ra), f.reg(rb));
            f.set_reg(dst, a.wrapping_shr(b & 0x1f));
        }
        0x9b..=0xa5 => {
            let a = f.reg_wide(ra) as i64;
            let v: i64 = if (0xa3..=0xa5).contains(&op) {
                let sh = (f.reg(rb) as i32 & 0x3f) as u32;
                match op {
                    0xa3 => (a as u64).wrapping_shl(sh) as i64,
                    0xa4 => a.wrapping_shr(sh),
                    _ => ((a as u64).wrapping_shr(sh)) as i64,
                }
            } else {
                let b = f.reg_wide(rb) as i64;
                match op {
                    0x9b => a.wrapping_add(b),
                    0x9c => a.wrapping_sub(b),
                    0x9d => a.wrapping_mul(b),
                    0x9e => {
                        if b == 0 {
                            return false;
                        }
                        div_long(a, b).unwrap()
                    }
                    0x9f => {
                        if b == 0 {
                            return false;
                        }
                        rem_long(a, b).unwrap()
                    }
                    0xa0 => a & b,
                    0xa1 => a | b,
                    _ => a ^ b,
                }
            };
            f.set_reg_wide(dst, v as u64);
        }
        0xa6..=0xaf => {
            if op >= 0xab {
                let a = f64::from_bits(f.reg_wide(ra));
                let b = f64::from_bits(f.reg_wide(rb));
                let v = match op {
                    0xab => a + b,
                    0xac => a - b,
                    0xad => a * b,
                    0xae => a / b,
                    _ => a % b,
                };
                f.set_reg_wide(dst, v.to_bits());
            } else {
                let a = f32::from_bits(f.reg(ra));
                let b = f32::from_bits(f.reg(rb));
                let v = match op {
                    0xa6 => a + b,
                    0xa7 => a - b,
                    0xa8 => a * b,
                    0xa9 => a / b,
                    _ => a % b,
                };
                f.set_reg(dst, v.to_bits());
            }
        }
        _ => return false,
    }
    true
}

#[inline(always)]
fn exec_arith2(f: &mut Frame, op: u8, dst: u16, rb: u16) -> bool {
    let r0 = dst;
    let r1 = rb;
    match op {
        0xb0..=0xba => {
            let a = f.reg(r0) as i32;
            let b = f.reg(r1);
            let v: i32 = match op {
                0xb0 => a.wrapping_add(b as i32),
                0xb1 => a.wrapping_sub(b as i32),
                0xb2 => a.wrapping_mul(b as i32),
                0xb3 => {
                    if b as i32 == 0 {
                        return false;
                    }
                    div_int(a, b as i32).unwrap()
                }
                0xb4 => {
                    if b as i32 == 0 {
                        return false;
                    }
                    rem_int(a, b as i32).unwrap()
                }
                0xb5 => a & b as i32,
                0xb6 => a | b as i32,
                0xb7 => a ^ b as i32,
                0xb8 => a.wrapping_shl(b & 0x1f),
                0xb9 => a.wrapping_shr(b & 0x1f),
                _ => (a as u32).wrapping_shr(b & 0x1f) as i32,
            };
            f.set_reg(r0, v as u32);
        }
        0xbb..=0xc5 => {
            let a = f.reg_wide(r0) as i64;
            let v: i64 = if (0xc3..=0xc5).contains(&op) {
                let sh = (f.reg(r1) as i32 & 0x3f) as u32;
                match op {
                    0xc3 => ((a as u64).wrapping_shl(sh)) as i64,
                    0xc4 => a.wrapping_shr(sh),
                    _ => ((a as u64).wrapping_shr(sh)) as i64,
                }
            } else {
                let b = f.reg_wide(r1) as i64;
                match op {
                    0xbb => a.wrapping_add(b),
                    0xbc => a.wrapping_sub(b),
                    0xbd => a.wrapping_mul(b),
                    0xbe => {
                        if b == 0 {
                            return false;
                        }
                        div_long(a, b).unwrap()
                    }
                    0xbf => {
                        if b == 0 {
                            return false;
                        }
                        rem_long(a, b).unwrap()
                    }
                    0xc0 => a & b,
                    0xc1 => a | b,
                    _ => a ^ b,
                }
            };
            f.set_reg_wide(r0, v as u64);
        }
        0xc6..=0xcf => {
            if op >= 0xcb {
                let a = f64::from_bits(f.reg_wide(r0));
                let b = f64::from_bits(f.reg_wide(r1));
                let v = match op {
                    0xcb => a + b,
                    0xcc => a - b,
                    0xcd => a * b,
                    0xce => a / b,
                    _ => a % b,
                };
                f.set_reg_wide(r0, v.to_bits());
            } else {
                let a = f32::from_bits(f.reg(r0));
                let b = f32::from_bits(f.reg(r1));
                let v = match op {
                    0xc6 => a + b,
                    0xc7 => a - b,
                    0xc8 => a * b,
                    0xc9 => a / b,
                    _ => a % b,
                };
                f.set_reg(r0, v.to_bits());
            }
        }
        _ => return false,
    }
    true
}

#[inline(always)]
fn exec_arith_lit(f: &mut Frame, op: u8, r0: u16, r1: u16, lit: i32) -> bool {
    let a = f.reg(r1) as i32;
    let v: i32 = match op {
        0xd0 | 0xd8 => a.wrapping_add(lit),
        0xd1 | 0xd9 => lit.wrapping_sub(a),
        0xd2 | 0xda => a.wrapping_mul(lit),
        0xd3 | 0xdb => {
            if lit == 0 {
                return false;
            }
            div_int(a, lit).unwrap()
        }
        0xd4 | 0xdc => {
            if lit == 0 {
                return false;
            }
            rem_int(a, lit).unwrap()
        }
        0xd5 | 0xdd => a & lit,
        0xd6 | 0xde => a | lit,
        0xd7 | 0xdf => a ^ lit,
        0xe0 => a.wrapping_shl(lit as u32 & 0x1f),
        0xe1 => a.wrapping_shr(lit as u32 & 0x1f),
        0xe2 => (a as u32).wrapping_shr(lit as u32 & 0x1f) as i32,
        _ => return false,
    };
    f.set_reg(r0, v as u32);
    true
}

/// A slow (VM-touching) op reached by the tight IR loop. The loop stops with
/// the frame positioned at the op and hands it to [`run_ir`], which runs it with
/// `&mut Vm` (allocation can run GC and `<clinit>`, so the frame borrow must be
/// released first) and then resumes the tight loop.
enum SlowOp {
    NewInstance { dst: u16, type_index: u32 },
    NewArray { dst: u16, type_index: u32, len: i32 },
    ConstString { dst: u16, string_index: u32 },
    Invoke,
}

/// Tight loop over the frame-local IR ops. Returns `(instructions executed,
/// final op index, slow op)`; `slow` is `Some` when the loop stopped on an
/// allocation or string op, and `None` when it stopped on an [`Op::Fallback`],
/// the cap, or the end of the program. The frame borrow lives for the whole
/// call, so no VM-touching op runs here; `run_ir` runs those and calls back in.
#[inline(always)]
fn run_ir_loop(
    f: &mut Frame,
    heap: &mut Heap,
    prog: &Program,
    cap: u64,
    quick: &[FieldQuick],
    classes: &mut [Class],
    mut pc: usize,
) -> (u64, usize, Option<SlowOp>) {
    let ops = prog.ops.as_slice();
    let op_len = prog.op_len.as_slice();
    let mut count = 0u64;

    loop {
        if count >= cap || pc >= ops.len() {
            break;
        }
        let op = ops[pc];
        let len = op_len[pc] as u64;
        match op {
            Op::Fallback => break,
            Op::NewInstance { dst, type_index } => {
                return (count, pc, Some(SlowOp::NewInstance { dst, type_index }))
            }
            Op::NewArray {
                dst,
                type_index,
                len,
            } => {
                return (
                    count,
                    pc,
                    Some(SlowOp::NewArray {
                        dst,
                        type_index,
                        len: f.reg(len) as i32,
                    }),
                )
            }
            Op::ConstString { dst, string_index } => {
                return (count, pc, Some(SlowOp::ConstString { dst, string_index }))
            }
            Op::Invoke => return (count, pc, Some(SlowOp::Invoke)),
            Op::Nop => {}
            Op::Move { kind, dst, src } => {
                let v = f.reg(src);
                match kind {
                    0 => {
                        if f.refs[src as usize] {
                            f.set_reg_ref(dst, v);
                        } else {
                            f.set_reg(dst, v);
                        }
                    }
                    1 => f.set_reg(dst, v),
                    2 => f.set_reg_ref(dst, v),
                    _ => f.set_reg_wide(dst, f.reg_wide(src)),
                }
            }
            Op::MoveRes { kind, dst } => {
                match kind {
                    0 => {
                        let v = f.pending_result as u32;
                        f.set_reg(dst, v);
                    }
                    1 => {
                        let v = f.pending_result;
                        f.set_reg_wide(dst, v);
                    }
                    _ => {
                        let v = f.pending_result as u32;
                        f.set_reg_ref(dst, v);
                    }
                }
                f.has_pending_result = false;
            }
            Op::MoveException { dst } => {
                let e = f.pending_exception;
                f.set_reg_ref(dst, e);
                f.pending_exception = crate::heap::NULL;
            }
            Op::Const { dst, val } => f.set_reg(dst, val as u32),
            Op::ConstWide { dst, val } => f.set_reg_wide(dst, val as u64),
            Op::ArrayLen { dst, arr } => {
                let h = f.reg(arr);
                match heap.get(h) {
                    Some(Object::Array(a)) => f.set_reg(dst, a.len() as u32),
                    _ => break,
                }
            }
            Op::ArrayGet { op, dst, arr, idx } => {
                let h = f.reg(arr);
                let i = f.reg(idx) as i32;
                match heap.get(h) {
                    Some(Object::Array(a)) if i >= 0 && (i as usize) < a.len() => {
                        let k = i as usize;
                        match &a.data {
                            ArrayData::Bool(v) => f.set_reg(dst, v[k] as u32),
                            ArrayData::Byte(v) => f.set_reg(dst, v[k] as i32 as u32),
                            ArrayData::Char(v) => f.set_reg(dst, v[k] as u32),
                            ArrayData::Short(v) => f.set_reg(dst, v[k] as i32 as u32),
                            ArrayData::Int(v) => f.set_reg(dst, v[k] as u32),
                            ArrayData::Long(v) => f.set_reg_wide(dst, v[k] as u64),
                            ArrayData::Float(v) => f.set_reg(dst, v[k].to_bits()),
                            ArrayData::Double(v) => f.set_reg_wide(dst, v[k].to_bits()),
                            ArrayData::Ref(v) => {
                                if op == 0x46 {
                                    f.set_reg_ref(dst, v[k]);
                                } else {
                                    f.set_reg(dst, v[k]);
                                }
                            }
                        }
                    }
                    _ => break,
                }
            }
            Op::ArrayPut { op, src, arr, idx } => {
                let h = f.reg(arr);
                let i = f.reg(idx) as i32;
                let rv = f.reg(src);
                let rv_wide = if op == 0x4c { f.reg_wide(src) } else { 0 };
                match heap.get_mut(h) {
                    Some(Object::Array(a)) if i >= 0 && (i as usize) < a.len() => {
                        let k = i as usize;
                        match &mut a.data {
                            ArrayData::Bool(v) => v[k] = rv != 0,
                            ArrayData::Byte(v) => v[k] = rv as i8,
                            ArrayData::Char(v) => v[k] = rv as u16,
                            ArrayData::Short(v) => v[k] = rv as i16,
                            ArrayData::Int(v) => v[k] = rv as i32,
                            ArrayData::Long(v) => v[k] = rv_wide as i64,
                            ArrayData::Float(v) => v[k] = f32::from_bits(rv),
                            ArrayData::Double(v) => v[k] = f64::from_bits(rv_wide),
                            ArrayData::Ref(v) => v[k] = rv,
                        }
                    }
                    _ => break,
                }
            }
            Op::InstanceGet { op, dst, obj, slot } => {
                let qf = match quick.get(slot as usize) {
                    Some(FieldQuick::Ready(qf)) => *qf,
                    _ => break,
                };
                let h = f.reg(obj);
                if h == NULL {
                    break;
                }
                match heap.get(h) {
                    Some(Object::Instance { fields, .. }) => {
                        let off = qf.offset as usize;
                        if qf.width == 2 {
                            if off + 1 >= fields.len() {
                                break;
                            }
                            let v = (fields[off] as u64) | ((fields[off + 1] as u64) << 32);
                            f.set_reg_wide(dst, v);
                        } else if off >= fields.len() {
                            break;
                        } else if op == 0x54 {
                            f.set_reg_ref(dst, fields[off]);
                        } else {
                            f.set_reg(dst, apply_sign(fields[off], qf.sign));
                        }
                    }
                    // The general path yields zero for a non-instance slot.
                    _ => {
                        if op == 0x54 {
                            f.set_reg_ref(dst, 0);
                        } else {
                            f.set_reg(dst, 0);
                        }
                    }
                }
            }
            Op::InstancePut { op, src, obj, slot } => {
                let qf = match quick.get(slot as usize) {
                    Some(FieldQuick::Ready(qf)) => *qf,
                    _ => break,
                };
                let h = f.reg(obj);
                if h == NULL {
                    break;
                }
                let off = qf.offset as usize;
                if qf.width == 2 {
                    let v = f.reg_wide(src);
                    match heap.get_mut(h) {
                        Some(Object::Instance { fields, .. }) if off + 1 < fields.len() => {
                            fields[off] = v as u32;
                            fields[off + 1] = (v >> 32) as u32;
                        }
                        // Non-instance / short slot: the general path writes
                        // nothing without throwing, so fall back for exactness.
                        _ => break,
                    }
                } else {
                    let v = if op == 0x5b {
                        f.reg(src)
                    } else {
                        apply_sign(f.reg(src), qf.sign)
                    };
                    match heap.get_mut(h) {
                        Some(Object::Instance { fields, .. }) if off < fields.len() => fields[off] = v,
                        _ => break,
                    }
                }
            }
            Op::StaticGet { op, dst, slot } => {
                let qf = match quick.get(slot as usize) {
                    Some(FieldQuick::Ready(qf)) => *qf,
                    _ => break,
                };
                let c = match classes.get(qf.class as usize) {
                    Some(c) if matches!(c.init, InitState::Init) => c,
                    // Not initialised (or bad id): the general path runs
                    // `initialize_class` (which may run `<clinit>`) first.
                    _ => break,
                };
                let off = qf.offset as usize;
                if qf.width == 2 {
                    if off + 1 >= c.statics.len() {
                        break;
                    }
                    let v = (c.statics[off] as u64) | ((c.statics[off + 1] as u64) << 32);
                    f.set_reg_wide(dst, v);
                } else if off >= c.statics.len() {
                    break;
                } else if op == 0x62 {
                    f.set_reg_ref(dst, c.statics[off]);
                } else {
                    f.set_reg(dst, apply_sign(c.statics[off], qf.sign));
                }
            }
            Op::StaticPut { op, src, slot } => {
                let qf = match quick.get(slot as usize) {
                    Some(FieldQuick::Ready(qf)) => *qf,
                    _ => break,
                };
                let c = match classes.get_mut(qf.class as usize) {
                    Some(c) if matches!(c.init, InitState::Init) => c,
                    _ => break,
                };
                let off = qf.offset as usize;
                if qf.width == 2 {
                    if off + 1 >= c.statics.len() {
                        break;
                    }
                    let v = f.reg_wide(src);
                    c.statics[off] = v as u32;
                    c.statics[off + 1] = (v >> 32) as u32;
                } else if off >= c.statics.len() {
                    break;
                } else {
                    let v = f.reg(src);
                    c.statics[off] = if op == 0x69 { v } else { apply_sign(v, qf.sign) };
                }
            }
            Op::Goto { target } => {
                if target == u32::MAX {
                    break;
                }
                pc = target as usize;
                count += len;
                continue;
            }
            Op::If { op, a, b, target } => {
                if target == u32::MAX {
                    break;
                }
                if cond_take(op, f.reg(a), f.reg(b)) {
                    pc = target as usize;
                } else {
                    pc += 1;
                }
                count += len;
                continue;
            }
            Op::Cmp { op, dst, a, b } => {
                let r: i32 = match op {
                    0x2d..=0x2e => {
                        let x = f32::from_bits(f.reg(a));
                        let y = f32::from_bits(f.reg(b));
                        if x.is_nan() || y.is_nan() {
                            if op == 0x2d { -1 } else { 1 }
                        } else if x < y {
                            -1
                        } else if x > y {
                            1
                        } else {
                            0
                        }
                    }
                    0x2f..=0x30 => {
                        let x = f64::from_bits(f.reg_wide(a));
                        let y = f64::from_bits(f.reg_wide(b));
                        if x.is_nan() || y.is_nan() {
                            if op == 0x2f { -1 } else { 1 }
                        } else if x < y {
                            -1
                        } else if x > y {
                            1
                        } else {
                            0
                        }
                    }
                    _ => {
                        let x = f.reg_wide(a) as i64;
                        let y = f.reg_wide(b) as i64;
                        if x < y {
                            -1
                        } else if x > y {
                            1
                        } else {
                            0
                        }
                    }
                };
                f.set_reg(dst, r as u32);
            }
            Op::Unary { op, dst, src } => match op {
                0x7b => {
                    let a = f.reg(src) as i32;
                    f.set_reg(dst, a.wrapping_neg() as u32);
                }
                0x7c => {
                    let a = f.reg(src) as i32;
                    f.set_reg(dst, (!a) as u32);
                }
                0x7d => {
                    let a = f.reg_wide(src) as i64;
                    f.set_reg_wide(dst, a.wrapping_neg() as u64);
                }
                0x7e => {
                    let a = f.reg_wide(src) as i64;
                    f.set_reg_wide(dst, (!a) as u64);
                }
                0x7f => {
                    let a = f32::from_bits(f.reg(src));
                    f.set_reg(dst, (-a).to_bits());
                }
                _ => {
                    let a = f64::from_bits(f.reg_wide(src));
                    f.set_reg_wide(dst, (-a).to_bits());
                }
            },
            Op::Convert { op, dst, src } => {
                match op {
                    0x81 => {
                        let v = f.reg(src) as i32 as i64;
                        f.set_reg_wide(dst, v as u64);
                    }
                    0x82 => {
                        let v = f.reg(src) as i32 as f32;
                        f.set_reg(dst, v.to_bits());
                    }
                    0x83 => {
                        let v = f.reg(src) as i32 as f64;
                        f.set_reg_wide(dst, v.to_bits());
                    }
                    0x84 => {
                        let v = f.reg_wide(src) as i64 as i32;
                        f.set_reg(dst, v as u32);
                    }
                    0x85 => {
                        let v = f.reg_wide(src) as i64 as f32;
                        f.set_reg(dst, v.to_bits());
                    }
                    0x86 => {
                        let v = f.reg_wide(src) as i64 as f64;
                        f.set_reg_wide(dst, v.to_bits());
                    }
                    0x87 => {
                        let v = f32::from_bits(f.reg(src)) as i32;
                        f.set_reg(dst, v as u32);
                    }
                    0x88 => {
                        let v = f32::from_bits(f.reg(src)) as i64;
                        f.set_reg_wide(dst, v as u64);
                    }
                    0x89 => {
                        let v = f32::from_bits(f.reg(src)) as f64;
                        f.set_reg_wide(dst, v.to_bits());
                    }
                    0x8a => {
                        let v = f64::from_bits(f.reg_wide(src)) as i32;
                        f.set_reg(dst, v as u32);
                    }
                    0x8b => {
                        let v = f64::from_bits(f.reg_wide(src)) as i64;
                        f.set_reg_wide(dst, v as u64);
                    }
                    0x8c => {
                        let v = f64::from_bits(f.reg_wide(src)) as f32;
                        f.set_reg(dst, v.to_bits());
                    }
                    0x8d => {
                        let v = f.reg(src) as i32 as i8 as i32;
                        f.set_reg(dst, v as u32);
                    }
                    0x8e => {
                        let v = f.reg(src) as u16;
                        f.set_reg(dst, v as u32);
                    }
                    _ => {
                        let v = f.reg(src) as i32 as i16 as i32;
                        f.set_reg(dst, v as u32);
                    }
                }
            }
            Op::Arith3 { op, dst, a, b } => {
                if !exec_arith3(f, op, dst, a, b) {
                    break;
                }
            }
            Op::Arith2 { op, dst, b } => {
                if !exec_arith2(f, op, dst, b) {
                    break;
                }
            }
            Op::ArithLit { op, dst, src, lit } => {
                if !exec_arith_lit(f, op, dst, src, lit) {
                    break;
                }
            }
            Op::ArithLitIf {
                aop,
                bop,
                dst,
                src,
                lit,
                a,
                b,
                target,
            } => {
                if target == u32::MAX || !exec_arith_lit(f, aop, dst, src, lit) {
                    break;
                }
                if cond_take(bop, f.reg(a), f.reg(b)) {
                    pc = target as usize;
                } else {
                    pc += 1;
                }
                count += len;
                continue;
            }
        }
        pc += 1;
        count += len;
    }

    (count, pc, None)
}

/// Execute the IR program on the current frame until the cap, the end of the
/// ops, or the next op the tier cannot run itself. Allocation and string ops
/// are run through the `interp` helpers with `&mut Vm`: the frame borrow is
/// released, the op runs (it may allocate, trigger GC, or run `<clinit>`), and
/// the tight loop resumes. The frame stays on the VM thread stack the whole
/// time, so registers/refs are visible to the mark phase. A generic
/// [`Op::Fallback`] still stops before the instruction and the caller runs it
/// through the general path, so catch-table and resolution-error semantics are
/// unchanged. Writes the current DEX pc/instruction index back onto the frame
/// and returns the number of DEX instructions executed.
#[inline]
pub(crate) fn run_ir(
    vm: &mut Vm,
    t: usize,
    fi: usize,
    prog: &Program,
    cap: u64,
) -> VmResult<(u64, bool)> {
    let mut pc = match prog.insn_to_op.get(vm.threads[t].frames[fi].insn_index) {
        Some(&i) if i != u32::MAX => i as usize,
        _ => return Ok((0, false)),
    };
    let mut total = 0u64;
    loop {
        let (n, next_pc, slow) = {
            let Vm {
                threads,
                heap,
                ir_fields,
                classes,
                ..
            } = &mut *vm;
            let f = &mut threads[t].frames[fi];
            let quick = ir_fields.as_slice();
            run_ir_loop(f, heap, prog, cap - total, quick, classes, pc)
        };
        total += n;
        pc = next_pc;
        let slow = match slow {
            Some(s) => s,
            None => break,
        };
        // Position the frame at the slow op so an exception it raises (negative
        // array size, OOM, `<clinit>` failure) is attributed to the original pc.
        {
            let f = &mut vm.threads[t].frames[fi];
            f.pc = prog.op_pc[pc];
            f.insn_index = prog.op_insn[pc] as usize;
        }
        match slow {
            SlowOp::NewInstance { dst, type_index } => {
                crate::interp::exec_new_instance(vm, t, fi, dst, type_index)?;
            }
            SlowOp::NewArray {
                dst,
                type_index,
                len,
            } => {
                crate::interp::exec_new_array(vm, t, fi, dst, type_index, len)?;
            }
            SlowOp::ConstString { dst, string_index } => {
                crate::interp::exec_const_string(vm, t, fi, dst, string_index)?;
            }
            SlowOp::Invoke => {
                // The frame is positioned at the invoke by the common code
                // above. `invoke` may push a callee frame or block the thread;
                // either way the caller's pc must stay on the invoke (a
                // returned callee advances it in `pop_frame`), so return now
                // with `handled = true` and let the scheduler run the callee.
                crate::interp::exec_invoke_at(vm, t, fi, prog.op_insn[pc] as usize)?;
                return Ok((total + prog.op_len[pc] as u64, true));
            }
        }
        total += prog.op_len[pc] as u64;
        pc += 1;
    }

    let (p, ins) = if pc < prog.ops.len() {
        (prog.op_pc[pc], prog.op_insn[pc])
    } else {
        (prog.end_pc, prog.end_insn)
    };
    let f = &mut vm.threads[t].frames[fi];
    f.pc = p;
    f.insn_index = ins as usize;
    Ok((total, false))
}
