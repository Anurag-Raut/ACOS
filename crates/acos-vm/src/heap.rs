//! The managed heap.
//!
//! Object references are 32-bit **handles** (indices into the heap slot table;
//! handle `0` is `null`). The heap never hands out host pointers: this keeps
//! later GC compaction, heap snapshot/restore and JNI handle tables possible.

use crate::error::{VmError, VmResult};

pub type Handle = u32;
pub const NULL: Handle = 0;

/// A user-defined or core-library instance payload.
#[derive(Clone, Debug)]
pub enum BuiltinData {
    StringBuilder(Vec<u16>),
    /// `java.util.List` / `ArrayList`.
    List(Vec<Handle>),
    /// `java.util.HashMap`.
    HashMap(Vec<(Handle, Handle)>),
    /// `java.util.TreeMap` (entries kept sorted by key).
    TreeMap(Vec<(Handle, Handle)>),
    /// `java.util.Map$Entry`.
    Entry { key: Handle, value: Handle },
    /// A snapshot iterator over a list/set of elements.
    Iterator { items: Vec<Handle>, index: usize },
    /// `java.util.EnumSet`.
    EnumSet { enum_class: u32, elements: Vec<Handle> },
    /// A live `java.lang.Thread`.
    Thread(ThreadData),
    /// `java.lang.StackTraceElement`.
    StackTraceElement {
        class_name: String,
        method_name: String,
        file_name: Option<String>,
        line: i32,
    },
}

#[derive(Clone, Debug)]
pub struct ThreadData {
    pub id: u32,
    pub name: Handle,
    pub target: Handle,
    pub alive: bool,
    pub daemon: bool,
}

/// A live heap object.
#[derive(Clone, Debug)]
pub enum Object {
    /// An instance with `class` and raw 32-bit field slots. Reference fields
    /// are traced by the class layout.
    Instance { class: u32, fields: Vec<u32> },
    /// A typed array.
    Array(ArrayObj),
    /// A `java.lang.String`, stored as UTF-16 code units (Java semantics).
    String(Vec<u16>),
    /// A `java.lang.Class` object.
    Class(u32),
    /// A core-library object with an internal Rust payload.
    Builtin { class: u32, data: BuiltinData },
}

/// A typed array.
#[derive(Clone, Debug)]
pub struct ArrayObj {
    /// Component type descriptor: `I`, `J`, `Z`, `[I`, `Ljava/lang/String;`, ...
    pub elem_desc: String,
    pub data: ArrayData,
}

impl ArrayObj {
    pub fn len(&self) -> usize {
        self.data.len()
    }

    pub fn is_empty(&self) -> bool {
        self.data.len() == 0
    }
}

#[derive(Clone, Debug)]
pub enum ArrayData {
    Bool(Vec<bool>),
    Byte(Vec<i8>),
    Char(Vec<u16>),
    Short(Vec<i16>),
    Int(Vec<i32>),
    Long(Vec<i64>),
    Float(Vec<f32>),
    Double(Vec<f64>),
    Ref(Vec<Handle>),
}

