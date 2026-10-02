//! Who runs each session: [`SessionAgents`], the lookup behind the hook ownership rule (see
//! [`RunnerHooks`](crate::RunnerHooks)).

use pitcrew_protocol::ids::{MemberId, SessionId};
use std::collections::HashMap;
use std::fmt;
use std::sync::{PoisonError, RwLock};

/// A session's agent, as the hub knows it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SessionAgent {
    /// The session has no agent: the hub stored it without one, or has not stored it yet (see
    /// [`SessionAgents`]).
    NoAgent,
    /// The session runs as `agent`, an agent member that the person `owner` owns.
    Agent {
        /// The agent member.
        agent: MemberId,
        /// The person who owns it; `None` if the hub records the agent without an owner. Then
        /// only the agent's own hooks change its sessions, and no person's.
        owner: Option<MemberId>,
    },
    /// Not known. The runner refuses every hook for the session.
    #[default]
    Unknown,
}

/// Who runs each session, as the hub knows it. The runner asks before it applies a hook (see
/// [`RunnerHooks`](crate::RunnerHooks)); the daemon implements it over the hub's sessions and
/// members.
///
/// # What to answer
///
/// - [`SessionAgent::Agent`] for a session the hub stored with an agent, naming the agent's owner
///   (or none, if the hub has none for it).
/// - [`SessionAgent::NoAgent`] for a session the hub stored without one, **or has not stored at
///   all**. The runner states every session it discovers without an agent, and the hub keeps an
///   agent it already had when a session is stated again; a dispatch stores its session, agent
///   named, before its CLI starts. So a session the hub has not stored cannot have an agent yet.
/// - [`SessionAgent::Unknown`] whenever in doubt: the lookup failed, the agent's member cannot
///   be read, or anything else is unclear. The runner refuses the hook.
/// - **A sub-agent session** (one with a `parent`) runs as its parent: answer the parent's agent.
///   The runner states sub-agent sessions without an agent of their own, so answering only the
///   session's own field would refuse the dispatched agent's hooks from its sub-agents and let
///   any person's through. For a sub-agent not stored yet, answer `NoAgent` as for any session:
///   when its transcript names a parent, the runner then asks about the parent itself.
///
/// # Rules for implementations
///
/// - **See the hub's latest session writes.** A cache or a lagging copy could answer `NoAgent`
///   for a session that has since gained an agent, and let any person's hook change it.
/// - **Never call back into the runner** (its handle, hooks, terminals or commands): the runner
///   asks from its watcher thread, so a call that waits for the runner waits for itself.
/// - **Answer quickly**: the watcher thread waits for the answer. A panic counts as `Unknown`.
///
/// # Today's session ids
///
/// The runner mints its own [`SessionId`] for each transcript it discovers and states it with no
/// agent. A dispatched session therefore has two ids today: the one the dispatch stored with its
/// agent, and the runner's, which has no agent. Hooks resolve to the runner's id, so a dispatched
/// agent's own hooks are refused, and its owner's (or any person's) hooks apply as for a session
/// without an agent. Once the runner starts a dispatch's CLI under the dispatch's session id, the
/// hub has the agent before the first hook, and the rule holds as intended; the runner's
/// re-statements must then keep naming no agent, which the hub reads as "keep the one it has".
pub trait SessionAgents: Send + Sync + fmt::Debug {
    /// The agent `session` runs as. See the [trait docs](Self) for what to answer.
    fn agent_of(&self, session: SessionId) -> SessionAgent;
}

/// [`SessionAgents`] in memory: **for tests, or for a host that fills it** from the hub's
/// sessions as they change. A session it was not told about has no agent (as a session the hub
/// has not stored), so an empty one lets any person's hook change any session and no agent's.
///
/// A host must fill it synchronously, as part of the hub's write: a dispatch's session must be
/// [`set`](Self::set) **before its CLI starts**, or its first hooks find no agent.
#[derive(Debug, Default)]
pub struct MemoryAgents {
    agents: RwLock<HashMap<SessionId, SessionAgent>>,
}

impl MemoryAgents {
    /// Knows no session yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Records what `session` runs as.
    pub fn set(&self, session: SessionId, agent: SessionAgent) {
        self.agents
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(session, agent);
    }

    /// Forgets `session`: it has no agent again.
    pub fn forget(&self, session: SessionId) {
        self.agents
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&session);
    }
}

impl SessionAgents for MemoryAgents {
    fn agent_of(&self, session: SessionId) -> SessionAgent {
        self.agents
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&session)
            .copied()
            .unwrap_or(SessionAgent::NoAgent)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_agents_answer_what_they_were_told() {
        let mem = MemoryAgents::new();
        let s = SessionId::new();
        assert_eq!(mem.agent_of(s), SessionAgent::NoAgent);
        let agent = SessionAgent::Agent {
            agent: MemberId::new(),
            owner: Some(MemberId::new()),
        };
        mem.set(s, agent);
        assert_eq!(mem.agent_of(s), agent);
        assert_eq!(mem.agent_of(SessionId::new()), SessionAgent::NoAgent);
        mem.set(s, SessionAgent::Unknown);
        assert_eq!(mem.agent_of(s), SessionAgent::Unknown);
        mem.forget(s);
        assert_eq!(mem.agent_of(s), SessionAgent::NoAgent);
    }
}
