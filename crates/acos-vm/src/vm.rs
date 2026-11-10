//! The ACOS virtual machine: heap ownership, class loading, resolution, GC and
//! the public `run` entry point.

use std::collections::HashMap;
use std::rc::Rc;
use std::time::Instant;

use acos_dex::{DexFile, EncodedValue};

use crate::class::{
    parse_signature, parse_type_list, Class, ClassId, ClassSpec, FieldDef, FieldSign, Insn,
    Method, MethodCode, MethodId, MethodImpl, ResolvedField, TryEntry, TypeId, TypeInfo, TypeKind,
};
use crate::error::{VmError, VmResult};
use crate::frame::Frame;
use crate::heap::{new_array_data, ArrayObj, BuiltinData, Handle, Heap, Object, NULL};

/// Per-DEX lazy caches. Indices are the DEX's own id indices.
#[derive(Debug)]
pub struct DexCtx {
    pub file: Rc<DexFile>,
    pub string_cache: Vec<Option<Handle>>,
    pub field_cache: Vec<Option<ResolvedField>>,
    pub method_cache: Vec<Option<MethodId>>,
    pub class_cache: Vec<Option<ClassId>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThreadStatus {
    Runnable,
    Running,
    BlockedMonitor(Handle),
    Sleeping,
    Waiting,
    Finished,
}

#[derive(Debug)]
pub struct ThreadRT {
    pub object: Handle,
    pub frames: Vec<Frame>,
    pub status: ThreadStatus,
    pub joiners: Vec<usize>,
    pub wake_at: Option<Instant>,
    /// Reusable register buffers for popped frames. Java calls happen in tight
    /// loops in real framework code; allocating a fresh `regs`/`refs` pair for
    /// every call dominated invoke cost. The pool is per-thread and capped.
    pub(crate) frame_pool: Vec<Frame>,
}

impl ThreadRT {
    pub(crate) fn new(object: Handle) -> ThreadRT {
        ThreadRT {
            object,
            frames: Vec::new(),
            status: ThreadStatus::Runnable,
            joiners: Vec::new(),
            wake_at: None,
            frame_pool: Vec::new(),
        }
    }

    pub fn top_frame(&self) -> Option<&Frame> {
        self.frames.last()
    }
}

/// VM execution counters and timings for the metrics block.
#[derive(Clone, Debug, Default)]
pub struct Metrics {
    pub instructions: u64,
    pub invokes: u64,
    pub allocations: u64,
    pub gc_count: u64,
    pub gc_ms: u128,
    pub heap_peak_bytes: usize,
    pub classes_loaded: usize,
}

/// A captured uncaught exception.
#[derive(Clone, Debug)]
pub struct Uncaught {
    pub class: String,
    pub message: Option<String>,
    pub stack: Vec<String>,
}

/// The result of running a program.
#[derive(Clone, Debug)]
pub struct RunOutcome {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub uncaught: Option<Uncaught>,
    pub metrics: Metrics,
    /// Present when the failure was a VM-level (non-Java) error.
    pub vm_error: Option<VmError>,
}

/// Options controlling a VM run.
#[derive(Clone, Debug)]
pub struct VmOptions {
    pub heap_max_bytes: usize,
    pub gc_stress: bool,
}

impl Default for VmOptions {
    fn default() -> Self {
        VmOptions {
            heap_max_bytes: 256 * 1024 * 1024,
            gc_stress: std::env::var_os("ACOS_GC_STRESS").is_some(),
        }
    }
}

/// A stack-trace entry captured for `printStackTrace`.
#[derive(Clone, Debug)]
pub struct StackEntry {
    pub class: String,
    pub method: String,
    pub line: u32,
}

#[derive(Debug)]
pub struct Vm {
    pub heap: Heap,
    pub dexs: Vec<DexCtx>,
    pub classes: Vec<Class>,
    pub class_by_desc: HashMap<String, ClassId>,
    pub types: Vec<TypeInfo>,
    pub type_by_desc: HashMap<String, TypeId>,
    pub methods: Vec<Method>,
    pub natives: HashMap<(String, String), crate::class::NativeFn>,
    pub strings: HashMap<Vec<u16>, Handle>,
    pub interned: Vec<Handle>,
    pub threads: Vec<ThreadRT>,
    pub current_thread: usize,
    pub next_thread_id: u32,
    pub native_roots: Vec<Handle>,
    pub throwable_stacks: HashMap<Handle, Vec<StackEntry>>,
    pub pending_exception: Handle,

    pub gc_stress: bool,
    gc_threshold: usize,

    // Pre-created fatal exceptions (cannot allocate when thrown).
    pub oom_object: Handle,
    pub stack_overflow_object: Handle,

    // Fast builtin class ids (filled during corelib install).
    pub c_object: ClassId,
    pub c_class: ClassId,
    pub c_string: ClassId,
    pub c_string_builder: ClassId,
    pub c_throwable: ClassId,
    pub c_integer: ClassId,
    pub c_long: ClassId,
    pub c_thread: ClassId,
    pub c_print_stream: ClassId,
    pub c_system: ClassId,

    pub out_buf: Vec<u8>,
    pub err_buf: Vec<u8>,
    pub exit_request: Option<i32>,
    pub stopping: bool,
    pub stdout: Handle,
    pub stderr: Handle,
    pub builtin_alloc: HashMap<ClassId, crate::corelib::BuiltinAlloc>,
    pub int_cache: HashMap<i32, Handle>,
    pub monitors: HashMap<Handle, (usize, u32)>,
    /// Global inline cache for `invoke-virtual` / `invoke-interface`: maps
    /// `(dex, method_ref index, receiver class)` to the resolved target. The
    /// mapping is a pure function of the declared signature and the receiver's
    /// dynamic class, so it never needs invalidation (classes are never
    /// unloaded). This removes the per-call `name`/`sig` string allocations and
    /// `HashMap<String,_>` probes from virtual/interface dispatch.
    pub dispatch_cache: HashMap<(u32, u32, ClassId), MethodId>,

