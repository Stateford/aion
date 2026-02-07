//! Generic arena for dense, ID-indexed storage of IR entities.
//!
//! The [`Arena`] provides O(1) insertion and lookup by opaque [`ArenaId`] keys,
//! cache-friendly sequential memory layout, and efficient iteration.

use serde::{Deserialize, Serialize};
use std::marker::PhantomData;
use std::ops::{Index, IndexMut};

/// Trait for opaque ID types used as arena keys.
///
/// Implementors must provide a bijection between `u32` indices and the ID type.
pub trait ArenaId: Copy {
    /// Creates an ID from a raw `u32` index.
    fn from_raw(index: u32) -> Self;

    /// Returns the raw `u32` index.
    fn as_raw(self) -> u32;
}

/// A dense, ID-indexed container for IR entities.
///
/// Items are always appended (never reordered or removed), making IDs stable
/// for the lifetime of the arena. Supports serialization via `bincode`/`serde`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Arena<I: ArenaId, T> {
    items: Vec<T>,
    #[serde(skip)]
    _marker: PhantomData<I>,
}

impl<I: ArenaId, T> Default for Arena<I, T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<I: ArenaId, T> Arena<I, T> {
    /// Creates a new, empty arena.
    pub fn new() -> Self {
        Self {
            items: Vec::new(),
            _marker: PhantomData,
        }
    }

    /// Allocates a new item in the arena and returns its ID.
    pub fn alloc(&mut self, item: T) -> I {
        let id = I::from_raw(self.items.len() as u32);
        self.items.push(item);
        id
    }

    /// Returns a reference to the item with the given ID.
    ///
    /// # Panics
    ///
    /// Panics if the ID is out of bounds.
    pub fn get(&self, id: I) -> &T {
        &self.items[id.as_raw() as usize]
    }

    /// Returns a mutable reference to the item with the given ID.
    ///
    /// # Panics
    ///
    /// Panics if the ID is out of bounds.
    pub fn get_mut(&mut self, id: I) -> &mut T {
        &mut self.items[id.as_raw() as usize]
    }

    /// Returns the number of items in the arena.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Returns `true` if the arena contains no items.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Iterates over `(ID, &T)` pairs in allocation order.
    pub fn iter(&self) -> impl Iterator<Item = (I, &T)> {
        self.items
            .iter()
            .enumerate()
            .map(|(i, item)| (I::from_raw(i as u32), item))
    }

    /// Iterates over `(ID, &mut T)` pairs in allocation order.
    pub fn iter_mut(&mut self) -> impl Iterator<Item = (I, &mut T)> {
        self.items
            .iter_mut()
            .enumerate()
            .map(|(i, item)| (I::from_raw(i as u32), item))
    }

    /// Iterates over references to items in allocation order.
    pub fn values(&self) -> impl Iterator<Item = &T> {
        self.items.iter()
    }
}

impl<I: ArenaId, T> Index<I> for Arena<I, T> {
    type Output = T;

    fn index(&self, id: I) -> &T {
        self.get(id)
    }
}

impl<I: ArenaId, T> IndexMut<I> for Arena<I, T> {
    fn index_mut(&mut self, id: I) -> &mut T {
        self.get_mut(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::ModuleId;

    #[test]
    fn alloc_and_get() {
        let mut arena: Arena<ModuleId, String> = Arena::new();
        let id = arena.alloc("hello".to_string());
        assert_eq!(arena[id], "hello");
    }

    #[test]
    fn multiple_allocs() {
        let mut arena: Arena<ModuleId, u32> = Arena::new();
        let a = arena.alloc(10);
        let b = arena.alloc(20);
        let c = arena.alloc(30);
        assert_eq!(arena[a], 10);
        assert_eq!(arena[b], 20);
        assert_eq!(arena[c], 30);
        assert_eq!(arena.len(), 3);
    }

    #[test]
    fn get_mut_modifies() {
        let mut arena: Arena<ModuleId, String> = Arena::new();
        let id = arena.alloc("original".to_string());
        *arena.get_mut(id) = "modified".to_string();
        assert_eq!(arena[id], "modified");
    }

    #[test]
    fn empty_arena() {
        let arena: Arena<ModuleId, u32> = Arena::new();
        assert!(arena.is_empty());
        assert_eq!(arena.len(), 0);
    }

    #[test]
    fn iter_returns_all_items() {
        let mut arena: Arena<ModuleId, &str> = Arena::new();
        arena.alloc("a");
        arena.alloc("b");
        arena.alloc("c");
        let collected: Vec<_> = arena.iter().map(|(_, v)| *v).collect();
        assert_eq!(collected, vec!["a", "b", "c"]);
    }

    #[test]
    fn iter_ids_are_sequential() {
        let mut arena: Arena<ModuleId, u32> = Arena::new();
        arena.alloc(100);
        arena.alloc(200);
        let ids: Vec<u32> = arena.iter().map(|(id, _)| id.as_raw()).collect();
        assert_eq!(ids, vec![0, 1]);
    }

    #[test]
    fn serde_roundtrip() {
        let mut arena: Arena<ModuleId, String> = Arena::new();
        arena.alloc("first".to_string());
        arena.alloc("second".to_string());
        let json = serde_json::to_string(&arena).unwrap();
        let restored: Arena<ModuleId, String> = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.len(), 2);
        assert_eq!(restored[ModuleId::from_raw(0)], "first");
        assert_eq!(restored[ModuleId::from_raw(1)], "second");
    }

    #[test]
    fn default_is_empty() {
        let arena: Arena<ModuleId, u32> = Arena::default();
        assert!(arena.is_empty());
    }
}
