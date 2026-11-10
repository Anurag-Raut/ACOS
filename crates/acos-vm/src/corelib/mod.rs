//! Core-library builtins.
//!
//! These are Rust implementations of the small `java.lang` / `java.util`
//! subset the fixture corpus needs. They are registered as ordinary runtime
//! classes so the interpreter is oblivious to whether a class came from DEX or
//! from Rust. Later, real core-library DEX can replace them while keeping only
//! true natives in this registry.

mod collections;
pub(crate) mod r#enum;
pub(crate) mod lang;
pub(crate) mod math;
pub(crate) mod thread;

use crate::class::{ClassId, MethodId, NativeCtx};
use crate::error::{VmError, VmResult};
use crate::frame::Frame;
use crate::heap::{BuiltinData, Handle, Object, NULL};
use crate::vm::Vm;

/// A convenient accessor trait for native method arguments and results.
impl Vm {
    pub(crate) fn ctx_frame(&self, ctx: NativeCtx) -> &Frame {
        &self.threads[ctx.thread].frames[ctx.frame]
    }

    pub(crate) fn ctx_method_id(&self, ctx: NativeCtx) -> MethodId {
        self.threads[ctx.thread].frames[ctx.frame].method
    }

    pub(crate) fn ctx_this(&self, ctx: NativeCtx) -> Option<Handle> {
        let m = &self.methods[self.ctx_method_id(ctx) as usize];
        if m.is_static() {
            None
        } else {
            let f = self.ctx_frame(ctx);
            let base = f.regs.len() - m.ins_size as usize;
            Some(f.regs[base])
        }
    }

    fn param_off(&self, ctx: NativeCtx, n: usize) -> usize {
        let m = &self.methods[self.ctx_method_id(ctx) as usize];
        let f = self.ctx_frame(ctx);
        let base = f.regs.len() - m.ins_size as usize;
        let mut off = base;
        if !m.is_static() {
            off += 1;
        }
        for i in 0..n {
            off += crate::class::type_width(self.type_kind(m.params[i])) as usize;
        }
        off
    }

    pub(crate) fn arg_u32(&self, ctx: NativeCtx, n: usize) -> u32 {
        let off = self.param_off(ctx, n);
        self.ctx_frame(ctx).regs[off]
    }

    pub(crate) fn arg_i32(&self, ctx: NativeCtx, n: usize) -> i32 {
        self.arg_u32(ctx, n) as i32
    }

    pub(crate) fn arg_u16(&self, ctx: NativeCtx, n: usize) -> u16 {
        self.arg_u32(ctx, n) as u16
    }

    pub(crate) fn arg_bool(&self, ctx: NativeCtx, n: usize) -> bool {
        self.arg_u32(ctx, n) != 0
    }

    pub(crate) fn arg_u64(&self, ctx: NativeCtx, n: usize) -> u64 {
        let off = self.param_off(ctx, n);
        let f = self.ctx_frame(ctx);
        (f.regs[off] as u64) | ((f.regs[off + 1] as u64) << 32)
    }

    pub(crate) fn arg_i64(&self, ctx: NativeCtx, n: usize) -> i64 {
        self.arg_u64(ctx, n) as i64
    }

    pub(crate) fn arg_f32(&self, ctx: NativeCtx, n: usize) -> f32 {
        f32::from_bits(self.arg_u32(ctx, n))
    }

    pub(crate) fn arg_f64(&self, ctx: NativeCtx, n: usize) -> f64 {
        f64::from_bits(self.arg_u64(ctx, n))
    }

    pub(crate) fn arg_ref(&self, ctx: NativeCtx, n: usize) -> Handle {
        self.arg_u32(ctx, n)
    }

    pub(crate) fn ret_u32(&mut self, ctx: NativeCtx, v: u32) {
        let f = &mut self.threads[ctx.thread].frames[ctx.frame];
        f.pending_result = v as u64;
        f.pending_result_is_ref = false;
    }

    pub(crate) fn ret_u16(&mut self, ctx: NativeCtx, v: u16) {
        self.ret_u32(ctx, v as u32);
    }

    pub(crate) fn ret_i32(&mut self, ctx: NativeCtx, v: i32) {
        self.ret_u32(ctx, v as u32);
    }

    pub(crate) fn ret_bool(&mut self, ctx: NativeCtx, v: bool) {
        self.ret_u32(ctx, v as u32);
    }

    pub(crate) fn ret_u64(&mut self, ctx: NativeCtx, v: u64) {
        let f = &mut self.threads[ctx.thread].frames[ctx.frame];
        f.pending_result = v;
        f.pending_result_is_ref = false;
    }

    pub(crate) fn ret_i64(&mut self, ctx: NativeCtx, v: i64) {
        self.ret_u64(ctx, v as u64);
    }

    pub(crate) fn ret_f32(&mut self, ctx: NativeCtx, v: f32) {
        self.ret_u32(ctx, v.to_bits());
    }

    pub(crate) fn ret_f64(&mut self, ctx: NativeCtx, v: f64) {
        self.ret_u64(ctx, v.to_bits());
    }

    pub(crate) fn ret_ref(&mut self, ctx: NativeCtx, h: Handle) {
        let f = &mut self.threads[ctx.thread].frames[ctx.frame];
        f.pending_result = h as u64;
        f.pending_result_is_ref = true;
    }