impl ArrayData {
    pub fn len(&self) -> usize {
        match self {
            ArrayData::Bool(v) => v.len(),
            ArrayData::Byte(v) => v.len(),
            ArrayData::Char(v) => v.len(),
            ArrayData::Short(v) => v.len(),
            ArrayData::Int(v) => v.len(),
            ArrayData::Long(v) => v.len(),
            ArrayData::Float(v) => v.len(),
            ArrayData::Double(v) => v.len(),
            ArrayData::Ref(v) => v.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// The handle table and its allocation bookkeeping.
#[derive(Debug)]
pub struct Heap {
    /// Slot 0 is permanently `None` (the `null` handle).
    pub(crate) objects: Vec<Option<Object>>,
    pub(crate) free: Vec<u32>,
    /// Recycled small instance field arrays. Frameworks (and allocation
    /// benchmarks) create many tiny instances in a loop; reusing their field
    /// storage removes a `Vec` allocation per object. Untraced: contents are
    /// zeroed on reuse.
    pub(crate) field_pool: Vec<Vec<u32>>,
    /// Approximate live bytes reported as `heap_peak_bytes`.
    pub live_bytes: usize,
    pub peak_bytes: usize,
    pub max_bytes: usize,
    pub allocations: u64,
    pub gc_count: u64,
    pub gc_nanos: u64,
}

impl Heap {
    pub fn new(max_bytes: usize) -> Heap {
        Heap {
            objects: vec![None],
            free: Vec::new(),
            field_pool: Vec::new(),
            live_bytes: 0,
            peak_bytes: 0,
            max_bytes,
            allocations: 0,
            gc_count: 0,
            gc_nanos: 0,
        }
    }

    #[inline]
    pub fn get(&self, h: Handle) -> Option<&Object> {
        self.objects.get(h as usize).and_then(|o| o.as_ref())
    }

    #[inline]
    pub fn get_mut(&mut self, h: Handle) -> Option<&mut Object> {
        self.objects.get_mut(h as usize).and_then(|o| o.as_mut())
    }

    pub fn is_live(&self, h: Handle) -> bool {
        h != NULL && self.get(h).is_some()
    }

    /// Allocate an object, even if this pushes the heap over its cap. The
    /// caller is responsible for running a GC first (see `Vm::alloc`).
    pub fn alloc_raw(&mut self, obj: Object) -> Handle {
        let size = object_size(&obj);
        let h = if let Some(h) = self.free.pop() {
            self.objects[h as usize] = Some(obj);
            h
        } else {
            self.objects.push(Some(obj));
            (self.objects.len() - 1) as u32
        };
        self.live_bytes += size;
        if self.live_bytes > self.peak_bytes {
            self.peak_bytes = self.live_bytes;
        }
        self.allocations += 1;
        h
    }

    /// Free a slot (used by the sweeper). Small instance field arrays are
    /// recycled instead of dropped.
    pub(crate) fn free_slot(&mut self, h: Handle) {
        if let Some(obj) = self.objects[h as usize].take() {
            let size = object_size(&obj);
            self.live_bytes = self.live_bytes.saturating_sub(size);
            if let Object::Instance { fields, .. } = obj {
                if fields.capacity() <= 64 && self.field_pool.len() < 4096 {
                    self.field_pool.push(fields);
                }
            }
        }
        self.free.push(h);
    }

    /// Take a field array with room for `slots` zeroed words, from the pool if
    /// possible (else a fresh allocation).
    #[inline]
    pub(crate) fn take_field_vec(&mut self, slots: usize) -> Vec<u32> {
        match self.field_pool.pop() {
            Some(mut v) => {
                v.resize(slots, 0);
                v.fill(0);
                v
            }
            None => vec![0; slots],
        }
    }

    pub fn oom_error(&self) -> VmError {
        VmError::Internal(format!(
            "OutOfMemoryError: heap limit {} bytes exceeded (live {})",
            self.max_bytes, self.live_bytes
        ))
    }
}

/// Approximate retained size of an object (metadata + payload).
fn object_size(obj: &Object) -> usize {
    let base = std::mem::size_of::<Object>();
    let payload = match obj {
        Object::Instance { fields, .. } => fields.len() * 4,
        Object::String(u) => u.len() * 2,
        Object::Array(a) => match &a.data {
            ArrayData::Bool(v) => v.len(),
            ArrayData::Byte(v) => v.len(),
            ArrayData::Char(v) => v.len() * 2,
            ArrayData::Short(v) => v.len() * 2,
            ArrayData::Int(v) => v.len() * 4,
            ArrayData::Long(v) => v.len() * 8,
            ArrayData::Float(v) => v.len() * 4,
            ArrayData::Double(v) => v.len() * 8,
            ArrayData::Ref(v) => v.len() * 4,
        },
        Object::Builtin { data, .. } => match data {
            BuiltinData::StringBuilder(v) => v.len() * 2,
            BuiltinData::List(v) | BuiltinData::Iterator { items: v, .. } => v.len() * 4,
            BuiltinData::HashMap(v) | BuiltinData::TreeMap(v) => v.len() * 8,
            BuiltinData::EnumSet { elements, .. } => elements.len() * 4,
            BuiltinData::Thread(_) => 32,
            BuiltinData::Entry { .. } => 8,
            BuiltinData::StackTraceElement { class_name, method_name, file_name, .. } => {
                class_name.len() + method_name.len() + file_name.as_ref().map_or(0, |s| s.len())
            }
        },
        Object::Class(_) => 16,
    };
    base + payload
}

/// Convert a descriptor's leading character to the array element kind.
pub fn element_kind(desc: &str) -> VmResult<char> {
    desc.chars()
        .next()
        .ok_or_else(|| VmError::Internal("empty array component descriptor".into()))
}

/// Create the backing store for an array of `len` elements of `elem_desc`.
pub fn new_array_data(elem_desc: &str, len: usize) -> VmResult<ArrayData> {
    let c = element_kind(elem_desc)?;
    Ok(match c {
        'Z' => ArrayData::Bool(vec![false; len]),
        'B' => ArrayData::Byte(vec![0; len]),
        'C' => ArrayData::Char(vec![0; len]),
        'S' => ArrayData::Short(vec![0; len]),
        'I' => ArrayData::Int(vec![0; len]),
        'J' => ArrayData::Long(vec![0; len]),
        'F' => ArrayData::Float(vec![0.0; len]),
        'D' => ArrayData::Double(vec![0.0; len]),
        'L' | '[' => ArrayData::Ref(vec![NULL; len]),
        other => {
            return Err(VmError::Internal(format!(
                "invalid array component descriptor {other:?}"
            )))
        }
    })
}
