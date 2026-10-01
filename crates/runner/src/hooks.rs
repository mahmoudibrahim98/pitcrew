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
//! for a session not indexed yet (its transcript has no first line yet) waits a while and
//! applies when the session is discovered.
//!
//! [`SessionId`]: pitcrew_protocol::ids::SessionId

use crate::derive::Reported;
use crate::watch::{Shared, Signal, Target};
use pitcrew_api::hooks::{HookEvent, HookSink};
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
/// [`RunnerHandle::hooks`]: crate::RunnerHandle::hooks
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
    use pitcrew_protocol::api::{Caller, TokenScope};
    use pitcrew_protocol::ids::MemberId;

    fn hook(engine: Engine, event: &str, payload: Value) -> HookEvent {
        let Value::Object(payload) = payload else {
            panic!("payload is an object");
        };
        HookEvent {
            engine,
            event: event.into(),
            caller: Caller {
                member: MemberId::new(),
                scope: TokenScope::Agent,
                on_behalf_of: None,
            },
            payload,
            received_at: 42,
        }
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
