//! The hard "never" list, checked in code for every action of every rule before it can be
//! emitted, whatever the rule or the events say:
//!
//! - **never send anything outward**: the office appends only internal events (task moves,
//!   comments, answers to agents) and never raises an approval ask, which is how outward writes
//!   to GitHub or Jira are requested. Nothing in [`Action`] reaches an external service;
//! - **never mark a task done unless it allows automatic acceptance**, as the office knows it
//!   from the log (where acceptance, once off, stays off);
//! - **never answer an ask addressed to a person**, nor a decision or an approval.
//!
//! Every move must also pass `TaskStatus::can_move` for `Mover::BackOffice` from the status the
//! task is in, and every action must cite evidence.

use crate::action::{Action, Refusal};
use crate::world::World;
use pitcrew_protocol::events::EventBody;
use pitcrew_protocol::model::{AskKind, Mover, TaskStatus};

/// The checks that need nothing but the action. [`crate::apply`] repeats them.
pub(crate) fn check_shape(action: &Action) -> Result<(), Refusal> {
    if action.receipts().is_empty() {
        return Err(Refusal::NoEvidence);
    }
    match action {
        Action::Append { body, .. } => match body {
            EventBody::TaskMoved { .. } | EventBody::AskAnswered { .. } => Ok(()),
            EventBody::CommentPosted {
                task, workstream, ..
            } if task.is_some() || workstream.is_some() => Ok(()),
            _ => Err(Refusal::NotAllowed),
        },
        Action::RaiseAsk { ask } if ask.kind == AskKind::Approval => Err(Refusal::SendsOutward),
        Action::RaiseAsk { .. } | Action::ProposeBrief { .. } => Ok(()),
    }
}

/// Every check, against what the office knows now.
pub(crate) fn check(action: &Action, world: &World) -> Result<(), Refusal> {
    check_shape(action)?;
    let Action::Append { body, .. } = action else {
        return Ok(());
    };
    match body {
        EventBody::TaskMoved {
            task,
            from,
            to,
            mover,
        } => {
            let info = world.task(*task).ok_or(Refusal::UnknownTask)?;
            if *to == TaskStatus::Done && !info.accept_auto {
                return Err(Refusal::MarksDone);
            }
            let ours = Mover::BackOffice {
                accept_auto: info.accept_auto,
            };
            if *mover != ours || *from != info.status || !from.can_move(*to, ours) {
                return Err(Refusal::MoveNotAllowed);
            }
            Ok(())
        }
        EventBody::AskAnswered { ask, .. } => {
            let open = world.ask(*ask).ok_or(Refusal::AnswersPerson)?;
            let a_persons = matches!(open.kind, AskKind::Decision | AskKind::Approval);
            if a_persons || !world.is_agent(open.to) {
                return Err(Refusal::AnswersPerson);
            }
            Ok(())
        }
        _ => Ok(()),
    }
}
