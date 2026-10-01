//! Applying the office's actions. The office never writes the store itself: the caller (stream
//! E's command layer) implements [`Commands`], which validates and appends, and [`apply`] hands it
//! the emitted actions.

use crate::action::{Action, AskDraft, Entry, Outcome, Refusal};
use crate::guard;
use pitcrew_protocol::events::EventBody;
use pitcrew_protocol::model::Receipt;
use pitcrew_recap::BriefProposal;

/// What the office may ask the hub to do. Each call appends events authored by the back office's
/// member; implementations still validate as for any caller (e.g. `TaskStatus::can_move`). None
/// of them reaches anything outside PitCrew.
pub trait Commands {
    /// Why a command failed.
    type Error;

    /// Appends an event, e.g. a task move with `Mover::BackOffice`. `because` is its evidence.
    ///
    /// # Errors
    ///
    /// When the hub rejects or cannot append it.
    fn append(&mut self, body: &EventBody, because: &[Receipt]) -> Result<(), Self::Error>;

    /// Raises an ask from the back office.
    ///
    /// # Errors
    ///
    /// When the hub rejects or cannot raise it.
    fn raise_ask(&mut self, ask: &AskDraft) -> Result<(), Self::Error>;

    /// Appends a brief proposal ([`BriefProposal::body`]), and for an automatic acceptance also
    /// [`BriefProposal::accepted_body`].
    ///
    /// # Errors
    ///
    /// When the hub rejects or cannot append it.
    fn propose_brief(&mut self, proposal: &BriefProposal) -> Result<(), Self::Error>;
}

/// Why an emitted action was not applied.
#[derive(Debug, PartialEq, Eq)]
pub enum ApplyError<E> {
    /// It failed the checks that need no state (it was not made by an office, or was altered).
    Refused(Refusal),
    /// The command failed.
    Failed(E),
}

/// Applies the emitted entries, in order, and returns each one's result. Capped and refused
/// entries are skipped: they are only logged. Each action's shape is checked again before it is
/// applied, so a hand-made entry cannot get past the "never" list either.
pub fn apply<C: Commands>(
    entries: &[Entry],
    commands: &mut C,
) -> Vec<Result<(), ApplyError<C::Error>>> {
    entries
        .iter()
        .filter(|e| e.outcome == Outcome::Emitted)
        .map(|e| {
            guard::check_shape(&e.action).map_err(ApplyError::Refused)?;
            match &e.action {
                Action::Append { body, because } => commands.append(body, because),
                Action::RaiseAsk { ask } => commands.raise_ask(ask),
                Action::ProposeBrief { proposal } => commands.propose_brief(proposal),
            }
            .map_err(ApplyError::Failed)
        })
        .collect()
}
