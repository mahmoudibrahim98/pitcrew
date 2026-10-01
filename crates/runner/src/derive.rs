//! Session state and events, derived from transcript items and reported states (hooks, the
//! runtime). Pure: no I/O.

use pitcrew_interfaces::source::TranscriptItem;
use pitcrew_protocol::events::EventBody;
use pitcrew_protocol::ids::{SessionId, WorkstreamId};
use pitcrew_protocol::model::{LinkBasis, Receipt, SessionState, TimestampMs};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

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
    /// When a hook (or the runtime) last reported the state. Transcript items up to this time
    /// were written before the report, so they do not move the state back.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reported_at: Option<TimestampMs>,
    /// Reports applied so far; they make the ids of reported events unique.
    #[serde(default)]
    pub reports: u32,
    /// The workstream the runner linked the session to, by folder or branch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub linked: Option<Linked>,
    /// Links emitted so far; they make the ids of link events unique.
    #[serde(default)]
    pub links: u32,
}

impl Default for Facts {
    fn default() -> Self {
        Self {
            state: SessionState::Starting,
            status_line: None,
            open_calls: Vec::new(),
            last_activity: 0,
            reported_at: None,
            reports: 0,
            linked: None,
            links: 0,
        }
    }
}

/// A link the runner made.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Linked {
    pub workstream: WorkstreamId,
    pub basis: LinkBasis,
}

/// A tool call without a result yet.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct OpenCall {
    pub call_id: String,
    pub tool: String,
    pub target: String,
    pub offset: u64,
}

/// An event body, with the key of the item it came from and when it happened.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Derived {
    pub key: u64,
    pub at: TimestampMs,
    pub body: EventBody,
}

/// What [`apply`] needs to know about the session.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Ctx<'a> {
    pub session: SessionId,
    pub cwd: Option<&'a str>,
    /// With `false`, state changes are folded but not emitted (the discovery read reports its
    /// final state in `session_discovered` instead).
    pub emit_states: bool,
}

/// Identifies an item within one reading of a transcript: its offset, its kind, and its own id
/// (a call id, a path, or its text).
///
/// An offset alone is not enough: one JSONL record gives several items, and OpenCode's offsets
/// are positions that a call and its result share. Identical items at one offset get the same
/// key; [`nth`] tells them apart.
pub(crate) fn item_key(item: &TranscriptItem) -> u64 {
    let (tag, id): (u8, &str) = match item {
        TranscriptItem::UserPrompt { text, .. } => (1, text),
        TranscriptItem::AssistantText { text, .. } => (2, text),
        TranscriptItem::ToolUse { call_id, .. } => (3, call_id),
        TranscriptItem::ToolResult { call_id, .. } => (4, call_id),
        TranscriptItem::FileEdit { path, .. } => (5, path),
        TranscriptItem::PlanUpdated { .. } => (6, ""),
        TranscriptItem::Question { text, .. } => (7, text),
        TranscriptItem::TurnEnded { .. } => (8, ""),
        _ => (0, ""),
    };
    let mut h = Sha256::new();
    h.update(item.offset().to_le_bytes());
    h.update([tag]);
    h.update(u64::try_from(id.len()).unwrap_or(u64::MAX).to_le_bytes());
    h.update(id.as_bytes());
    first_u64(&h.finalize())
}

/// The key of the `n`-th repeat of an identical item.
pub(crate) fn nth(key: u64, n: u32) -> u64 {
    let mut h = Sha256::new();
    h.update(key.to_le_bytes());
    h.update(n.to_le_bytes());
    first_u64(&h.finalize())
}

fn first_u64(digest: &[u8]) -> u64 {
    let mut b = [0u8; 8];
    b.copy_from_slice(&digest[..8]);
    u64::from_le_bytes(b)
}

