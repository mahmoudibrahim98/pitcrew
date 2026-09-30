use crate::error::{Error, Result};
use crate::migrations::{self, Migration};
use pitcrew_protocol::events::{Event, EventBody};
use pitcrew_protocol::ids::{EventId, MemberId, WorkspaceId};
use rusqlite::{Connection, OpenFlags, Row};
use std::path::Path;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;
use tokio::sync::broadcast;

/// How to open a store.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct StoreOptions {
    /// How long a writer waits for a lock held by another connection before failing.
    pub busy_timeout: Duration,
    /// How many revision ranges a slow subscriber may fall behind before it lags.
    pub subscriber_capacity: usize,
}

impl Default for StoreOptions {
    fn default() -> Self {
        Self {
            busy_timeout: Duration::from_secs(5),
            subscriber_capacity: 1024,
        }
    }
}

/// Revisions `from_rev..=to_rev`, as in `StreamFrame::Events`. Empty when `to_rev < from_rev`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RevRange {
    /// First revision.
    pub from_rev: u64,
    /// Last revision.
    pub to_rev: u64,
}

impl RevRange {
    /// Whether the range holds no revisions.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.to_rev < self.from_rev
    }

    /// How many revisions it holds.
    #[must_use]
    pub fn len(&self) -> u64 {
        (self.to_rev + 1).saturating_sub(self.from_rev)
    }
}

/// An event with its revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredEvent {
    /// The hub-local, gap-free revision.
    pub rev: u64,
    /// The event.
    pub event: Event,
}

/// Which events to return. Empty matches everything. More fields (project, workstream, task,
/// session) will be added; build it with `EventFilter::default()` and the methods.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct EventFilter {
    /// Only these event types (the `type` tag, e.g. `task_moved`). `None` for all.
    pub types: Option<Vec<String>>,
}

impl EventFilter {
    /// Only events of these types.
    #[must_use]
    pub fn types<I, S>(mut self, types: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.types = Some(types.into_iter().map(Into::into).collect());
        self
    }
}

/// The hub's SQLite store.
#[derive(Debug)]
pub struct Store {
    conn: Mutex<Connection>,
    revs: broadcast::Sender<RevRange>,
}

impl Store {
    /// Opens or creates the store at `path` and applies the migrations built into this binary.
    ///
    /// # Errors
    ///
    /// Database errors, a failed migration, or [`Error::SchemaTooNew`] if the database was written
    /// by a newer build.
    pub fn open(path: impl AsRef<Path>, options: StoreOptions) -> Result<Self> {
        Self::open_with_migrations(path, options, migrations::embedded())
    }

    /// Like [`Store::open`], with an explicit list of migrations. For tests and tools.
    ///
    /// # Errors
    ///
    /// As [`Store::open`].
    pub fn open_with_migrations(
        path: impl AsRef<Path>,
        options: StoreOptions,
        migrations: &[Migration],
    ) -> Result<Self> {
        let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_URI;
        let mut conn = Connection::open_with_flags(path, flags)?;
        conn.busy_timeout(options.busy_timeout)?;
        // Local disks only for now; NFS mode (journal_mode=DELETE plus a lease) comes later.
        let mode: String = conn
            .pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get::<_, String>(0))?;
        if !mode.eq_ignore_ascii_case("wal") {
            return Err(Error::JournalMode {
                wanted: "wal",
                got: mode,
            });
        }
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        migrations::apply(&mut conn, migrations)?;
        let (revs, _) = broadcast::channel(options.subscriber_capacity.max(1));
        Ok(Self {
            conn: Mutex::new(conn),
            revs,
        })
    }

    /// The highest applied migration, or `None` if none are.
    ///
    /// # Errors
    ///
    /// Database errors.
    pub fn schema_version(&self) -> Result<Option<u32>> {
        migrations::current_version(&self.conn())
    }

    /// Appends events in one transaction and returns their revisions. Subscribers are told after
    /// the commit. An empty slice appends nothing and returns an empty range.
    ///
    /// # Errors
    ///
    /// Database errors (including a duplicate event id) or JSON errors; nothing is appended then.
    pub fn append(&self, events: &[Event]) -> Result<RevRange> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let before = latest_rev(&tx)?;
        {
            let mut insert = tx.prepare_cached(
                "INSERT INTO events (id, at, workspace, author, on_behalf_of, type, data)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )?;
            for event in events {
                let (kind, data) = encode_body(&event.body)?;
                insert.execute(rusqlite::params![
                    event.id.0.to_string(),
                    event.at,
                    event.workspace.0.to_string(),
                    event.author.0.to_string(),
                    event.on_behalf_of.map(|m| m.0.to_string()),
                    kind,
                    data,
                ])?;
            }
        }
        let after = latest_rev(&tx)?;
        tx.commit()?;
        drop(conn);
        let range = RevRange {
            from_rev: before + 1,
            to_rev: after,
        };
        if !range.is_empty() {
            // An error only means nobody is subscribed.
            let _ = self.revs.send(range);
        }
        Ok(range)
    }

    /// The newest revision, or 0 for an empty log.
    ///
    /// # Errors
    ///
    /// Database errors.
    pub fn latest_rev(&self) -> Result<u64> {
        latest_rev(&self.conn())
    }

    /// Up to `limit` events after `rev`, oldest first. `since(0, n)` starts at the beginning.
    ///
    /// # Errors
    ///
    /// Database errors, or [`Error::Corrupt`] for a row that does not decode.
    pub fn since(&self, rev: u64, limit: usize) -> Result<Vec<StoredEvent>> {
        let conn = self.conn();
        let mut stmt = conn.prepare_cached(
            "SELECT rev, id, at, workspace, author, on_behalf_of, type, data
             FROM events WHERE rev > ?1 ORDER BY rev ASC LIMIT ?2",
        )?;
        let rows = stmt.query_map(
            rusqlite::params![to_sql_rev(rev), to_limit(limit)],
            read_row,
        )?;
        rows.map(|r| r?.decode()).collect()
    }

    /// Up to `limit` events matching `filter` just before `rev`, oldest first. For paging back
    /// through history: pass the first returned revision as the next `rev`.
    ///
    /// # Errors
    ///
    /// Database errors, or [`Error::Corrupt`] for a row that does not decode.
    pub fn before(&self, rev: u64, limit: usize, filter: &EventFilter) -> Result<Vec<StoredEvent>> {
        let mut sql = String::from(
            "SELECT rev, id, at, workspace, author, on_behalf_of, type, data
             FROM events WHERE rev < ?",
        );
        let mut params: Vec<rusqlite::types::Value> = vec![to_sql_rev(rev).into()];
        if let Some(types) = &filter.types {
            if types.is_empty() {
                return Ok(Vec::new());
            }
            sql.push_str(" AND type IN (");
            for (i, t) in types.iter().enumerate() {
                sql.push_str(if i == 0 { "?" } else { ", ?" });
                params.push(t.clone().into());
            }
            sql.push(')');
        }
        sql.push_str(" ORDER BY rev DESC LIMIT ?");
        params.push(to_limit(limit).into());

        let conn = self.conn();
        let mut stmt = conn.prepare_cached(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(params), read_row)?;
        let mut events = rows.map(|r| r?.decode()).collect::<Result<Vec<_>>>()?;
        events.reverse();
        Ok(events)
    }

    /// A receiver of the revision ranges appended from now on. A receiver that falls more than
    /// `subscriber_capacity` ranges behind gets `Lagged` and should catch up with
    /// [`Store::since`].
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<RevRange> {
        self.revs.subscribe()
    }

    fn conn(&self) -> MutexGuard<'_, Connection> {
        // A panic mid-transaction rolls the transaction back when it drops, so the connection is
        // still usable.
        self.conn.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The `type` tag of an event body, e.g. `task_moved`.
