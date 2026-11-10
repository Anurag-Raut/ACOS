//! Rust host ↔ Java object bridge.
//!
//! Services, `bindApplication`, reply construction and the parcelable codec all
//! need to create and inspect *real* framework objects (a `Rect`, an
//! `ApplicationInfo`, a `Bundle`, ...) from Rust. Doing that by hand with raw
//! handles and field offsets is both unsafe and easy to get wrong, so this
//! module provides a small typed API on top of the existing VM primitives:
//!
//! * create instances, with or without running the real `<init>` (explicit);
//! * read/write fields by name with **hard** type checking (never a coercion);
//! * create strings and primitive/object arrays;
//! * call instance and static Java methods;
//! * convert between [`Handle`]s and [`JavaValue`]s.
//!
//! # GC safety
//!
//! The host may hold a handle across many VM calls, and the collector runs
//! before every allocation (and on every allocation under `ACOS_GC_STRESS=1`).
//! Every handle this module hands back to Rust (and every handle it puts on the
//! stack for a call) is therefore registered in the VM's `native_roots` set,
//! which the precise collector always traces. Handles stay rooted for the
//! lifetime of the VM; call [`Vm::release_bridge_root`] for long-lived hosts
//! that create many short-lived objects.
//!
//! The module deliberately uses only the public VM surface
//! (`interp::call_method`/`push_frame_values`, `load_class`, `find_method`,
//! `dispatch`, `alloc`, ...). It does not touch the interpreter, frame or heap
//! internals.

use crate::class::{type_width, ClassId, FieldDef, MethodId, TypeId};
use crate::error::{VmError, VmResult};
use crate::heap::{Handle, Object, NULL};
use crate::interp;
use crate::vm::{InvokeKind, ThreadRT, Vm};

/// A value crossing the Rust ↔ Java boundary.
///
/// Reference values are heap [`Handle`]s. `Null` is the Java `null` reference;
/// `Void` is only produced for a `void` method return (it is never valid as a
/// field or argument value).
#[derive(Clone, Debug, PartialEq)]
pub enum JavaValue {
    Boolean(bool),
    Byte(i8),
    Char(u16),
    Short(i16),
    Int(i32),
    Long(i64),
    Float(f32),
    Double(f64),
    Object(Handle),
    Null,
    Void,
}

impl JavaValue {
    /// A short human-readable description used in type-mismatch errors.
    pub fn kind(&self) -> &'static str {
        match self {
            JavaValue::Boolean(_) => "boolean",
            JavaValue::Byte(_) => "byte",
            JavaValue::Char(_) => "char",
            JavaValue::Short(_) => "short",
            JavaValue::Int(_) => "int",
            JavaValue::Long(_) => "long",
            JavaValue::Float(_) => "float",
            JavaValue::Double(_) => "double",
            JavaValue::Object(_) => "object",
            JavaValue::Null => "null",
            JavaValue::Void => "void",
        }
    }

    /// The heap handle for an object value, `NULL` for `null`.
    pub fn as_handle(&self) -> Option<Handle> {
        match self {
            JavaValue::Object(h) => Some(*h),
            JavaValue::Null => Some(NULL),
            _ => None,
        }
    }
}

