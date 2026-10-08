//! Hub inclusion state; transcripts and the underlying projections remain intact.
use crate::error::Result;
use crate::query::{self, SessionFilter};
use crate::{WorkError, WorkService};
use pitcrew_protocol::events::Event;
use pitcrew_protocol::ids::SessionId;
use pitcrew_protocol::import::{
    ImportChoice, ImportDryRun, ImportFilter, ImportMode, ImportResult,
};
use pitcrew_protocol::model::Session;
use pitcrew_store::sql::Connection;
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::PoisonError;

/// Sub-agents followed up their chain of parents for at most this many sessions, the session
/// itself included: past it the session stands on its own.
const MAX_PARENT_CHAIN: usize = 16;

/// The top of `session`'s chain of parents: the first session whose parent the hub does not know
/// (it names none, or one the hub never saw), `session` itself when that is it. `None` when the
/// chain ends nowhere (a loop, or one past [`MAX_PARENT_CHAIN`]): the session then stands on its
/// own, as clients show it.
fn root<'a>(
    session: &'a Session,
    by_id: &impl Fn(&SessionId) -> Option<&'a Session>,
) -> Option<&'a Session> {
    let mut at = session;
    for _ in 0..MAX_PARENT_CHAIN {
        match at.parent.as_ref().and_then(by_id) {
            Some(parent) => at = parent,
            None => return Some(at),
        }
    }
    None
}

/// The session whose inclusion decides `session`'s: the top of its chain, or itself when it
/// stands on its own ([`root`]).
fn deciding<'a>(
    session: &'a Session,
    by_id: &impl Fn(&SessionId) -> Option<&'a Session>,
) -> &'a Session {
    root(session, by_id).unwrap_or(session)
}

/// Whether `session` is included, a sub-agent through the top of its chain of parents ([`root`]).
fn included(c: &Connection, choice: &ImportChoice, session: &Session) -> Result<bool> {
    let mut at = session.clone();
    for _ in 0..MAX_PARENT_CHAIN {
        let parent = match at.parent {
            Some(parent) => query::session(c, &parent)?,
            None => None,
        };
        match parent {
            Some(p) => at = p,
            None => return Ok(choice.includes(&at)),
        }
    }
    Ok(choice.includes(session))
}

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

    /// Counts indexed sessions, without changing the choice: sessions, and apart from them the
    /// sub-agents that come with them.
    /// # Errors
    /// Invalid filter or database errors.
    pub fn import_dry_run(&self, filter: ImportFilter) -> Result<ImportDryRun> {
        filter.validate().map_err(WorkError::invalid)?;
        let (count, subagents) = self.import_count(&ImportChoice {
            filter,
            committed_at: Some(self.now()),
        })?;
        Ok(ImportDryRun { count, subagents })
    }

    /// Included sessions, and apart from them included sub-agents: sessions nested under the top
    /// of their chain of parents ([`root`]), which they follow.
    fn import_count(&self, choice: &ImportChoice) -> Result<(usize, usize)> {
        let sessions = self.read(|c| query::sessions(c, &SessionFilter::default()))?;
        let by_id: HashMap<SessionId, &Session> = sessions.iter().map(|s| (s.id, s)).collect();
        let lookup = |id: &SessionId| by_id.get(id).copied();
        let (mut count, mut subagents) = (0, 0);
        for s in &sessions {
            let top = deciding(s, &lookup);
            if !choice.includes(top) {
                continue;
            }
            // A sub-agent nests under the top of its chain; one naming a parent the hub never saw,
            // or in a loop of parents, is a session of its own.
            if top.id == s.id {
                count += 1;
            } else {
                subagents += 1;
            }
        }
        Ok((count, subagents))
    }

    /// Stores a choice atomically before publishing it to readers.
    /// # Errors
    /// Invalid filter, database or persistence errors.
    pub fn commit_import(&self, filter: ImportFilter) -> Result<ImportResult> {
        filter.validate().map_err(WorkError::invalid)?;
        let _write = self.lock();
        let mut state = self.import.lock().unwrap_or_else(PoisonError::into_inner);
        let choice = ImportChoice {
            filter,
            committed_at: Some(self.now()),
        };
        let (imported, subagents) = self.import_count(&choice)?;
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
        Ok(ImportResult {
            imported,
            subagents,
        })
    }

    /// The indexed sessions `filter` matches that the import includes: a sub-agent exactly when
    /// its parent is, whether or not the filter matches the parent.
    /// # Errors
    /// Database errors.
    pub fn included_sessions(&self, filter: &SessionFilter) -> Result<Vec<Session>> {
        let choice = self.import_choice();
        self.read(|c| {
            let mut out = Vec::new();
            for s in query::sessions(c, filter)? {
                if included(c, &choice, &s)? {
                    out.push(s);
                }
            }
            Ok(out)
        })
    }

    /// Whether an indexed session is included. Unknown sessions remain visible to events until
    /// discovery: inclusion is decided from session metadata, never guessed from an event time.
    /// # Errors
    /// Database errors.
    pub fn session_included(&self, id: &SessionId) -> Result<bool> {
        let choice = self.import_choice();
        self.read(|c| match query::session(c, id)? {
            None => Ok(true),
            Some(s) => included(c, &choice, &s),
        })
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
                    && !included(c, choice, &s)?
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