/// Folds one item into `facts`, appending the events it causes to `out`.
pub(crate) fn apply(
    facts: &mut Facts,
    ctx: &Ctx<'_>,
    item: &TranscriptItem,
    key: u64,
    out: &mut Vec<Derived>,
) {
    let session = ctx.session;
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
            offset,
            ..
        } => {
            if facts.open_calls.len() >= MAX_OPEN_CALLS {
                facts.open_calls.remove(0);
            }
            facts.open_calls.push(OpenCall {
                call_id: call_id.clone(),
                tool: tool.clone(),
                target: target.clone(),
                offset: *offset,
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
            offset,
            ..
        } => {
            out.push(Derived {
                key,
                at: *at,
                body: EventBody::FileEdited {
                    session,
                    path: relative_to(path, ctx.cwd),
                    added: *added,
                    removed: *removed,
                    receipt: Some(Receipt::Transcript {
                        session,
                        offset: *offset,
                    }),
                },
            });
            (*at, Next::Working)
        }
        TranscriptItem::Question { at, text, .. } => (*at, Next::Waiting(clip(first_line(text)))),
        TranscriptItem::TurnEnded { at, offset } => {
            facts.open_calls.clear();
            out.push(Derived {
                key,
                at: *at,
                body: EventBody::TurnEnded {
                    session,
                    receipt: Receipt::Transcript {
                        session,
                        offset: *offset,
                    },
                },
            });
            (*at, Next::Idle)
        }
        // Items added to the protocol later change nothing until the runner learns them.
        _ => return,
    };
    facts.last_activity = facts.last_activity.max(at);
    // A hook reported a state after this item was written (a CLI fires its hooks after writing,
    // and its transcript may be read later): the report is newer, so it stands.
    if facts.reported_at.is_some_and(|r| at <= r) {
        return;
    }

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
        if ctx.emit_states {
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

/// A state reported outside the transcript: by an agent hook, or by the runtime when it ended a
/// session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Reported {
    /// When it was reported.
    pub at: TimestampMs,
    pub to: SessionState,
    pub status_line: Option<String>,
}

/// Applies a reported state. Returns the state it replaced, if it changed anything.
///
/// A report older than the newest transcript item or report is stale and changes nothing, so a
/// late hook never moves a session back. A report that repeats the current state (a `Stop` hook
/// after the transcript's turn end) changes nothing either.
pub(crate) fn report(facts: &mut Facts, r: &Reported) -> Option<SessionState> {
    if r.at < facts.last_activity || facts.reported_at.is_some_and(|p| r.at < p) {
        return None;
    }
    facts.reported_at = Some(r.at);
    if r.to == facts.state {
        if r.status_line.is_some() {
            facts.status_line.clone_from(&r.status_line);
        }
        return None;
    }
    let from = facts.state;
    facts.state = r.to;
    facts.status_line = r.status_line.as_deref().map(|s| clip(first_line(s)));
    Some(from)
}

/// The events a report that changed the state from `from` causes.
pub(crate) fn reported_events(
    session: SessionId,
    from: SessionState,
    facts: &Facts,
) -> Vec<EventBody> {
    let mut out = vec![EventBody::SessionStateChanged {
        session,
        from,
        to: facts.state,
        status_line: facts.status_line.clone(),
    }];
    if facts.state == SessionState::Ended {
        out.push(EventBody::SessionEnded { session });
    }
    out
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

    fn run_with(facts: &mut Facts, items: &[TranscriptItem]) -> Vec<Derived> {
        let mut out = Vec::new();
        let ctx = Ctx {
            session: SessionId::new(),
            cwd: Some("/w/p"),
            emit_states: true,
        };
        for i in items {
            apply(facts, &ctx, i, item_key(i), &mut out);
        }
        out
    }

    fn run(items: &[TranscriptItem]) -> (Facts, Vec<EventBody>) {
        let mut facts = Facts::default();
        let out = run_with(&mut facts, items);
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
                EventBody::SessionEnded { .. } => "ended".into(),
                other => format!("{other:?}"),
            })
            .collect()
    }

    fn edit(offset: u64) -> TranscriptItem {
        TranscriptItem::FileEdit {
            at: 3,
            path: "/w/p/src/a.rs".into(),
            added: 2,
            removed: 1,
            diff: None,
            offset,
        }
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
            edit(30),
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
    fn a_file_edit_has_a_transcript_receipt() {
        let (_, out) = run(&[edit(30)]);
        let Some(EventBody::FileEdited {
            receipt, session, ..
        }) = out.last()
        else {
            panic!("no file_edited: {out:?}");
        };
        assert_eq!(
            receipt,
            &Some(Receipt::Transcript {
                session: *session,
                offset: 30
            })
        );
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
        let item = TranscriptItem::TurnEnded { at: 1, offset: 0 };
        let ctx = Ctx {
            session: SessionId::new(),
            cwd: None,
            emit_states: false,
        };
        apply(&mut facts, &ctx, &item, item_key(&item), &mut out);
        assert_eq!(facts.state, SessionState::Idle);
        assert_eq!(
            kinds(&out.into_iter().map(|d| d.body).collect::<Vec<_>>()),
            ["turn"]
        );
    }

    /// OpenCode: offsets are positions. A call and its result share one, and a result can come
    /// in a later read, after items with higher offsets.
    #[test]
    fn a_shared_offset_and_a_late_result_keep_their_events_and_distinct_keys() {
        let call = use_(100, "c", "Bash");
        let res = result(100, "c", false);
        assert_ne!(item_key(&call), item_key(&res));
        assert_ne!(
            item_key(&use_(100, "a", "Bash")),
            item_key(&use_(100, "b", "Bash"))
        );
        assert_ne!(item_key(&call), nth(item_key(&call), 1));

        let mut facts = Facts::default();
        let first = run_with(
            &mut facts,
            &[
                call,
                TranscriptItem::AssistantText {
                    at: 1,
                    text: "meanwhile".into(),
                    offset: 200,
                },
            ],
        );
        let late = run_with(&mut facts, std::slice::from_ref(&res));
        assert_eq!(kinds(&[first[0].body.clone()]), ["state:Working"]);
        assert_eq!(late.len(), 1, "{late:?}");
        assert_eq!(late[0].key, item_key(&res));
        assert!(matches!(
            &late[0].body,
            EventBody::ToolRan {
                receipt: Receipt::Transcript { offset: 100, .. },
                ..
            }
        ));
    }

    fn reported(at: TimestampMs, to: SessionState) -> Reported {
        Reported {
            at,
            to,
            status_line: None,
        }
    }

    #[test]
    fn a_report_is_applied_once_and_items_written_before_it_do_not_undo_it() {
        let mut facts = Facts::default();
        run_with(&mut facts, &[use_(0, "c", "Bash")]);
        assert_eq!(facts.state, SessionState::Working);

        // The Stop hook arrives before the watcher reads the end of the turn.
        let from = report(&mut facts, &reported(10, SessionState::Idle));
        assert_eq!(from, Some(SessionState::Working));
        assert_eq!(
            kinds(&reported_events(
                SessionId::new(),
                SessionState::Working,
                &facts
            )),
            ["state:Idle"]
        );

        // The transcript catches up: its items are older than the hook. The tool still ran and
        // the turn still ended, but the state is not changed or reported again.
        let out = run_with(
            &mut facts,
            &[
                result(5, "c", false),
                TranscriptItem::TurnEnded { at: 9, offset: 6 },
            ],
        );
        assert_eq!(
            kinds(&out.into_iter().map(|d| d.body).collect::<Vec<_>>()),
            ["tool:Bash:false", "turn"]
        );
        assert_eq!(facts.state, SessionState::Idle);

        // A new prompt after the hook moves it again.
        let out = run_with(
            &mut facts,
            &[TranscriptItem::UserPrompt {
                at: 11,
                text: "next".into(),
                offset: 7,
            }],
        );
        assert_eq!(
            kinds(&out.into_iter().map(|d| d.body).collect::<Vec<_>>()),
            ["state:Working"]
        );
    }

    #[test]
    fn a_stale_report_changes_nothing() {
        let mut facts = Facts::default();
        run_with(
            &mut facts,
            &[TranscriptItem::UserPrompt {
                at: 50,
                text: "go".into(),
                offset: 0,
            }],
        );
        // A Stop hook from before the newest item: the session has moved on.
        assert_eq!(report(&mut facts, &reported(40, SessionState::Idle)), None);
        assert_eq!(facts.state, SessionState::Working);
        // Reports never go back behind an earlier report either.
        assert!(report(&mut facts, &reported(60, SessionState::Ended)).is_some());
        assert_eq!(report(&mut facts, &reported(55, SessionState::Idle)), None);
        assert_eq!(facts.state, SessionState::Ended);
        assert_eq!(
            kinds(&reported_events(
                SessionId::new(),
                SessionState::Idle,
                &facts
            )),
            ["state:Ended", "ended"]
        );
        // A repeat of the current state is not a change.
        assert_eq!(report(&mut facts, &reported(70, SessionState::Ended)), None);
    }

    #[test]
    fn paths_outside_cwd_stay_absolute() {
        assert_eq!(relative_to("/w/p/a", Some("/w/p/")), "a");
        assert_eq!(relative_to("/w/pq/a", Some("/w/p")), "/w/pq/a");
        assert_eq!(relative_to(r"C:\w\p\a.rs", Some(r"C:\w\p")), "a.rs");
        assert_eq!(relative_to("/w/p/a", None), "/w/p/a");
    }
}
