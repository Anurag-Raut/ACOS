//! Runtime class model, method/field representation and linking.
//!
//! Classes come from two providers behind one interface: DEX files and
//! Rust-implemented core-library ("builtin") classes. Both produce the same
//! [`Class`] structure so the interpreter never special-cases where a class
//! came from.

use std::collections::HashMap;
use std::sync::Arc;

use borsh::{BorshDeserialize, BorshSerialize};

use crate::error::{VmError, VmResult};
use crate::heap::Handle;
use crate::vm::Vm;

pub type ClassId = u32;
pub type MethodId = u32;
pub type TypeId = u32;

/// A decoded Dalvik instruction (pre-decoded once per method).
#[derive(Clone, Copy, Debug, BorshSerialize, BorshDeserialize)]
pub struct Insn {
    pub op: u8,
    pub size: u8,
    pub regs: [u16; 5],
    pub reg_count: u8,
    pub literal: i64,
    pub index: u32,
    pub index2: u32,
    pub target: i32,
}

impl Insn {
    pub fn mnemonic(&self) -> &'static str {
        acos_dex::Opcode::from_u8(self.op)
            .map(|o| o.mnemonic())
            .unwrap_or("unused")
    }
}

/// A try region mapped to a handler descriptor.
#[derive(Clone, Debug, BorshSerialize, BorshDeserialize)]
pub struct TryEntry {
    pub start: u32,
    pub end: u32,
    pub typed: Vec<(String, u32)>,
    pub catch_all: Option<u32>,
}

/// Pre-decoded method body. Shared through `Arc` so a frame can hold it without
/// borrowing the owning class.
#[derive(Debug, BorshSerialize, BorshDeserialize)]
pub struct MethodCode {
    pub registers_size: u16,
    pub ins_size: u16,
    pub outs_size: u16,
    pub insns: Vec<Insn>,
    /// Maps a code-unit pc to an index into `insns` (or `u32::MAX`).
    pub pc_to_idx: Vec<u32>,
    pub raw: Vec<u16>,
    pub tries: Vec<TryEntry>,
    pub line_table: Vec<(u32, u32)>,
    pub source_file: Option<String>,
}

impl MethodCode {
    pub fn line_for_pc(&self, pc: u32) -> u32 {
        let mut line = 0;
        for &(addr, l) in &self.line_table {
            if addr <= pc {
                line = l;
            } else {
                break;
            }
        }
        line
    }
}

/// A field declared by a class, with its resolved slot offset.
#[derive(Clone, Debug, BorshSerialize, BorshDeserialize)]
pub struct FieldDef {
    pub name: String,
    pub desc: String,
    pub access: u32,
    pub class: ClassId,
    /// For instance fields: absolute slot offset from the object base.
    /// For static fields: slot offset into the declaring class's static area.
    pub offset: u32,
    pub is_ref: bool,
    pub width: u32,
}

/// How a field's storage narrows/widens on load/store (the Java primitive
/// sub-word kinds). Cached in [`ResolvedField`] so the interpreter never has
/// to look up the descriptor string on a field access.
#[derive(Clone, Copy, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub enum FieldSign {
    None,
    Byte,
    Short,
    Char,
    Bool,
}

impl FieldSign {
    /// Classify a field descriptor, e.g. `B` / `S` / `C` / `Z`.
    pub fn of_desc(desc: &str) -> FieldSign {
        match desc.as_bytes().first() {
            Some(b'B') => FieldSign::Byte,
            Some(b'S') => FieldSign::Short,
            Some(b'C') => FieldSign::Char,
            Some(b'Z') => FieldSign::Bool,
            _ => FieldSign::None,
        }
    }
}

/// A prototype for resolving field references.
#[derive(Clone, Copy, Debug)]
pub struct ResolvedField {
    pub class: ClassId,
    pub offset: u32,
    pub is_static: bool,
    pub is_ref: bool,
    pub width: u32,
    pub sign: FieldSign,
}

/// The implementation of a method.
#[derive(Clone)]
pub enum MethodImpl {
    /// Bytecode from a DEX file.
    Dex {
        dex: u32,
        method_idx: u32,
        code: Option<Arc<MethodCode>>,
    },
    /// A Rust native.
    Native(NativeFn),
    /// Abstract / no body.
    Abstract,
}

impl std::fmt::Debug for MethodImpl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MethodImpl::Dex { method_idx, code, .. } => write!(
                f,
                "Dex(method_idx={method_idx}, code={})",
                code.is_some()
            ),
            MethodImpl::Native(_) => write!(f, "Native"),
            MethodImpl::Abstract => write!(f, "Abstract"),
        }
    }
}

