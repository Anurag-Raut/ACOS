//! Explicit interpreter frames.
//!
//! Java invocation never recurses on the host Rust stack. Every Java method
//! (DEX or native) runs in a [`Frame`] on a VM-managed per-thread stack. Each
//! frame keeps a parallel reference bitmap so GC roots are exact.

use std::rc::Rc;

use crate::class::{ClassId, MethodCode, MethodId};
use crate::heap::Handle;

/// A 64-bit value split across two 32-bit register slots (Dalvik quoting).
#[derive(Clone, Debug)]
pub struct Frame {
    pub method: MethodId,
    pub class: ClassId,
    pub code: Rc<MethodCode>,
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
}

impl Frame {
    pub fn new(
        method: MethodId,
        class: ClassId,
        code: Rc<MethodCode>,
        regs_size: usize,
    ) -> Frame {
        Frame {
            method,
            class,
            code,
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
        }
    }

    #[inline]
    pub fn reg(&self, r: u16) -> u32 {
        self.regs[r as usize]
    }

    #[inline]
    pub fn set_reg(&mut self, r: u16, v: u32) {
        self.regs[r as usize] = v;
        self.refs[r as usize] = false;
    }

    #[inline]
    pub fn set_reg_ref(&mut self, r: u16, v: Handle) {
        self.regs[r as usize] = v;
        self.refs[r as usize] = true;
    }

    /// Set a 64-bit value into two adjacent slots.
    #[inline]
    pub fn set_reg_wide(&mut self, r: u16, v: u64) {
        self.regs[r as usize] = v as u32;
        self.regs[r as usize + 1] = (v >> 32) as u32;
        self.refs[r as usize] = false;
        self.refs[r as usize + 1] = false;
    }

    /// Read a 64-bit value from two adjacent slots.
    #[inline]
    pub fn reg_wide(&self, r: u16) -> u64 {
        (self.regs[r as usize] as u64) | ((self.regs[r as usize + 1] as u64) << 32)
    }
}