/// A hard bridge error. Type errors are never silent coercions.
#[derive(Clone, Debug, thiserror::Error)]
pub enum BridgeError {
    #[error("class not found: {0}")]
    ClassNotFound(String),
    #[error("field {class}.{field} not found")]
    FieldNotFound { class: String, field: String },
    #[error("field {class}.{field} is {declared}, cannot assign {provided}")]
    FieldTypeMismatch {
        class: String,
        field: String,
        declared: String,
        provided: String,
    },
    #[error("field {class}.{field} is static; use the static field API")]
    FieldIsStatic { class: String, field: String },
    #[error("no method {class}.{name}{sig}")]
    MethodNotFound {
        class: String,
        name: String,
        sig: String,
    },
    #[error("{class}.{name}{sig} expects {expected} argument(s), got {got}")]
    ArgCount {
        class: String,
        name: String,
        sig: String,
        expected: usize,
        got: usize,
    },
    #[error("{class}.{name} argument {index}: expected {expected}, got {provided}")]
    ArgType {
        class: String,
        // `name` already carries the full `name(sig)` (kept as one field so the
        // error enum stays small enough for `clippy::result_large_err`).
        name: String,
        index: usize,
        expected: String,
        provided: String,
    },
    #[error("array element {index}: expected {expected}, got {provided}")]
    ArrayElementType {
        index: usize,
        expected: String,
        provided: String,
    },
    #[error("invalid array component type {0:?}")]
    BadArrayType(String),
    #[error("handle {0} is not a live object")]
    DeadHandle(Handle),
    #[error("java exception {class}: {message}")]
    JavaException { class: String, message: String },
    #[error("vm error: {0}")]
    Vm(#[from] VmError),
}

pub type BridgeResult<T> = Result<T, BridgeError>;

// ---------------------------------------------------------------------------
// Descriptor helpers
// ---------------------------------------------------------------------------

/// Render a class name as a DEX descriptor. Accepts `android.graphics.Rect`,
/// `java/lang/String`, or an already-formed `L...;` descriptor.
pub fn class_descriptor(name: &str) -> String {
    if name.starts_with('L') && name.ends_with(';') {
        return name.to_string();
    }
    if name.starts_with('[') {
        return name.to_string();
    }
    if name.len() == 1 && name.as_bytes()[0].is_ascii_alphabetic() {
        return name.to_string();
    }
    format!("L{};", name.replace('.', "/"))
}

/// Render an array *component* descriptor. Accepts `I`, `Ljava/lang/String;`,
/// `[I`, a dotted class name, or a component that already starts with `L`.
pub fn component_descriptor(name: &str) -> String {
    let name = name.trim();
    if name.len() == 1 && name.as_bytes()[0].is_ascii_alphabetic() {
        return name.to_string();
    }
    if name.starts_with('[') || (name.starts_with('L') && name.ends_with(';')) {
        return name.to_string();
    }
    format!("L{};", name.replace('.', "/"))
}

fn field_value_name(desc: &str) -> &'static str {
    match desc.as_bytes().first().copied() {
        Some(b'Z') => "boolean",
        Some(b'B') => "byte",
        Some(b'C') => "char",
        Some(b'S') => "short",
        Some(b'I') => "int",
        Some(b'J') => "long",
        Some(b'F') => "float",
        Some(b'D') => "double",
        Some(b'L') => "object",
        Some(b'[') => "array",
        _ => "reference",
    }
}

// ---------------------------------------------------------------------------
// Public bridge API
// ---------------------------------------------------------------------------

impl Vm {
    /// Allocate a `class` instance **without running any constructor**.
    ///
    /// The fields start at their zero values, exactly like an allocation before
    /// `<init>`, and the class is not initialised (no `<clinit>`). This is the
    /// explicit "no ctor" choice; use [`Vm::new_instance_ctor`] to run the real
    /// constructor.
    pub fn new_instance(&mut self, class: &str) -> BridgeResult<Handle> {
        let cid = self.load_class_named(class)?;
        let h = self.alloc_instance(cid).map_err(BridgeError::from)?;
        self.bridge_root(h);
        Ok(h)
    }

    /// Allocate a `class` instance and run its real constructor `<init>`.
    ///
    /// `args` are type-checked against the signature before anything runs. The
    /// instance is rooted before `<init>` executes, so a constructor that
    /// allocates can never lose `this`.
    pub fn new_instance_ctor(
        &mut self,
        class: &str,
        sig: &str,
        args: &[JavaValue],
    ) -> BridgeResult<Handle> {
        let cid = self.load_class_named(class)?;
        let mid = self.resolve_method(cid, class, "<init>", sig, InvokeKind::Direct)?;
        let h = self.alloc_instance(cid).map_err(BridgeError::from)?;
        self.bridge_root(h);
        self.invoke(mid, h, args, class, "<init>", sig)?;
        Ok(h)
    }

    /// Create a rooted Java `String` from a Rust string.
    pub fn new_string_rooted(&mut self, s: &str) -> BridgeResult<Handle> {
        let h = self.new_string(s).map_err(BridgeError::from)?;
        self.bridge_root(h);
        Ok(h)
    }

    /// Create a rooted Java `String` from UTF-16 code units.
    pub fn new_string_utf16_rooted(&mut self, units: Vec<u16>) -> BridgeResult<Handle> {
        let h = self.new_string_utf16(units).map_err(BridgeError::from)?;
        self.bridge_root(h);
        Ok(h)
    }