/// Placeholder bound into a `MethodImpl::Native` decoded from an on-disk boot
/// image: a function pointer cannot cross a process boundary. Every such method
/// is re-bound from the VM's native registry by
/// `Vm::install_boot_image`/`rebind_serialized_natives`; if the registry has no
/// entry (a build mismatch) the call fails structurally instead of running the
/// wrong code.
fn unbound_native(
    _vm: &mut Vm,
    _ctx: NativeCtx,
) -> VmResult<()> {
    Err(VmError::UnsupportedApi {
        api: "<serialized native>".to_string(),
        at: "restored boot image".to_string(),
    })
}

/// `MethodImpl` cannot derive `Borsh*` because `NativeFn` is a raw function
/// pointer. Dex/Abstract round-trip exactly; Native is written as a bare tag
/// and decoded to [`unbound_native`] for the caller to re-bind.
impl BorshSerialize for MethodImpl {
    fn serialize<W: borsh::io::Write>(&self, w: &mut W) -> borsh::io::Result<()> {
        match self {
            MethodImpl::Dex { dex, method_idx, code } => {
                0u8.serialize(w)?;
                dex.serialize(w)?;
                method_idx.serialize(w)?;
                match code {
                    Some(c) => {
                        1u8.serialize(w)?;
                        c.serialize(w)?;
                    }
                    None => 0u8.serialize(w)?,
                }
            }
            MethodImpl::Native(_) => 1u8.serialize(w)?,
            MethodImpl::Abstract => 2u8.serialize(w)?,
        }
        Ok(())
    }
}

impl BorshDeserialize for MethodImpl {
    fn deserialize_reader<R: borsh::io::Read>(r: &mut R) -> borsh::io::Result<Self> {
        let tag = u8::deserialize_reader(r)?;
        Ok(match tag {
            0 => {
                let dex = u32::deserialize_reader(r)?;
                let method_idx = u32::deserialize_reader(r)?;
                let code = if u8::deserialize_reader(r)? != 0 {
                    Some(Arc::new(MethodCode::deserialize_reader(r)?))
                } else {
                    None
                };
                MethodImpl::Dex {
                    dex,
                    method_idx,
                    code,
                }
            }
            1 => MethodImpl::Native(unbound_native),
            2 => MethodImpl::Abstract,
            other => {
                return Err(borsh::io::Error::new(
                    borsh::io::ErrorKind::InvalidData,
                    format!("bad MethodImpl tag {other}"),
                ))
            }
        })
    }
}

/// A native method: receives the VM and the index of its own call frame.
pub type NativeFn = fn(&mut Vm, NativeCtx) -> VmResult<()>;

/// Identifies the frame a native is executing in.
#[derive(Clone, Copy, Debug)]
pub struct NativeCtx {
    pub thread: usize,
    pub frame: usize,
}

#[derive(Clone, Debug, BorshSerialize, BorshDeserialize)]
pub struct Class {
    pub descriptor: String,
    pub access: u32,
    pub super_class: Option<ClassId>,
    pub interfaces: Vec<ClassId>,
    pub is_interface: bool,
    pub is_array: bool,
    pub is_primitive: bool,
    pub is_builtin: bool,
    /// Index into `Vm::dexs`, if DEX-backed.
    pub dex: Option<u32>,
    pub def_idx: Option<usize>,
    pub source_file: Option<String>,
    pub declared_instance_fields: Vec<FieldDef>,
    pub declared_static_fields: Vec<FieldDef>,
    pub instance_slot_count: u32,
    /// Whether each instance slot holds a reference (GC tracing).
    pub instance_refs: Vec<bool>,
    pub statics: Vec<u32>,
    pub static_refs: Vec<bool>,
    pub declared_methods: Vec<MethodId>,
    pub direct_map: HashMap<(String, String), MethodId>,
    pub virtual_map: HashMap<(String, String), MethodId>,
    pub vtable: Vec<MethodId>,
    pub vtable_index: HashMap<(String, String), usize>,
    pub class_obj: Handle,
    pub init: InitState,
    /// `(name, sig)` keys that are abstract (used for default-method merging).
    pub(crate) abstract_methods: std::collections::HashSet<(String, String)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub enum InitState {
    NotInit,
    InProgress,
    Init,
    Failed,
}

impl Class {
    pub(crate) fn new(descriptor: String) -> Class {
        Class {
            descriptor,
            access: 0,
            super_class: None,
            interfaces: Vec::new(),
            is_interface: false,
            is_array: false,
            is_primitive: false,
            is_builtin: false,
            dex: None,
            def_idx: None,
            source_file: None,
            declared_instance_fields: Vec::new(),
            declared_static_fields: Vec::new(),
            instance_slot_count: 0,
            instance_refs: Vec::new(),
            statics: Vec::new(),
            static_refs: Vec::new(),
            declared_methods: Vec::new(),
            direct_map: HashMap::new(),
            virtual_map: HashMap::new(),
            vtable: Vec::new(),
            vtable_index: HashMap::new(),
            class_obj: 0,
            init: InitState::NotInit,
            abstract_methods: std::collections::HashSet::new(),
        }
    }
}

/// A runtime method.
#[derive(Clone, Debug, BorshSerialize, BorshDeserialize)]
pub struct Method {
    pub class: ClassId,
    pub name: String,
    pub sig: String,
    pub params: Vec<TypeId>,
    pub ret: TypeId,
    pub access: u32,
    pub imp: MethodImpl,
    /// Total incoming register slots (receiver + parameters).
    pub ins_size: u32,
    /// Whether each incoming slot holds a reference (for GC roots).
    pub ins_ref: Vec<bool>,
}

impl Method {
    pub fn is_static(&self) -> bool {
        self.access & acos_dex::access::STATIC != 0
    }

