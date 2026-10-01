//! State saved row by row. The office's state can be large (every task and open ask it knows), so
//! it is not rewritten after each event: each map remembers which keys changed, and only those
//! rows are saved.

use pitcrew_protocol::ids::{AskId, DispatchId, MemberId, SessionId, TaskId, WorkstreamId};
use serde::Serialize;
use std::borrow::Borrow;
use std::collections::{BTreeMap, BTreeSet};

/// One row of saved state: a key and its JSON value, or no value when the key was removed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StateRow {
    /// The key, e.g. `task/01JB…` or `memo/remind_stale_asks/cursor`.
    pub key: String,
    /// The value as JSON; `None` to delete the row.
    pub value: Option<String>,
}

/// Most entries per map. Ids come from untrusted events; past this, new ones are ignored.
pub(crate) const MAX_ENTRIES: usize = 100_000;

/// A key that can be written into a row key and read back.
pub(crate) trait RowKey: Ord + Clone + Sized {
    fn to_row(&self) -> String;
    fn from_row(s: &str) -> Option<Self>;
}

impl RowKey for String {
    fn to_row(&self) -> String {
        self.clone()
    }

    fn from_row(s: &str) -> Option<Self> {
        Some(s.to_owned())
    }
}

macro_rules! id_row_key {
    ($($t:ty),*) => {$(
        impl RowKey for $t {
            fn to_row(&self) -> String {
                self.0.to_string()
            }

            fn from_row(s: &str) -> Option<Self> {
                s.parse().ok()
            }
        }
    )*};
}

id_row_key!(MemberId, TaskId, DispatchId, SessionId, AskId, WorkstreamId);

/// A map that remembers which keys changed since it was last saved. Iteration is in key order.
#[derive(Clone, Debug)]
pub(crate) struct Tracked<K, V> {
    map: BTreeMap<K, V>,
    dirty: BTreeSet<K>,
}

impl<K: Ord, V> Default for Tracked<K, V> {
    fn default() -> Self {
        Self {
            map: BTreeMap::new(),
            dirty: BTreeSet::new(),
        }
    }
}

impl<K: RowKey, V> Tracked<K, V> {
    pub(crate) fn get<Q>(&self, key: &Q) -> Option<&V>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.map.get(key)
    }

    pub(crate) fn len(&self) -> usize {
        self.map.len()
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = (&K, &V)> {
        self.map.iter()
    }

    /// Inserts or replaces. A new key past [`MAX_ENTRIES`] is ignored; returns whether it was
    /// stored.
    pub(crate) fn insert(&mut self, key: K, value: V) -> bool {
        if self.map.len() >= MAX_ENTRIES && !self.map.contains_key(&key) {
            return false;
        }
        self.dirty.insert(key.clone());
        self.map.insert(key, value);
        true
    }

    /// Changes an entry in place, if it exists.
    pub(crate) fn update<Q>(&mut self, key: &Q, f: impl FnOnce(&mut V)) -> bool
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        match self.map.get_key_value(key) {
            Some((k, _)) => {
                let k = k.clone();
                if let Some(v) = self.map.get_mut(key) {
                    f(v);
                }
                self.dirty.insert(k);
                true
            }
            None => false,
        }
    }

    pub(crate) fn remove<Q>(&mut self, key: &Q) -> Option<V>
    where
        K: Borrow<Q>,
        Q: Ord + ?Sized,
    {
        let (k, v) = self.map.remove_entry(key)?;
        self.dirty.insert(k);
        Some(v)
    }

    /// Puts back a saved entry, without marking it changed.
    pub(crate) fn load(&mut self, key: K, value: V) {
        self.map.insert(key, value);
    }

    /// Every key changed since the last call, with its value now (`None` if removed).
    pub(crate) fn changes(&mut self) -> impl Iterator<Item = (K, Option<&V>)> {
        let dirty = std::mem::take(&mut self.dirty);
        let map = &self.map;
        dirty.into_iter().map(move |k| {
            let v = map.get(&k);
            (k, v)
        })
    }

    /// The rows for every key changed since the last save, as `<kind>/<key>`.
    pub(crate) fn save(&mut self, kind: &str, out: &mut Vec<StateRow>) -> serde_json::Result<()>
    where
        V: Serialize,
    {
        for (key, value) in self.changes() {
            out.push(StateRow {
                key: format!("{kind}/{}", key.to_row()),
                value: value.map(serde_json::to_string).transpose()?,
            });
        }
        Ok(())
    }
}
