//! Field ownership and `plan`: turning an [`UpstreamChange`] into abstract hub [`Intent`]s.
//!
//! The [`Intent`] type itself — and [`FieldOwner`]/[`FieldOwnership`], the shape of one row of a
//! field-ownership table — are reused directly from `pitcrew_sync_github::ownership` rather than
//! redefined: both are already generic over the hub's own model (`TaskId`, `ExternalRef`,
//! `Mover`, …), carrying nothing GitHub-specific, so the hub that eventually applies these can
//! treat a GitHub-sourced and a Jira-sourced intent identically. Its `milestone` field is reused
//! here to mean "the linked epic" — the Jira analogue of a GitHub milestone (see
//! [`ISSUE_FIELD_OWNERSHIP`]).
//!
//! Applying intents to the hub is a later brief; this crate only decides *what* should happen,
//! never writes anything itself.

use crate::change::UpstreamChange;
use pitcrew_protocol::ids::TaskId;
use pitcrew_protocol::model::{ExternalRef, Mover, Task, TaskStatus};
pub use pitcrew_sync_github::ownership::{FieldOwner, FieldOwnership, Intent};

/// The field-ownership table for Jira issues mirrored as tasks.
pub const ISSUE_FIELD_OWNERSHIP: &[FieldOwnership] = &[
    FieldOwnership {
        field: "title",
        owner: FieldOwner::Upstream,
        note: "The issue summary always overwrites the task's title.",
    },
    FieldOwnership {
        field: "body",
        owner: FieldOwner::Upstream,
        note: "The issue description always overwrites the task's description (ADF converted to \
               plain text for Cloud; already plain text for Data Center).",
    },
    FieldOwnership {
        field: "labels",
        owner: FieldOwner::Upstream,
        note: "Jira labels always overwrite the task's labels.",
    },
    FieldOwnership {
        field: "milestone",
        owner: FieldOwner::Upstream,
        note: "The issue's epic (fields.parent, or the configured epic-link custom field on Data \
               Center) always overwrites the task's linked workstream reference.",
    },
    FieldOwnership {
        field: "status",
        owner: FieldOwner::Mirrored,
        note: "Moved only through TaskStatus::can_move(.., Mover::Sync): IssueDone proposes \
               `done`; IssueReopened proposes `todo`, unconditionally — not gated on the hub's \
               own current status, the same way GitHub's IssueReopened is unconditional — so a \
               disallowed move (the task is in progress, or has no linked task at all) still \
               raises a conflict ask instead of being silently dropped. A move between `new` and \
               `indeterminate` is not reported upstream at all, so it never reaches `plan`.",
    },
    FieldOwnership {
        field: "assignee",
        owner: FieldOwner::Hub,
        note: "The hub's assignee is never changed by sync. Upstream assignee changes are still \
               recorded as UpstreamChange::IssueReassigned for visibility, but `plan` emits no \
               intent for them.",
    },
];

fn no_task_conflict(source: &ExternalRef, what: &str) -> Vec<Intent> {
    vec![Intent::ConflictAsk {
        task: None,
        source: source.clone(),
        reason: format!("upstream {what}, but no linked task was found"),
    }]
}

/// Shared logic for a move into `done` and a move back out of it: propose the move only when
/// `TaskStatus::can_move` allows it from the task's current status, otherwise raise a conflict
/// instead of silently dropping the change or moving work a person or an agent is still doing.
fn propose_move_or_conflict(
    current: Option<&Task>,
    source: &ExternalRef,
    to: TaskStatus,
    what: &str,
) -> Vec<Intent> {
    match current {
        Some(t) if t.status.can_move(to, Mover::Sync) => {
            vec![Intent::ProposeMove {
                task: t.id,
                to,
                mover: Mover::Sync,
            }]
        }
        Some(t) => vec![Intent::ConflictAsk {
            task: Some(t.id),
            source: source.clone(),
            reason: format!(
                "upstream {what}, but the task is {:?} and a sync cannot move it to {:?}",
                t.status, to
            ),
        }],
        None => no_task_conflict(source, what),
    }
}

fn update_or_conflict(
    current: Option<&Task>,
    source: &ExternalRef,
    what: &str,
    build: impl FnOnce(TaskId) -> Intent,
) -> Vec<Intent> {
    match current {
        Some(t) => vec![build(t.id)],
        None => no_task_conflict(source, what),
    }
}

