//! Keyed selector and geometry caches.

use super::snapshot::{DeltaId, ObjectId};
use std::collections::{HashMap, VecDeque};
use std::hash::Hash;

/// A selector key tied to both the source object and the exact view delta.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SelectorKey {
    /// Stable source/object identity.
    pub object: ObjectId,
    /// Exact delta that produced the projection.
    pub delta: DeltaId,
}

impl SelectorKey {
    /// Creates a selector key.
    #[must_use]
    pub const fn new(object: ObjectId, delta: DeltaId) -> Self {
        Self { object, delta }
    }
}

/// Small deterministic memo table for pure selectors.
#[derive(Clone, Debug)]
pub struct KeyedSelectorCache<K, V> {
    values: HashMap<K, V>,
    order: VecDeque<K>,
    capacity: usize,
}

impl<K: Eq + Hash + Clone, V> Default for KeyedSelectorCache<K, V> {
    fn default() -> Self {
        Self::with_capacity(256)
    }
}

impl<K: Eq + Hash + Clone, V> KeyedSelectorCache<K, V> {
    /// Creates an empty cache.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a bounded cache. The least-recently-used key is evicted first.
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            values: HashMap::new(),
            order: VecDeque::new(),
            capacity: capacity.max(1),
        }
    }

    /// Looks up a value without recomputing it.
    #[must_use]
    pub fn get(&mut self, key: &K) -> Option<&V> {
        if self.values.contains_key(key) {
            self.order.retain(|candidate| candidate != key);
            self.order.push_back(key.clone());
        }
        self.values.get(key)
    }

    /// Returns an existing value or computes and inserts one.
    pub fn get_or_insert_with(&mut self, key: K, compute: impl FnOnce() -> V) -> &V {
        if !self.values.contains_key(&key) {
            while self.values.len() >= self.capacity {
                let Some(oldest) = self.order.pop_front() else {
                    break;
                };
                self.values.remove(&oldest);
            }
        }
        match self.values.entry(key.clone()) {
            std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
            std::collections::hash_map::Entry::Vacant(entry) => {
                self.order.push_back(key);
                entry.insert(compute())
            }
        }
    }

    /// Invalidates one key.
    pub fn invalidate(&mut self, key: &K) -> Option<V> {
        let value = self.values.remove(key);
        if value.is_some() {
            self.order.retain(|candidate| candidate != key);
        }
        value
    }

    /// Invalidates every selector whose key names the supplied delta.
    pub fn invalidate_delta(&mut self, delta: DeltaId)
    where
        K: SelectorKeyLike,
    {
        self.values.retain(|key, _| key.delta() != delta);
        self.order.retain(|key| self.values.contains_key(key));
    }

    /// Retains only entries accepted by a bounded invalidation predicate.
    pub fn retain(&mut self, mut keep: impl FnMut(&K, &V) -> bool) {
        self.values.retain(|key, value| keep(key, value));
        self.order.retain(|key| self.values.contains_key(key));
    }

    /// Returns the number of memoized values.
    #[must_use]
    pub fn len(&self) -> usize {
        self.values.len()
    }

    /// Returns whether no values are memoized.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    /// Returns the configured entry bound.
    #[must_use]
    pub const fn capacity(&self) -> usize {
        self.capacity
    }
}

/// Key types that carry a delta identity.
pub trait SelectorKeyLike {
    /// Returns the delta bound to this key.
    fn delta(&self) -> DeltaId;
}

impl SelectorKeyLike for SelectorKey {
    fn delta(&self) -> DeltaId {
        self.delta
    }
}

/// The width/content key used for row measurement.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct LayoutKey {
    /// Stable source/object identity.
    pub object: ObjectId,
    /// Exact delta that produced the row content.
    pub delta: DeltaId,
    /// Available width in physical pixels.
    pub width: u32,
}

impl LayoutKey {
    /// Creates a layout key with a normalized width.
    #[must_use]
    pub const fn new(object: ObjectId, delta: DeltaId, width: u32) -> Self {
        Self {
            object,
            delta,
            width,
        }
    }
}

impl SelectorKeyLike for LayoutKey {
    fn delta(&self) -> DeltaId {
        self.delta
    }
}

/// Memoized row heights and layout measurements.
#[derive(Clone, Debug, Default)]
pub struct RowHeightCache {
    values: KeyedSelectorCache<LayoutKey, u32>,
}

impl RowHeightCache {
    /// Returns a cached row height or measures it once.
    pub fn height(&mut self, key: LayoutKey, measure: impl FnOnce() -> u32) -> u32 {
        *self.values.get_or_insert_with(key, measure)
    }

    /// Invalidates all widths for one object.
    pub fn invalidate_object(&mut self, object: ObjectId) {
        self.values.retain(|key, _| key.object != object);
    }

    /// Invalidates every cached row produced by one delta.
    pub fn invalidate_delta(&mut self, delta: DeltaId) {
        self.values.invalidate_delta(delta);
    }

    /// Returns the number of retained measurements.
    #[must_use]
    pub fn len(&self) -> usize {
        self.values.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selectors_are_reused_until_their_key_changes() {
        let key = SelectorKey::new(ObjectId::test(1), DeltaId::test(2));
        let mut cache = KeyedSelectorCache::new();
        let mut calls = 0;
        assert_eq!(
            *cache.get_or_insert_with(key, || {
                calls += 1;
                9_u32
            }),
            9
        );
        assert_eq!(
            *cache.get_or_insert_with(key, || {
                calls += 1;
                10_u32
            }),
            9
        );
        assert_eq!(calls, 1);
        cache.invalidate_delta(DeltaId::test(2));
        assert!(cache.get(&key).is_none());
    }

    #[test]
    fn row_height_cache_is_keyed_by_object_delta_and_width() {
        let mut cache = RowHeightCache::default();
        let a = LayoutKey::new(ObjectId::test(1), DeltaId::test(1), 400);
        let b = LayoutKey::new(ObjectId::test(1), DeltaId::test(1), 500);
        assert_eq!(cache.height(a, || 20), 20);
        assert_eq!(cache.height(a, || 30), 20);
        assert_eq!(cache.height(b, || 30), 30);
        assert_eq!(cache.len(), 2);
        cache.invalidate_object(ObjectId::test(1));
        assert_eq!(cache.len(), 0);
    }

    #[test]
    fn selector_cache_has_an_explicit_entry_bound() {
        let mut cache = KeyedSelectorCache::with_capacity(2);
        let first = SelectorKey::new(ObjectId::test(1), DeltaId::test(1));
        let second = SelectorKey::new(ObjectId::test(2), DeltaId::test(1));
        let third = SelectorKey::new(ObjectId::test(3), DeltaId::test(1));
        cache.get_or_insert_with(first, || 1);
        cache.get_or_insert_with(second, || 2);
        cache.get_or_insert_with(third, || 3);
        assert_eq!(cache.len(), 2);
        assert!(cache.get(&first).is_none());
        assert_eq!(cache.get(&third), Some(&3));
    }
}
