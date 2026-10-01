//! Field ownership and `plan`: turning an [`UpstreamChange`] into abstract hub [`Intent`]s.
//!
//! Applying intents to the hub is a later brief (stream E's commands); this crate only decides
//! *what* should happen, never writes anything itself.

use crate::change::UpstreamChange;
use crate::state::CloseReason;
use pitcrew_protocol::ids::TaskId;
use pitcrew_protocol::model::{ExternalRef, Mover, Receipt, Task, TaskStatus};

/// Who owns a field PitCrew mirrors from GitHub.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldOwner {
    /// GitHub always wins: sync overwrites it, a hub-side edit is never sent upstream.
    Upstream,
    /// The hub always wins: sync never touches it, even when GitHub disagrees.
    Hub,
    /// Neither side simply overwrites the other: it moves only through mover rules
    /// (`TaskStatus::can_move` with [`Mover::Sync`]), and a disallowed move becomes a conflict ask
    /// instead of being silently dropped.
    Mirrored,
}

/// One row of the field-ownership table.
#[derive(Clone, Copy, Debug)]
pub struct FieldOwnership {
    /// The task field.
    pub field: &'static str,
    /// Who owns it.
    pub owner: FieldOwner,
    /// Why.
    pub note: &'static str,
}

/// The field-ownership table for GitHub issues mirrored as tasks.
pub const ISSUE_FIELD_OWNERSHIP: &[FieldOwnership] = &[
    FieldOwnership {
        field: "title",
        owner: FieldOwner::Upstream,
        note: "The issue title always overwrites the task's title.",
    },
    FieldOwnership {
        field: "body",
        owner: FieldOwner::Upstream,
        note: "The issue body always overwrites the task's description.",
    },
    FieldOwnership {
        field: "labels",
        owner: FieldOwner::Upstream,
        note: "GitHub labels always overwrite the task's labels.",
    },
    FieldOwnership {
        field: "milestone",
        owner: FieldOwner::Upstream,
        note: "The issue's milestone always overwrites the task's linked milestone reference.",
    },
    FieldOwnership {
        field: "status",
        owner: FieldOwner::Mirrored,
        note: "Moved only through TaskStatus::can_move(.., Mover::Sync): an upstream close \
               proposes `done`, a reopen proposes `todo`, and in-progress work is never touched. \
               A disallowed move raises a conflict ask instead of being dropped.",
    },
    FieldOwnership {
        field: "assignee",
        owner: FieldOwner::Hub,
        note: "The hub's assignee is never changed by sync. Upstream assignee changes are still \
               recorded as UpstreamChange::IssueReassigned for visibility, but `plan` emits no \
               intent for them.",
    },
];

/// An abstract hub action, proposed by `plan` from one [`UpstreamChange`]. The hub (a later
/// brief) is responsible for actually applying these.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Intent {
    /// Create a task mirroring a newly seen upstream issue.
    CreateTask {
        /// The issue.
        source: ExternalRef,
        /// Title.
        title: String,
        /// Body.
        body: String,
        /// Labels.
        labels: Vec<String>,
        /// Linked milestone, if any.
        milestone: Option<ExternalRef>,
    },
    /// Update the upstream-owned fields of an existing task. `None` means "unchanged"; for
    /// `milestone`, `Some(None)` means "cleared" and `Some(Some(r))` means "set to `r`".
    UpdateOwnedFields {
        /// The task to update.
        task: TaskId,
        /// New title, if it changed.
        title: Option<String>,
        /// New body, if it changed.
        body: Option<String>,
        /// New label set, if it changed.
        labels: Option<Vec<String>>,
        /// New milestone link, if it changed.
        milestone: Option<Option<ExternalRef>>,
    },
    /// Propose moving a task's status. The hub still checks `TaskStatus::can_move` itself before
    /// applying; `plan` only proposes moves it already believes are allowed.
    ProposeMove {
        /// The task.
        task: TaskId,
        /// The proposed status.
        to: TaskStatus,
        /// Always [`Mover::Sync`] from this crate.
        mover: Mover,
    },
    /// Attach a pull request as a [`Receipt`] on a task it closes.
    AttachReceipt {
        /// The task.
        task: TaskId,
        /// The receipt.
        receipt: Receipt,
    },
    /// Raise something for a person to resolve: an upstream change that cannot be safely applied
    /// automatically.
    ConflictAsk {
        /// The task, if one is known.
        task: Option<TaskId>,
        /// The upstream item.
        source: ExternalRef,
        /// Plain-text reason, shown to the person who answers the ask.
        reason: String,
    },
}

