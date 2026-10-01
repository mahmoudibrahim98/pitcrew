//! Hooks for sessions not indexed yet, held until their transcript is discovered.
//!
//! Each entry keeps who sent it, so it is checked like any hook when its session is discovered.
//! Senders are authenticated members, and none can crowd out another:
//! - each sender holds at most [`Limits::per_sender`] entries; at its quota it loses its own
//!   oldest;
//! - past [`Limits::total`] entries, the sender holding the most loses its oldest;
//! - entries expire after [`Limits::keep_for`];
//! - a new entry may trigger a rediscovery (its transcript may have just appeared), at most once
//!   per [`Limits::rediscover_gap`] per sender.

use crate::derive::Reported;
use crate::hooks::Sender;
use pitcrew_protocol::ids::MemberId;
use pitcrew_protocol::model::Engine;
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// How much is held, and for how long.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Limits {
    pub per_sender: usize,
    pub total: usize,
    pub keep_for: Duration,
    pub rediscover_gap: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            per_sender: 32,
            total: 1024,
            keep_for: Duration::from_secs(10 * 60),
            rediscover_gap: Duration::from_secs(5),
        }
    }
}

/// A session, by the CLI's own id.
type Native = (Engine, String);

#[derive(Debug)]
struct Entry {
    /// When it was held (or last replaced).
    since: Instant,
    report: Reported,
}

/// Held hooks: per session, the newest report of each sender.
#[derive(Debug)]
pub(crate) struct Held {
    limits: Limits,
    sessions: HashMap<Native, HashMap<Sender, Entry>>,
    /// Entries per member.
    counts: HashMap<MemberId, usize>,
    /// When each member last triggered a rediscovery.
    rediscovered: HashMap<MemberId, Instant>,
}

impl Default for Held {
    fn default() -> Self {
        Self::new(Limits::default())
    }
}

impl Held {
    pub fn new(limits: Limits) -> Self {
        Self {
            limits: Limits {
                per_sender: limits.per_sender.max(1),
                total: limits.total.max(1),
                ..limits
            },
            sessions: HashMap::new(),
            counts: HashMap::new(),
            rediscovered: HashMap::new(),
        }
    }

    /// Holds `sender`'s report for a session, unless it already holds a newer one. Returns
    /// whether to look for the session's transcript now.
    pub fn hold(
        &mut self,
        engine: Engine,
        native_id: String,
        sender: Sender,
        report: Reported,
        now: Instant,
    ) -> bool {
        self.expire(now);
        let key = (engine, native_id);
        if let Some(e) = self.sessions.get_mut(&key).and_then(|s| s.get_mut(&sender)) {
            if e.report.at <= report.at {
                *e = Entry { since: now, report };
            }
            return false;
        }
        let member = sender.member();
        if self.count(member) >= self.limits.per_sender {
            self.evict_oldest_of(member);
        } else if self.len() >= self.limits.total
            && let Some(most) = self.most_holding()
        {
            self.evict_oldest_of(most);
        }
        self.sessions
            .entry(key)
            .or_default()
            .insert(sender, Entry { since: now, report });
        *self.counts.entry(member).or_default() += 1;
        let due = self
            .rediscovered
            .get(&member)
            .is_none_or(|t| now.duration_since(*t) >= self.limits.rediscover_gap);
        if due {
            self.rediscovered.insert(member, now);
        }
        due
    }

    /// Takes every report held for a session, oldest first, with who sent each.
    pub fn take(
        &mut self,
        engine: Engine,
        native_id: &str,
        now: Instant,
    ) -> Vec<(Sender, Reported)> {
        self.expire(now);
        let Some(entries) = self.sessions.remove(&(engine, native_id.to_owned())) else {
            return Vec::new();
        };
        let mut out: Vec<(Sender, Reported)> = entries
            .into_iter()
            .map(|(sender, e)| {
                self.uncount(sender.member());
                (sender, e.report)
            })
            .collect();
        out.sort_by_key(|(_, r)| r.at);
        out
    }

    /// Entries held.
    pub fn len(&self) -> usize {
        self.counts.values().sum()
    }

    fn count(&self, member: MemberId) -> usize {
        self.counts.get(&member).copied().unwrap_or(0)
    }