    /// Create a rooted Java array from typed element values.
    ///
    /// `elem_desc` is the *component* descriptor (`I`, `Ljava/lang/String;`,
    /// `[I`, or a dotted class name). Every value is type-checked; a mismatch is
    /// a hard error.
    pub fn new_array(&mut self, elem_desc: &str, values: &[JavaValue]) -> BridgeResult<Handle> {
        let desc = component_descriptor(elem_desc);
        let data = self.build_array_data(&desc, values)?;
        let h = self
            .alloc(Object::Array(crate::heap::ArrayObj { elem_desc: desc, data }))
            .map_err(BridgeError::from)?;
        self.bridge_root(h);
        Ok(h)
    }

    /// Create a rooted zeroed Java array of `len` elements.
    pub fn new_array_empty(&mut self, elem_desc: &str, len: usize) -> BridgeResult<Handle> {
        let desc = component_descriptor(elem_desc);
        let data = crate::heap::new_array_data(&desc, len).map_err(|e| {
            BridgeError::BadArrayType(format!("{desc}: {e}"))
        })?;
        let h = self
            .alloc(Object::Array(crate::heap::ArrayObj { elem_desc: desc, data }))
            .map_err(BridgeError::from)?;
        self.bridge_root(h);
        Ok(h)
    }

    /// Read an instance field by name, walking the superclass chain.
    ///
    /// The field's declared type decides the returned [`JavaValue`] variant; a
    /// reference value is rooted.
    pub fn get_field(&mut self, obj: Handle, name: &str) -> BridgeResult<JavaValue> {
        let (_, field) = self.find_field(obj, name)?;
        let raw = self.read_instance_slots(obj, &field)?;
        self.value_from_field(&field, &raw)
    }

    /// Write an instance field by name with hard type checking.
    pub fn set_field(&mut self, obj: Handle, name: &str, value: JavaValue) -> BridgeResult<()> {
        let (class, field) = self.find_field(obj, name)?;
        let words = self.encode_field(&class, &field, &value)?;
        self.write_instance_slots(obj, &field, &words)
    }

    /// Read a static field by name from `class` (or a superclass).
    ///
    /// This does not run `<clinit>`; compile-time constants are already set at
    /// class load. Use [`Vm::call_static`] if class initialisation is wanted.
    pub fn get_static_field(&mut self, class: &str, name: &str) -> BridgeResult<JavaValue> {
        let cid = self.load_class_named(class)?;
        let (decl, field) = self.find_static_field(cid, class, name)?;
        let raw = {
            let statics = &self.classes[decl as usize].statics;
            let off = field.offset as usize;
            statics
                .get(off..off + field.width as usize)
                .ok_or_else(|| BridgeError::FieldNotFound {
                    class: class.to_string(),
                    field: name.to_string(),
                })?
                .to_vec()
        };
        self.value_from_field(&field, &raw)
    }

    /// Write a static field by name with hard type checking.
    ///
    /// Like [`Vm::get_static_field`], this does not run `<clinit>`.
    pub fn set_static_field(
        &mut self,
        class: &str,
        name: &str,
        value: JavaValue,
    ) -> BridgeResult<()> {
        let cid = self.load_class_named(class)?;
        let (decl, field) = self.find_static_field(cid, class, name)?;
        let words = self.encode_field(class, &field, &value)?;
        let statics = &mut self.classes[decl as usize].statics;
        let off = field.offset as usize;
        for (i, w) in words.iter().enumerate() {
            statics[off + i] = *w;
        }
        Ok(())
    }

    /// Call an instance method, using normal virtual dispatch for `obj`.
    pub fn call_method(
        &mut self,
        obj: Handle,
        name: &str,
        sig: &str,
        args: &[JavaValue],
    ) -> BridgeResult<JavaValue> {
        if obj == NULL {
            return Err(BridgeError::DeadHandle(NULL));
        }
        if self.heap.get(obj).is_none() {
            return Err(BridgeError::DeadHandle(obj));
        }
        let class = self.class_name_of(obj);
        let mid = self
            .dispatch(obj, name, sig)
            .map_err(|_| BridgeError::MethodNotFound {
                class: class.clone(),
                name: name.to_string(),
                sig: sig.to_string(),
            })?;
        self.invoke(mid, obj, args, &class, name, sig)
    }

