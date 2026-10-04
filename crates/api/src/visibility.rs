//! One visibility check for activity and replay/live delivery. Sources remain gap-free.
use crate::source::SourceError;
use pitcrew_protocol::events::{Event, EventBody};
use pitcrew_protocol::ids::MemberId;
use std::sync::Arc;

/// Hub-specific inclusion, evaluated on the blocking pool.
pub trait EventVisibility: Send + Sync + std::fmt::Debug + 'static {
    /// Whether an event's sessions are included.
    /// # Errors
    /// Inclusion metadata could not be read; callers fail closed.
    fn includes(&self, event: &Event) -> Result<bool, SourceError>;
}

/// Cursor privacy plus the hub's reversible session choice.
#[derive(Clone, Debug, Default)]
pub struct Visibility(pub Option<Arc<dyn EventVisibility>>);

impl Visibility {
    /// `person` is present for streams; activity omits every cursor write.
    /// # Errors
    /// Inclusion metadata could not be read.
    pub fn visible(&self, event: &Event, person: Option<MemberId>) -> Result<bool, SourceError> {
        if matches!(event.body, EventBody::CursorMoved { .. }) {
            return Ok(person == Some(event.author));
        }
        self.0.as_ref().map_or(Ok(true), |v| v.includes(event))
    }
}

#[cfg(test)]
#[derive(Debug)]
pub(crate) struct ExcludeSession(pub pitcrew_protocol::ids::SessionId);
#[cfg(test)]
impl EventVisibility for ExcludeSession {
    fn includes(&self, event: &Event) -> Result<bool, SourceError> {
        Ok(!matches!(event.body, EventBody::SessionEnded { session } if session == self.0))
    }
}