/// Turns one [`UpstreamChange`] into the hub actions it implies. `current` is the task already
/// linked to the change's source, if the hub has one.
///
/// This is pure: it never touches the network, the store or the event log. The hub applies the
/// returned intents (and, for `ProposeMove`, re-checks `TaskStatus::can_move` before doing so).
#[must_use]
pub fn plan(change: &UpstreamChange, current: Option<&Task>) -> Vec<Intent> {
    use UpstreamChange::{
        EpicClosed, EpicCreated, EpicRenamed, IssueBodyEdited, IssueCreated, IssueDone,
        IssueReassigned, IssueRelabelled, IssueReopened, IssueReparented, IssueRetitled,
    };

    match change {
        IssueCreated {
            source,
            title,
            body,
            labels,
            epic,
            ..
        } => match current {
            None => vec![Intent::CreateTask {
                source: source.clone(),
                title: title.clone(),
                body: body.clone(),
                labels: labels.clone(),
                milestone: epic.clone(),
            }],
            // Defensive: a task already exists for a source we just saw as "created" (e.g. it was
            // imported by hand before the first sync). Bring its owned fields into line instead
            // of proposing a duplicate create.
            Some(t) => vec![Intent::UpdateOwnedFields {
                task: t.id,
                title: Some(title.clone()),
                body: Some(body.clone()),
                labels: Some(labels.clone()),
                milestone: Some(epic.clone()),
            }],
        },

        IssueRetitled { source, title, .. } => {
            update_or_conflict(current, source, "retitled an issue", |task| {
                Intent::UpdateOwnedFields {
                    task,
                    title: Some(title.clone()),
                    body: None,
                    labels: None,
                    milestone: None,
                }
            })
        }

        IssueBodyEdited { source, body, .. } => {
            update_or_conflict(current, source, "edited an issue's description", |task| {
                Intent::UpdateOwnedFields {
                    task,
                    title: None,
                    body: Some(body.clone()),
                    labels: None,
                    milestone: None,
                }
            })
        }

        IssueRelabelled { source, labels, .. } => {
            update_or_conflict(current, source, "relabelled an issue", |task| {
                Intent::UpdateOwnedFields {
                    task,
                    title: None,
                    body: None,
                    labels: Some(labels.clone()),
                    milestone: None,
                }
            })
        }

        IssueReparented { source, epic, .. } => {
            update_or_conflict(current, source, "changed an issue's epic", |task| {
                Intent::UpdateOwnedFields {
                    task,
                    title: None,
                    body: None,
                    labels: None,
                    milestone: Some(epic.clone()),
                }
            })
        }

        // The hub owns the assignee (see ISSUE_FIELD_OWNERSHIP): this is recorded upstream for
        // visibility only, and intentionally produces no intent.
        IssueReassigned { .. } => vec![],

        IssueDone {
            source, resolution, ..
        } => propose_move_or_conflict(
            current,
            source,
            TaskStatus::Done,
            &format!(
                "moved to a done status{}",
                resolution
                    .as_deref()
                    .map(|r| format!(" ({r})"))
                    .unwrap_or_default()
            ),
        ),

        // Unconditional — not gated on `current`'s own status — the same way GitHub's
        // IssueReopened is: the change already means a genuine done-to-not-done transition (see
        // its doc comment), so whatever the hub currently shows, `propose_move_or_conflict`
        // decides whether that is a legal move or a conflict worth raising.
        IssueReopened { source, .. } => {
            propose_move_or_conflict(current, source, TaskStatus::Todo, "reopened an issue")
        }

        // Epics map to workstreams, not tasks; `plan`'s signature here only takes a task, so epic
        // changes currently produce no task intent. See "What I did not do".
        EpicCreated { .. } | EpicRenamed { .. } | EpicClosed { .. } => vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::time::JiraTimestamp;
    use pitcrew_protocol::ids::{ProjectId, ProjectKey, TaskKey};
    use pitcrew_protocol::model::{ExternalSystem, Priority};

    fn task(status: TaskStatus) -> Task {
        Task {
            id: TaskId::new(),
            key: TaskKey::new(ProjectKey::new("CMP").unwrap(), 1).unwrap(),
            project: ProjectId::new(),
            workstream: None,
            title: "old title".into(),
            description: String::new(),
            status,
            priority: Priority::None,
            assignee: None,
            labels: vec![],
            start: None,
            due: None,
            blocked_by: vec![],
            source: None,
            accept_auto: false,
            subtasks: vec![],
        }
    }

    fn external_ref() -> ExternalRef {
        ExternalRef {
            system: ExternalSystem::Jira,
            key: "DEMO-1".into(),
            url: None,
        }
    }

    fn at() -> JiraTimestamp {
        JiraTimestamp::new("2026-01-01T00:00:00.000+0000")
    }

    #[test]
    fn a_move_to_done_never_moves_an_in_progress_task() {
        let t = task(TaskStatus::InProgress);
        let change = UpstreamChange::IssueDone {
            source: external_ref(),
            at: at(),
            resolution: Some("Done".into()),
        };
        let intents = plan(&change, Some(&t));
        assert!(
            !intents
                .iter()
                .any(|i| matches!(i, Intent::ProposeMove { .. }))
        );
        assert!(
            matches!(intents.as_slice(), [Intent::ConflictAsk { task: Some(id), .. }] if *id == t.id)
        );
    }

    #[test]
    fn a_move_to_done_moves_review_to_done() {
        let t = task(TaskStatus::Review);
        let change = UpstreamChange::IssueDone {
            source: external_ref(),
            at: at(),
            resolution: None,
        };
        assert_eq!(
            plan(&change, Some(&t)),
            vec![Intent::ProposeMove {
                task: t.id,
                to: TaskStatus::Done,
                mover: Mover::Sync
            }]
        );
    }

    /// Mirrors `pitcrew_sync_github::ownership::tests::a_reopen_moves_done_to_todo_but_not_other_statuses`
    /// exactly: `IssueReopened` is unconditional in `plan`, so a non-`Done` task is a conflict
    /// ask (through the disallowed-move branch of `propose_move_or_conflict`), not silently
    /// nothing.
    #[test]
    fn a_reopen_moves_done_to_todo_but_not_other_statuses() {
        let done = task(TaskStatus::Done);
        let change = UpstreamChange::IssueReopened {
            source: external_ref(),
            at: at(),
        };
        assert_eq!(
            plan(&change, Some(&done)),
            vec![Intent::ProposeMove {
                task: done.id,
                to: TaskStatus::Todo,
                mover: Mover::Sync
            }]
        );

        let in_progress = task(TaskStatus::InProgress);
        let intents = plan(&change, Some(&in_progress));
        assert!(matches!(intents.as_slice(), [Intent::ConflictAsk { .. }]));
    }

    #[test]
    fn a_reopen_with_no_linked_task_is_a_conflict() {
        let change = UpstreamChange::IssueReopened {
            source: external_ref(),
            at: at(),
        };
        assert!(matches!(
            plan(&change, None).as_slice(),
            [Intent::ConflictAsk { task: None, .. }]
        ));
    }

    #[test]
    fn a_title_edit_only_touches_the_title_field() {
        let t = task(TaskStatus::InProgress);
        let change = UpstreamChange::IssueRetitled {
            source: external_ref(),
            at: at(),
            title: "new title".into(),
        };
        assert_eq!(
            plan(&change, Some(&t)),
            vec![Intent::UpdateOwnedFields {
                task: t.id,
                title: Some("new title".into()),
                body: None,
                labels: None,
                milestone: None,
            }]
        );
    }

    #[test]
    fn a_re_parent_updates_only_the_milestone_field() {
        let t = task(TaskStatus::Todo);
        let epic = ExternalRef {
            system: ExternalSystem::Jira,
            key: "DEMO-9".into(),
            url: Some("https://jira.example.com/browse/DEMO-9".into()),
        };
        let change = UpstreamChange::IssueReparented {
            source: external_ref(),
            at: at(),
            epic: Some(epic.clone()),
        };
        assert_eq!(
            plan(&change, Some(&t)),
            vec![Intent::UpdateOwnedFields {
                task: t.id,
                title: None,
                body: None,
                labels: None,
                milestone: Some(Some(epic)),
            }]
        );
    }

    #[test]
    fn a_change_with_no_linked_task_is_a_conflict() {
        let change = UpstreamChange::IssueRetitled {
            source: external_ref(),
            at: at(),
            title: "t".into(),
        };
        assert!(matches!(
            plan(&change, None).as_slice(),
            [Intent::ConflictAsk { task: None, .. }]
        ));
    }

    #[test]
    fn reassignment_never_produces_an_intent() {
        let t = task(TaskStatus::Todo);
        let change = UpstreamChange::IssueReassigned {
            source: external_ref(),
            at: at(),
            assignee: Some("demo.user".into()),
        };
        assert!(plan(&change, Some(&t)).is_empty());
        assert!(plan(&change, None).is_empty());
    }

    #[test]
    fn epic_changes_produce_no_task_intent() {
        let source = external_ref();
        for change in [
            UpstreamChange::EpicCreated {
                source: source.clone(),
                at: at(),
                title: "Epic".into(),
            },
            UpstreamChange::EpicRenamed {
                source: source.clone(),
                at: at(),
                title: "Renamed".into(),
            },
            UpstreamChange::EpicClosed { source, at: at() },
        ] {
            assert!(plan(&change, None).is_empty());
        }
    }
}