    /// Call an instance method with **direct** (non-virtual) dispatch.
    ///
    /// Framework entry points such as the private
    /// `android.app.ActivityThread.attach(ZJ)V` are not in any vtable, so
    /// [`Vm::call_method`] (virtual dispatch) cannot reach them. This resolves
    /// the method on the object's exact class, like `invoke-direct`.
    pub fn call_method_direct(
        &mut self,
        obj: Handle,
        name: &str,
        sig: &str,
        args: &[JavaValue],
    ) -> BridgeResult<JavaValue> {
        if obj == NULL {
            return Err(BridgeError::DeadHandle(NULL));
        }
        if self.heap.get(obj).is_none() {
            return Err(BridgeError::DeadHandle(obj));
        }
        let class = self.class_name_of(obj);
        let cid = self
            .class_of_object(obj)
            .ok_or(BridgeError::DeadHandle(obj))?;
        let mid = self.resolve_method(cid, &class, name, sig, InvokeKind::Direct)?;
        self.invoke(mid, obj, args, &class, name, sig)
    }

    /// Call a static method on `class`.
    pub fn call_static(
        &mut self,
        class: &str,
        name: &str,
        sig: &str,
        args: &[JavaValue],
    ) -> BridgeResult<JavaValue> {
        let cid = self.load_class_named(class)?;
        // `<clinit>` runs as a Java frame, so the host thread and its sentinel
        // base frame must exist before class initialisation.
        self.ensure_host_thread().map_err(BridgeError::from)?;
        if let Err(e) = self.initialize_class(cid) {
            let _ = self.ensure_host_thread();
            return Err(self.map_vm_error(e));
        }
        let mid = self.resolve_method(cid, class, name, sig, InvokeKind::Static)?;
        self.invoke(mid, NULL, args, class, name, sig)
    }

    /// Read one element of a Java array. Reference elements are rooted.
    pub fn get_array_element(&mut self, array: Handle, index: usize) -> BridgeResult<JavaValue> {
        let (elem_desc, raw) = {
            let a = match self.heap.get(array) {
                Some(Object::Array(a)) => a,
                _ => return Err(BridgeError::DeadHandle(array)),
            };
            let elem = a.elem_desc.clone();
            let raw = array_read(a, index).ok_or_else(|| {
                BridgeError::Vm(VmError::Internal(format!(
                    "array index {index} out of bounds (len {})",
                    a.len()
                )))
            })?;
            (elem, raw)
        };
        self.value_from_desc(&elem_desc, &raw)
    }

    /// Write one element of a Java array with hard type checking.
    pub fn set_array_element(
        &mut self,
        array: Handle,
        index: usize,
        value: JavaValue,
    ) -> BridgeResult<()> {
        let elem_desc = match self.heap.get(array) {
            Some(Object::Array(a)) => a.elem_desc.clone(),
            _ => return Err(BridgeError::DeadHandle(array)),
        };
        let words = self.encode_desc(&elem_desc, &value).map_err(|e| match e {
            BridgeError::FieldTypeMismatch { declared, provided, .. } => {
                BridgeError::ArrayElementType {
                    index,
                    expected: declared,
                    provided,
                }
            }
            other => other,
        })?;
        let a = match self.heap.get_mut(array) {
            Some(Object::Array(a)) => a,
            _ => return Err(BridgeError::DeadHandle(array)),
        };
        array_write(a, index, &words).ok_or_else(|| {
            BridgeError::Vm(VmError::Internal(format!(
                "array index {index} out of bounds (len {})",
                a.len()
            )))
        })
    }

    /// Number of elements in a Java array.
    pub fn array_length(&self, array: Handle) -> BridgeResult<usize> {
        match self.heap.get(array) {
            Some(Object::Array(a)) => Ok(a.len()),
            _ => Err(BridgeError::DeadHandle(array)),
        }
    }

    /// Convert a Java `String` handle to a Rust `String`.
    pub fn java_string_to_rust(&self, h: Handle) -> BridgeResult<String> {
        self.string_to_rust(h)
            .ok_or(BridgeError::DeadHandle(h))
    }

    // ---- root management -------------------------------------------------

    /// Root an object handle so the collector cannot free it across calls.
    ///
    /// Handles returned by this module are already rooted; this is for handles
    /// obtained some other way.
    pub fn root_handle(&mut self, h: Handle) {
        self.bridge_root(h);
    }

    /// Drop one rooting registration for `h`, if present.
    ///
    /// Only affects roots registered by the bridge (never interpreter/native
    /// transient roots). Returns `true` if a root was released.
    pub fn release_bridge_root(&mut self, h: Handle) -> bool {
        if let Some(i) = self.native_roots.iter().rposition(|&r| r == h) {
            self.native_roots.remove(i);
            true
        } else {
            false
        }
    }

