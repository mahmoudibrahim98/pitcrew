//! The [`Rule`] trait, what a rule sees ([`Context`]) and what it may remember ([`Memo`]).

use crate::Action;
use crate::office::Config;
use crate::state::{StateRow, Tracked};
use crate::world::World;
use pitcrew_protocol::events::Event;
use pitcrew_protocol::model::TimestampMs;
use serde::Serialize;
use serde::de::DeserializeOwned;

/// A small, deterministic rule that acts on evidence.
///
/// `on_event` is called once for every event, in log order, after the office's [`World`] has
/// learnt from it. It returns the actions it wants; the office checks each against the "never"
/// list and the caps, logs it, and emits only what passes. A rule must be deterministic given the
/// events: no clock (use [`Context::now`]), no I/O, no randomness.
///
/// A rule's fields are its settings. Anything it must remember between events goes in
/// [`Context::memo`], which the office saves with its state, so a rule restored from a saved
/// state behaves exactly as one that saw every event.
pub trait Rule: Send {
    /// A stable name, e.g. `remind_stale_asks`. It keys the rule's cap, memo and run-log rows.
    /// Use lowercase letters, digits and `_`.
    fn name(&self) -> &'static str;

    /// Reacts to one event.
    fn on_event(&mut self, ctx: &mut Context<'_>, event: &Event) -> Vec<Action>;
}

/// What a rule sees when an event arrives.
#[derive(Debug)]
pub struct Context<'a> {
    /// What the office knows, including this event.
    pub world: &'a World,
    /// The office's settings.
    pub config: &'a Config,
    /// The office's clock: the latest event time so far. It never goes back.
    pub now: TimestampMs,
    /// The event's revision.
    pub rev: u64,
    /// This rule's memory.
    pub memo: &'a mut Memo,
}

impl Context<'_> {
    /// Whether the back office itself wrote the event. Rules do not react to their own office's
    /// events, so the office never feeds on itself.
    #[must_use]
    pub fn from_office(&self, event: &Event) -> bool {
        self.config.office == Some(event.author)
    }
}

/// A rule's memory: small JSON values by key, saved with the office's state. New keys past
/// 100,000 entries are ignored, so a rule should remove what it no longer needs.
#[derive(Clone, Debug, Default)]
pub struct Memo {
    entries: Tracked<String, String>,
}

impl Memo {
    /// The value under `key`, if any and if it reads as `T`.
    #[must_use]
    pub fn get<T: DeserializeOwned>(&self, key: &str) -> Option<T> {
        self.entries
            .get(key)
            .and_then(|v| serde_json::from_str(v).ok())
    }

    /// Stores `value` under `key`. Returns whether it was stored.
    pub fn put<T: Serialize>(&mut self, key: &str, value: &T) -> bool {
        match serde_json::to_string(value) {
            Ok(v) => self.entries.insert(key.to_owned(), v),
            Err(_) => false,
        }
    }

    /// Forgets `key`.
    pub fn remove(&mut self, key: &str) {
        self.entries.remove(key);
    }

    /// How many keys it holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether it holds nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.len() == 0
    }

    /// Values are JSON already, so they are saved as they are.
    pub(crate) fn save(&mut self, rule: &str, out: &mut Vec<StateRow>) {
        for (key, value) in self.entries.changes() {
            out.push(StateRow {
                key: format!("memo/{rule}/{key}"),
                value: value.cloned(),
            });
        }
    }

    pub(crate) fn load(&mut self, key: &str, value: &str) {
        self.entries.load(key.to_owned(), value.to_owned());
    }
}