fn no_task_conflict(source: &ExternalRef, what: &str) -> Vec<Intent> {
    vec![Intent::ConflictAsk {
        task: None,
        source: source.clone(),
        reason: format!("upstream {what}, but no linked task was found"),
    }]
}

/// Turns one [`UpstreamChange`] into the hub actions it implies. `current` is the task already
/// linked to the change's source, if the hub has one.
///
/// This is pure: it never touches the network, the store or the event log. The hub applies the
/// returned intents (and, for `ProposeMove`, re-checks `TaskStatus::can_move` before doing so).
#[must_use]
pub fn plan(change: &UpstreamChange, current: Option<&Task>) -> Vec<Intent> {
    use UpstreamChange::{
        IssueBodyEdited, IssueClosed, IssueMilestoned, IssueOpened, IssueReassigned,
        IssueRelabelled, IssueReopened, IssueRetitled, MilestoneClosed, MilestoneCreated,
        MilestoneRenamed, PullRequestClosed, PullRequestMerged, PullRequestOpened,
    };

    match change {
        IssueOpened {
            source,
            title,
            body,
            labels,
            milestone,
            ..
        } => match current {
            None => vec![Intent::CreateTask {
                source: source.clone(),
                title: title.clone(),
                body: body.clone(),
                labels: labels.clone(),
                milestone: milestone.clone(),
            }],
            // Defensive: a task already exists for a source we just saw as "opened" (e.g. it was
            // imported by hand before the first sync). Bring its owned fields into line instead
            // of proposing a duplicate create.
            Some(t) => vec![Intent::UpdateOwnedFields {
                task: t.id,
                title: Some(title.clone()),
                body: Some(body.clone()),
                labels: Some(labels.clone()),
                milestone: Some(milestone.clone()),
            }],
        },

        IssueRetitled { source, title, .. } => match current {
            Some(t) => vec![Intent::UpdateOwnedFields {
                task: t.id,
                title: Some(title.clone()),
                body: None,
                labels: None,
                milestone: None,
            }],
            None => no_task_conflict(source, "retitled an issue"),
        },

        IssueBodyEdited { source, body, .. } => match current {
            Some(t) => vec![Intent::UpdateOwnedFields {
                task: t.id,
                title: None,
                body: Some(body.clone()),
                labels: None,
                milestone: None,
            }],
            None => no_task_conflict(source, "edited an issue's body"),
        },

        IssueRelabelled { source, labels, .. } => match current {
            Some(t) => vec![Intent::UpdateOwnedFields {
                task: t.id,
                title: None,
                body: None,
                labels: Some(labels.clone()),
                milestone: None,
            }],
            None => no_task_conflict(source, "relabelled an issue"),
        },

        IssueMilestoned {
            source, milestone, ..
        } => match current {
            Some(t) => vec![Intent::UpdateOwnedFields {
                task: t.id,
                title: None,
                body: None,
                labels: None,
                milestone: Some(milestone.clone()),
            }],
            None => no_task_conflict(source, "changed an issue's milestone"),
        },

        // The hub owns the assignee (see ISSUE_FIELD_OWNERSHIP): this is recorded upstream for
        // visibility only, and intentionally produces no intent.
        IssueReassigned { .. } => vec![],

        IssueClosed { source, reason, .. } => propose_move_or_conflict(
            current,
            source,
            TaskStatus::Done,
            &format!("closed an issue ({})", close_reason_text(*reason)),
        ),

        IssueReopened { source, .. } => {
            propose_move_or_conflict(current, source, TaskStatus::Todo, "reopened an issue")
        }

        // Milestones map to workstreams, not tasks; `plan`'s signature here only takes a task, so
        // milestone changes currently produce no task intent. See "What I did not do".
        MilestoneCreated { .. } | MilestoneRenamed { .. } | MilestoneClosed { .. } => vec![],

        PullRequestOpened { .. } => vec![],

        PullRequestMerged { source, .. } => match current {
            Some(t) => vec![Intent::AttachReceipt {
                task: t.id,
                receipt: Receipt::PullRequest {
                    url: source.url.clone().unwrap_or_default(),
                },
            }],
            None => no_task_conflict(source, "merged a pull request linked to an issue"),
        },

        // Closed without merging: nothing completed, nothing to attach or move.
        PullRequestClosed { .. } => vec![],
    }
}