    // ---- internals -------------------------------------------------------

    fn bridge_root(&mut self, h: Handle) {
        if h != NULL {
            self.native_roots.push(h);
        }
    }

    /// Make sure the host has a thread and a sentinel base frame so
    /// `interp::call_method` can return a result even at depth zero.
    fn ensure_host_thread(&mut self) -> VmResult<()> {
        if self.threads.is_empty() {
            let obj = self.make_thread("main", NULL)?;
            self.threads.clear();
            self.threads.push(ThreadRT::new(obj));
            self.current_thread = 0;
        }
        let t = self.current_thread;
        if self.threads[t].frames.is_empty() {
            let mid = self.sentinel_method()?;
            interp::push_frame_values(self, mid, NULL, &[])?;
        }
        Ok(())
    }

    fn sentinel_method(&mut self) -> VmResult<MethodId> {
        if self.c_object != 0 {
            if let Ok(m) = self.find_method(self.c_object, "<init>", "()V", InvokeKind::Direct) {
                return Ok(m);
            }
        }
        if self.methods.is_empty() {
            return Err(VmError::Internal(
                "javabridge: no method available for the host sentinel frame".into(),
            ));
        }
        Ok(0)
    }

    fn load_class_named(&mut self, class: &str) -> BridgeResult<ClassId> {
        let desc = class_descriptor(class);
        self.load_class(&desc).map_err(|e| match e {
            VmError::UnsupportedApi { .. } => BridgeError::ClassNotFound(class.to_string()),
            other => BridgeError::Vm(other),
        })
    }

    fn class_name_of(&mut self, obj: Handle) -> String {
        self.class_of_object(obj)
            .map(|c| self.class_desc_of_id(c))
            .unwrap_or_else(|| "<unknown>".to_string())
    }

    fn resolve_method(
        &mut self,
        cid: ClassId,
        class: &str,
        name: &str,
        sig: &str,
        kind: InvokeKind,
    ) -> BridgeResult<MethodId> {
        self.find_method(cid, name, sig, kind)
            .map_err(|_| BridgeError::MethodNotFound {
                class: class.to_string(),
                name: name.to_string(),
                sig: sig.to_string(),
            })
    }

    fn invoke(
        &mut self,
        mid: MethodId,
        this: Handle,
        args: &[JavaValue],
        class: &str,
        name: &str,
        sig: &str,
    ) -> BridgeResult<JavaValue> {
        let params: Vec<TypeId> = self.methods[mid as usize].params.clone();
        if args.len() != params.len() {
            return Err(BridgeError::ArgCount {
                class: class.to_string(),
                name: name.to_string(),
                sig: sig.to_string(),
                expected: params.len(),
                got: args.len(),
            });
        }
        let mut words: Vec<u64> = Vec::new();
        for (i, (&p, v)) in params.iter().zip(args.iter()).enumerate() {
            let desc = self.type_desc(p).to_string();
            let encoded = self.encode_desc(&desc, v).map_err(|e| match e {
                BridgeError::FieldTypeMismatch { declared, provided, .. } => {
                    BridgeError::ArgType {
                        class: class.to_string(),
                        name: format!("{name}{sig}"),
                        index: i,
                        expected: declared,
                        provided,
                    }
                }
                other => other,
            })?;
            if type_width(self.type_kind(p)) == 2 {
                words.push((encoded[0] as u64) | ((encoded[1] as u64) << 32));
            } else {
                words.push(encoded[0] as u64);
            }
        }
        self.ensure_host_thread().map_err(BridgeError::from)?;
        let raw = match interp::call_method(self, mid, this, &words) {
            Ok(v) => v,
            Err(e) => {
                let _ = self.ensure_host_thread();
                return Err(self.map_vm_error(e));
            }
        };
        let ret = self.methods[mid as usize].ret;
        self.value_from_type(ret, raw)
    }

    fn map_vm_error(&mut self, e: VmError) -> BridgeError {
        match e {
            VmError::JavaException(h) => {
                let class = self
                    .class_of_object(h)
                    .map(|c| self.class_desc_of_id(c))
                    .unwrap_or_else(|| "java.lang.Throwable".to_string());
                let message = self.throwable_message(h).unwrap_or_default();
                BridgeError::JavaException { class, message }
            }
            other => BridgeError::Vm(other),
        }
    }

