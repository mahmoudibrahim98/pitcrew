//! One workspace's open asks addressed to the person: what its Inbox would list
//! (`GET /v1/asks?to=<me>&state=open`), kept current from the stream's `ask_raised` and
//! `ask_answered`. Pure: no I/O, so it is tested on its own.
//!
//! **Bounded.** At most `max_open` asks are kept; past that the count reads "`max_open`+" until
//! the next snapshot, and the watcher is asked for one. At most `max_members` names are kept.

use pitcrew_protocol::ids::{AskId, MemberId, TaskId};
use pitcrew_protocol::model::{Ask, AskKind, AskState, Member};
use serde::Deserialize;
use std::collections::{BTreeMap, HashMap};

/// How many asks need the person.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Count {
    /// Open asks known.
    pub open: usize,
    /// There are more than `open`: the bound was reached.
    pub more: bool,
}

impl Count {
    /// Adds two counts.
    #[must_use]
    pub fn plus(self, other: Self) -> Self {
        Self {
            open: self.open + other.open,
            more: self.more || other.more,
        }
    }
}

/// A new ask for the person, as a notification needs it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewAsk {
    /// Its id.
    pub id: AskId,
    /// Question, decision, review, approval or mention.
    pub kind: AskKind,
    /// Who asks.
    pub asker: MemberId,
    /// Their name, when known.
    pub from: Option<String>,
    /// Its one-line title.
    pub title: String,
    /// Its context.
    pub body: String,
    /// The task it is about.
    pub task: Option<TaskId>,
}

#[derive(Clone, Debug)]
struct Open {
    kind: AskKind,
    from: MemberId,
}

/// What applying one event did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Applied {
    /// The count changed.
    pub changed: bool,
    /// A new ask for the person.
    pub new: Option<NewAsk>,
    /// The count can no longer be trusted: fetch a snapshot.
    pub refetch: bool,
}

/// The tracker.
#[derive(Debug)]
pub struct Tracker {
    me: Option<MemberId>,
    open: BTreeMap<AskId, Open>,
    over: bool,
    names: HashMap<MemberId, String>,
    max_open: usize,
    max_members: usize,
}

#[derive(Deserialize)]
struct Raised {
    ask: Ask,
}

#[derive(Deserialize)]
struct Answered {
    ask: AskId,
}

#[derive(Deserialize)]
struct MemberAdded {
    member: Member,
}

impl Tracker {
    /// An empty tracker that keeps at most `max_open` asks and `max_members` names.
    #[must_use]
    pub fn new(max_open: usize, max_members: usize) -> Self {
        Self {
            me: None,
            open: BTreeMap::new(),
            over: false,
            names: HashMap::new(),
            max_open,
            max_members,
        }
    }

    /// Who the person is, once known.
    #[must_use]
    pub fn me(&self) -> Option<MemberId> {
        self.me
    }

    /// The count.
    #[must_use]
    pub fn count(&self) -> Count {
        Count {
            open: self.open.len(),
            more: self.over,
        }
    }

    /// Starts afresh from a snapshot: the person, their open asks (the Inbox), and the members.
    /// Only open asks addressed to `me` count, whatever the snapshot holds.
    pub fn reset(&mut self, me: MemberId, asks: Vec<Ask>, members: Vec<Member>) {
        self.me = Some(me);
        self.open.clear();
        self.over = false;
        self.names.clear();
        for member in members {
            self.learn(member);
        }
        for ask in asks {
            if ask.to == me && ask.state == AskState::Open {
                self.insert(&ask);
            }
        }
    }

    /// Applies one event, given by its type and data (`Event.body`). Events of other kinds, and
    /// asks for other members, change nothing.
    pub fn apply(&mut self, kind: &str, data: serde_json::Value) -> Applied {
        let Some(me) = self.me else {
            return Applied::default();
        };
        match kind {
            "ask_raised" => {
                let Ok(Raised { ask }) = serde_json::from_value(data) else {
                    return Applied::default();
                };
                if ask.to != me || ask.state != AskState::Open || self.open.contains_key(&ask.id) {
                    return Applied::default();
                }
                let before = self.count();
                self.insert(&ask);
                Applied {
                    changed: self.count() != before,
                    new: Some(NewAsk {
                        id: ask.id,
                        kind: ask.kind,
                        asker: ask.from,
                        from: self.names.get(&ask.from).cloned(),
                        title: ask.title,
                        body: ask.body,
                        task: ask.task,
                    }),
                    refetch: false,
                }
            }
            "ask_answered" => {
                let Ok(Answered { ask }) = serde_json::from_value(data) else {
                    return Applied::default();
                };
                if self.open.remove(&ask).is_some() {
                    Applied {
                        changed: true,
                        ..Applied::default()
                    }
                } else {
                    // Over the bound, an ask not kept may be one of the person's.
                    Applied {
                        refetch: self.over,
                        ..Applied::default()
                    }
                }
            }
            "member_added" => {
                if let Ok(MemberAdded { member }) = serde_json::from_value(data) {
                    self.learn(member);
                }
                Applied::default()
            }
            _ => Applied::default(),
        }
    }

    /// The name of member `id`, if known.
    #[must_use]
    pub fn name_of(&self, id: &MemberId) -> Option<&str> {
        self.names.get(id).map(String::as_str)
    }

