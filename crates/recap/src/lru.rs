//! A map with a most number of entries, for the directory. Past it, the entry used longest ago
//! goes. Only writes and [`Lru::touch`] count as use, never reads, so what a map holds depends on
//! the order of those calls alone: not on map order, and not on who read what.

use crate::hash::IdMap;
use std::collections::BTreeMap;
use std::hash::Hash;

#[derive(Clone, Debug, PartialEq, Eq)]
struct Slot<V> {
    used: u64,
    value: V,
}

/// At most `cap` entries; the least recently used goes first.
#[derive(Clone, Debug)]
pub(crate) struct Lru<K, V> {
    map: IdMap<K, Slot<V>>,
    /// Each key by its last use, oldest first.
    order: BTreeMap<u64, K>,
    clock: u64,
    cap: usize,
}

/// What a write put out: the value it replaced, and the entry it evicted to make room.
pub(crate) struct Put<K, V> {
    pub(crate) old: Option<V>,
    pub(crate) evicted: Option<(K, V)>,
}

impl<K: Copy + Hash + Eq, V> Lru<K, V> {
    /// An empty map that keeps at most `cap` entries (at least one).
    pub(crate) fn new(cap: usize) -> Self {
        Self {
            map: IdMap::default(),
            order: BTreeMap::new(),
            clock: 0,
            cap: cap.max(1),
        }
    }

    pub(crate) fn get(&self, key: &K) -> Option<&V> {
        self.map.get(key).map(|s| &s.value)
    }

    /// The entry for `key`, to change in place. This is not a use: call [`Lru::touch`] too.
    pub(crate) fn get_mut(&mut self, key: &K) -> Option<&mut V> {
        self.map.get_mut(key).map(|s| &mut s.value)
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.map.len()
    }

    /// Marks `key` as used now. Nothing happens for a key the map does not hold.
    pub(crate) fn touch(&mut self, key: K) {
        let stamp = self.clock.wrapping_add(1);
        if let Some(slot) = self.map.get_mut(&key) {
            self.clock = stamp;
            self.order.remove(&slot.used);
            slot.used = stamp;
            self.order.insert(stamp, key);
        }
    }

    /// Sets `key` to `value`, as used now.
    pub(crate) fn insert(&mut self, key: K, value: V) -> Put<K, V> {
        let evicted = self.make_room(&key);
        let stamp = self.next();
        let old = self.map.insert(key, Slot { used: stamp, value });
        if let Some(old) = &old {
            self.order.remove(&old.used);
        }
        self.order.insert(stamp, key);
        Put {
            old: old.map(|s| s.value),
            evicted,
        }
    }

    /// The entry for `key`, as used now; a new one from `make` if there is none. Also returns the
    /// entry evicted to make room for it.
    pub(crate) fn entry(&mut self, key: K, make: impl FnOnce() -> V) -> (&mut V, Option<(K, V)>) {
        let evicted = self.make_room(&key);
        let stamp = self.next();
        let slot = self.map.entry(key).or_insert_with(|| Slot {
            used: stamp,
            value: make(),
        });
        if slot.used != stamp {
            self.order.remove(&slot.used);
            slot.used = stamp;
        }
        self.order.insert(stamp, key);
        (&mut slot.value, evicted)
    }

    fn next(&mut self) -> u64 {
        self.clock = self.clock.wrapping_add(1);
        self.clock
    }

    /// Evicts the least recently used entry when `key` is new and the map is full.
    fn make_room(&mut self, key: &K) -> Option<(K, V)> {
        if self.map.len() < self.cap || self.map.contains_key(key) {
            return None;
        }
        let (_, oldest) = self.order.pop_first()?;
        self.map.remove(&oldest).map(|s| (oldest, s.value))
    }
}

impl<K: Hash + Eq, V: PartialEq> PartialEq for Lru<K, V> {
    fn eq(&self, other: &Self) -> bool {
        self.cap == other.cap
            && self.clock == other.clock
            && self.order == other.order
            && self.map == other.map
    }
}

impl<K: Hash + Eq, V: Eq> Eq for Lru<K, V> {}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(lru: &Lru<u32, &str>) -> Vec<u32> {
        lru.order.values().copied().collect()
    }

    #[test]
    fn the_least_recently_used_goes_first() {
        let mut lru = Lru::new(3);
        for (k, v) in [(1, "a"), (2, "b"), (3, "c")] {
            assert!(lru.insert(k, v).evicted.is_none());
        }
        // Reads are not uses; touches and writes are.
        assert_eq!(lru.get(&1), Some(&"a"));
        lru.touch(1);
        lru.touch(9);
        let put = lru.insert(2, "B");
        assert_eq!(put.old, Some("b"));
        assert!(put.evicted.is_none());
        assert_eq!(keys(&lru), [3, 1, 2]);
        let put = lru.insert(4, "d");
        assert_eq!(put.evicted, Some((3, "c")));
        let (value, evicted) = lru.entry(5, || "e");
        assert_eq!(*value, "e");
        assert_eq!(evicted, Some((1, "a")));
        let (value, evicted) = lru.entry(2, || "never");
        assert_eq!(*value, "B");
        assert!(evicted.is_none());
        assert_eq!(keys(&lru), [4, 5, 2]);
        assert_eq!(lru.len(), 3);
        assert_eq!(lru.map.len(), lru.order.len());
    }

    #[test]
    fn equal_calls_give_equal_maps() {
        let run = || {
            let mut lru = Lru::new(2);
            for k in [1u32, 2, 1, 3, 2, 4] {
                lru.insert(k, "x");
                lru.touch(1);
            }
            lru
        };
        assert_eq!(run(), run());
        let mut one = Lru::new(0);
        one.insert(1u32, "a");
        assert_eq!(one.insert(2, "b").evicted, Some((1, "a")));
    }
}
