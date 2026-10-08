//! Who runs each session, for the runner's hook ownership rule: [`HubAgents`], the runner's
//! `SessionAgents` over hub-work's sessions and members.
//!
//! - **Always current.** Every answer is one read of the hub's tables (`Store::read`, a fresh
//!   read transaction), with no cache: it sees every write committed before it, whoever made it.
//!   A cached "no agent" for a session that has since gained one would let any person's hook
//!   change it.
//! - **Sub-agents run as their parent.** A session with a `parent` answers the agent found up its
//!   chain of parents: the runner states sub-agent sessions without an agent of their own.
//! - **`Unknown` when in doubt:** the read failed, the agent is not a member the hub knows as an
//!   agent, two sessions of the chain name different agents, or the chain is longer than
//!   [`MAX_CHAIN`] sessions or loops. The runner refuses every hook for such a session.
//! - It never calls back into the runner: it reads the store only.
//!
//! **A window at discovery.** The runner decides the hooks it held for a new session when it
//! discovers it, before the hub has stored that session (the `session_discovered` is on its way
//! to the store). The session is then not in the tables, so its `parent` cannot be seen either,
//! and the answer is `NoAgent`, even for a sub-agent whose parent has an agent. Harmless today:
//! every parent is the runner's own session, which has no agent. Once the runner adopts dispatch
//! ids, a dispatched agent's sub-agents would answer `NoAgent` in that window, so a person's held
//! hooks for them would apply and the agent's own would not. The fix belongs to the runner: when
//! it holds a sub-agent's hooks, ask about its parent too (`agent_of(parent)` when the sub-agent
//! answers `NoAgent`), or pass the parent in.