    /// The member holding the most entries.
    fn most_holding(&self) -> Option<MemberId> {
        self.counts
            .iter()
            .max_by_key(|(_, n)| **n)
            .map(|(member, _)| *member)
    }

    fn evict_oldest_of(&mut self, member: MemberId) {
        let oldest = self
            .sessions
            .iter()
            .flat_map(|(key, s)| s.iter().map(move |(sender, e)| (key, sender, e.since)))
            .filter(|(_, sender, _)| sender.member() == member)
            .min_by_key(|(_, _, since)| *since)
            .map(|(key, sender, _)| (key.clone(), *sender));
        if let Some((key, sender)) = oldest {
            tracing::debug!(%member, engine = ?key.0, session = %key.1, "too many held hooks; dropped this sender's oldest");
            self.remove(&key, &sender);
        }
    }

    fn remove(&mut self, key: &Native, sender: &Sender) {
        let Some(s) = self.sessions.get_mut(key) else {
            return;
        };
        let removed = s.remove(sender).is_some();
        if s.is_empty() {
            self.sessions.remove(key);
        }
        if removed {
            self.uncount(sender.member());
        }
    }

    fn uncount(&mut self, member: MemberId) {
        if let Some(n) = self.counts.get_mut(&member) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                self.counts.remove(&member);
            }
        }
    }

    fn expire(&mut self, now: Instant) {
        let keep_for = self.limits.keep_for;
        let mut gone: Vec<MemberId> = Vec::new();
        self.sessions.retain(|_, s| {
            s.retain(|sender, e| {
                let keep = now.duration_since(e.since) < keep_for;
                if !keep {
                    gone.push(sender.member());
                }
                keep
            });
            !s.is_empty()
        });
        for member in gone {
            self.uncount(member);
        }
        let gap = self.limits.rediscover_gap;
        self.rediscovered
            .retain(|_, t| now.duration_since(*t) < gap);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pitcrew_protocol::api::{Caller, TokenScope};
    use pitcrew_protocol::model::SessionState;

    fn sender() -> Sender {
        Sender::new(Caller {
            member: MemberId::new(),
            scope: TokenScope::Device,
            on_behalf_of: None,
        })
    }

    fn report(at: i64, to: SessionState) -> Reported {
        Reported {
            at,
            to,
            status_line: None,
        }
    }

    fn limits(per_sender: usize, total: usize) -> Limits {
        Limits {
            per_sender,
            total,
            keep_for: Duration::from_secs(60),
            rediscover_gap: Duration::from_secs(5),
        }
    }

    fn ms(t0: Instant, n: u64) -> Instant {
        t0 + Duration::from_millis(n)
    }

    fn states(got: &[(Sender, Reported)]) -> Vec<SessionState> {
        got.iter().map(|(_, r)| r.to).collect()
    }

    #[test]
    fn each_sender_keeps_its_newest_report_per_session() {
        use SessionState::{Ended, Idle, Waiting};
        let t0 = Instant::now();
        let mut held = Held::new(limits(4, 16));
        let (a, b) = (sender(), sender());
        held.hold(Engine::Claude, "s".into(), a, report(10, Idle), t0);
        // Older than what `a` holds: ignored. Newer: replaces it.
        held.hold(Engine::Claude, "s".into(), a, report(5, Waiting), ms(t0, 1));
        held.hold(Engine::Claude, "s".into(), b, report(7, Waiting), ms(t0, 2));
        held.hold(Engine::Claude, "s".into(), a, report(20, Ended), ms(t0, 3));
        held.hold(Engine::Codex, "s".into(), a, report(1, Idle), ms(t0, 4));
        assert_eq!(held.len(), 3);
        // Every sender's, oldest first; then nothing is left for the session.
        let got = held.take(Engine::Claude, "s", ms(t0, 5));
        assert_eq!(states(&got), [Waiting, Ended]);
        assert_eq!(got[0].0, b);
        assert_eq!(got[1].0, a);
        assert!(held.take(Engine::Claude, "s", ms(t0, 6)).is_empty());
        assert_eq!(held.len(), 1);
        assert_eq!(states(&held.take(Engine::Codex, "s", ms(t0, 7))), [Idle]);
        assert_eq!(held.len(), 0);
    }

    #[test]
    fn a_sender_at_its_quota_loses_its_own_oldest() {
        let t0 = Instant::now();
        let mut held = Held::new(limits(3, 100));
        let (flood, other) = (sender(), sender());
        held.hold(
            Engine::Claude,
            "mine".into(),
            other,
            report(1, SessionState::Ended),
            t0,
        );
        for i in 0..50u64 {
            held.hold(
                Engine::Claude,
                format!("x{i}"),
                flood,
                report(2, SessionState::Idle),
                ms(t0, i + 1),
            );
        }
        assert_eq!(held.len(), 4);
        // The flood kept only its newest three.
        for i in 0..47u64 {
            assert!(
                held.take(Engine::Claude, &format!("x{i}"), ms(t0, 60))
                    .is_empty()
            );
        }
        for i in 47..50u64 {
            assert_eq!(
                held.take(Engine::Claude, &format!("x{i}"), ms(t0, 60))
                    .len(),
                1
            );
        }
        let mine = held.take(Engine::Claude, "mine", ms(t0, 60));
        assert_eq!(states(&mine), [SessionState::Ended]);
        assert_eq!(mine[0].0, other);
    }

    #[test]
    fn at_the_global_cap_the_sender_holding_the_most_loses_its_oldest() {
        let t0 = Instant::now();
        let mut held = Held::new(limits(4, 6));
        let (big, small, newcomer) = (sender(), sender(), sender());
        for i in 0..4u64 {
            held.hold(
                Engine::Claude,
                format!("b{i}"),
                big,
                report(1, SessionState::Idle),
                ms(t0, i),
            );
        }
        for i in 0..2u64 {
            held.hold(
                Engine::Claude,
                format!("s{i}"),
                small,
                report(1, SessionState::Idle),
                ms(t0, 10 + i),
            );
        }
        assert_eq!(held.len(), 6);
        // Full: the newcomer's entry costs `big` its oldest, not `small` (whose entries are newer
        // than `b0` but fewer) nor anyone's newest.
        held.hold(
            Engine::Claude,
            "n".into(),
            newcomer,
            report(1, SessionState::Ended),
            ms(t0, 20),
        );
        assert_eq!(held.len(), 6);
        assert!(held.take(Engine::Claude, "b0", ms(t0, 30)).is_empty());
        for kept in ["b1", "b2", "b3", "s0", "s1", "n"] {
            assert_eq!(
                held.take(Engine::Claude, kept, ms(t0, 30)).len(),
                1,
                "{kept}"
            );
        }
    }

    #[test]
    fn held_reports_expire() {
        let t0 = Instant::now();
        let mut held = Held::new(limits(4, 16));
        held.hold(
            Engine::Claude,
            "old".into(),
            sender(),
            report(1, SessionState::Idle),
            t0,
        );
        let later = t0 + Duration::from_secs(30);
        held.hold(
            Engine::Claude,
            "new".into(),
            sender(),
            report(1, SessionState::Idle),
            later,
        );
        let past = t0 + Duration::from_secs(61);
        assert!(held.take(Engine::Claude, "old", past).is_empty());
        assert_eq!(held.len(), 1);
        assert_eq!(held.take(Engine::Claude, "new", past).len(), 1);
    }

    #[test]
    fn rediscovery_is_rate_limited_per_sender() {
        let t0 = Instant::now();
        let mut held = Held::new(limits(64, 1024));
        let (flood, other) = (sender(), sender());
        let mut hold = |s: Sender, id: &str, at: Instant| {
            held.hold(
                Engine::Claude,
                id.into(),
                s,
                report(1, SessionState::Idle),
                at,
            )
        };
        assert!(hold(flood, "a", t0));
        // New sessions from the same sender within the gap trigger nothing more...
        let triggered = (0..40u64)
            .filter(|i| hold(flood, &format!("f{i}"), ms(t0, 10 + i)))
            .count();
        assert_eq!(triggered, 0);
        // ...another sender still triggers its own...
        assert!(hold(other, "b", ms(t0, 100)));
        // ...a repeat for a held session never does...
        assert!(!hold(flood, "a", t0 + Duration::from_secs(6)));
        // ...and after the gap the sender may trigger again.
        assert!(hold(flood, "c", t0 + Duration::from_secs(6)));
    }
}
