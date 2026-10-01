//! Agent hooks as session state: [`RunnerHooks`] is the API's `HookSink`.
//!
//! Hooks beat the transcript watcher on latency (no debounce, no read), so they move a session's
//! state first. Both sources feed one place, the watcher thread, which keeps them in agreement:
//! - a hook that repeats the transcript's state emits nothing, and neither does the transcript
//!   catching up with a hook;
//! - a hook older than the newest transcript item or hook is stale and ignored, so a late hook
//!   never moves a session back (see `derive::report`).
//!
//! Hooks name the CLI's own session id; the runner's index maps it to the [`SessionId`]. A hook
//! for a session not indexed yet (its transcript has no first line yet) is held a while and
//! applies when the session is discovered (see `held`).
//!
//! Every hook carries its sender (the token's caller) to where its session is resolved, as
//! [`Origin::Hook`]; only there is the session's agent known, and the ownership rule
//! ([`refusal`]) decides. A held hook keeps its sender and is decided at discovery.
//!
//! [`SessionId`]: pitcrew_protocol::ids::SessionId

use crate::agents::SessionAgent;
use crate::derive::Reported;
use crate::watch::{Origin, Shared, Signal, Target};
use pitcrew_api::hooks::{HookEvent, HookSink};
use pitcrew_protocol::api::{Caller, TokenScope};
use pitcrew_protocol::ids::MemberId;
use pitcrew_protocol::model::{Engine, SessionState};
use serde_json::{Map, Value};
use std::sync::Arc;

/// Turns agent hooks into session state. Get it from [`RunnerHandle::hooks`] and hand it to the
/// API's `HookIntake`.
///
/// Understood:
/// - Claude Code: `SessionStart` (idle; not after a compaction), `UserPromptSubmit` (working),
///   `Stop` (idle), `SessionEnd` (ended), and `Notification` (waiting for a permission prompt or
///   a dialog; other notifications change nothing);
/// - Codex: its `notify` payload `agent-turn-complete` (idle).
///
/// Every other hook is ignored.
///
/// # Who may change a session
///
/// A hook changes a session only if its sender may, by the session's agent as
/// [`SessionAgents`] tells it:
/// - **an agent token** (`scope` agent): only a session whose agent is the token's member;
/// - **a device token** (a person): only a session with no agent, or whose agent that person
///   owns;
/// - anything else, and any session whose agent is [unknown](SessionAgent::Unknown) (no
///   [`SessionAgents`] configured, a failed or panicking lookup), is refused: dropped and logged
///   at debug with the reason, never applied and never held.
///
/// The token's `on_behalf_of` never widens what an agent may change. A hook for a session not
/// indexed yet is held with its sender and decided when the session is discovered; one refused
/// then is dropped. Codex's `notify` follows the same rule. Each sender holds at most 32 hooks:
/// a flood from one sender drops its own oldest, not another's.
///
/// [`RunnerHandle::hooks`]: crate::RunnerHandle::hooks
/// [`SessionAgents`]: crate::SessionAgents
#[derive(Clone, Debug)]
pub struct RunnerHooks {
    shared: Arc<Shared>,
}

impl RunnerHooks {
    pub(crate) fn new(shared: Arc<Shared>) -> Self {
        Self { shared }
    }
}

impl HookSink for RunnerHooks {
    fn deliver(&self, event: HookEvent) {
        match signal(&event) {
            Some(s) => self.shared.signal(s),
            None => tracing::trace!(engine = ?event.engine, event = %event.event, "hook ignored"),
        }
    }
}

/// Who sent a hook: the caller its token names. Built only from a delivered hook, so a
/// [`Signal`] from a hook always says who sent it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct Sender(Caller);

impl Sender {
    #[cfg(test)]
    pub(crate) fn new(caller: Caller) -> Self {
        Self(caller)
    }

    /// The member the token belongs to.
    pub(crate) fn member(&self) -> MemberId {
        self.0.member
    }
}