    /// Find an instance field by name, walking the superclass chain. Finding a
    /// static field with the same name is a hard error (the caller should use
    /// the static field API).
    fn find_field(&mut self, obj: Handle, name: &str) -> BridgeResult<(String, FieldDef)> {
        let class = self.class_of_object(obj).ok_or(BridgeError::DeadHandle(obj))?;
        let start_name = self.class_desc_of_id(class);
        let mut cur = Some(class);
        while let Some(c) = cur {
            let (instance, statics) = {
                let cl = &self.classes[c as usize];
                (cl.declared_instance_fields.clone(), cl.declared_static_fields.clone())
            };
            if let Some(f) = instance.iter().find(|f| f.name == name) {
                return Ok((start_name, f.clone()));
            }
            if statics.iter().any(|f| f.name == name) {
                return Err(BridgeError::FieldIsStatic {
                    class: start_name,
                    field: name.to_string(),
                });
            }
            cur = self.classes[c as usize].super_class;
        }
        Err(BridgeError::FieldNotFound {
            class: start_name,
            field: name.to_string(),
        })
    }

    fn find_static_field(
        &mut self,
        class: ClassId,
        class_name: &str,
        name: &str,
    ) -> BridgeResult<(ClassId, FieldDef)> {
        let mut cur = Some(class);
        while let Some(c) = cur {
            if let Some(f) = self.classes[c as usize]
                .declared_static_fields
                .iter()
                .find(|f| f.name == name)
            {
                return Ok((c, f.clone()));
            }
            cur = self.classes[c as usize].super_class;
        }
        Err(BridgeError::FieldNotFound {
            class: class_name.to_string(),
            field: name.to_string(),
        })
    }

    fn read_instance_slots(&self, obj: Handle, field: &FieldDef) -> BridgeResult<Vec<u32>> {
        let fields = match self.heap.get(obj) {
            Some(Object::Instance { fields, .. }) => fields,
            _ => return Err(BridgeError::DeadHandle(obj)),
        };
        let off = field.offset as usize;
        fields
            .get(off..off + field.width as usize)
            .map(|s| s.to_vec())
            .ok_or(BridgeError::DeadHandle(obj))
    }

    fn write_instance_slots(
        &mut self,
        obj: Handle,
        field: &FieldDef,
        words: &[u32],
    ) -> BridgeResult<()> {
        let fields = match self.heap.get_mut(obj) {
            Some(Object::Instance { fields, .. }) => fields,
            _ => return Err(BridgeError::DeadHandle(obj)),
        };
        let off = field.offset as usize;
        if off + words.len() > fields.len() {
            return Err(BridgeError::DeadHandle(obj));
        }
        for (i, w) in words.iter().enumerate() {
            fields[off + i] = *w;
        }
        Ok(())
    }

    fn encode_field(
        &mut self,
        class: &str,
        field: &FieldDef,
        value: &JavaValue,
    ) -> BridgeResult<Vec<u32>> {
        self.encode_desc(&field.desc, value).map_err(|e| match e {
            BridgeError::FieldTypeMismatch { declared, provided, .. } => {
                BridgeError::FieldTypeMismatch {
                    class: class.to_string(),
                    field: field.name.clone(),
                    declared,
                    provided,
                }
            }
            other => other,
        })
    }

