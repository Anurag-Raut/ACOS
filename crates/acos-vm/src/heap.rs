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
    ///
    /// Boxed: this is the widest variant (three `String`s), so leaving it
    /// inline set the size of every `Object` slot and every allocation/free
    /// moved ~100 bytes. The variant is only ever built from a snapshot, so the
    /// extra indirection on the cold path costs nothing.
    StackTraceElement(Box<StackTraceElementData>),
}

#[derive(Clone, Debug)]
pub struct StackTraceElementData {
    pub class_name: String,
    pub method_name: String,
    pub file_name: Option<String>,
    pub line: i32,
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
    /// Total capacity (in `u32` words) currently retained by `field_pool`.
    /// Bounds the pool by bytes rather than by a fixed entry count: the pool is
    /// the free list for instance field arrays, so a small fixed cap (4096)
    /// recycled almost nothing on the allocation benchmarks, which free
    /// hundreds of thousands of tiny instances per collection. The budget
    /// keeps reuse high while capping retained memory.
    pub(crate) field_pool_words: usize,
    /// Approximate live bytes reported as `heap_peak_bytes`.
    pub live_bytes: usize,
    pub peak_bytes: usize,
    pub max_bytes: usize,
    pub allocations: u64,
    pub gc_count: u64,
    pub gc_nanos: u64,
    /// Reused GC mark generation bitmap: `gc_mark[slot] == gc_gen` means live.
    /// A generation counter avoids clearing the whole bitmap on every
    /// collection (allocation + `memset` per GC), which matters on the
    /// allocation-heavy benchmarks.
    pub(crate) gc_mark: Vec<u32>,
    pub(crate) gc_gen: u32,
    /// Reused mark worklist. Kept on the heap so a collection does not have to
    /// allocate and grow a fresh stack every time.
    pub(crate) gc_stack: Vec<Handle>,
}

impl Heap {
    pub fn new(max_bytes: usize) -> Heap {
        Heap {
            objects: vec![None],
            free: Vec::new(),
            field_pool: Vec::new(),
            field_pool_words: 0,
            live_bytes: 0,
            peak_bytes: 0,
            max_bytes,
            allocations: 0,
            gc_count: 0,
            gc_nanos: 0,
            gc_mark: Vec::new(),
            gc_gen: 0,
            gc_stack: Vec::new(),
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
                let cap = fields.capacity();
                if cap <= 64 && self.field_pool_words + cap <= FIELD_POOL_MAX_WORDS {
                    self.field_pool_words += cap;
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
                self.field_pool_words = self.field_pool_words.saturating_sub(v.capacity());
                // `clear` + `resize` zeroes every slot without first zeroing a
                // possibly-nonzero buffer twice (the old `resize` + `fill`).
                v.clear();
                v.resize(slots, 0);
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

/// Upper bound on the total `u32` capacity retained by the instance
/// field-array free list (32 MiB of words). The pool is byte-bounded rather
/// than entry-bounded so a run that frees a large young generation can recycle
/// it on the next one instead of falling back to one host allocation per
/// object.
const FIELD_POOL_MAX_WORDS: usize = 32 * 1024 * 1024 / 4;

/// Approximate retained size of an object (metadata + payload).
#[inline]
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
            BuiltinData::StackTraceElement(d) => {
                d.class_name.len() + d.method_name.len() + d.file_name.as_ref().map_or(0, |s| s.len())
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

/// Bytes per element for an array component descriptor (`L`/`[` are 32-bit
/// handles). Used to bound a guest array request before it reaches the host
/// allocator (`HONESTY_AUDIT.md` #2).
pub fn array_element_size(desc: &str) -> usize {
    match element_kind(desc).unwrap_or('L') {
        'Z' | 'B' => 1,
        'C' | 'S' => 2,
        'I' | 'F' | 'L' | '[' => 4,
        'J' | 'D' => 8,
        _ => 4,
    }
}

/// Hard per-allocation ceiling on a single array's backing store (64 MiB),
/// independent of the guest heap cap. A single guest request must never drive
/// a multi-GB host allocation even when the guest heap would allow it.
pub const MAX_SINGLE_ARRAY_BYTES: usize = 64 * 1024 * 1024;

/// `true` when `len` elements of `elem_desc` fit both the guest heap cap and
/// the hard single-allocation cap.
#[must_use]
pub fn array_len_fits(elem_desc: &str, len: usize, heap_max_bytes: usize) -> bool {
    let bytes = len.saturating_mul(array_element_size(elem_desc));
    bytes <= heap_max_bytes.min(MAX_SINGLE_ARRAY_BYTES)
}

/// Create the backing store for an array of `len` elements of `elem_desc`.
///
/// The hard single-allocation cap is enforced here as a last-resort backstop so
/// a caller that forgets the Java-level check still cannot abort the host
/// allocator. Preferred callers check [`array_len_fits`] first and throw a Java
/// `OutOfMemoryError`.
pub fn new_array_data(elem_desc: &str, len: usize) -> VmResult<ArrayData> {
    let bytes = len.saturating_mul(array_element_size(elem_desc));
    if bytes > MAX_SINGLE_ARRAY_BYTES {
        return Err(VmError::Internal(format!(
            "array allocation of {bytes} bytes (len {len}) exceeds the {MAX_SINGLE_ARRAY_BYTES}-byte per-array cap"
        )));
    }
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
