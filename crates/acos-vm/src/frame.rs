//! Explicit interpreter frames.
//!
//! Java invocation never recurses on the host Rust stack. Every Java method
//! (DEX or native) runs in a [`Frame`] on a VM-managed per-thread stack. Each
//! frame keeps a parallel reference bitmap so GC roots are exact.

use std::sync::Arc;

use crate::class::{ClassId, MethodId};
use crate::heap::Handle;

/// A 64-bit value split across two 32-bit register slots (Dalvik quoting).
#[derive(Clone, Debug)]
pub struct Frame {
    pub method: MethodId,
    pub class: ClassId,
    /// Current code-unit address (used for try tables and line numbers).
    pub pc: u32,
    /// Index into `code.insns`.
    pub insn_index: usize,
    /// Untyped 32-bit registers; wide values occupy two adjacent slots.
    pub regs: Vec<u32>,
    /// True where the corresponding register currently holds a reference.
    pub refs: Vec<bool>,
    /// Result of the last call, for `move-result`.
    pub pending_result: u64,
    pub pending_result_is_ref: bool,
    pub has_pending_result: bool,
    /// Exception object produced by `move-exception` target (see interpreter).
    pub pending_exception: Handle,
    /// Monitor held for a `synchronized` method (released on return).
    pub monitor: Option<Handle>,
    /// Whether returning from this frame should advance the caller past the
    /// originating `invoke-*` instruction (true) or leave its pc alone (native
    /// callbacks and class initialisers).
    pub advance_on_return: bool,
    /// Byte size of the `invoke-*` instruction that called into this frame,
    /// recorded by `invoke` so `pop_frame` can advance the caller without a
    /// per-return lookup of the caller's instruction stream.
    pub advance_size: u8,
    /// Lazily-lowered typed-IR program for this body (`ACOS_EXEC=ir`). Cached
    /// here so the per-batch dispatch does not probe the process-global cache.
    /// Only loop bodies get a program; straight-line bodies are run by
    /// `fast_run` and are marked `ir_checked` so the decision is made once.
    pub ir: Option<Arc<crate::exec::Program>>,
    /// Whether the IR decision above has been made for this frame's body.
    pub ir_checked: bool,
    /// Whether this frame's IR field quickening slots have already been
    /// resolved in this VM. Re-entering the IR after an `invoke` returns then
    /// skips the (per-slot) quickening scan; cleared on frame reuse and by
    /// [`crate::vm::Vm::invalidate_ir_quickening`].
    pub ir_quickened: bool,
}

impl Frame {
    pub fn new(method: MethodId, class: ClassId, regs_size: usize) -> Frame {
        Frame {
            method,
            class,
            pc: 0,
            insn_index: 0,
            regs: vec![0; regs_size],
            refs: vec![false; regs_size],
            pending_result: 0,
            pending_result_is_ref: false,
            has_pending_result: false,
            pending_exception: 0,
            monitor: None,
            advance_on_return: false,
            advance_size: 0,
            ir: None,
            ir_checked: false,
            ir_quickened: false,
        }
    }

    #[inline(always)]
    pub fn reg(&self, r: u16) -> u32 {
        self.regs[r as usize]
    }

    #[inline(always)]
    pub fn set_reg(&mut self, r: u16, v: u32) {
        self.regs[r as usize] = v;
        // Guarded clear: for the common integer write the slot is already
        // non-reference, so the store is skipped (branch predicted).
        if self.refs[r as usize] {
            self.refs[r as usize] = false;
        }
    }

    #[inline(always)]
    pub fn set_reg_ref(&mut self, r: u16, v: Handle) {
        self.regs[r as usize] = v;
        if !self.refs[r as usize] {
            self.refs[r as usize] = true;
        }
    }

    /// Set a 64-bit value into two adjacent slots.
    #[inline(always)]
    pub fn set_reg_wide(&mut self, r: u16, v: u64) {
        self.regs[r as usize] = v as u32;
        self.regs[r as usize + 1] = (v >> 32) as u32;
        if self.refs[r as usize] {
            self.refs[r as usize] = false;
        }
        if self.refs[r as usize + 1] {
            self.refs[r as usize + 1] = false;
        }
    }

    /// Read a 64-bit value from two adjacent slots.
    #[inline(always)]
    pub fn reg_wide(&self, r: u16) -> u64 {
        (self.regs[r as usize] as u64) | ((self.regs[r as usize + 1] as u64) << 32)
    }

    /// Advance past the current instruction.
    #[inline(always)]
    pub fn advance(&mut self, size: u8) {
        self.pc += size as u32;
        self.insn_index += 1;
    }

    /// Redirect execution to a code-unit address, resolving it to an
    /// instruction index. Returns `false` (leaving state untouched) when the
    /// target is not a valid instruction boundary; the caller then reports the
    /// structured internal error through the general path.
    #[inline(always)]
    pub fn branch_to(&mut self, pc_to_idx: &[u32], target_pc: u32) -> bool {
        match pc_to_idx.get(target_pc as usize).copied() {
            Some(i) if i != u32::MAX => {
                self.pc = target_pc;
                self.insn_index = i as usize;
                true
            }
            _ => false,
        }
    }
}