/// Why `sender` may not change a session run as `agent`; `None` if it may. The rule is on
/// [`RunnerHooks`].
pub(crate) fn refusal(sender: &Sender, agent: &SessionAgent) -> Option<&'static str> {
    let caller = &sender.0;
    // Every scope and every answer is matched explicitly: a new one must be decided here.
    match (caller.scope, agent) {
        (_, SessionAgent::Unknown) => Some("the session's agent is unknown"),
        (TokenScope::Agent, SessionAgent::Agent { agent: runs_as, .. }) => {
            (*runs_as != caller.member).then_some("the session is another agent's")
        }
        (TokenScope::Agent, SessionAgent::NoAgent) => {
            Some("an agent's hook for a session without an agent")
        }
        (TokenScope::Device, SessionAgent::NoAgent) => None,
        (TokenScope::Device, SessionAgent::Agent { owner, .. }) => {
            (*owner != caller.member).then_some("the session's agent is another person's")
        }
    }
}

/// The state change a hook reports, if it reports one.
pub(crate) fn signal(event: &HookEvent) -> Option<Signal> {
    let p = &event.payload;
    let (native_id, to, status_line) = match event.engine {
        Engine::Claude => {
            let (to, status_line) = claude(&event.event, p)?;
            (text(p, "session_id")?, to, status_line)
        }
        Engine::Codex => {
            if text(p, "type")? != "agent-turn-complete" {
                return None;
            }
            let id = text(p, "thread-id")
                .or_else(|| text(p, "thread_id"))
                .or_else(|| text(p, "session_id"))?;
            (id, SessionState::Idle, None)
        }
        _ => return None,
    };
    Some(Signal {
        target: Target::Native {
            engine: event.engine,
            native_id: native_id.to_owned(),
        },
        report: Reported {
            at: event.received_at,
            to,
            status_line,
        },
        origin: Origin::Hook(Sender(event.caller)),
    })
}

fn claude(event: &str, p: &Map<String, Value>) -> Option<(SessionState, Option<String>)> {
    match event {
        // After a compaction the turn goes on; any other start is a session waiting for a prompt.
        "SessionStart" if text(p, "source") == Some("compact") => None,
        "SessionStart" | "Stop" => Some((SessionState::Idle, None)),
        "UserPromptSubmit" => Some((SessionState::Working, None)),
        "SessionEnd" => Some((SessionState::Ended, None)),
        "Notification" => {
            let message = text(p, "message").unwrap_or("");
            let waiting = match text(p, "notification_type") {
                Some(kind) => matches!(kind, "permission_prompt" | "elicitation_dialog"),
                // Older versions send only the message.
                None => message.contains("permission"),
            };
            waiting.then(|| {
                (
                    SessionState::Waiting,
                    Some(message.to_owned()).filter(|m| !m.is_empty()),
                )
            })
        }
        _ => None,
    }
}

