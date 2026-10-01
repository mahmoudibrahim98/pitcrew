//! Caps: at most so many emitted actions per rule in any hour, and so many over all rules. Time is
//! the office's clock (the latest event time), never the wall clock.

use crate::action::CapScope;
use crate::state::{StateRow, Tracked};
use pitcrew_protocol::model::TimestampMs;
use std::collections::VecDeque;

const HOUR_MS: i64 = 3_600_000;
/// The key of the window over all rules. Rule names never contain `*`.
const GLOBAL: &str = "*";

/// The times of the actions emitted in the last hour, per rule and over all.
#[derive(Clone, Debug, Default)]
pub(crate) struct Caps {
    windows: Tracked<String, VecDeque<TimestampMs>>,
}

impl Caps {
    /// Counts an action of `rule` at `now` if both caps allow it.
    pub(crate) fn take(
        &mut self,
        rule: &str,
        now: TimestampMs,
        per_rule: usize,
        global: usize,
    ) -> Result<(), CapScope> {
        let since = now.saturating_sub(HOUR_MS);
        if self.count(rule, since) >= per_rule {
            return Err(CapScope::Rule);
        }
        if self.count(GLOBAL, since) >= global {
            return Err(CapScope::Global);
        }
        for key in [rule, GLOBAL] {
            if !self.windows.update(key, |w| w.push_back(now)) {
                self.windows.insert(key.to_owned(), VecDeque::from([now]));
            }
        }
        Ok(())
    }

    /// Drops times at or before `since` from a window, and counts the rest.
    fn count(&mut self, key: &str, since: TimestampMs) -> usize {
        let stale = self
            .windows
            .get(key)
            .is_some_and(|w| w.front().is_some_and(|t| *t <= since));
        if stale {
            self.windows.update(key, |w| {
                while w.front().is_some_and(|t| *t <= since) {
                    w.pop_front();
                }
            });
        }
        self.windows.get(key).map_or(0, VecDeque::len)
    }

    pub(crate) fn save(&mut self, out: &mut Vec<StateRow>) -> serde_json::Result<()> {
        self.windows.save("cap", out)
    }

    pub(crate) fn load(&mut self, key: &str, value: &str) -> serde_json::Result<()> {
        self.windows
            .load(key.to_owned(), serde_json::from_str(value)?);
        Ok(())
    }
}