use pitcrew_hub_work::{WorkService, query};
use pitcrew_protocol::ids::{MemberId, SessionId};
use pitcrew_protocol::model::MemberKind;
use pitcrew_runner::{SessionAgent, SessionAgents};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Most sessions read up a chain of parents, the session itself included (Claude's sub-agents
/// are one level deep). A chain that goes on past them, to a 17th session whether the hub has
/// stored it or not, is `Unknown`: safe, as an unknown agent refuses the hook.
pub const MAX_CHAIN: usize = 16;

/// The runner's `SessionAgents` over the hub's sessions and members.
#[derive(Debug)]
pub struct HubAgents {
    work: Arc<WorkService>,
    /// A read failed; warned once, then logged at debug.
    failed: AtomicBool,
}

impl HubAgents {
    /// Reads `work`'s tables.
    #[must_use]
    pub fn new(work: Arc<WorkService>) -> Self {
        Self {
            work,
            failed: AtomicBool::new(false),
        }
    }
}

impl SessionAgents for HubAgents {
    fn agent_of(&self, session: SessionId) -> SessionAgent {
        match self.work.read(|conn| agent_in(conn, session)) {
            Ok(agent) => agent,
            Err(e) => {
                if self.failed.swap(true, Ordering::Relaxed) {
                    tracing::debug!(error = %e, %session, "cannot read who runs a session; its hooks are refused");
                } else {
                    tracing::warn!(error = %e, %session, "cannot read who runs a session; its hooks are refused (later failures are logged at debug)");
                }
                SessionAgent::Unknown
            }
        }
    }
}

/// What `session` runs as, in one snapshot of the tables.
fn agent_in(
    conn: &pitcrew_store::sql::Connection,
    session: SessionId,
) -> pitcrew_hub_work::Result<SessionAgent> {
    let mut named: Option<MemberId> = None;
    let mut id = session;
    for _ in 0..MAX_CHAIN {
        // A session the hub has not stored has no agent yet (see `SessionAgents`), and neither
        // does a sub-agent whose parent it has not stored.
        let Some(s) = query::session(conn, &id)? else {
            return answer(conn, named);
        };
        match (named, s.agent) {
            (Some(a), Some(b)) if a != b => {
                tracing::debug!(%session, "sessions of one chain name different agents");
                return Ok(SessionAgent::Unknown);
            }
            (None, Some(agent)) => named = Some(agent),
            _ => {}
        }
        match s.parent {
            Some(parent) => id = parent,
            None => return answer(conn, named),
        }
    }
    tracing::debug!(%session, "a chain of parent sessions is too long or loops");
    Ok(SessionAgent::Unknown)
}

/// The answer for a chain that named `named` (or no agent).
fn answer(
    conn: &pitcrew_store::sql::Connection,
    named: Option<MemberId>,
) -> pitcrew_hub_work::Result<SessionAgent> {
    match named {
        Some(agent) => agent_member(conn, agent),
        None => Ok(SessionAgent::NoAgent),
    }
}

/// `agent` as the session's agent: it must be an agent member the hub knows.
fn agent_member(
    conn: &pitcrew_store::sql::Connection,
    agent: MemberId,
) -> pitcrew_hub_work::Result<SessionAgent> {
    Ok(match query::member(conn, &agent)? {
        Some(member) if member.kind == MemberKind::Agent => SessionAgent::Agent {
            agent,
            owner: member.owner,
        },
        Some(_) => {
            tracing::debug!(member = %agent, "a session's agent is a person");
            SessionAgent::Unknown
        }
        None => {
            tracing::debug!(member = %agent, "a session's agent is not a member");
            SessionAgent::Unknown
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use pitcrew_protocol::events::{Event, EventBody};
    use pitcrew_protocol::ids::{MachineId, WorkspaceId};
    use pitcrew_protocol::model::{Engine, Member, Session, SessionState, Workspace};
    use pitcrew_store::{Store, StoreOptions};

    struct Hub {
        _tmp: tempfile::TempDir,
        store: Arc<Store>,
        work: Arc<WorkService>,
        workspace: WorkspaceId,
        person: MemberId,
    }

    impl Hub {
        fn new() -> Self {
            let tmp = tempfile::tempdir().unwrap();
            let store = Arc::new(
                Store::open_with(
                    tmp.path().join("hub.db"),
                    StoreOptions::default(),
                    pitcrew_hub_work::projections(),
                )
                .unwrap(),
            );
            let workspace = Workspace {
                id: WorkspaceId::new(),
                name: "Lab".into(),
            };
            let work = Arc::new(WorkService::new(Arc::clone(&store), workspace.clone()));
            let hub = Self {
                _tmp: tmp,
                store,
                work,
                workspace: workspace.id,
                person: MemberId::new(),
            };
            hub.add_member(hub.person, MemberKind::Human, None);
            hub
        }

        fn append(&self, body: EventBody) {
            self.store
                .append(&[Event::now(self.workspace, self.person, body)])
                .unwrap();
        }

        fn add_member(&self, id: MemberId, kind: MemberKind, owner: Option<MemberId>) {
            self.append(EventBody::MemberAdded {
                member: Member {
                    id,
                    kind,
                    handle: format!("@{id}"),
                    name: "M".into(),
                    owner,
                    persona: None,
                    avatar: None,
                },
            });
        }

        /// States a session (again), as the runner or a dispatch does.
        fn state(&self, id: SessionId, agent: Option<MemberId>, parent: Option<SessionId>) {
            self.append(EventBody::SessionDiscovered {
                session: Session {
                    id,
                    engine: Engine::Claude,
                    native_id: id.to_string(),
                    machine: MachineId::new(),
                    cwd: "/w".into(),
                    branch: None,
                    title: None,
                    agent,
                    workstream: None,
                    task: None,
                    link_basis: None,
                    state: SessionState::Working,
                    status_line: None,
                    started: 1,
                    last_activity: 1,
                    terminal: None,
                    parent,
                },
            });
        }

        fn agents(&self) -> HubAgents {
            HubAgents::new(Arc::clone(&self.work))
        }
    }

    #[test]
    fn sessions_answer_their_agent_and_its_owner() {
        let hub = Hub::new();
        let agents = hub.agents();
        let writer = MemberId::new();
        hub.add_member(writer, MemberKind::Agent, Some(hub.person));
        let ownerless = MemberId::new();
        hub.add_member(ownerless, MemberKind::Agent, None);

        let (unstored, plain, run, solo) = (
            SessionId::new(),
            SessionId::new(),
            SessionId::new(),
            SessionId::new(),
        );
        hub.state(plain, None, None);
        hub.state(run, Some(writer), None);
        hub.state(solo, Some(ownerless), None);
        assert_eq!(agents.agent_of(unstored), SessionAgent::NoAgent);
        assert_eq!(agents.agent_of(plain), SessionAgent::NoAgent);
        assert_eq!(
            agents.agent_of(run),
            SessionAgent::Agent {
                agent: writer,
                owner: Some(hub.person)
            }
        );
        assert_eq!(
            agents.agent_of(solo),
            SessionAgent::Agent {
                agent: ownerless,
                owner: None
            }
        );
    }

    /// No cache: a session that gains an agent is answered with it at once, and a re-statement
    /// without one (the runner's) keeps it, as the hub does.
    #[test]
    fn the_latest_writes_are_seen() {
        let hub = Hub::new();
        let agents = hub.agents();
        let writer = MemberId::new();
        hub.add_member(writer, MemberKind::Agent, Some(hub.person));
        let s = SessionId::new();
        hub.state(s, None, None);
        assert_eq!(agents.agent_of(s), SessionAgent::NoAgent);
        hub.state(s, Some(writer), None);
        let expected = SessionAgent::Agent {
            agent: writer,
            owner: Some(hub.person),
        };
        assert_eq!(agents.agent_of(s), expected);
        hub.state(s, None, None);
        assert_eq!(agents.agent_of(s), expected);
    }

    #[test]
    fn sub_agents_run_as_their_parent() {
        let hub = Hub::new();
        let agents = hub.agents();
        let (writer, reviewer) = (MemberId::new(), MemberId::new());
        hub.add_member(writer, MemberKind::Agent, Some(hub.person));
        hub.add_member(reviewer, MemberKind::Agent, Some(hub.person));
        let as_writer = SessionAgent::Agent {
            agent: writer,
            owner: Some(hub.person),
        };

        let (main, sub, subsub) = (SessionId::new(), SessionId::new(), SessionId::new());
        hub.state(main, Some(writer), None);
        hub.state(sub, None, Some(main));
        hub.state(subsub, None, Some(sub));
        assert_eq!(agents.agent_of(sub), as_writer);
        assert_eq!(agents.agent_of(subsub), as_writer);

        // A sub-agent naming its parent's agent is that agent; naming another is in doubt.
        let (same, other) = (SessionId::new(), SessionId::new());
        hub.state(same, Some(writer), Some(main));
        hub.state(other, Some(reviewer), Some(main));
        assert_eq!(agents.agent_of(same), as_writer);
        assert_eq!(agents.agent_of(other), SessionAgent::Unknown);

        // A sub-agent whose parent the hub has not stored has no agent, like its parent.
        let orphan = SessionId::new();
        hub.state(orphan, None, Some(SessionId::new()));
        assert_eq!(agents.agent_of(orphan), SessionAgent::NoAgent);
        // ...unless it names one itself.
        let named = SessionId::new();
        hub.state(named, Some(writer), Some(SessionId::new()));
        assert_eq!(agents.agent_of(named), as_writer);

        // A chain that loops is in doubt.
        let (a, b) = (SessionId::new(), SessionId::new());
        hub.state(a, None, Some(b));
        hub.state(b, None, Some(a));
        assert_eq!(agents.agent_of(a), SessionAgent::Unknown);
    }

    /// A chain of `MAX_CHAIN` sessions is followed to its root; one going on past it, to a parent
    /// stored or not, is in doubt.
    #[test]
    fn chains_are_followed_for_max_chain_sessions() {
        let hub = Hub::new();
        let agents = hub.agents();
        let writer = MemberId::new();
        hub.add_member(writer, MemberKind::Agent, Some(hub.person));
        // A root with the agent, and below it a chain of sub-agents: chain[k] has k ancestors.
        let root = SessionId::new();
        hub.state(root, Some(writer), None);
        let mut chain = vec![root];
        for _ in 1..=MAX_CHAIN {
            let next = SessionId::new();
            hub.state(next, None, chain.last().copied());
            chain.push(next);
        }
        let as_writer = SessionAgent::Agent {
            agent: writer,
            owner: Some(hub.person),
        };
        // `MAX_CHAIN` sessions, the root included.
        assert_eq!(agents.agent_of(chain[MAX_CHAIN - 1]), as_writer);
        // One more.
        assert_eq!(agents.agent_of(chain[MAX_CHAIN]), SessionAgent::Unknown);
        // `MAX_CHAIN` sessions whose root names a parent the hub has not stored: the 17th is not
        // read, so in doubt, although it would have had no agent.
        let mut unrooted = vec![SessionId::new()];
        hub.state(unrooted[0], None, Some(SessionId::new()));
        for _ in 1..MAX_CHAIN {
            let next = SessionId::new();
            hub.state(next, None, unrooted.last().copied());
            unrooted.push(next);
        }
        assert_eq!(
            agents.agent_of(unrooted[MAX_CHAIN - 1]),
            SessionAgent::Unknown
        );
        assert_eq!(
            agents.agent_of(unrooted[MAX_CHAIN - 2]),
            SessionAgent::NoAgent
        );
    }

    #[test]
    fn an_agent_the_hub_does_not_know_as_one_is_unknown() {
        let hub = Hub::new();
        let agents = hub.agents();
        let (stranger, person_run) = (SessionId::new(), SessionId::new());
        hub.state(stranger, Some(MemberId::new()), None);
        hub.state(person_run, Some(hub.person), None);
        assert_eq!(agents.agent_of(stranger), SessionAgent::Unknown);
        assert_eq!(agents.agent_of(person_run), SessionAgent::Unknown);
        // A sub-agent of such a session too.
        let sub = SessionId::new();
        hub.state(sub, None, Some(stranger));
        assert_eq!(agents.agent_of(sub), SessionAgent::Unknown);
    }
}
