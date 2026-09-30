//! Session state and events, derived from transcript items. Pure: no I/O.

use pitcrew_interfaces::source::TranscriptItem;
use pitcrew_protocol::events::EventBody;
use pitcrew_protocol::ids::SessionId;
use pitcrew_protocol::model::{Receipt, SessionState, TimestampMs};
use serde::{Deserialize, Serialize};

/// Open tool calls kept per session; the oldest is dropped past this.
const MAX_OPEN_CALLS: usize = 256;
/// Longest status line, in characters.
const MAX_STATUS_CHARS: usize = 120;

/// What the runner remembers about a session between reads. Saved with the cursor.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Facts {
    pub state: SessionState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_line: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub open_calls: Vec<OpenCall>,
    #[serde(default)]
    pub last_activity: TimestampMs,
    /// Offset of the last item that produced events, and how many it produced so far. Together
    /// with the offset they make event ids repeatable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_key: Option<u64>,
    #[serde(default)]
    pub seq: u32,
}

impl Default for Facts {
    fn default() -> Self {
        Self {
            state: SessionState::Starting,
            status_line: None,
            open_calls: Vec::new(),
            last_activity: 0,
            last_key: None,
            seq: 0,
        }
    }
}

/// A tool call without a result yet.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct OpenCall {
    pub call_id: String,
    pub tool: String,
    pub target: String,
    pub offset: u64,
}

/// An event body, with the item offset it came from and when it happened.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Derived {
    pub key: u64,
    pub at: TimestampMs,
    pub body: EventBody,
}

/// Folds one item into `facts`, appending the events it causes to `out`. With `emit_states`
/// false, state changes are folded but not emitted (the discovery read reports its final state in
/// `session_discovered` instead).
pub(crate) fn apply(
    facts: &mut Facts,
    session: SessionId,
    cwd: Option<&str>,
    item: &TranscriptItem,
    emit_states: bool,
    out: &mut Vec<Derived>,
) {
    let key = item.offset();
    let start = out.len();
    let (at, next) = match item {
        TranscriptItem::UserPrompt { at, .. }
        | TranscriptItem::AssistantText { at, .. }
        | TranscriptItem::PlanUpdated { at, .. } => (*at, Next::Working),
        TranscriptItem::ToolUse {
            at,
            call_id,
            tool,
            target,
            ..
        } => {
            if facts.open_calls.len() >= MAX_OPEN_CALLS {
                facts.open_calls.remove(0);
            }
            facts.open_calls.push(OpenCall {
                call_id: call_id.clone(),
                tool: tool.clone(),
                target: target.clone(),
                offset: key,
            });
            (*at, Next::Working)
        }
        TranscriptItem::ToolResult {
            at,
            call_id,
            is_error,
            summary,
            ..
        } => {
            match facts.open_calls.iter().position(|c| &c.call_id == call_id) {
                Some(i) => {
                    let call = facts.open_calls.remove(i);
                    out.push(Derived {
                        key,
                        at: *at,
                        body: EventBody::ToolRan {
                            session,
                            tool: call.tool,
                            target: call.target,
                            outcome: clip(first_line(summary)),
                            failed: *is_error,
                            receipt: Receipt::Transcript {
                                session,
                                offset: call.offset,
                            },
                        },
                    });
                }
                None => tracing::debug!(%session, call_id, "tool result without a known call"),
            }
            (*at, Next::Working)
        }
        TranscriptItem::FileEdit {
            at,
            path,
            added,
            removed,
            ..
        } => {
            out.push(Derived {
                key,
                at: *at,
                body: EventBody::FileEdited {
                    session,
                    path: relative_to(path, cwd),
                    added: *added,
                    removed: *removed,
                },
            });
            (*at, Next::Working)
        }
        TranscriptItem::Question { at, text, .. } => (*at, Next::Waiting(clip(first_line(text)))),
        TranscriptItem::TurnEnded { at, .. } => {
            facts.open_calls.clear();
            out.push(Derived {
                key,
                at: *at,
                body: EventBody::TurnEnded {
                    session,
                    receipt: Receipt::Transcript {
                        session,
                        offset: key,
                    },
                },
            });
            (*at, Next::Idle)
        }
        // Items added to the protocol later change nothing until the runner learns them.
        _ => return,
    };
    facts.last_activity = facts.last_activity.max(at);

    let (state, status_line) = match next {
        Next::Working => (
            SessionState::Working,
            facts
                .open_calls
                .last()
                .map(|c| clip(&format!("{}: {}", c.tool, first_line(&c.target)))),
        ),
        Next::Waiting(q) => (SessionState::Waiting, Some(q)),
        Next::Idle => (SessionState::Idle, None),
    };
    if state != facts.state {
        let from = facts.state;
        facts.state = state;
        facts.status_line.clone_from(&status_line);
        if emit_states {
            // The state change goes before the event that caused it, e.g. idle after the turn end
            // is reported with the turn end.
            out.insert(
                start,
                Derived {
                    key,
                    at,
                    body: EventBody::SessionStateChanged {
                        session,
                        from,
                        to: state,
                        status_line,
                    },
                },
            );
        }
    } else {
        facts.status_line = status_line;
    }
}

