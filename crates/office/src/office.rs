//! The office: the rules, what they know, their caps, and the run log they produce.

use crate::action::{Entry, Outcome};
use crate::caps::Caps;
use crate::guard;
use crate::rule::{Context, Memo, Rule};
use crate::rules::default_rules;
use crate::state::StateRow;
use crate::world::{LoadError, World};
use pitcrew_protocol::events::Event;
use pitcrew_protocol::ids::MemberId;
use pitcrew_protocol::model::TimestampMs;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

const HOUR_MS: i64 = 3_600_000;
/// Most actions taken from one rule for one event. A rule that returns more is faulty; the rest
/// are dropped.
pub const MAX_ACTIONS_PER_EVENT: usize = 64;

/// The office's settings. The live office and its run log must use the same settings: the run
/// log replays the rules with them, so changing them changes what a rebuild says it did.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    /// The back office's own member. Rules ignore its events, and they are not activity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub office: Option<MemberId>,
    /// Remind about an ask open this long, in milliseconds. Default: 24 hours.
    pub remind_after_ms: i64,
    /// Ask "paused?" about an active workstream with no activity this long. Default: 3 days.
    pub quiet_after_ms: i64,
    /// Failed test runs in a row in one session before asking its owner. Default: 3.
    pub failing_runs: u32,
    /// Ask again about divergence in the same session only after this long. Default: 12 hours.
    pub diverged_repeat_ms: i64,
    /// Most emitted actions per rule in any hour. Default: 20.
    pub per_rule_per_hour: usize,
    /// Most emitted actions over all rules in any hour. Default: 60.
    pub global_per_hour: usize,
    /// UTC offset for dates in text, in minutes. Default: 0.
    pub utc_offset_minutes: i32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            office: None,
            remind_after_ms: 24 * HOUR_MS,
            quiet_after_ms: 72 * HOUR_MS,
            failing_runs: 3,
            diverged_repeat_ms: 12 * HOUR_MS,
            per_rule_per_hour: 20,
            global_per_hour: 60,
            utc_offset_minutes: 0,
        }
    }
}

/// A saved state that could not be read back.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RestoreError {
    /// A row's key is not one the office writes.
    #[error("unknown office state key {0:?}")]
    Key(String),
    /// A row's value does not read.
    #[error("office state {key:?} does not read: {source}")]
    Value {
        /// The row's key.
        key: String,
        /// Why.
        #[source]
        source: serde_json::Error,
    },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Meta {
    rev: u64,
    now: TimestampMs,
}

/// The back office: rules over the events, with caps and a run log.
///
/// Feed it events in log order with [`Office::on_event`]; it returns the run log's entries for
/// each, and the caller applies the emitted ones with [`apply`](crate::apply). It is
/// deterministic: the same events give the same entries, and an event at or before the last
/// revision seen is ignored, so replaying events never acts twice. Its state can be saved row by
/// row ([`Office::take_changes`]) and restored ([`Office::restore`]).
pub struct Office {
    config: Config,
    rules: Vec<Box<dyn Rule>>,
    world: World,
    memos: BTreeMap<String, Memo>,
    caps: Caps,
    meta: Meta,
    meta_changed: bool,
}

impl std::fmt::Debug for Office {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let rules: Vec<&str> = self.rules.iter().map(|r| r.name()).collect();
        f.debug_struct("Office")
            .field("config", &self.config)
            .field("rules", &rules)
            .field("rev", &self.meta.rev)
            .field("now", &self.meta.now)
            .finish_non_exhaustive()
    }
}

impl Office {
    /// An office with the default rules.
    #[must_use]
    pub fn new(config: Config) -> Self {
        Self::with_rules(config, default_rules())
    }

    /// An office with the given rules, in the order they run.
    #[must_use]
    pub fn with_rules(config: Config, rules: Vec<Box<dyn Rule>>) -> Self {
        Self {
            config,
            rules,
            world: World::default(),
            memos: BTreeMap::new(),
            caps: Caps::default(),
            meta: Meta::default(),
            meta_changed: false,
        }
    }

    /// The settings.
    #[must_use]
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// What the office knows.
    #[must_use]
    pub fn world(&self) -> &World {
        &self.world
    }