    pub metrics: Metrics,
    /// Execution histogram for the 256 opcode bytes (populated when
    /// `count_opcodes` is true).
    pub opcode_counts: [u64; 256],
    pub count_opcodes: bool,
    /// Maximum Java frames per thread before `StackOverflowError`.
    pub max_frames: usize,
}

impl Vm {
    pub fn new(options: VmOptions) -> VmResult<Vm> {
        let heap = Heap::new(options.heap_max_bytes);
        let mut vm = Vm {
            heap,
            dexs: Vec::new(),
            classes: Vec::new(),
            class_by_desc: HashMap::new(),
            types: Vec::new(),
            type_by_desc: HashMap::new(),
            methods: Vec::new(),
            natives: HashMap::new(),
            strings: HashMap::new(),
            interned: Vec::new(),
            threads: Vec::new(),
            current_thread: 0,
            next_thread_id: 1,
            native_roots: Vec::new(),
            throwable_stacks: HashMap::new(),
            pending_exception: NULL,
            gc_stress: options.gc_stress,
            gc_threshold: (options.heap_max_bytes / 16).max(1 << 20),
            oom_object: NULL,
            stack_overflow_object: NULL,
            c_object: 0,
            c_class: 0,
            c_string: 0,
            c_string_builder: 0,
            c_throwable: 0,
            c_integer: 0,
            c_long: 0,
            c_thread: 0,
            c_print_stream: 0,
            c_system: 0,
            out_buf: Vec::new(),
            err_buf: Vec::new(),
            exit_request: None,
            stopping: false,
            stdout: NULL,
            stderr: NULL,
            builtin_alloc: HashMap::new(),
            int_cache: HashMap::new(),
            monitors: HashMap::new(),
            dispatch_cache: HashMap::new(),
            metrics: Metrics::default(),
            opcode_counts: [0; 256],
            count_opcodes: false,
            max_frames: 2048,
        };
        crate::corelib::install(&mut vm)?;
        Ok(vm)
    }

    // ---- types -----------------------------------------------------------

    pub fn intern_type(&mut self, desc: &str) -> TypeId {
        if let Some(&id) = self.type_by_desc.get(desc) {
            return id;
        }
        let id = self.types.len() as TypeId;
        self.types.push(TypeInfo {
            descriptor: desc.to_string(),
            kind: TypeKind::from_descriptor(desc),
        });
        self.type_by_desc.insert(desc.to_string(), id);
        id
    }

    pub fn type_kind(&self, id: TypeId) -> TypeKind {
        self.types[id as usize].kind
    }

    pub fn type_desc(&self, id: TypeId) -> &str {
        &self.types[id as usize].descriptor
    }

    pub fn type_is_ref(&self, id: TypeId) -> bool {
        self.types[id as usize].kind.is_ref()
    }

    // ---- class construction ---------------------------------------------

    pub fn add_class(&mut self, spec: ClassSpec) -> ClassId {
        if let Some(&id) = self.class_by_desc.get(&spec.descriptor) {
            return id;
        }
        let id = self.classes.len() as ClassId;
        let mut class = Class::new(spec.descriptor.clone());
        class.access = spec.access;
        class.super_class = spec.super_class;
        class.interfaces = spec.interfaces;
        class.is_interface = spec.is_interface;
        class.is_builtin = spec.is_builtin;
        if let Some(sup) = spec.super_class {
            class.instance_slot_count = self.classes[sup as usize].instance_slot_count;
            class.instance_refs = self.classes[sup as usize].instance_refs.clone();
        }
        self.classes.push(class);
        self.class_by_desc.insert(spec.descriptor, id);
        id
    }

    pub fn class_desc(&self, id: ClassId) -> &str {
        &self.classes[id as usize].descriptor
    }

    pub fn find_class(&self, desc: &str) -> Option<ClassId> {
        self.class_by_desc.get(desc).copied()
    }

    /// Add an instance field to a class and assign its absolute slot offset.
    pub fn add_instance_field(&mut self, class: ClassId, name: &str, desc: &str, access: u32) {
        let is_ref = self.intern_type(desc);
        let is_ref = self.type_is_ref(is_ref);
        let kind = TypeKind::from_descriptor(desc);
        let width = crate::class::type_width(kind);
        let offset = self.classes[class as usize].instance_slot_count;
        let def = FieldDef {
            name: name.to_string(),
            desc: desc.to_string(),
            access,
            class,
            offset,
            is_ref,
            width,
        };
        self.classes[class as usize]
            .declared_instance_fields
            .push(def);
        for _ in 0..width {
            self.classes[class as usize].instance_refs.push(false);
        }
        if is_ref {
            let n = self.classes[class as usize].instance_refs.len();
            self.classes[class as usize].instance_refs[n - 1] = true;
        }
        self.classes[class as usize].instance_slot_count += width;
    }

    /// Add a static field and reserve its static slot.
    pub fn add_static_field(&mut self, class: ClassId, name: &str, desc: &str, access: u32) {
        let tid = self.intern_type(desc);
        let is_ref = self.type_is_ref(tid);
        let kind = TypeKind::from_descriptor(desc);
        let width = crate::class::type_width(kind);
        let offset = self.classes[class as usize].statics.len() as u32;
        let def = FieldDef {
            name: name.to_string(),
            desc: desc.to_string(),
            access,
            class,
            offset,
            is_ref,
            width,
        };
        self.classes[class as usize].declared_static_fields.push(def);
        for _ in 0..width {
            self.classes[class as usize].statics.push(0);
            self.classes[class as usize].static_refs.push(false);
        }
        if is_ref {
            let n = self.classes[class as usize].static_refs.len();
            self.classes[class as usize].static_refs[n - 1] = true;
        }
    }

    /// Register a native method (builtin classes).
    pub fn add_native(
        &mut self,
        class: ClassId,
        name: &str,
        sig: &str,
        access: u32,
        func: crate::class::NativeFn,
    ) -> MethodId {
        let (params, ret) = parse_signature(sig).expect("builtin signature must be valid");
        let params: Vec<TypeId> = params.iter().map(|p| self.intern_type(p)).collect();
        let ret = self.intern_type(&ret);
        self.add_method(class, name, sig, params, ret, access, MethodImpl::Native(func))
    }

