//! Generation-checked handle tables shared by the `android.graphics.*` natives.
//!
//! The Android framework passes opaque `jlong` values across the JNI boundary
//! for objects that live in native memory (`Paint`, `Path`, `Matrix`, `Region`,
//! `Typeface`, `ColorFilter`, `Canvas`, `RenderNode`, `Bitmap`, …). ACOS keeps
//! each such object in a Rust handle table and maps the `jlong` onto a
//! generation-checked [`Handle`].
//!
//! A handle is `(generation << 32) | index`. Freeing a slot bumps the
//! generation, so a *stale* `jlong` (used after `NativeAllocationRegistry`
//! freed the object, or a forged/truncated value) resolves to
//! [`HandleError::Stale`] instead of aliasing a newly-created object. Natives
//! turn that into a structured `VM` error; they never panic on a bad handle.
//!
//! This module is owned by W19-B (T-31) and imported by W19-C's
//! `gfx_canvas.rs` / `gfx_node.rs` / `gfx_bitmap.rs`. Every graphics native that
//! owns a `jlong` should keep one [`HandleTable`] per object type.

use std::collections::HashMap;

/// An opaque, generation-checked handle to an object in a [`HandleTable`].
#[derive(Copy, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct Handle(pub u64);

impl Handle {
    /// Low 32 bits: slot index.
    #[inline]
    pub const fn index(self) -> u32 {
        self.0 as u32
    }

    /// High 32 bits: generation counter.
    #[inline]
    pub const fn generation(self) -> u32 {
        (self.0 >> 32) as u32
    }

    #[inline]
    const fn new(index: u32, generation: u32) -> Self {
        Handle(((generation as u64) << 32) | index as u64)
    }

    /// Encode for the JNI boundary. `0` is Android's null pointer and is never
    /// a valid handle, so it is never returned by [`HandleTable::insert`].
    #[inline]
    pub const fn to_jlong(self) -> i64 {
        self.0 as i64
    }

    /// Decode a `jlong` from the JNI boundary. `0` maps to [`Handle::NULL`].
    #[inline]
    pub const fn from_jlong(v: i64) -> Self {
        Handle(v as u64)
    }

    /// Android's null native pointer (`jlong 0`).
    pub const NULL: Handle = Handle(0);

    /// Whether this is the null sentinel.
    #[inline]
    pub const fn is_null(self) -> bool {
        self.0 == 0
    }
}

impl Default for Handle {
    fn default() -> Self {
        Handle::NULL
    }
}

/// Why a `jlong` did not resolve to a live object.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
pub enum HandleError {
    /// `jlong 0` — Android's null pointer.
    Null,
    /// The slot was freed (generation mismatch) or the value is forged.
    Stale { handle: Handle },
}

impl HandleError {
    /// A short machine-readable reason for the structured VM error message.
    pub fn reason(&self) -> &'static str {
        match self {
            HandleError::Null => "null native handle",
            HandleError::Stale { .. } => "stale native handle",
        }
    }
}

impl std::fmt::Display for HandleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HandleError::Null => write!(f, "null native handle"),
            HandleError::Stale { handle } => write!(
                f,
                "stale native handle 0x{:016x} (index {}, generation {})",
                handle.0,
                handle.index(),
                handle.generation()
            ),
        }
    }
}

struct Slot<T> {
    generation: u32,
    value: Option<T>,
}

/// A dense, generation-checked object table.
///
/// Lookups are O(1). The table owns its values, so it never hands out a
/// reference that can outlive the value; callers borrow through
/// [`HandleTable::get`] / [`HandleTable::get_mut`].
pub struct HandleTable<T> {
    slots: Vec<Slot<T>>,
    free: Vec<u32>,
    live: usize,
}

impl<T> std::fmt::Debug for HandleTable<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HandleTable")
            .field("slots", &self.slots.len())
            .field("live", &self.live)
            .finish()
    }
}

impl<T> Default for HandleTable<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> HandleTable<T> {
    pub fn new() -> Self {
        HandleTable {
            slots: Vec::new(),
            free: Vec::new(),
            live: 0,
        }
    }

    /// Number of live objects.
    pub fn len(&self) -> usize {
        self.live
    }

