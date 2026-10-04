//! Hub session inclusion for the API's shared cursor/session visibility check.
use pitcrew_hub_work::WorkService;
use pitcrew_protocol::events::Event;
use std::sync::Arc;
#[derive(Debug)]
pub struct WorkVisibility(pub Arc<WorkService>);
impl pitcrew_api::visibility::EventVisibility for WorkVisibility {
    fn includes(&self, event: &Event) -> Result<bool, pitcrew_api::source::SourceError> {
        self.0.event_included(event).map_err(Into::into)
    }
}