    /// Encode `value` as raw slot words for a declared descriptor.
    fn encode_desc(&mut self, desc: &str, value: &JavaValue) -> BridgeResult<Vec<u32>> {
        match desc.as_bytes().first().copied() {
            Some(b'Z') => match value {
                JavaValue::Boolean(v) => Ok(vec![*v as u32]),
                other => Err(self.mismatch(desc, other)),
            },
            Some(b'B') => match value {
                JavaValue::Byte(v) => Ok(vec![*v as i32 as u32]),
                other => Err(self.mismatch(desc, other)),
            },
            Some(b'C') => match value {
                JavaValue::Char(v) => Ok(vec![*v as u32]),
                other => Err(self.mismatch(desc, other)),
            },
            Some(b'S') => match value {
                JavaValue::Short(v) => Ok(vec![*v as i32 as u32]),
                other => Err(self.mismatch(desc, other)),
            },
            Some(b'I') => match value {
                JavaValue::Int(v) => Ok(vec![*v as u32]),
                other => Err(self.mismatch(desc, other)),
            },
            Some(b'J') => match value {
                JavaValue::Long(v) => Ok(vec![*v as u32, (*v >> 32) as u32]),
                other => Err(self.mismatch(desc, other)),
            },
            Some(b'F') => match value {
                JavaValue::Float(v) => Ok(vec![v.to_bits()]),
                other => Err(self.mismatch(desc, other)),
            },
            Some(b'D') => match value {
                JavaValue::Double(v) => {
                    let bits = v.to_bits();
                    Ok(vec![bits as u32, (bits >> 32) as u32])
                }
                other => Err(self.mismatch(desc, other)),
            },
            Some(b'L') | Some(b'[') => {
                let h = match value {
                    JavaValue::Object(h) => *h,
                    JavaValue::Null => NULL,
                    other => return Err(self.mismatch(desc, other)),
                };
                if h != NULL {
                    let want = self.load_class(desc).map_err(|_| {
                        BridgeError::FieldTypeMismatch {
                            class: String::new(),
                            field: String::new(),
                            declared: desc.to_string(),
                            provided: value.kind().to_string(),
                        }
                    })?;
                    let got = self
                        .class_of_object(h)
                        .ok_or(BridgeError::DeadHandle(h))?;
                    if !self.is_assignable(got, want).map_err(BridgeError::from)? {
                        return Err(self.mismatch(desc, value));
                    }
                    self.bridge_root(h);
                }
                Ok(vec![h])
            }
            _ => Err(BridgeError::BadArrayType(desc.to_string())),
        }
    }

    fn mismatch(&self, declared: &str, provided: &JavaValue) -> BridgeError {
        BridgeError::FieldTypeMismatch {
            class: String::new(),
            field: String::new(),
            declared: field_value_name(declared).to_string(),
            provided: provided.kind().to_string(),
        }
    }

    fn value_from_field(&mut self, field: &FieldDef, raw: &[u32]) -> BridgeResult<JavaValue> {
        self.value_from_desc(&field.desc, raw)
    }

    fn value_from_desc(&mut self, desc: &str, raw: &[u32]) -> BridgeResult<JavaValue> {
        let w0 = raw.first().copied().unwrap_or(0);
        let w1 = raw.get(1).copied().unwrap_or(0);
        let v = match desc.as_bytes().first().copied() {
            Some(b'Z') => JavaValue::Boolean(w0 != 0),
            Some(b'B') => JavaValue::Byte(w0 as u8 as i8),
            Some(b'C') => JavaValue::Char(w0 as u16),
            Some(b'S') => JavaValue::Short(w0 as u16 as i16),
            Some(b'I') => JavaValue::Int(w0 as i32),
            Some(b'J') => JavaValue::Long(((w0 as u64) | ((w1 as u64) << 32)) as i64),
            Some(b'F') => JavaValue::Float(f32::from_bits(w0)),
            Some(b'D') => {
                JavaValue::Double(f64::from_bits((w0 as u64) | ((w1 as u64) << 32)))
            }
            Some(b'L') | Some(b'[') => {
                if w0 == NULL {
                    JavaValue::Null
                } else {
                    self.bridge_root(w0);
                    JavaValue::Object(w0)
                }
            }
            _ => {
                return Err(BridgeError::BadArrayType(desc.to_string()));
            }
        };
        Ok(v)
    }

    fn value_from_type(&mut self, ty: TypeId, raw: u64) -> BridgeResult<JavaValue> {
        if self.type_kind(ty) == crate::class::TypeKind::Void {
            return Ok(JavaValue::Void);
        }
        let desc = self.type_desc(ty).to_string();
        let w0 = raw as u32;
        let w1 = (raw >> 32) as u32;
        self.value_from_desc(&desc, &[w0, w1])
    }

