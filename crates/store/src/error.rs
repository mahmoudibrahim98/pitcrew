use std::fmt;

/// Errors from the store.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// SQLite reported an error.
    #[error("database error: {0}")]
    Database(#[source] DbError),
    /// A migration failed to apply; nothing from it was kept.
    #[error("migration {version:04}_{name} failed: {source}")]
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
    #[error("event JSON: {0}")]
    Json(#[from] serde_json::Error),
    /// A stored row does not decode into an event.
    #[error("stored event at rev {rev} is invalid: {reason}")]
    Corrupt {
        /// Its revision.
        rev: u64,
        /// What is wrong.
        reason: String,
    },
}

/// An opaque SQLite error, so the public API does not depend on `rusqlite`'s types.
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
}

/// Shorthand for results from this crate.
pub type Result<T, E = Error> = std::result::Result<T, E>;