    /// The kind and asker of open ask `id` (tests).
    #[must_use]
    pub fn open_ask(&self, id: &AskId) -> Option<(AskKind, MemberId)> {
        self.open.get(id).map(|o| (o.kind, o.from))
    }

    fn insert(&mut self, ask: &Ask) {
        if self.open.len() < self.max_open {
            self.open.insert(
                ask.id,
                Open {
                    kind: ask.kind,
                    from: ask.from,
                },
            );
        } else {
            self.over = true;
        }
    }

    fn learn(&mut self, member: Member) {
        if self.names.len() < self.max_members || self.names.contains_key(&member.id) {
            self.names.insert(member.id, member.name);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pitcrew_protocol::model::MemberKind;
    use serde_json::json;

    fn member(n: u8, name: &str) -> Member {
        Member {
            id: id(n),
            kind: if n == 1 {
                MemberKind::Human
            } else {
                MemberKind::Agent
            },
            handle: format!("@{}", name.to_lowercase()),
            name: name.into(),
            owner: (n != 1).then(|| id(1)),
            persona: None,
        }
    }

    fn id(n: u8) -> MemberId {
        format!("01JA00000000000000000000{n:02}").parse().unwrap()
    }

    fn ask(n: u8, from: u8, to: u8) -> Ask {
        Ask {
            id: format!("01JB00000000000000000000{n:02}").parse().unwrap(),
            kind: AskKind::Question,
            from: id(from),
            to: id(to),
            task: None,
            session: None,
            title: format!("Question {n}"),
            body: String::new(),
            options: vec![],
            receipts: vec![],
            state: AskState::Open,
            answer: None,
            created: 1,
        }
    }

    fn raised(a: &Ask) -> serde_json::Value {
        json!({ "ask": a })
    }

    #[test]
    fn snapshot_raised_answered_and_others_ignored() {
        let mut t = Tracker::new(100, 100);
        assert_eq!(
            t.apply("ask_raised", raised(&ask(9, 2, 1))),
            Applied::default(),
            "no snapshot yet"
        );
        let mut closed = ask(3, 2, 1);
        closed.state = AskState::Answered;
        t.reset(
            id(1),
            vec![ask(1, 2, 1), ask(2, 2, 5), closed],
            vec![member(1, "Sam"), member(2, "Writer")],
        );
        assert_eq!(
            t.count(),
            Count {
                open: 1,
                more: false
            }
        );

        let a = t.apply("ask_raised", raised(&ask(4, 2, 1)));
        assert!(a.changed);
        let new = a.new.unwrap();
        assert_eq!(
            (new.from.as_deref(), new.title.as_str()),
            (Some("Writer"), "Question 4")
        );
        assert_eq!(t.count().open, 2);

        // Again (a replay): known, not new.
        assert_eq!(
            t.apply("ask_raised", raised(&ask(4, 2, 1))),
            Applied::default()
        );
        // For another member: ignored.
        assert_eq!(
            t.apply("ask_raised", raised(&ask(5, 2, 7))),
            Applied::default()
        );
        // Malformed or other kinds: ignored.
        assert_eq!(
            t.apply("ask_raised", json!({ "ask": 1 })),
            Applied::default()
        );
        assert_eq!(t.apply("task_moved", json!({})), Applied::default());

        let a = t.apply(
            "ask_answered",
            json!({ "ask": ask(1, 2, 1).id, "answer": {} }),
        );
        assert!(a.changed && a.new.is_none());
        assert_eq!(t.count().open, 1);
        // Another member's ask answered: nothing.
        assert_eq!(
            t.apply("ask_answered", json!({ "ask": ask(2, 2, 5).id })),
            Applied::default()
        );

        // A new member's name is learnt.
        t.apply("member_added", json!({ "member": member(6, "Reviewer") }));
        let new = t.apply("ask_raised", raised(&ask(6, 6, 1))).new.unwrap();
        assert_eq!(new.from.as_deref(), Some("Reviewer"));
        // An unknown asker has no name.
        assert_eq!(
            t.apply("ask_raised", raised(&ask(7, 8, 1)))
                .new
                .unwrap()
                .from,
            None
        );
    }

    #[test]
    fn the_bound() {
        let mut t = Tracker::new(3, 2);
        t.reset(
            id(1),
            (1..=5).map(|n| ask(n, 2, 1)).collect(),
            vec![member(1, "Sam"), member(2, "Writer"), member(3, "Third")],
        );
        assert_eq!(
            t.count(),
            Count {
                open: 3,
                more: true
            }
        );
        assert!(t.name_of(&id(3)).is_none(), "names are bounded too");
        // A new ask over the bound is still news, but not kept.
        let a = t.apply("ask_raised", raised(&ask(9, 2, 1)));
        assert!(a.new.is_some() && !a.changed);
        assert_eq!(
            t.count(),
            Count {
                open: 3,
                more: true
            }
        );
        // An answer for an ask not kept: the count is unsure, so fetch again.
        let a = t.apply("ask_answered", json!({ "ask": ask(5, 2, 1).id }));
        assert!(a.refetch && !a.changed);
        // A snapshot within the bound clears it.
        t.reset(id(1), vec![ask(1, 2, 1)], vec![]);
        assert_eq!(
            t.count(),
            Count {
                open: 1,
                more: false
            }
        );
        assert!(
            !t.apply("ask_answered", json!({ "ask": ask(5, 2, 1).id }))
                .refetch
        );
    }
}