    pub fn is_empty(&self) -> bool {
        self.live == 0
    }

    /// Insert a value and return its handle. The returned handle is never null.
    pub fn insert(&mut self, value: T) -> Handle {
        self.live += 1;
        if let Some(index) = self.free.pop() {
            let slot = &mut self.slots[index as usize];
            debug_assert!(slot.value.is_none());
            slot.value = Some(value);
            Handle::new(index, slot.generation)
        } else {
            let index = self.slots.len() as u32;
            // Slot 0 must never be produced as generation 0 for null-ish
            // confusion; the first generation is 1.
            self.slots.push(Slot {
                generation: 1,
                value: Some(value),
            });
            Handle::new(index, 1)
        }
    }

    /// Re-install a value at a previously serialised handle, preserving its
    /// slot index and generation.
    ///
    /// Snapshot restore needs this: the framework's Java fields hold the exact
    /// `(generation << 32) | index` value that was live when the snapshot was
    /// taken, so the table must hand back the same handle rather than a fresh
    /// one. A handle already live in the table is replaced without bumping its
    /// generation; holes left by the serialised set are kept on the free list.
    /// Returns `false` for the null sentinel.
    pub fn reinsert(&mut self, handle: Handle, value: T) -> bool {
        if handle.is_null() {
            return false;
        }
        let index = handle.index() as usize;
        let old_len = self.slots.len();
        if old_len <= index {
            self.slots.resize_with(index + 1, || Slot {
                generation: 1,
                value: None,
            });
            for i in old_len..index {
                self.free.push(i as u32);
            }
        }
        let slot = &mut self.slots[index];
        if slot.value.is_none() {
            self.live += 1;
        }
        slot.generation = handle.generation();
        slot.value = Some(value);
        if let Some(pos) = self.free.iter().position(|&i| i == index as u32) {
            self.free.swap_remove(pos);
        }
        true
    }

    /// Drop every live object and reset handle generations.
    ///
    /// Used when importing a restored handle table over an already-initialised
    /// process-global table.
    pub fn clear(&mut self) {
        self.slots.clear();
        self.free.clear();
        self.live = 0;
    }

    /// Resolve a handle to a shared reference.
    pub fn get(&self, handle: Handle) -> Result<&T, HandleError> {
        if handle.is_null() {
            return Err(HandleError::Null);
        }
        let slot = self
            .slots
            .get(handle.index() as usize)
            .filter(|slot| slot.generation == handle.generation())
            .ok_or(HandleError::Stale { handle })?;
        slot.value.as_ref().ok_or(HandleError::Stale { handle })
    }

    /// Resolve a handle to a mutable reference.
    pub fn get_mut(&mut self, handle: Handle) -> Result<&mut T, HandleError> {
        if handle.is_null() {
            return Err(HandleError::Null);
        }
        let slot = self
            .slots
            .get_mut(handle.index() as usize)
            .filter(|slot| slot.generation == handle.generation())
            .ok_or(HandleError::Stale { handle })?;
        slot.value.as_mut().ok_or(HandleError::Stale { handle })
    }

    /// True if the handle currently resolves.
    pub fn contains(&self, handle: Handle) -> bool {
        self.get(handle).is_ok()
    }

    /// Remove an object, invalidating its handle.
    pub fn remove(&mut self, handle: Handle) -> Result<T, HandleError> {
        if handle.is_null() {
            return Err(HandleError::Null);
        }
        let slot = self
            .slots
            .get_mut(handle.index() as usize)
            .filter(|slot| slot.generation == handle.generation())
            .ok_or(HandleError::Stale { handle })?;
        let value = slot.value.take().ok_or(HandleError::Stale { handle })?;
        // Bump the generation so the stale handle no longer resolves. `max(1)`
        // keeps a wrapped generation from colliding with the null sentinel.
        slot.generation = slot.generation.wrapping_add(1).max(1);
        self.free.push(handle.index());
        self.live -= 1;
        Ok(value)
    }

    /// Iterate over all `(handle, &value)` pairs.
    pub fn iter(&self) -> impl Iterator<Item = (Handle, &T)> {
        self.slots.iter().enumerate().filter_map(|(i, slot)| {
            slot.value
                .as_ref()
                .map(|v| (Handle::new(i as u32, slot.generation), v))
        })
    }

