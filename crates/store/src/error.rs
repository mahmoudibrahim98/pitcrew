use pitcrew_protocol::ids::EventId;
use std::fmt;

/// Errors from the store. Messages do not repeat their cause; walk `source()` for it.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// SQLite reported an error.
    #[error("database error")]
    Database(#[source] DbError),
    /// An appended event has an id that is already stored, or appears twice in the batch. `id` is
    /// the first such id; others may follow it. **Nothing from the batch was stored**, including
    /// its new events. A caller retrying a batch must not treat this as done: drop the ids already
    /// stored and append the rest, which [`Store::append_new`](crate::Store::append_new) does in
    /// one transaction.
    #[error("event {id} is already in the log")]
    DuplicateEvent {
        /// The repeated id.
        id: EventId,
    },
    /// A projection's `apply` or `reset` failed. The append, rebuild or open it was part of is
    /// rolled back: no event is stored and no revision used.
    #[error("projection {name} failed at rev {rev}")]
    Projection {
        /// The projection.
        name: String,
        /// The event being applied, or 0 while resetting.
        rev: u64,
        /// Why.
        #[source]
        source: crate::projection::BoxError,
    },
    /// The store holds a projection at another version than this build's, and the store will not
    /// rebuild it:
    /// - on open or [`Store::rebuild`](crate::Store::rebuild), only when the stored version is
    ///   **higher** (a newer PitCrew rebuilt it; this build would undo that). Upgrade.
    /// - on an append, for any difference: another process, running another version of the
    ///   projection, rebuilt it after this store opened. Rebuilding it back on every append would
    ///   replay the whole log each time. Nothing is appended; stop one of the two processes.
    #[error("projection {name} is at version {stored} in the store, but this build has {ours}")]
    ProjectionVersion {
        /// The projection.
        name: String,
        /// The version in `projection_state`.
        stored: u32,
        /// This build's version.
        ours: u32,
    },
    /// Two projections passed to one store share a name.
    #[error("two projections are named {name}")]
    DuplicateProjection {
        /// The name.
        name: String,
    },
    /// No projection by this name is registered.
    #[error("no projection named {name}")]
    UnknownProjection {
        /// The name.
        name: String,
    },
    /// A migration failed to apply; nothing from it was kept.
    #[error("migration {version:04}_{name} failed")]
    Migration {
        /// Its number.
        version: u32,
        /// Its name.
        name: String,
        /// Why.
        #[source]
        source: DbError,
    },
    /// The database was written by a newer PitCrew.
    #[error(
        "the store's schema is version {found}, but this build only knows up to {supported}; \
         upgrade PitCrew to open it"
    )]
    SchemaTooNew {
        /// The highest version applied to the database.
        found: u32,
        /// The highest version this build knows.
        supported: u32,
    },
    /// The database has a migration this build does not know, below its newest one. It was
    /// probably written by a build from another branch.
    #[error("the store has migration {version:04}, which this build does not know")]
    UnknownMigration {
        /// The unknown version.
        version: u32,
    },
    /// Migration files are misnamed or share a number.
    #[error("migrations: {0}")]
    BadMigrations(String),
    /// The journal mode could not be set.
    #[error("could not set journal mode {wanted}; SQLite kept {got}")]
    JournalMode {
        /// What was asked for.
        wanted: &'static str,
        /// What SQLite reports.
        got: String,
    },
    /// An event could not be encoded or decoded as JSON.
    #[error("event JSON")]
    Json(#[from] serde_json::Error),
    /// A stored row does not decode into an event.
    #[error("stored event at rev {rev} is invalid: {reason}")]
    Corrupt {
        /// Its revision.
        rev: u64,
        /// What is wrong.
        reason: String,
    },
    /// Network mode: another, live owner holds the lease. `until` is a best guess (host and pid
    /// are `"unknown"`/`0`) when the file was unreadable and only its age said it was not yet
    /// stale.
    #[error("the store is leased by {host} (pid {pid}) until {until}")]
    Leased {
        /// The current owner's host name.
        host: String,
        /// The current owner's process id.
        pid: u32,
        /// When the lease expires, in milliseconds since the Unix epoch.
        until: i64,
    },
    /// Network mode: this store's lease was taken over by another host or process (its clock
    /// stalled, or it was suspended past the lease length). Every append fails with this from
    /// here on; nothing further is written.
    #[error("the network-mode lease was taken over by another host or process")]
    LeaseLost,
    /// Network mode: a filesystem error acquiring, renewing or releasing the lease (not a SQLite
    /// error).
    #[error("network-mode lease: {0}")]
    LeaseIo(#[source] std::io::Error),
    /// An I/O error from [`Store::export`](crate::Store::export) or
    /// [`Store::import`](crate::Store::import) (not the lease, and not SQLite).
    #[error("store export/import: {0}")]
    Io(#[source] std::io::Error),
    /// [`Store::import`](crate::Store::import) was called on a store that already holds events.
    /// Import seeds a fresh store; appending into one that already has history would mix two
    /// logs' revisions.
    #[error("the store must be empty to import into")]
    NotEmpty,
}

/// A SQLite error. It wraps the store's `rusqlite` error, re-exported as
/// [`sql::Error`](crate::sql::Error); [`DbError::as_sql`] reaches it, e.g. for its error code.
pub struct DbError(rusqlite::Error);

impl fmt::Debug for DbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.0, f)
    }
}

impl fmt::Display for DbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}

impl std::error::Error for DbError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.0.source()
    }
}

impl From<rusqlite::Error> for Error {
    fn from(e: rusqlite::Error) -> Self {
        Self::Database(DbError(e))
    }
}

impl DbError {
    pub(crate) fn new(e: rusqlite::Error) -> Self {
        Self(e)
    }

    /// The underlying error, e.g. for
    /// [`sqlite_error_code`](crate::sql::Error::sqlite_error_code).
    #[must_use]
    pub fn as_sql(&self) -> &crate::sql::Error {
        &self.0
    }
}

/// Shorthand for results from this crate.
pub type Result<T, E = Error> = std::result::Result<T, E>;