///
/// # Errors
///
/// JSON errors, which the protocol types rule out.
pub fn event_type(body: &EventBody) -> Result<String> {
    Ok(encode_body(body)?.0)
}

fn latest_rev(conn: &Connection) -> Result<u64> {
    let rev: i64 = conn.query_row("SELECT COALESCE(MAX(rev), 0) FROM events", [], |row| {
        row.get(0)
    })?;
    Ok(u64::try_from(rev).unwrap_or(0))
}

fn to_sql_rev(rev: u64) -> i64 {
    i64::try_from(rev).unwrap_or(i64::MAX)
}

fn to_limit(limit: usize) -> i64 {
    i64::try_from(limit).unwrap_or(i64::MAX)
}

/// Splits a body into its serde tag and its content as JSON text.
fn encode_body(body: &EventBody) -> Result<(String, String)> {
    fn shape(msg: &str) -> Error {
        Error::Json(<serde_json::Error as serde::ser::Error>::custom(msg))
    }
    let mut value = serde_json::to_value(body)?;
    let obj = value
        .as_object_mut()
        .ok_or_else(|| shape("an event body serializes to a JSON object"))?;
    let kind = match obj.remove("type") {
        Some(serde_json::Value::String(s)) => s,
        _ => return Err(shape("an event body has a string `type` tag")),
    };
    let data = obj.remove("data").unwrap_or(serde_json::Value::Null);
    Ok((kind, serde_json::to_string(&data)?))
}

struct RawRow {
    rev: i64,
    id: String,
    at: i64,
    workspace: String,
    author: String,
    on_behalf_of: Option<String>,
    kind: String,
    data: String,
}

fn read_row(row: &Row<'_>) -> rusqlite::Result<RawRow> {
    Ok(RawRow {
        rev: row.get(0)?,
        id: row.get(1)?,
        at: row.get(2)?,
        workspace: row.get(3)?,
        author: row.get(4)?,
        on_behalf_of: row.get(5)?,
        kind: row.get(6)?,
        data: row.get(7)?,
    })
}

impl RawRow {
    fn decode(self) -> Result<StoredEvent> {
        let rev = u64::try_from(self.rev).unwrap_or(0);
        let corrupt = |reason: String| Error::Corrupt { rev, reason };
        let data: serde_json::Value =
            serde_json::from_str(&self.data).map_err(|e| corrupt(format!("data: {e}")))?;
        let body: EventBody = serde_json::from_value(serde_json::json!({
            "type": self.kind,
            "data": data,
        }))
        .map_err(|e| corrupt(format!("body: {e}")))?;
        let event = Event {
            id: self
                .id
                .parse::<EventId>()
                .map_err(|e| corrupt(e.to_string()))?,
            at: self.at,
            workspace: self
                .workspace
                .parse::<WorkspaceId>()
                .map_err(|e| corrupt(e.to_string()))?,
            author: self
                .author
                .parse::<MemberId>()
                .map_err(|e| corrupt(e.to_string()))?,
            on_behalf_of: self
                .on_behalf_of
                .map(|m| m.parse::<MemberId>())
                .transpose()
                .map_err(|e| corrupt(e.to_string()))?,
            body,
        };
        Ok(StoredEvent { rev, event })
    }
}