    /// The last revision seen.
    #[must_use]
    pub fn rev(&self) -> u64 {
        self.meta.rev
    }

    /// The office's clock: the latest event time seen.
    #[must_use]
    pub fn now(&self) -> TimestampMs {
        self.meta.now
    }

    /// Runs the rules over one event at revision `rev` and returns what each did. Events at or
    /// before the last revision seen are ignored.
    pub fn on_event(&mut self, rev: u64, event: &Event) -> Vec<Entry> {
        if rev <= self.meta.rev {
            return Vec::new();
        }
        self.meta.rev = rev;
        self.meta.now = self.meta.now.max(event.at);
        self.meta_changed = true;
        let now = self.meta.now;
        self.world.observe(rev, now, event, self.config.office);

        let mut entries = Vec::new();
        for rule in &mut self.rules {
            let name = rule.name();
            let memo = self.memos.entry(name.to_owned()).or_default();
            let mut ctx = Context {
                world: &self.world,
                config: &self.config,
                now,
                rev,
                memo,
            };
            let actions = rule.on_event(&mut ctx, event);
            for action in actions.into_iter().take(MAX_ACTIONS_PER_EVENT) {
                let outcome = match guard::check(&action, &self.world) {
                    Err(refusal) => Outcome::Refused { refusal },
                    Ok(()) => match self.caps.take(
                        name,
                        now,
                        self.config.per_rule_per_hour,
                        self.config.global_per_hour,
                    ) {
                        Ok(()) => Outcome::Emitted,
                        Err(scope) => Outcome::Capped { scope },
                    },
                };
                entries.push(Entry {
                    rev,
                    seq: u32::try_from(entries.len()).unwrap_or(u32::MAX),
                    event: event.id,
                    at: event.at,
                    rule: name.to_owned(),
                    action,
                    outcome,
                });
            }
        }
        entries
    }

    /// The rows of state changed since the last call, to save. Saving every row ever returned,
    /// in order, and restoring from them gives an office that behaves exactly like this one.
    ///
    /// # Errors
    ///
    /// A value that does not serialize, which the office's own types never cause.
    pub fn take_changes(&mut self) -> serde_json::Result<Vec<StateRow>> {
        let mut out = Vec::new();
        if std::mem::take(&mut self.meta_changed) {
            out.push(StateRow {
                key: "meta".to_owned(),
                value: Some(serde_json::to_string(&self.meta)?),
            });
        }
        self.world.save(&mut out)?;
        for (rule, memo) in &mut self.memos {
            memo.save(rule, &mut out);
        }
        self.caps.save(&mut out)?;
        Ok(out)
    }

    /// An office restored from saved rows (`key`, `value`), with the given settings and rules.
    ///
    /// # Errors
    ///
    /// [`RestoreError`] for a row the office did not write.
    pub fn restore<'a>(
        config: Config,
        rules: Vec<Box<dyn Rule>>,
        rows: impl IntoIterator<Item = (&'a str, &'a str)>,
    ) -> Result<Self, RestoreError> {
        let mut office = Self::with_rules(config, rules);
        for (key, value) in rows {
            let bad_value = |source| RestoreError::Value {
                key: key.to_owned(),
                source,
            };
            if key == "meta" {
                office.meta = serde_json::from_str(value).map_err(bad_value)?;
                continue;
            }
            let Some((kind, rest)) = key.split_once('/') else {
                return Err(RestoreError::Key(key.to_owned()));
            };
            match kind {
                "memo" => {
                    let Some((rule, memo_key)) = rest.split_once('/') else {
                        return Err(RestoreError::Key(key.to_owned()));
                    };
                    office
                        .memos
                        .entry(rule.to_owned())
                        .or_default()
                        .load(memo_key, value);
                }
                "cap" => office.caps.load(rest, value).map_err(bad_value)?,
                _ => match office.world.load(kind, rest, value) {
                    Ok(true) => {}
                    Ok(false) | Err(LoadError::Key) => {
                        return Err(RestoreError::Key(key.to_owned()));
                    }
                    Err(LoadError::Value(source)) => return Err(bad_value(source)),
                },
            }
        }
        Ok(office)
    }
}