    #[allow(clippy::too_many_arguments)]
    pub fn add_dex_method(
        &mut self,
        class: ClassId,
        name: &str,
        sig: &str,
        params: Vec<TypeId>,
        ret: TypeId,
        access: u32,
        dex: u32,
        method_idx: u32,
        code: Option<Rc<MethodCode>>,
    ) -> MethodId {
        self.add_method(
            class,
            name,
            sig,
            params,
            ret,
            access,
            MethodImpl::Dex {
                dex,
                method_idx,
                code,
            },
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn add_method(
        &mut self,
        class: ClassId,
        name: &str,
        sig: &str,
        params: Vec<TypeId>,
        ret: TypeId,
        access: u32,
        imp: MethodImpl,
    ) -> MethodId {
        let is_static = access & acos_dex::access::STATIC != 0;
        let mut ins_size = if is_static { 0 } else { 1 };
        let mut ins_ref = Vec::new();
        if !is_static {
            ins_ref.push(true);
        }
        for &p in &params {
            let kind = self.type_kind(p);
            let w = crate::class::type_width(kind);
            ins_size += w;
            for i in 0..w {
                ins_ref.push(i == 0 && kind.is_ref());
            }
        }
        let id = self.methods.len() as MethodId;
        self.methods.push(Method {
            class,
            name: name.to_string(),
            sig: sig.to_string(),
            params,
            ret,
            access,
            imp,
            ins_size,
            ins_ref,
        });
        self.classes[class as usize].declared_methods.push(id);
        let key = (name.to_string(), sig.to_string());
        let is_direct = is_static
            || access & acos_dex::access::PRIVATE != 0
            || access & acos_dex::access::CONSTRUCTOR != 0
            || name == "<init>"
            || name == "<clinit>";
        if is_direct {
            self.classes[class as usize].direct_map.insert(key, id);
        } else {
            self.classes[class as usize].virtual_map.insert(key, id);
        }
        id
    }

    /// Build the vtable / interface default tables for a class.
    pub fn link_class(&mut self, id: ClassId) -> VmResult<()> {
        // Start from the superclass tables.
        let (mut vtable, mut vindex, mut vmap) = if let Some(sup) =
            self.classes[id as usize].super_class
        {
            let s = &self.classes[sup as usize];
            (s.vtable.clone(), s.vtable_index.clone(), s.virtual_map.clone())
        } else {
            (Vec::new(), HashMap::new(), HashMap::new())
        };

        // Declared virtual methods override/add slots.
        let declared = self.classes[id as usize].declared_methods.clone();
        for mid in &declared {
            let m = &self.methods[*mid as usize];
            let is_direct = m.is_static()
                || m.access & acos_dex::access::PRIVATE != 0
                || m.access & acos_dex::access::CONSTRUCTOR != 0
                || m.name == "<init>"
                || m.name == "<clinit>";
            if is_direct {
                continue;
            }
            let key = (m.name.clone(), m.sig.clone());
            match vindex.get(&key) {
                Some(&slot) => vtable[slot] = *mid,
                None => {
                    vindex.insert(key.clone(), vtable.len());
                    vtable.push(*mid);
                }
            }
            vmap.insert(key, *mid);
            if m.is_abstract() {
                self.classes[id as usize].abstract_methods.insert((m.name.clone(), m.sig.clone()));
            }
        }

        // Merge interface default methods.
        let interfaces = self.classes[id as usize].interfaces.clone();
        let mut defaults: Vec<((String, String), MethodId)> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for iface in &interfaces {
            self.collect_defaults(*iface, &mut defaults, &mut seen);
        }
        for (key, mid) in defaults {
            match vindex.get(&key) {
                Some(&slot) => {
                    let existing = vtable[slot];
                    if self.methods[existing as usize].is_abstract() {
                        vtable[slot] = mid;
                        vmap.insert(key, mid);
                    }
                }
                None => {
                    vindex.insert(key.clone(), vtable.len());
                    vtable.push(mid);
                    vmap.insert(key, mid);
                }
            }
        }

        let c = &mut self.classes[id as usize];
        c.vtable = vtable;
        c.vtable_index = vindex;
        c.virtual_map = vmap;
        Ok(())
    }

    fn collect_defaults(
        &self,
        iface: ClassId,
        out: &mut Vec<((String, String), MethodId)>,
        seen: &mut std::collections::HashSet<(String, String)>,
    ) {
        let c = &self.classes[iface as usize];
        for (key, mid) in &c.virtual_map {
            if seen.contains(key) {
                continue;
            }
            let m = &self.methods[*mid as usize];
            if !m.is_abstract() {
                seen.insert(key.clone());
                out.push((key.clone(), *mid));
            }
        }
        for sup in &c.interfaces {
            self.collect_defaults(*sup, out, seen);
        }
    }

    // ---- loading ---------------------------------------------------------

    /// Load a class by descriptor, lazily, from the DEX providers then builtins.
    pub fn load_class(&mut self, desc: &str) -> VmResult<ClassId> {
        if let Some(&id) = self.class_by_desc.get(desc) {
            return Ok(id);
        }
        if desc.starts_with('[') {
            return self.load_array_class(desc);
        }
        if desc.starts_with('L') && desc.ends_with(';') {
            for di in 0..self.dexs.len() {
                let file = self.dexs[di].file.clone();
                if let Some(def) = file.find_class(desc) {
                    let def_idx = file
                        .class_defs()
                        .iter()
                        .position(|d| std::ptr::eq(d, def))
                        .unwrap_or(0);
                    return self.load_dex_class(di as u32, def_idx);
                }
            }
        }
        Err(VmError::unsupported_api(desc.to_string(), String::new()))
    }

    fn load_array_class(&mut self, desc: &str) -> VmResult<ClassId> {
        let object = self.load_class("Ljava/lang/Object;")?;
        let id = self.add_class(ClassSpec {
            descriptor: desc.to_string(),
            access: acos_dex::access::PUBLIC | acos_dex::access::FINAL,
            super_class: Some(object),
            interfaces: Vec::new(),
            is_interface: false,
            is_builtin: false,
        });
        self.classes[id as usize].is_array = true;
        self.link_class(id)?;
        Ok(id)
    }

    fn load_dex_class(&mut self, dex_idx: u32, def_idx: usize) -> VmResult<ClassId> {
        let file = self.dexs[dex_idx as usize].file.clone();
        let def = &file.class_defs()[def_idx];
        let desc = file.class_descriptor(def)?.to_string();
        if let Some(&id) = self.class_by_desc.get(&desc) {
            return Ok(id);
        }
        let is_interface = def.access_flags & acos_dex::access::INTERFACE != 0;
        let id = self.add_class(ClassSpec {
            descriptor: desc.clone(),
            access: def.access_flags,
            super_class: None,
            interfaces: Vec::new(),
            is_interface,
            is_builtin: false,
        });
        self.classes[id as usize].dex = Some(dex_idx);
        self.classes[id as usize].def_idx = Some(def_idx);

        // Superclass + interfaces.
        if let Some(sup) = file.superclass_descriptor(def)?.map(|s| s.to_string()) {
            let sup_id = self.load_class(&sup)?;
            self.classes[id as usize].super_class = Some(sup_id);
            self.classes[id as usize].instance_slot_count =
                self.classes[sup_id as usize].instance_slot_count;
            self.classes[id as usize].instance_refs =
                self.classes[sup_id as usize].instance_refs.clone();
        }
        let mut ifaces = Vec::new();
        for &ti in &def.interfaces {
            let d = file.type_descriptor(ti)?.to_string();
            ifaces.push(self.load_class(&d)?);
        }
        self.classes[id as usize].interfaces = ifaces;
        self.classes[id as usize].source_file =
            file.source_file(def)?.map(|s| s.to_string());

        // Static field initial values (applied in order before <clinit>).
        let static_values = file.static_values(def)?.unwrap_or_default();

        // Fields.
        let class_data = file.class_data(def)?;
        if let Some(cd) = &class_data {
            for f in &cd.static_fields {
                let fr = file.field_ref(f.field_idx)?;
                let name = fr.name.to_string();
                let fdesc = fr.type_.to_string();
                self.add_static_field(id, &name, &fdesc, f.access_flags);
            }
            for f in &cd.instance_fields {
                let fr = file.field_ref(f.field_idx)?;
                let name = fr.name.to_string();
                let fdesc = fr.type_.to_string();
                self.add_instance_field(id, &name, &fdesc, f.access_flags);
            }
            // Apply encoded static values by position.
            let statics: Vec<FieldDef> =
                self.classes[id as usize].declared_static_fields.clone();
            for (i, val) in static_values.iter().enumerate() {
                if i >= statics.len() {
                    break;
                }
                let f = &statics[i];
                let raw = self.encoded_to_slots(val, f)?;
                let off = f.offset as usize;
                for (j, slot) in raw.into_iter().enumerate() {
                    self.classes[id as usize].statics[off + j] = slot;
                }
            }

            // Methods.
            for m in cd.direct_methods.iter().chain(cd.virtual_methods.iter()) {
                self.add_dex_method_from(file.clone(), dex_idx, id, m.method_idx, m.access_flags, m.code_off)?;
            }
        }

        self.link_class(id)?;
        self.metrics.classes_loaded = self.classes.len();
        Ok(id)
    }

    fn encoded_to_slots(&mut self, v: &EncodedValue, field: &FieldDef) -> VmResult<Vec<u32>> {
        let _ = field;
        Ok(match v {
            EncodedValue::Boolean(b) => vec![*b as u32],
            EncodedValue::Byte(b) => vec![*b as i32 as u32],
            EncodedValue::Short(s) => vec![*s as i32 as u32],
            EncodedValue::Char(c) => vec![*c as u32],
            EncodedValue::Int(i) => vec![*i as u32],
            EncodedValue::Long(l) => vec![*l as u32, (*l >> 32) as u32],
            EncodedValue::Float(f) => vec![*f],
            EncodedValue::Double(d) => vec![*d as u32, (*d >> 32) as u32],
            EncodedValue::String(s) => {
                let text = self.dexs[0].file.string(*s)?.to_string();
                let h = self.intern_str(&text)?;
                vec![h]
            }
            EncodedValue::Null => vec![0],
            other => {
                return Err(VmError::Load(format!(
                    "unsupported static value {other:?}"
                )))
            }
        })
    }

    fn add_dex_method_from(
        &mut self,
        file: Rc<DexFile>,
        dex_idx: u32,
        class: ClassId,
        method_idx: u32,
        access: u32,
        code_off: u32,
    ) -> VmResult<()> {
        let mr = file.method_ref(method_idx)?;
        let name = mr.name.to_string();
        let ret = mr.return_type.to_string();
        let param_descs: Vec<String> = mr.parameters.iter().map(|s| s.to_string()).collect();
        let sig = format!("({}){}", param_descs.concat(), ret);
        let params: Vec<TypeId> = param_descs.iter().map(|d| self.intern_type(d)).collect();
        let ret_id = self.intern_type(&ret);
        let code = if code_off != 0 {
            let ci = file.code_item(code_off)?;
            let code = self.decode_method(&file, &ci)?;
            // Intern the method's string constants eagerly so that later
            // `const-string` never has to allocate (critical after OOM).
            for insn in &code.insns {
                if insn.op == 0x1a || insn.op == 0x1b {
                    self.dex_string(dex_idx, insn.index)?;
                }
            }
            Some(Rc::new(code))
        } else {
            None
        };
        self.add_dex_method(
            class, &name, &sig, params, ret_id, access, dex_idx, method_idx, code,
        );
        Ok(())
    }

    /// Intern a DEX string constant by index (cached per DEX).
    pub(crate) fn dex_string(&mut self, dex: u32, index: u32) -> VmResult<Handle> {
        if let Some(h) = self.dexs[dex as usize].string_cache[index as usize] {
            return Ok(h);
        }
        let s = self.dexs[dex as usize].file.string(index)?.to_string();
        let h = self.intern_str(&s)?;
        self.dexs[dex as usize].string_cache[index as usize] = Some(h);
        Ok(h)
    }

    fn decode_method(&self, file: &DexFile, ci: &acos_dex::CodeItem) -> VmResult<MethodCode> {
        let raw = &ci.insns;
        let mut insns = Vec::new();
        let mut pc_to_idx = vec![u32::MAX; raw.len() + 1];
        let mut i = 0usize;
        while i < raw.len() {
            if acos_dex::payload_kind(raw, i).is_some() {
                let len = acos_dex::payload_len(raw, i)
                    .map_err(|e| VmError::Load(format!("payload: {e}")))?;
                i += len;
                continue;
            }
            let ins = acos_dex::decode(raw, i)
                .map_err(|e| VmError::Load(format!("decode: {e}")))?;
            pc_to_idx[i] = insns.len() as u32;
            insns.push(Insn {
                op: ins.opcode.byte(),
                size: ins.size,
                regs: ins.regs,
                reg_count: ins.reg_count,
                literal: ins.literal,
                index: ins.index,
                index2: ins.index2,
                target: ins.target,
            });
            i += ins.size as usize;
        }

        let mut tries = Vec::new();
        for t in &ci.tries {
            let h = &ci.handlers[t.handler_index];
            let mut typed = Vec::new();
            for p in &h.handlers {
                let d = file.type_descriptor(p.type_idx)?.to_string();
                typed.push((d, p.addr));
            }
            tries.push(TryEntry {
                start: t.start_addr,
                end: t.start_addr + t.insn_count as u32,
                typed,
                catch_all: h.catch_all_addr,
            });
        }
        let line_table = file.line_table(ci)?;
        Ok(MethodCode {
            registers_size: ci.registers_size,
            ins_size: ci.ins_size,
            outs_size: ci.outs_size,
            insns,
            pc_to_idx,
            raw: raw.clone(),
            tries,
            line_table,
            source_file: None,
        })
    }

    /// Register a DEX image as a class provider.
    pub fn add_dex(&mut self, file: DexFile) -> usize {
        let strings = file.strings().len();
        let fields = file.field_ids().len();
        let methods = file.method_ids().len();
        let types = file.type_ids().len();
        let idx = self.dexs.len();
        self.dexs.push(DexCtx {
            file: Rc::new(file),
            string_cache: vec![None; strings],
            field_cache: vec![None; fields],
            method_cache: vec![None; methods],
            class_cache: vec![None; types],
        });
        idx
    }

    // ---- allocation / strings -------------------------------------------

    /// Allocate an object, running GC when required and throwing a Java
    /// `OutOfMemoryError` if the heap cannot be satisfied.
    pub fn alloc(&mut self, obj: Object) -> VmResult<Handle> {
        if let Object::Array(a) = &obj {
            let desc = format!("[{}", a.elem_desc);
            if !self.class_by_desc.contains_key(&desc) {
                self.load_class(&desc)?;
            }
        }
        if self.gc_stress || self.heap.live_bytes >= self.gc_threshold {
            self.gc();
        }
        if self.heap.live_bytes > self.heap.max_bytes {
            return Err(VmError::JavaException(self.oom_object));
        }
        Ok(self.heap.alloc_raw(obj))
    }

    pub fn alloc_instance(&mut self, class: ClassId) -> VmResult<Handle> {
        let slots = self.classes[class as usize].instance_slot_count as usize;
        self.alloc(Object::Instance {
            class,
            fields: vec![0; slots],
        })
    }

    pub fn new_string_utf16(&mut self, units: Vec<u16>) -> VmResult<Handle> {
        self.alloc(Object::String(units))
    }

    pub fn new_string(&mut self, s: &str) -> VmResult<Handle> {
        let units: Vec<u16> = s.encode_utf16().collect();
        self.new_string_utf16(units)
    }

    pub fn intern_str(&mut self, s: &str) -> VmResult<Handle> {
        let units: Vec<u16> = s.encode_utf16().collect();
        if let Some(&h) = self.strings.get(&units) {
            return Ok(h);
        }
        let h = self.alloc_raw_no_gc(Object::String(units.clone()))?;
        self.strings.insert(units, h);
        self.interned.push(h);
        Ok(h)
    }

    fn alloc_raw_no_gc(&mut self, obj: Object) -> VmResult<Handle> {
        if self.heap.live_bytes > self.heap.max_bytes {
            return Err(VmError::JavaException(self.oom_object));
        }
        Ok(self.heap.alloc_raw(obj))
    }

    pub fn str_units(&self, h: Handle) -> Option<&[u16]> {
        match self.heap.get(h) {
            Some(Object::String(u)) => Some(u),
            _ => None,
        }
    }

    pub fn string_to_rust(&self, h: Handle) -> Option<String> {
        match self.heap.get(h) {
            Some(Object::String(u)) => Some(String::from_utf16_lossy(u)),
            Some(Object::Builtin {
                data: BuiltinData::StringBuilder(u),
                ..
            }) => Some(String::from_utf16_lossy(u)),
            _ => None,
        }
    }

    // ---- GC --------------------------------------------------------------

    pub fn gc(&mut self) {
        let start = Instant::now();
        let n = self.heap.objects.len();
        let mut mark = vec![false; n];
        let mut stack: Vec<Handle> = Vec::new();

        for t in &self.threads {
            if t.object != NULL {
                stack.push(t.object);
            }
            for f in &t.frames {
                for (i, &r) in f.refs.iter().enumerate() {
                    if r {
                        stack.push(f.regs[i]);
                    }
                }
            }
        }
        for c in &self.classes {
            for (i, &r) in c.static_refs.iter().enumerate() {
                if r {
                    stack.push(c.statics[i]);
                }
            }
            if c.class_obj != NULL {
                stack.push(c.class_obj);
            }
        }
        stack.extend(self.interned.iter().copied());
        stack.extend(self.int_cache.values().copied());
        stack.extend(self.native_roots.iter().copied());
        stack.push(self.pending_exception);
        stack.push(self.oom_object);
        stack.push(self.stack_overflow_object);

        while let Some(h) = stack.pop() {
            if h == NULL || h as usize >= n || mark[h as usize] {
                continue;
            }
            mark[h as usize] = true;
            match &self.heap.objects[h as usize] {
                Some(Object::Array(a)) => {
                    if let crate::heap::ArrayData::Ref(v) = &a.data {
                        for &e in v {
                            stack.push(e);
                        }
                    }
                }
                Some(Object::Instance { class, fields }) => {
                    if let Some(c) = self.classes.get(*class as usize) {
                        for (i, &is_ref) in c.instance_refs.iter().enumerate() {
                            if is_ref {
                                stack.push(fields[i]);
                            }
                        }
                    }
                }
                Some(Object::Builtin { data, .. }) => match data {
                    BuiltinData::List(v) | BuiltinData::Iterator { items: v, .. } => {
                        stack.extend(v.iter().copied())
                    }
                    BuiltinData::HashMap(v) | BuiltinData::TreeMap(v) => {
                        for (k, val) in v {
                            stack.push(*k);
                            stack.push(*val);
                        }
                    }
                    BuiltinData::EnumSet { elements, .. } => {
                        stack.extend(elements.iter().copied())
                    }
                    BuiltinData::Entry { key, value } => {
                        stack.push(*key);
                        stack.push(*value);
                    }
                    BuiltinData::Thread(t) => {
                        stack.push(t.name);
                        stack.push(t.target);
                    }
                    _ => {}
                },
                _ => {}
            }
        }

        let mut i = 1usize;
        while i < self.heap.objects.len() {
            if self.heap.objects[i].is_some() && !mark[i] {
                self.heap.free_slot(i as u32);
            }
            i += 1;
        }

        self.heap.gc_count += 1;
        self.heap.gc_nanos += start.elapsed().as_nanos() as u64;
        self.metrics.gc_count = self.heap.gc_count;
        self.gc_threshold = (self.heap.live_bytes * 2)
            .max(self.heap.max_bytes / 16)
            .max(1 << 20);
    }

    // ---- class objects ---------------------------------------------------

    pub fn class_object(&mut self, id: ClassId) -> VmResult<Handle> {
        let existing = self.classes[id as usize].class_obj;
        if existing != NULL {
            return Ok(existing);
        }
        let class_id = self.c_class;
        let h = self.alloc(Object::Class(id))?;
        // Object::Class does not carry the instance class; record for getClass.
        if let Some(Object::Class(_)) = self.heap.get(h) {
            // nothing
        }
        let _ = class_id;
        self.classes[id as usize].class_obj = h;
        Ok(h)
    }

    pub fn class_of_object(&self, h: Handle) -> Option<ClassId> {
        match self.heap.get(h)? {
            Object::Instance { class, .. } => Some(*class),
            Object::Array(a) => {
                let desc = format!("[{}", a.elem_desc);
                self.class_by_desc.get(&desc).copied()
            }
            Object::String(_) => Some(self.c_string),
            Object::Class(_) => Some(self.c_class),
            Object::Builtin { class, .. } => Some(*class),
        }
    }

    // ---- type checks -----------------------------------------------------

    pub fn is_assignable(&mut self, from: ClassId, to: ClassId) -> VmResult<bool> {
        if from == to {
            return Ok(true);
        }
        let to_desc = self.class_desc(to).to_string();
        if to_desc == "Ljava/lang/Object;" {
            return Ok(true);
        }
        let from_desc = self.class_desc(from).to_string();
        if from_desc.starts_with('[') || to_desc.starts_with('[') {
            return self.array_assignable(&from_desc, &to_desc);
        }
        // Walk superclass chain and interfaces.
        let mut c = Some(from);
        let mut seen = std::collections::HashSet::new();
        while let Some(cur) = c {
            if !seen.insert(cur) {
                break;
            }
            if cur == to {
                return Ok(true);
            }
            let ifaces = self.classes[cur as usize].interfaces.clone();
            for i in ifaces {
                if self.is_assignable(i, to)? {
                    return Ok(true);
                }
            }
            c = self.classes[cur as usize].super_class;
        }
        Ok(false)
    }

    fn array_assignable(&mut self, from: &str, to: &str) -> VmResult<bool> {
        if from == "Ljava/lang/Object;" {
            return Ok(false);
        }
        if to == "Ljava/lang/Cloneable;" || to == "Ljava/io/Serializable;" {
            return Ok(true);
        }
        let from_arr = from.starts_with('[');
        let to_arr = to.starts_with('[');
        if !from_arr || !to_arr {
            return Ok(false);
        }
        let fc = &from[1..];
        let tc = &to[1..];
        let fprim = !fc.starts_with('L') && !fc.starts_with('[');
        let tprim = !tc.starts_with('L') && !tc.starts_with('[');
        if fprim || tprim {
            return Ok(fc == tc);
        }
        if tc == "Ljava/lang/Object;" {
            return Ok(true);
        }
        let fcid = self.load_class(fc)?;
        let tcid = self.load_class(tc)?;
        self.is_assignable(fcid, tcid)
    }

    // ---- field resolution ------------------------------------------------

    pub fn resolve_field(&mut self, dex: u32, field_idx: u32) -> VmResult<ResolvedField> {
        if let Some(rf) = self.dexs[dex as usize].field_cache[field_idx as usize] {
            return Ok(rf);
        }
        let (class_desc, name, fdesc) = {
            let f = self.dexs[dex as usize].file.field_ref(field_idx)?;
            (f.class.to_string(), f.name.to_string(), f.type_.to_string())
        };
        let cid = self.load_class(&class_desc)?;
        let sign = FieldSign::of_desc(&fdesc);
        let mut c = Some(cid);
        let mut found: Option<ResolvedField> = None;
        while let Some(cur) = c {
            {
                let class = &self.classes[cur as usize];
                if let Some(f) = class
                    .declared_instance_fields
                    .iter()
                    .find(|f| f.name == name && f.desc == fdesc)
                {
                    found = Some(ResolvedField {
                        class: cur,
                        offset: f.offset,
                        is_static: false,
                        is_ref: f.is_ref,
                        width: f.width,
                        sign,
                    });
                    break;
                }
                if let Some(f) = class
                    .declared_static_fields
                    .iter()
                    .find(|f| f.name == name && f.desc == fdesc)
                {
                    found = Some(ResolvedField {
                        class: cur,
                        offset: f.offset,
                        is_static: true,
                        is_ref: f.is_ref,
                        width: f.width,
                        sign,
                    });
                    break;
                }
            }
            c = self.classes[cur as usize].super_class;
        }
        let rf = found.ok_or_else(|| {
            VmError::Load(format!("field not found: {class_desc}.{name}:{fdesc}"))
        })?;
        self.dexs[dex as usize].field_cache[field_idx as usize] = Some(rf);
        Ok(rf)
    }

    // ---- method resolution ----------------------------------------------

    pub fn resolve_method_ref(
        &mut self,
        dex: u32,
        method_idx: u32,
        kind: InvokeKind,
    ) -> VmResult<MethodId> {
        if kind != InvokeKind::Virtual && kind != InvokeKind::Interface {
            if let Some(m) = self.dexs[dex as usize].method_cache[method_idx as usize] {
                return Ok(m);
            }
        }
        let (class_desc, name, sig) = {
            let r = self.dexs[dex as usize].file.method_ref(method_idx)?;
            (
                r.class.to_string(),
                r.name.to_string(),
                format!("({}){}", r.parameters.concat(), r.return_type),
            )
        };
        let cid = self.load_class(&class_desc)?;
        let mid = self.find_method(cid, &name, &sig, kind)?;
        if kind != InvokeKind::Virtual && kind != InvokeKind::Interface {
            self.dexs[dex as usize].method_cache[method_idx as usize] = Some(mid);
        }
        Ok(mid)
    }

    pub fn find_method(
        &mut self,
        class: ClassId,
        name: &str,
        sig: &str,
        kind: InvokeKind,
    ) -> VmResult<MethodId> {
        let key = (name.to_string(), sig.to_string());
        // Direct lookup in the class's declared maps.
        if let Some(&m) = self.classes[class as usize].direct_map.get(&key) {
            return Ok(m);
        }
        if kind == InvokeKind::Virtual || kind == InvokeKind::Interface || kind == InvokeKind::Super {
            if let Some(&m) = self.classes[class as usize].virtual_map.get(&key) {
                return Ok(m);
            }
        }
        // Inherited.
        if let Some(sup) = self.classes[class as usize].super_class {
            return self.find_method(sup, name, sig, kind);
        }
        Err(VmError::UnsupportedApi {
            api: format!("{}.{}{}", self.class_desc(class), name, sig),
            at: String::new(),
        })
    }

    /// Dynamic dispatch used by invoke-virtual / invoke-interface.
    pub fn dispatch(&mut self, receiver: Handle, name: &str, sig: &str) -> VmResult<MethodId> {
        let class = self
            .class_of_object(receiver)
            .or_else(|| self.array_class(receiver))
            .ok_or_else(|| VmError::Internal(format!("no class for receiver {receiver}")))?;
        let key = (name.to_string(), sig.to_string());
        if let Some(&m) = self.classes[class as usize].virtual_map.get(&key) {
            return Ok(m);
        }
        if let Some(sup) = self.classes[class as usize].super_class {
            return self.dispatch_in(sup, name, sig);
        }
        Err(VmError::UnsupportedApi {
            api: format!("{}.{name}{sig}", self.class_desc(class)),
            at: String::new(),
        })
    }

    fn dispatch_in(&mut self, class: ClassId, name: &str, sig: &str) -> VmResult<MethodId> {
        let key = (name.to_string(), sig.to_string());
        if let Some(&m) = self.classes[class as usize].virtual_map.get(&key) {
            return Ok(m);
        }
        if let Some(sup) = self.classes[class as usize].super_class {
            return self.dispatch_in(sup, name, sig);
        }
        Err(VmError::UnsupportedApi {
            api: format!("{}.{name}{sig}", self.class_desc(class)),
            at: String::new(),
        })
    }

    /// Resolve the class for an array handle by descriptor.
    pub fn array_class(&mut self, h: Handle) -> Option<ClassId> {
        let desc = match self.heap.get(h)? {
            Object::Array(a) => format!("[{}", a.elem_desc),
            _ => return None,
        };
        self.class_by_desc.get(&desc).copied()
    }

    pub fn ensure_array_class(&mut self, h: Handle) -> VmResult<ClassId> {
        if let Some(c) = self.array_class(h) {
            return Ok(c);
        }
        let desc = match self.heap.get(h) {
            Some(Object::Array(a)) => format!("[{}", a.elem_desc),
            _ => return Err(VmError::Internal("ensure_array_class on non-array".into())),
        };
        self.load_class(&desc)
    }

    // ---- exception helpers ----------------------------------------------

    pub fn new_throwable(&mut self, desc: &str, message: Option<&str>) -> VmResult<Handle> {
        let cid = self.load_class(desc)?;
        let h = self.builtin_instance(cid)?;
        self.native_roots.push(h);
        let result = (|| -> VmResult<()> {
            if let Some(msg) = message {
                let mh = self.new_string(msg)?;
                self.native_roots.push(mh);
                self.set_field_by_name(cid, h, "detailMessage", mh)?;
                self.native_roots.pop();
            }
            Ok(())
        })();
        self.native_roots.pop();
        result?;
        Ok(h)
    }

    fn set_field_by_name(
        &mut self,
        class: ClassId,
        obj: Handle,
        name: &str,
        value: Handle,
    ) -> VmResult<()> {
        let mut c = Some(class);
        while let Some(cur) = c {
            let f = self.classes[cur as usize]
                .declared_instance_fields
                .iter()
                .find(|f| f.name == name)
                .cloned();
            if let Some(f) = f {
                if let Some(Object::Instance { fields, .. }) = self.heap.get_mut(obj) {
                    fields[f.offset as usize] = value;
                    return Ok(());
                }
            }
            c = self.classes[cur as usize].super_class;
        }
        Ok(())
    }

    pub fn throw_new(&mut self, desc: &str, message: Option<&str>) -> VmError {
        match self.new_throwable(desc, message) {
            Ok(h) => VmError::JavaException(h),
            Err(_) => VmError::JavaException(self.oom_object),
        }
    }

    pub fn capture_stack(&mut self, throwable: Handle, thread: usize) {
        let mut entries = Vec::new();
        if let Some(t) = self.threads.get(thread) {
            for f in t.frames.iter().rev() {
                let class = self.class_desc_of_id(f.class);
                let method = self
                    .methods
                    .get(f.method as usize)
                    .map(|m| m.name.clone())
                    .unwrap_or_default();
                let line = f.code.line_for_pc(f.pc);
                entries.push(StackEntry { class, method, line });
            }
        }
        self.throwable_stacks.insert(throwable, entries);
    }

    pub fn class_desc_of_id(&self, id: ClassId) -> String {
        let desc = &self.classes[id as usize].descriptor;
        descriptor_to_dotted(desc)
    }

    // ---- public entry ----------------------------------------------------

    pub fn run(&mut self, main_class: &str, args: &[String]) -> RunOutcome {
        match self.run_inner(main_class, args) {
            Ok(mut outcome) => {
                outcome.metrics = self.metrics_block();
                outcome
            }
            Err(e) => {
                let mut m = self.metrics_block();
                m.classes_loaded = self.classes.len();
                RunOutcome {
                    exit_code: 70,
                    stdout: String::from_utf8_lossy(&self.out_buf).into_owned(),
                    stderr: format!("acos-vm error: {}: {}\n", e.code(), e),
                    uncaught: None,
                    metrics: m,
                    vm_error: Some(e),
                }
            }
        }
    }

    fn run_inner(&mut self, main_class: &str, args: &[String]) -> VmResult<RunOutcome> {
        let desc = format!("L{};", main_class.replace('.', "/"));
        let cid = self.load_class(&desc)?;
        let main_sig = "([Ljava/lang/String;)V";
        let mid = self.find_method(cid, "main", main_sig, InvokeKind::Static)?;

        // Main thread + thread object.
        let thread_obj = self.make_thread("main", NULL)?;
        self.threads.clear();
        self.threads.push(ThreadRT::new(thread_obj));
        self.current_thread = 0;

        // Build the args String[].
        let arr = self.alloc(Object::Array(ArrayObj {
            elem_desc: "Ljava/lang/String;".to_string(),
            data: crate::heap::ArrayData::Ref(vec![NULL; args.len()]),
        }))?;
        self.native_roots.push(arr);
        for (i, a) in args.iter().enumerate() {
            let h = self.new_string(a)?;
            if let Some(Object::Array(ArrayObj {
                data: crate::heap::ArrayData::Ref(v),
                ..
            })) = self.heap.get_mut(arr)
            {
                v[i] = h;
            }
        }
        // Initialize the class, then push main.
        self.initialize_class(cid)?;
        crate::interp::push_frame_values(self, mid, NULL, &[arr as u64])?;
        self.native_roots.pop();

        crate::interp::run(self)?;

        // Collect an uncaught exception if any.
        if self.pending_exception != NULL {
            let h = self.pending_exception;
            self.pending_exception = NULL;
            let uncaught = self.describe_uncaught(h);
            let mut stderr = String::new();
            stderr.push_str(&format!("Exception in thread \"main\" {}\n", uncaught.1));
            for line in &uncaught.2 {
                stderr.push_str(line);
                stderr.push('\n');
            }
            return Ok(RunOutcome {
                exit_code: 1,
                stdout: String::from_utf8_lossy(&self.out_buf).into_owned(),
                stderr,
                uncaught: Some(uncaught.0),
                metrics: Metrics::default(),
                vm_error: None,
            });
        }

        Ok(RunOutcome {
            exit_code: self.exit_code(),
            stdout: String::from_utf8_lossy(&self.out_buf).into_owned(),
            stderr: String::from_utf8_lossy(&self.err_buf).into_owned(),
            uncaught: None,
            metrics: Metrics::default(),
            vm_error: None,
        })
    }

    /// Exit code requested by `System.exit`, if any.
    pub fn exit_code(&self) -> i32 {
        self.exit_request.unwrap_or(0)
    }

    fn describe_uncaught(&mut self, h: Handle) -> (Uncaught, String, Vec<String>) {
        let class = self
            .class_of_object(h)
            .map(|c| self.class_desc_of_id(c))
            .unwrap_or_else(|| "java.lang.Throwable".to_string());
        let message = self.throwable_message(h);
        let mut stack = Vec::new();
        if let Some(entries) = self.throwable_stacks.get(&h) {
            for e in entries {
                stack.push(format!("\tat {}.{}({})", e.class, e.method, e.line));
            }
        }
        let display = match &message {
            Some(m) => format!("{class}: {m}"),
            None => class.clone(),
        };
        let uncaught = Uncaught {
            class: class.clone(),
            message: message.clone(),
            stack: stack.clone(),
        };
        (uncaught, display, stack)
    }

    pub fn throwable_message(&self, h: Handle) -> Option<String> {
        let class = self.class_of_object(h)?;
        let mut c = Some(class);
        while let Some(cur) = c {
            if let Some(f) = self.classes[cur as usize]
                .declared_instance_fields
                .iter()
                .find(|f| f.name == "detailMessage")
            {
                if let Some(Object::Instance { fields, .. }) = self.heap.get(h) {
                    let mh = fields[f.offset as usize];
                    if mh == NULL {
                        return None;
                    }
                    return self.string_to_rust(mh);
                }
            }
            c = self.classes[cur as usize].super_class;
        }
        None
    }

    pub fn make_thread(&mut self, name: &str, target: Handle) -> VmResult<Handle> {
        let name_h = self.intern_str(name)?;
        let id = self.next_thread_id;
        self.next_thread_id += 1;
        let obj = self.alloc(Object::Builtin {
            class: self.c_thread,
            data: BuiltinData::Thread(crate::heap::ThreadData {
                id,
                name: name_h,
                target,
                alive: true,
                daemon: false,
            }),
        })?;
        Ok(obj)
    }

    pub fn metrics_block(&self) -> Metrics {
        Metrics {
            instructions: self.metrics.instructions,
            invokes: self.metrics.invokes,
            allocations: self.heap.allocations,
            gc_count: self.heap.gc_count,
            gc_ms: self.heap.gc_nanos as u128 / 1_000_000,
            heap_peak_bytes: self.heap.peak_bytes,
            classes_loaded: self.classes.len(),
        }
    }
}

/// Which resolution rule an `invoke-*` uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InvokeKind {
    Virtual,
    Super,
    Direct,
    Static,
    Interface,
}

/// Render a descriptor as a dotted binary name (`Ljava/lang/String;` ->
/// `java.lang.String`).
pub fn descriptor_to_dotted(desc: &str) -> String {
    if let Some(rest) = desc.strip_prefix('L') {
        if let Some(inner) = rest.strip_suffix(';') {
            return inner.replace('/', ".");
        }
    }
    if desc.starts_with('[') {
        return array_descriptor_to_name(desc);
    }
    desc.to_string()
}

pub fn array_descriptor_to_name(desc: &str) -> String {
    let mut dims = 0;
    let mut rest = desc;
    while let Some(r) = rest.strip_prefix('[') {
        dims += 1;
        rest = r;
    }
    let base = match rest.as_bytes().first() {
        Some(b'Z') => "boolean".to_string(),
        Some(b'B') => "byte".to_string(),
        Some(b'C') => "char".to_string(),
        Some(b'S') => "short".to_string(),
        Some(b'I') => "int".to_string(),
        Some(b'J') => "long".to_string(),
        Some(b'F') => "float".to_string(),
        Some(b'D') => "double".to_string(),
        _ => {
            let dotted = descriptor_to_dotted(rest);
            let part = dotted.rsplit('.').next().unwrap_or(&dotted);
            match part.rfind('$') {
                Some(i) => part[i + 1..].to_string(),
                None => part.to_string(),
            }
        }
    };
    let mut s = base;
    for _ in 0..dims {
        s.push_str("[]");
    }
    s
}

/// Build a runtime array from a list of element handles.
pub fn make_ref_array(desc: &str, elements: Vec<Handle>) -> VmResult<Object> {
    Ok(Object::Array(ArrayObj {
        elem_desc: desc.to_string(),
        data: crate::heap::ArrayData::Ref(elements),
    }))
}

/// Validate an array component descriptor.
pub fn simple_array_object(elem: &str, len: usize) -> VmResult<Object> {
    Ok(Object::Array(ArrayObj {
        elem_desc: elem.to_string(),
        data: new_array_data(elem, len)?,
    }))
}

/// Parse a list of string descriptors (re-export for corelib).
pub fn parse_types(s: &str) -> VmResult<Vec<String>> {
    parse_type_list(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::heap::{ArrayData, ArrayObj, Object};

    fn ref_array() -> Object {
        Object::Array(ArrayObj {
            elem_desc: "Ljava/lang/Object;".to_string(),
            data: ArrayData::Ref(vec![NULL]),
        })
    }

    #[test]
    fn gc_collects_unreachable_cycles() {
        let mut vm = Vm::new(VmOptions::default()).unwrap();
        let a = vm.alloc(ref_array()).unwrap();
        let b = vm.alloc(ref_array()).unwrap();
        if let Some(Object::Array(ArrayObj {
            data: ArrayData::Ref(v),
            ..
        })) = vm.heap.get_mut(a)
        {
            v[0] = b;
        }
        if let Some(Object::Array(ArrayObj {
            data: ArrayData::Ref(v),
            ..
        })) = vm.heap.get_mut(b)
        {
            v[0] = a;
        }
        let before = vm.heap.live_bytes;
        vm.gc();
        assert!(!vm.heap.is_live(a), "cycle should be collected");
        assert!(!vm.heap.is_live(b), "cycle should be collected");
        assert!(vm.heap.live_bytes < before);
    }

    #[test]
    fn native_roots_survive_collection() {
        let mut vm = Vm::new(VmOptions::default()).unwrap();
        let h = vm.alloc(ref_array()).unwrap();
        vm.native_roots.push(h);
        let _junk = vm.alloc(ref_array()).unwrap();
        vm.gc();
        assert!(vm.heap.is_live(h), "native root must be traced");
        vm.native_roots.pop();
    }

    #[test]
    fn out_of_memory_throws_java_error() {
        let mut vm = Vm::new(VmOptions {
            heap_max_bytes: 64 * 1024,
            gc_stress: false,
        })
        .unwrap();
        let mut result = None;
        for _ in 0..100_000 {
            match vm.alloc(Object::String(vec![0u16; 128])) {
                Ok(h) => vm.native_roots.push(h),
                Err(e) => {
                    result = Some(e);
                    break;
                }
            }
        }
        match result {
            Some(VmError::JavaException(h)) => assert_eq!(h, vm.oom_object),
            other => panic!("expected OutOfMemoryError, got {other:?}"),
        }
    }

    #[test]
    fn unsupported_api_reports_exact_site() {
        let e = VmError::unsupported_api(
            "Ljava/util/concurrent/Executors;->newFixedThreadPool(I)Ljava/util/concurrent/ExecutorService;",
            "LMain;->main([Ljava/lang/String;)V pc=3",
        );
        assert_eq!(e.code(), "UNSUPPORTED_JAVA_API");
        assert!(e.to_string().contains("newFixedThreadPool"));
    }
}
