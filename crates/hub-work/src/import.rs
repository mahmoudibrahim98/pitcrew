//! Hub inclusion state; transcripts and the underlying projections remain intact.
use crate::error::Result;
use crate::query::{self, SessionFilter};
use crate::{WorkError, WorkService};
use pitcrew_protocol::events::Event;
use pitcrew_protocol::ids::SessionId;
use pitcrew_protocol::import::{ImportChoice, ImportFilter, ImportMode};
use serde_json::Value;
use std::path::PathBuf;
use std::sync::PoisonError;

#[derive(Debug, Default)]
pub(crate) struct ImportState {
    pub choice: ImportChoice,
    pub file: Option<PathBuf>,
}

impl WorkService {
    /// Loads the durable choice before serving any requests.
    /// # Errors
    /// Unreadable or invalid persisted state.
    pub fn with_import_file(self, file: PathBuf) -> Result<Self> {
        let choice = match std::fs::read(&file) {
            Ok(bytes) => serde_json::from_slice::<ImportChoice>(&bytes)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => ImportChoice::default(),
            Err(e) => return Err(WorkError::internal(format!("reading import choice: {e}"))),
        };
        choice.filter.validate().map_err(WorkError::internal)?;
        *self.import.lock().unwrap_or_else(PoisonError::into_inner) = ImportState {
            choice,
            file: Some(file),
        };
        Ok(self)
    }

    /// The current reversible choice.
    #[must_use]
    pub fn import_choice(&self) -> ImportChoice {
        self.import
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .choice
            .clone()
    }

    /// Counts indexed sessions, without changing the choice.
    /// # Errors
    /// Invalid filter or database errors.
    pub fn import_dry_run(&self, filter: ImportFilter) -> Result<usize> {
        filter.validate().map_err(WorkError::invalid)?;
        self.import_count(&ImportChoice {
            filter,
            committed_at: Some(self.now()),
        })
    }

    fn import_count(&self, choice: &ImportChoice) -> Result<usize> {
        Ok(self
            .read(|c| query::sessions(c, &SessionFilter::default()))?
            .iter()
            .filter(|s| choice.includes(s))
            .count())
    }

    /// Stores a choice atomically before publishing it to readers.
    /// # Errors
    /// Invalid filter, database or persistence errors.
    pub fn commit_import(&self, filter: ImportFilter) -> Result<usize> {
        filter.validate().map_err(WorkError::invalid)?;
        let _write = self.lock();
        let mut state = self.import.lock().unwrap_or_else(PoisonError::into_inner);
        let choice = ImportChoice {
            filter,
            committed_at: Some(self.now()),
        };
        let count = self.import_count(&choice)?;
        if let Some(file) = &state.file {
            let parent = file
                .parent()
                .ok_or_else(|| WorkError::internal("import file has no parent"))?;
            let mut pending = tempfile::NamedTempFile::new_in(parent)
                .map_err(|e| WorkError::internal(format!("creating import choice: {e}")))?;
            use std::io::Write;
            pending
                .write_all(&serde_json::to_vec(&choice)?)
                .and_then(|()| pending.as_file().sync_all())
                .map_err(|e| WorkError::internal(format!("writing import choice: {e}")))?;
            pending
                .persist(file)
                .map_err(|e| WorkError::internal(format!("saving import choice: {e}")))?;
        }
        state.choice = choice;
        Ok(count)
    }

    /// Whether an indexed session is included. Unknown sessions remain visible to events until
    /// discovery: inclusion is decided from session metadata, never guessed from an event time.
    /// # Errors
    /// Database errors.
    pub fn session_included(&self, id: &SessionId) -> Result<bool> {
        let choice = self.import_choice();
        self.read(|c| Ok(query::session(c, id)?.is_none_or(|s| choice.includes(&s))))
    }

    /// Applies inclusion to any session fields in an event, including transcript receipts.
    /// # Errors
    /// Database or serialization errors.
    pub fn event_included(&self, event: &Event) -> Result<bool> {
        self.event_included_for(event, &self.import_choice())
    }

    pub(crate) fn event_included_for(&self, event: &Event, choice: &ImportChoice) -> Result<bool> {
        if choice.filter.mode == ImportMode::All {
            return Ok(true);
        }
        let value = serde_json::to_value(&event.body)?;
        let mut ids = Vec::new();
        session_fields(&value, &mut ids);
        self.read(|c| {
            match &event.body {
                pitcrew_protocol::events::EventBody::DispatchFinished { dispatch, .. } => {
                    if let Some(dispatch) = query::dispatch(c, dispatch)?
                        && let Some(session) = dispatch.session
                    {
                        ids.push(session);
                    }
                }
                pitcrew_protocol::events::EventBody::AskAnswered { ask, .. } => {
                    if let Some(ask) = query::ask(c, ask)?
                        && let Some(session) = ask.session
                    {
                        ids.push(session);
                    }
                }
                _ => {}
            }
            for id in ids {
                if let Some(s) = query::session(c, &id)?
                    && !choice.includes(&s)
                {
                    return Ok(false);
                }
            }
            Ok(true)
        })
    }
}

fn session_fields(value: &Value, ids: &mut Vec<SessionId>) {
    match value {
        Value::Object(map) => {
            for (key, v) in map {
                if key == "session" {
                    let id = v.as_str().or_else(|| v.get("id").and_then(Value::as_str));
                    if let Some(id) = id.and_then(|id| id.parse().ok()) {
                        ids.push(id);
                    }
                }
                session_fields(v, ids);
            }
        }
        Value::Array(items) => {
            for item in items {
                session_fields(item, ids);
            }
        }
        _ => {}
    }
}
