//! Generation-checked integer handles.
//!
//! The Android `android.graphics.*` native methods pass opaque `jlong` values
//! around (typically pointers to C++ objects). ACOS keeps every object in a
//! handle table owned by [`crate::RenderContext`], so the JNI binding task can
//! map those `jlong`s onto the `u64` handles produced here. A handle encodes
//! `(generation << 32) | index`; reusing a freed slot bumps the generation, so
//! a stale handle is rejected as [`crate::RenderError::BadHandle`] instead of
//! silently aliasing a new object (the classic use-after-free bug class).

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

    /// A handle value that is never valid (useful as a null sentinel, matching
    /// Android's `0` pointer).
    pub const NULL: Handle = Handle(0);
}

impl Default for Handle {
    fn default() -> Self {
        Handle::NULL
    }
}

struct Slot<T> {
    generation: u32,
    value: Option<T>,
}

/// A dense, generation-checked object table.
///
/// `T: Clone` is intentionally not required. Lookups are O(1) and never
/// allocate the value again.
pub struct HandleTable<T> {
    slots: Vec<Slot<T>>,
    free: Vec<u32>,
    live: usize,
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

    /// Insert a value and return its handle.
    pub fn insert(&mut self, value: T) -> Handle {
        self.live += 1;
        if let Some(index) = self.free.pop() {
            let slot = &mut self.slots[index as usize];
            debug_assert!(slot.value.is_none());
            slot.value = Some(value);
            Handle::new(index, slot.generation)
        } else {
            let index = self.slots.len() as u32;
            self.slots.push(Slot {
                generation: 1,
                value: Some(value),
            });
            Handle::new(index, 1)
        }
    }

    /// Resolve a handle to a shared reference.
    pub fn get(&self, handle: Handle) -> Option<&T> {
        let slot = self.slots.get(handle.index() as usize)?;
        if slot.generation != handle.generation() {
            return None;
        }
        slot.value.as_ref()
    }

    /// Resolve a handle to a mutable reference.
    pub fn get_mut(&mut self, handle: Handle) -> Option<&mut T> {
        let slot = self.slots.get_mut(handle.index() as usize)?;
        if slot.generation != handle.generation() {
            return None;
        }
        slot.value.as_mut()
    }

    /// True if the handle currently resolves.
    pub fn contains(&self, handle: Handle) -> bool {
        self.get(handle).is_some()
    }

    /// Remove an object, invalidating its handle.
    pub fn remove(&mut self, handle: Handle) -> Option<T> {
        let slot = self.slots.get_mut(handle.index() as usize)?;
        if slot.generation != handle.generation() {
            return None;
        }
        let value = slot.value.take()?;
        // Bump generation so the stale handle no longer resolves.
        slot.generation = slot.generation.wrapping_add(1).max(1);
        self.free.push(handle.index());
        self.live -= 1;
        Some(value)
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

/// A simple string→index interner used for names, feature strings and the
/// "unsupported op" list (deduplicated, ordered).
#[derive(Default)]
pub struct Interner {
    map: HashMap<String, usize>,
    items: Vec<String>,
}

impl Interner {
    pub fn intern(&mut self, s: &str) -> usize {
        if let Some(&i) = self.map.get(s) {
            return i;
        }
        let i = self.items.len();
        self.items.push(s.to_string());
        self.map.insert(s.to_string(), i);
        i
    }

    pub fn items(&self) -> &[String] {
        &self.items
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_handle_is_rejected() {
        let mut table: HandleTable<u32> = HandleTable::new();
        let h1 = table.insert(10);
        assert_eq!(table.get(h1), Some(&10));
        assert_eq!(table.remove(h1), Some(10));
        assert!(table.get(h1).is_none());
        let h2 = table.insert(20);
        assert_ne!(h1, h2);
        assert_eq!(table.get(h2), Some(&20));
        assert_eq!(table.get(h1), None);
    }

    #[test]
    fn null_handle_never_resolves() {
        let table: HandleTable<u32> = HandleTable::new();
        assert!(table.get(Handle::NULL).is_none());
    }
}