fn text<'a>(p: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    p.get(key).and_then(Value::as_str).filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member(n: u128) -> MemberId {
        MemberId(ulid::Ulid::from_parts(1, n))
    }

    fn caller() -> Caller {
        Caller {
            member: member(1),
            scope: TokenScope::Agent,
            on_behalf_of: Some(member(2)),
        }
    }

    fn hook(engine: Engine, event: &str, payload: Value) -> HookEvent {
        let Value::Object(payload) = payload else {
            panic!("payload is an object");
        };
        HookEvent {
            engine,
            event: event.into(),
            caller: caller(),
            payload,
            received_at: 42,
        }
    }

    #[test]
    fn hooks_carry_their_sender() {
        let claude = serde_json::json!({"session_id": "s1", "hook_event_name": "Stop"});
        let codex = serde_json::json!({"type": "agent-turn-complete", "thread-id": "t1"});
        for (engine, event, payload) in [
            (Engine::Claude, "Stop", claude),
            (Engine::Codex, "notify", codex),
        ] {
            let s = signal(&hook(engine, event, payload));
            assert_eq!(
                s.map(|s| s.origin),
                Some(Origin::Hook(Sender(caller()))),
                "{engine:?}"
            );
        }
    }

    #[test]
    fn the_ownership_rule() {
        use SessionAgent::{Agent, NoAgent, Unknown};
        let (person, other_person) = (member(10), member(11));
        let (agent, other_agent) = (member(20), member(21));
        let device = |m| {
            Sender(Caller {
                member: m,
                scope: TokenScope::Device,
                on_behalf_of: None,
            })
        };
        let agent_token = |m, owner| {
            Sender(Caller {
                member: m,
                scope: TokenScope::Agent,
                on_behalf_of: Some(owner),
            })
        };
        let persons_agent = Agent {
            agent,
            owner: person,
        };
        let others_agent = Agent {
            agent: other_agent,
            owner: other_person,
        };
        let allowed = |s: Sender, a: SessionAgent| refusal(&s, &a).is_none();

        // An agent: only its own sessions; its owner (on_behalf_of) widens nothing.
        assert!(allowed(agent_token(agent, person), persons_agent));
        assert!(!allowed(
            agent_token(other_agent, other_person),
            persons_agent
        ));
        assert!(!allowed(agent_token(other_agent, person), persons_agent));
        assert!(!allowed(agent_token(agent, person), NoAgent));
        assert!(!allowed(agent_token(agent, person), Unknown));
        // Nor does naming the session's owner as its member.
        assert!(!allowed(agent_token(person, person), persons_agent));
        // A person: sessions without an agent, and their own agents' sessions.
        assert!(allowed(device(person), NoAgent));
        assert!(allowed(device(person), persons_agent));
        assert!(!allowed(device(person), others_agent));
        assert!(!allowed(device(person), Unknown));
        // A person's member as the session's agent is not ownership.
        assert!(!allowed(
            device(person),
            Agent {
                agent: person,
                owner: other_person
            }
        ));
        assert_eq!(
            refusal(&device(other_person), &persons_agent),
            Some("the session's agent is another person's")
        );
    }

    fn state(engine: Engine, event: &str, payload: Value) -> Option<(String, SessionState)> {
        signal(&hook(engine, event, payload)).map(|s| {
            let Target::Native { native_id, .. } = s.target else {
                panic!("hooks name native ids");
            };
            assert_eq!(s.report.at, 42);
            (native_id, s.report.to)
        })
    }

    #[test]
    fn claude_hooks() {
        let s = |event: &str, extra: Value| {
            let mut p = serde_json::json!({"session_id": "s1", "cwd": "/w"});
            if let (Some(p), Value::Object(extra)) = (p.as_object_mut(), extra) {
                p.extend(extra);
            }
            state(Engine::Claude, event, p).map(|(id, to)| {
                assert_eq!(id, "s1");
                to
            })
        };
        let none = Value::Null;
        use SessionState::{Ended, Idle, Waiting, Working};
        assert_eq!(
            s("SessionStart", serde_json::json!({"source": "startup"})),
            Some(Idle)
        );
        assert_eq!(
            s("SessionStart", serde_json::json!({"source": "compact"})),
            None
        );
        assert_eq!(s("UserPromptSubmit", none.clone()), Some(Working));
        assert_eq!(s("Stop", none.clone()), Some(Idle));
        assert_eq!(s("SessionEnd", none.clone()), Some(Ended));
        assert_eq!(
            s(
                "Notification",
                serde_json::json!({"notification_type": "permission_prompt",
                                   "message": "Claude needs your permission to use Bash"})
            ),
            Some(Waiting)
        );
        assert_eq!(
            s(
                "Notification",
                serde_json::json!({"notification_type": "idle_prompt",
                                   "message": "Claude is waiting for your input"})
            ),
            None
        );
        assert_eq!(
            s(
                "Notification",
                serde_json::json!({"message": "Claude needs your permission to use Edit"})
            ),
            Some(Waiting)
        );
        for unknown in [
            "PreToolUse",
            "PostToolUse",
            "SubagentStop",
            "PreCompact",
            "Nope",
        ] {
            assert_eq!(s(unknown, none.clone()), None, "{unknown}");
        }
        // No session id, no state.
        assert_eq!(state(Engine::Claude, "Stop", serde_json::json!({})), None);
    }

    #[test]
    fn a_waiting_notification_keeps_its_message() {
        let got = signal(&hook(
            Engine::Claude,
            "Notification",
            serde_json::json!({"session_id": "s", "notification_type": "permission_prompt",
                               "message": "Claude needs your permission to use Bash"}),
        ))
        .map(|s| s.report.status_line);
        assert_eq!(
            got,
            Some(Some("Claude needs your permission to use Bash".into()))
        );
    }

    #[test]
    fn codex_notify() {
        let turn = serde_json::json!({"type": "agent-turn-complete", "thread-id": "t1",
                                      "turn-id": "1", "cwd": "/w"});
        assert_eq!(
            state(Engine::Codex, "notify", turn),
            Some(("t1".into(), SessionState::Idle))
        );
        let other = serde_json::json!({"type": "something-else", "thread-id": "t1"});
        assert_eq!(state(Engine::Codex, "notify", other), None);
        assert_eq!(
            state(
                Engine::OpenCode,
                "session.idle",
                serde_json::json!({"session_id": "o"})
            ),
            None
        );
    }
}