    pub fn is_abstract(&self) -> bool {
        self.access & acos_dex::access::ABSTRACT != 0
    }

    pub fn is_native(&self) -> bool {
        self.access & acos_dex::access::NATIVE != 0
    }

    pub fn is_synchronized(&self) -> bool {
        self.access & (acos_dex::access::SYNCHRONIZED | acos_dex::access::DECLARED_SYNCHRONIZED) != 0
    }

    /// Human-readable `Lclass;->name(sig)` for error reporting.
    pub fn api_name(&self, class_desc: &str) -> String {
        format!("{class_desc}->{}{}", self.name, self.sig)
    }
}

/// Describes a method to add to a class.
#[derive(Debug)]
pub struct MethodSpec {
    pub name: String,
    pub params: Vec<TypeId>,
    pub ret: TypeId,
    pub access: u32,
    pub imp: MethodImpl,
    pub sig: String,
}

/// Structural description of a class to add.
#[derive(Debug)]
pub struct ClassSpec {
    pub descriptor: String,
    pub access: u32,
    pub super_class: Option<ClassId>,
    pub interfaces: Vec<ClassId>,
    pub is_interface: bool,
    pub is_builtin: bool,
}

impl ClassSpec {
    pub fn builtin(descriptor: &str, access: u32) -> ClassSpec {
        ClassSpec {
            descriptor: descriptor.to_string(),
            access,
            super_class: None,
            interfaces: Vec::new(),
            is_interface: false,
            is_builtin: true,
        }
    }
}

/// Compute the number of 32-bit register slots a type occupies.
pub fn type_width(kind: TypeKind) -> u32 {
    match kind {
        TypeKind::Long | TypeKind::Double => 2,
        TypeKind::Void => 0,
        _ => 1,
    }
}

/// Type classification, used by arithmetic and GC.
#[derive(Clone, Copy, Debug, PartialEq, Eq, BorshSerialize, BorshDeserialize)]
pub enum TypeKind {
    Void,
    Boolean,
    Byte,
    Char,
    Short,
    Int,
    Long,
    Float,
    Double,
    Reference,
}

impl TypeKind {
    pub fn from_descriptor(desc: &str) -> TypeKind {
        match desc.as_bytes().first().copied() {
            Some(b'V') => TypeKind::Void,
            Some(b'Z') => TypeKind::Boolean,
            Some(b'B') => TypeKind::Byte,
            Some(b'C') => TypeKind::Char,
            Some(b'S') => TypeKind::Short,
            Some(b'I') => TypeKind::Int,
            Some(b'J') => TypeKind::Long,
            Some(b'F') => TypeKind::Float,
            Some(b'D') => TypeKind::Double,
            _ => TypeKind::Reference,
        }
    }

    pub fn is_ref(self) -> bool {
        matches!(self, TypeKind::Reference)
    }
}

#[derive(Clone, Debug, BorshSerialize, BorshDeserialize)]
pub struct TypeInfo {
    pub descriptor: String,
    pub kind: TypeKind,
}

/// Parse a method signature `(params)ret` into `(params, ret)` descriptors.
pub fn parse_signature(sig: &str) -> VmResult<(Vec<String>, String)> {
    let open = sig
        .find('(')
        .ok_or_else(|| VmError::Internal(format!("bad signature {sig}")))?;
    let close = sig
        .find(')')
        .ok_or_else(|| VmError::Internal(format!("bad signature {sig}")))?;
    let params = parse_type_list(&sig[open + 1..close])?;
    let ret = sig[close + 1..].to_string();
    if ret.is_empty() {
        return Err(VmError::Internal(format!("bad signature {sig}")));
    }
    Ok((params, ret))
}

/// Parse a concatenated descriptor list (`ILjava/lang/String;`) into parts.
pub fn parse_type_list(s: &str) -> VmResult<Vec<String>> {
    let mut out = Vec::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let start = i;
        while i < bytes.len() && bytes[i] == b'[' {
            i += 1;
        }
        if i >= bytes.len() {
            return Err(VmError::Internal(format!("bad type list {s}")));
        }
        match bytes[i] {
            b'L' => {
                let end = s[i..]
                    .find(';')
                    .ok_or_else(|| VmError::Internal(format!("bad type list {s}")))?
                    + i;
                i = end + 1;
            }
            _ => i += 1,
        }
        out.push(s[start..i].to_string());
    }
    Ok(out)
}