fn close_reason_text(reason: CloseReason) -> &'static str {
    match reason {
        CloseReason::Completed => "completed",
        CloseReason::NotPlanned => "not planned",
    }
}

/// Shared logic for `IssueClosed` (→ done) and `IssueReopened` (→ todo): propose the move only
/// when `TaskStatus::can_move` allows it from the task's current status, otherwise raise a
/// conflict instead of silently dropping the change or moving work a person or an agent is still
/// doing.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::change::UpstreamChange;
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
            system: ExternalSystem::Github,
            key: "example-org/demo-repo#1".into(),
            url: None,
        }
    }

    fn at() -> crate::time::GithubTimestamp {
        crate::time::GithubTimestamp::new("2026-01-01T00:00:00Z")
    }

    #[test]
    fn a_close_never_moves_an_in_progress_task() {
        let t = task(TaskStatus::InProgress);
        let change = UpstreamChange::IssueClosed {
            source: external_ref(),
            at: at(),
            reason: CloseReason::Completed,
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
    fn a_close_moves_review_to_done() {
        let t = task(TaskStatus::Review);
        let change = UpstreamChange::IssueClosed {
            source: external_ref(),
            at: at(),
            reason: CloseReason::Completed,
        };
        let intents = plan(&change, Some(&t));
        assert_eq!(
            intents,
            vec![Intent::ProposeMove {
                task: t.id,
                to: TaskStatus::Done,
                mover: Mover::Sync
            }]
        );
    }

    #[test]
    fn a_close_moves_backlog_and_todo_and_canceled_to_done() {
        for status in [TaskStatus::Backlog, TaskStatus::Todo, TaskStatus::Canceled] {
            let t = task(status);
            let change = UpstreamChange::IssueClosed {
                source: external_ref(),
                at: at(),
                reason: CloseReason::NotPlanned,
            };
            let intents = plan(&change, Some(&t));
            assert_eq!(
                intents,
                vec![Intent::ProposeMove {
                    task: t.id,
                    to: TaskStatus::Done,
                    mover: Mover::Sync
                }],
                "status {status:?}"
            );
        }
    }

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
    fn a_title_edit_only_touches_the_title_field() {
        let t = task(TaskStatus::InProgress);
        let change = UpstreamChange::IssueRetitled {
            source: external_ref(),
            at: at(),
            title: "new title".into(),
        };
        let intents = plan(&change, Some(&t));
        assert_eq!(
            intents,
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
    fn a_change_with_no_linked_task_is_a_conflict() {
        let change = UpstreamChange::IssueRetitled {
            source: external_ref(),
            at: at(),
            title: "t".into(),
        };
        let intents = plan(&change, None);
        assert!(matches!(
            intents.as_slice(),
            [Intent::ConflictAsk { task: None, .. }]
        ));
    }

    #[test]
    fn reassignment_never_produces_an_intent() {
        let t = task(TaskStatus::Todo);
        let change = UpstreamChange::IssueReassigned {
            source: external_ref(),
            at: at(),
            assignees: vec!["octo-demo".into()],
        };
        assert!(plan(&change, Some(&t)).is_empty());
        assert!(plan(&change, None).is_empty());
    }

    #[test]
    fn a_merged_pull_request_attaches_a_receipt() {
        let t = task(TaskStatus::InProgress);
        let source = ExternalRef {
            system: ExternalSystem::Github,
            key: "example-org/demo-repo#7".into(),
            url: Some("https://github.com/example-org/demo-repo/pull/7".into()),
        };
        let change = UpstreamChange::PullRequestMerged {
            source: source.clone(),
            at: at(),
            closes: vec![],
        };
        assert_eq!(
            plan(&change, Some(&t)),
            vec![Intent::AttachReceipt {
                task: t.id,
                receipt: Receipt::PullRequest {
                    url: source.url.unwrap()
                }
            }]
        );
    }
}