    /// Define a builtin class and return its id.
    pub(crate) fn def_class(
        &mut self,
        desc: &str,
        access: u32,
        super_desc: Option<&str>,
        interfaces: &[&str],
    ) -> VmResult<ClassId> {
        let sup = match super_desc {
            Some(s) => Some(self.load_class(s)?),
            None => None,
        };
        let mut ifaces = Vec::new();
        for i in interfaces {
            ifaces.push(self.load_class(i)?);
        }
        Ok(self.add_class(crate::class::ClassSpec {
            descriptor: desc.to_string(),
            access,
            super_class: sup,
            interfaces: ifaces,
            is_interface: access & acos_dex::access::INTERFACE != 0,
            is_builtin: true,
        }))
    }

    /// Bootstrapping definition used while installing `java.lang.Object`.
    pub(crate) fn def_root_class(&mut self, desc: &str, access: u32) -> ClassId {
        self.add_class(crate::class::ClassSpec {
            descriptor: desc.to_string(),
            access,
            super_class: None,
            interfaces: Vec::new(),
            is_interface: access & acos_dex::access::INTERFACE != 0,
            is_builtin: true,
        })
    }

    /// Allocate the correct internal representation for a builtin instance.
    pub(crate) fn builtin_instance(&mut self, class: ClassId) -> VmResult<Handle> {
        if class == self.c_string && self.c_string != 0 {
            return self.alloc(Object::String(Vec::new()));
        }
        // The Rust builtin payloads (StringBuilder/Thread/collections) only
        // exist in bootstrap mode; with the real core library every class has
        // a real DEX instance layout.
        if self.corelib != crate::vm::CoreLib::Bootstrap {
            return self.alloc_instance(class);
        }
        if class == self.c_string_builder && self.c_string_builder != 0 {
            return self.alloc(Object::Builtin {
                class,
                data: BuiltinData::StringBuilder(Vec::new()),
            });
        }
        if class == self.c_thread && self.c_thread != 0 {
            return self.alloc(Object::Builtin {
                class,
                data: BuiltinData::Thread(crate::heap::ThreadData {
                    id: 0,
                    name: NULL,
                    target: NULL,
                    alive: true,
                    daemon: false,
                }),
            });
        }
        if let Some(&kind) = self.builtin_alloc.get(&class) {
            let data = match kind {
                BuiltinAlloc::List => BuiltinData::List(Vec::new()),
                BuiltinAlloc::Map => BuiltinData::HashMap(Vec::new()),
                BuiltinAlloc::SortedMap => BuiltinData::TreeMap(Vec::new()),
                BuiltinAlloc::EnumSet => BuiltinData::EnumSet {
                    enum_class: 0,
                    elements: Vec::new(),
                },
            };
            return self.alloc(Object::Builtin { class, data });
        }
        self.alloc_instance(class)
    }

    pub(crate) fn throw_npe(&mut self, what: &str) -> VmError {
        self.throw_new("Ljava/lang/NullPointerException;", Some(what))
    }

    /// Java `Object.equals` semantics across the builtin representations.
    pub(crate) fn value_equals(&mut self, a: Handle, b: Handle) -> VmResult<bool> {
        if a == b {
            return Ok(true);
        }
        if a == NULL || b == NULL {
            return Ok(false);
        }
        if let (Some(x), Some(y)) = (self.str_units(a).map(|u| u.to_vec()), self.str_units(b).map(|u| u.to_vec())) {
            return Ok(x == y);
        }
        // Boxed `Integer.equals` fast path: `HashMap`/`ArrayList` linear scans
        // call this tens of millions of times in collection-heavy code, and
        // the generic path pays a full Java dispatch + `call_method` per probe.
        if self.class_of_object(a) == Some(self.c_integer)
            && self.class_of_object(b) == Some(self.c_integer)
        {
            return Ok(self.int_field(a) == self.int_field(b));
        }
        let m = match self.dispatch(a, "equals", "(Ljava/lang/Object;)Z") {
            Ok(m) => m,
            Err(_) => return Ok(false),
        };
        let r = crate::interp::call_method(self, m, a, &[b as u64])?;
        Ok(r as u32 != 0)
    }

    /// Natural ordering via `compareTo`.
    pub(crate) fn compare_objects(&mut self, a: Handle, b: Handle) -> VmResult<i32> {
        if let (Some(x), Some(y)) = (self.str_units(a).map(|u| u.to_vec()), self.str_units(b).map(|u| u.to_vec())) {
            let n = x.len().min(y.len());
            for i in 0..n {
                if x[i] != y[i] {
                    return Ok(x[i] as i32 - y[i] as i32);
                }
            }
            return Ok(x.len() as i32 - y.len() as i32);
        }
        if self.class_of_object(a) == Some(self.c_integer)
            && self.class_of_object(b) == Some(self.c_integer)
        {
            let av = self.int_field(a);
            let bv = self.int_field(b);
            return Ok(av.cmp(&bv) as i32);
        }
        let m = self.dispatch(a, "compareTo", "(Ljava/lang/Object;)I")?;
        let r = crate::interp::call_method(self, m, a, &[b as u64])?;
        Ok(r as u32 as i32)
    }

    fn int_field(&self, h: Handle) -> i32 {
        match self.heap.get(h) {
            Some(Object::Instance { class, fields }) => {
                let c = &self.classes[*class as usize];
                if let Some(f) = c.declared_instance_fields.iter().find(|f| f.name == "value") {
                    fields[f.offset as usize] as i32
                } else {
                    0
                }
            }
            _ => 0,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum BuiltinAlloc {
    List,
    Map,
    SortedMap,
    EnumSet,
}

pub(crate) fn install(vm: &mut Vm) -> VmResult<()> {
    lang::install(vm)?;
    math::install(vm)?;
    r#enum::install(vm)?;
    collections::install(vm)?;
    thread::install(vm)?;
    Ok(())
}