enum Next {
    Working,
    Waiting(String),
    Idle,
}

fn first_line(s: &str) -> &str {
    s.lines().next().unwrap_or("").trim()
}

fn clip(s: &str) -> String {
    match s.char_indices().nth(MAX_STATUS_CHARS) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_owned(),
    }
}

/// `path` relative to `cwd` when it is inside it; otherwise unchanged.
fn relative_to(path: &str, cwd: Option<&str>) -> String {
    let Some(cwd) = cwd
        .map(|c| c.trim_end_matches(['/', '\\']))
        .filter(|c| !c.is_empty())
    else {
        return path.to_owned();
    };
    match path.strip_prefix(cwd) {
        Some(rest) if rest.starts_with(['/', '\\']) && rest.len() > 1 => rest[1..].to_owned(),
        _ => path.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn use_(offset: u64, id: &str, tool: &str) -> TranscriptItem {
        TranscriptItem::ToolUse {
            at: 1,
            call_id: id.into(),
            tool: tool.into(),
            target: "cargo test".into(),
            input: None,
            offset,
        }
    }

    fn result(offset: u64, id: &str, is_error: bool) -> TranscriptItem {
        TranscriptItem::ToolResult {
            at: 2,
            call_id: id.into(),
            is_error,
            summary: "3 passed\nmore".into(),
            offset,
        }
    }

    fn run(items: &[TranscriptItem]) -> (Facts, Vec<EventBody>) {
        let mut facts = Facts::default();
        let mut out = Vec::new();
        let s = SessionId::new();
        for i in items {
            apply(&mut facts, s, Some("/w/p"), i, true, &mut out);
        }
        (facts, out.into_iter().map(|d| d.body).collect())
    }

    fn kinds(bodies: &[EventBody]) -> Vec<String> {
        bodies
            .iter()
            .map(|b| match b {
                EventBody::SessionStateChanged { to, .. } => format!("state:{to:?}"),
                EventBody::ToolRan { tool, failed, .. } => format!("tool:{tool}:{failed}"),
                EventBody::FileEdited { path, .. } => format!("edit:{path}"),
                EventBody::TurnEnded { .. } => "turn".into(),
                other => format!("{other:?}"),
            })
            .collect()
    }

    #[test]
    fn a_turn_goes_working_then_idle() {
        let (facts, out) = run(&[
            TranscriptItem::UserPrompt {
                at: 1,
                text: "go".into(),
                offset: 0,
            },
            use_(10, "c1", "Bash"),
            result(20, "c1", true),
            TranscriptItem::FileEdit {
                at: 3,
                path: "/w/p/src/a.rs".into(),
                added: 2,
                removed: 1,
                diff: None,
                offset: 30,
            },
            TranscriptItem::TurnEnded { at: 4, offset: 40 },
        ]);
        assert_eq!(
            kinds(&out),
            [
                "state:Working",
                "tool:Bash:true",
                "edit:src/a.rs",
                "state:Idle",
                "turn"
            ]
        );
        assert_eq!(facts.state, SessionState::Idle);
        assert!(facts.open_calls.is_empty());
        assert_eq!(facts.last_activity, 4);
    }

    #[test]
    fn a_question_waits_and_its_answer_resumes_work() {
        let (_, out) = run(&[
            use_(0, "q", "AskUserQuestion"),
            TranscriptItem::Question {
                at: 1,
                text: "Which one?".into(),
                options: vec![],
                offset: 0,
            },
            result(10, "q", false),
        ]);
        assert_eq!(
            kinds(&out),
            [
                "state:Working",
                "state:Waiting",
                "state:Working",
                "tool:AskUserQuestion:false"
            ]
        );
    }

    #[test]
    fn tool_ran_points_at_the_call_and_clips_the_outcome() {
        let (_, out) = run(&[use_(7, "c", "Bash"), result(9, "c", false)]);
        let Some(EventBody::ToolRan {
            outcome, receipt, ..
        }) = out.last()
        else {
            panic!("no tool_ran: {out:?}");
        };
        assert_eq!(outcome, "3 passed");
        assert!(matches!(receipt, Receipt::Transcript { offset: 7, .. }));
    }

    #[test]
    fn states_can_be_folded_silently() {
        let mut facts = Facts::default();
        let mut out = Vec::new();
        apply(
            &mut facts,
            SessionId::new(),
            None,
            &TranscriptItem::TurnEnded { at: 1, offset: 0 },
            false,
            &mut out,
        );
        assert_eq!(facts.state, SessionState::Idle);
        assert_eq!(
            kinds(&out.into_iter().map(|d| d.body).collect::<Vec<_>>()),
            ["turn"]
        );
    }

    #[test]
    fn paths_outside_cwd_stay_absolute() {
        assert_eq!(relative_to("/w/p/a", Some("/w/p/")), "a");
        assert_eq!(relative_to("/w/pq/a", Some("/w/p")), "/w/pq/a");
        assert_eq!(relative_to(r"C:\w\p\a.rs", Some(r"C:\w\p")), "a.rs");
        assert_eq!(relative_to("/w/p/a", None), "/w/p/a");
    }
}