    /// Iterate over all `(handle, &mut value)` pairs.
    pub fn iter_mut(&mut self) -> impl Iterator<Item = (Handle, &mut T)> {
        self.slots.iter_mut().enumerate().filter_map(|(i, slot)| {
            let generation = slot.generation;
            slot.value
                .as_mut()
                .map(move |v| (Handle::new(i as u32, generation), v))
        })
    }
}

/// A named registry of `jlong`-keyed graphics objects, one [`HandleTable`] per
/// type. Convenience for the per-native `thread_local!` state used by the gfx
/// modules.
pub struct GfxRegistry<T> {
    table: HandleTable<T>,
    by_name: HashMap<&'static str, Handle>,
}

impl<T> std::fmt::Debug for GfxRegistry<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GfxRegistry")
            .field("live", &self.table.live)
            .field("named", &self.by_name.len())
            .finish()
    }
}

impl<T> Default for GfxRegistry<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> GfxRegistry<T> {
    pub fn new() -> Self {
        GfxRegistry {
            table: HandleTable::new(),
            by_name: HashMap::new(),
        }
    }

    pub fn insert(&mut self, value: T) -> Handle {
        self.table.insert(value)
    }

    pub fn get(&self, handle: Handle) -> Result<&T, HandleError> {
        self.table.get(handle)
    }

    pub fn get_mut(&mut self, handle: Handle) -> Result<&mut T, HandleError> {
        self.table.get_mut(handle)
    }

    pub fn remove(&mut self, handle: Handle) -> Result<T, HandleError> {
        self.table.remove(handle)
    }

    pub fn len(&self) -> usize {
        self.table.len()
    }

    pub fn is_empty(&self) -> bool {
        self.table.is_empty()
    }

    /// Bind a well-known singleton (e.g. `Typeface.DEFAULT`) to a name.
    pub fn set_named(&mut self, name: &'static str, handle: Handle) {
        self.by_name.insert(name, handle);
    }

    /// Resolve a well-known singleton.
    pub fn named(&self, name: &str) -> Option<Handle> {
        self.by_name.get(name).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_get_round_trip() {
        let mut t: HandleTable<u32> = HandleTable::new();
        let h = t.insert(10);
        assert_eq!(t.get(h), Ok(&10));
        assert_eq!(t.len(), 1);
    }

    #[test]
    fn stale_handle_is_rejected_not_reused() {
        let mut t: HandleTable<u32> = HandleTable::new();
        let h1 = t.insert(10);
        assert_eq!(t.remove(h1), Ok(10));
        assert!(matches!(t.get(h1), Err(HandleError::Stale { .. })));
        let h2 = t.insert(20);
        assert_ne!(h1, h2, "reused slot must have a new generation");
        assert_eq!(t.get(h2), Ok(&20));
        assert!(matches!(t.get(h1), Err(HandleError::Stale { .. })));
    }

    #[test]
    fn null_never_resolves() {
        let t: HandleTable<u32> = HandleTable::new();
        assert_eq!(t.get(Handle::NULL), Err(HandleError::Null));
        assert_eq!(Handle::NULL.to_jlong(), 0);
    }

    #[test]
    fn jlong_round_trip_preserves_generation() {
        let mut t: HandleTable<u32> = HandleTable::new();
        let h = t.insert(7);
        let j = h.to_jlong();
        assert_eq!(Handle::from_jlong(j), h);
        assert_eq!(t.get(Handle::from_jlong(j)), Ok(&7));
    }

    #[test]
    fn remove_twice_is_stale_the_second_time() {
        let mut t: HandleTable<u32> = HandleTable::new();
        let h = t.insert(1);
        assert_eq!(t.remove(h), Ok(1));
        assert!(matches!(t.remove(h), Err(HandleError::Stale { .. })));
    }

    #[test]
    fn forged_handle_is_stale() {
        let t: HandleTable<u32> = HandleTable::new();
        assert!(matches!(
            t.get(Handle(0xdead_beef)),
            Err(HandleError::Stale { .. })
        ));
    }
}