    fn build_array_data(
        &mut self,
        elem_desc: &str,
        values: &[JavaValue],
    ) -> BridgeResult<crate::heap::ArrayData> {
        use crate::heap::ArrayData;
        let mut words: Vec<u32> = Vec::with_capacity(values.len());
        for (i, v) in values.iter().enumerate() {
            let enc = self.encode_desc(elem_desc, v).map_err(|e| match e {
                BridgeError::FieldTypeMismatch { declared, provided, .. } => {
                    BridgeError::ArrayElementType {
                        index: i,
                        expected: declared,
                        provided,
                    }
                }
                other => other,
            })?;
            words.push(enc[0]);
        }
        let data = match elem_desc.as_bytes().first().copied() {
            Some(b'Z') => ArrayData::Bool(words.iter().map(|w| *w != 0).collect()),
            Some(b'B') => ArrayData::Byte(words.iter().map(|w| *w as u8 as i8).collect()),
            Some(b'C') => ArrayData::Char(words.iter().map(|w| *w as u16).collect()),
            Some(b'S') => ArrayData::Short(words.iter().map(|w| *w as u16 as i16).collect()),
            Some(b'I') => ArrayData::Int(words.iter().map(|w| *w as i32).collect()),
            Some(b'J') => ArrayData::Long(
                values
                    .iter()
                    .map(|v| match v {
                        JavaValue::Long(x) => *x,
                        _ => 0,
                    })
                    .collect(),
            ),
            Some(b'F') => ArrayData::Float(
                values
                    .iter()
                    .map(|v| match v {
                        JavaValue::Float(x) => *x,
                        _ => 0.0,
                    })
                    .collect(),
            ),
            Some(b'D') => ArrayData::Double(
                values
                    .iter()
                    .map(|v| match v {
                        JavaValue::Double(x) => *x,
                        _ => 0.0,
                    })
                    .collect(),
            ),
            Some(b'L') | Some(b'[') => ArrayData::Ref(words),
            _ => return Err(BridgeError::BadArrayType(elem_desc.to_string())),
        };
        Ok(data)
    }
}

// ---------------------------------------------------------------------------
// Array raw access
// ---------------------------------------------------------------------------

fn array_read(a: &crate::heap::ArrayObj, index: usize) -> Option<Vec<u32>> {
    use crate::heap::ArrayData;
    Some(match &a.data {
        ArrayData::Bool(v) => vec![*v.get(index)? as u32],
        ArrayData::Byte(v) => vec![*v.get(index)? as i32 as u32],
        ArrayData::Char(v) => vec![*v.get(index)? as u32],
        ArrayData::Short(v) => vec![*v.get(index)? as i32 as u32],
        ArrayData::Int(v) => vec![*v.get(index)? as u32],
        ArrayData::Long(v) => {
            let x = *v.get(index)?;
            vec![x as u32, (x >> 32) as u32]
        }
        ArrayData::Float(v) => vec![v.get(index)?.to_bits()],
        ArrayData::Double(v) => {
            let bits = v.get(index)?.to_bits();
            vec![bits as u32, (bits >> 32) as u32]
        }
        ArrayData::Ref(v) => vec![*v.get(index)?],
    })
}

fn array_write(a: &mut crate::heap::ArrayObj, index: usize, words: &[u32]) -> Option<()> {
    use crate::heap::ArrayData;
    let w0 = *words.first()?;
    let w1 = words.get(1).copied().unwrap_or(0);
    let wide = (w0 as u64) | ((w1 as u64) << 32);
    match &mut a.data {
        ArrayData::Bool(v) => *v.get_mut(index)? = w0 != 0,
        ArrayData::Byte(v) => *v.get_mut(index)? = w0 as u8 as i8,
        ArrayData::Char(v) => *v.get_mut(index)? = w0 as u16,
        ArrayData::Short(v) => *v.get_mut(index)? = w0 as u16 as i16,
        ArrayData::Int(v) => *v.get_mut(index)? = w0 as i32,
        ArrayData::Long(v) => *v.get_mut(index)? = wide as i64,
        ArrayData::Float(v) => *v.get_mut(index)? = f32::from_bits(w0),
        ArrayData::Double(v) => *v.get_mut(index)? = f64::from_bits(wide),
        ArrayData::Ref(v) => *v.get_mut(index)? = w0,
    }
    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptors_are_normalised() {
        assert_eq!(class_descriptor("android.graphics.Rect"), "Landroid/graphics/Rect;");
        assert_eq!(class_descriptor("java/lang/String"), "Ljava/lang/String;");
        assert_eq!(class_descriptor("Ljava/lang/String;"), "Ljava/lang/String;");
        assert_eq!(component_descriptor("I"), "I");
        assert_eq!(component_descriptor("[I"), "[I");
        assert_eq!(component_descriptor("java.lang.String"), "Ljava/lang/String;");
    }

    #[test]
    fn value_kinds_are_reported() {
        assert_eq!(JavaValue::Int(1).kind(), "int");
        assert_eq!(JavaValue::Null.kind(), "null");
        assert_eq!(JavaValue::Object(3).as_handle(), Some(3));
        assert_eq!(JavaValue::Boolean(true).as_handle(), None);
    }
}
