//! Converting protocol types to and from SQL columns.
//!
//! - Ids are stored as bare ULIDs ([`IdText::text`]), never the prefixed display form.
//! - Unit enums are stored as their serde names (`in_progress`), through serde itself, so the
//!   columns always match the wire format.
//! - Nested values (locations, receipts, external refs) are stored as their serde JSON.

use pitcrew_protocol::ids::{
    AskId, DispatchId, EventId, MachineId, MemberId, PersonaId, ProjectId, SessionId, SubtaskId,
    TaskId, TeamId, TerminalId, WorkstreamId,
};
use pitcrew_store::sql::types::Type;
use pitcrew_store::sql::{self, Row};
use serde::Serialize;
use serde::de::{DeserializeOwned, IntoDeserializer, value::Error as ValueError};
use std::str::FromStr;

/// An id's column form: the bare ULID.
pub(crate) trait IdText {
    /// The bare ULID, e.g. `01JB000000000000000TSK0004`.
    fn text(&self) -> String;
}

macro_rules! id_text {
    ($($t:ty),*) => {
        $(impl IdText for $t {
            fn text(&self) -> String {
                self.0.to_string()
            }
        })*
    };
}

id_text!(
    AskId,
    DispatchId,
    EventId,
    MachineId,
    MemberId,
    PersonaId,
    ProjectId,
    SessionId,
    SubtaskId,
    TaskId,
    TeamId,
    TerminalId,
    WorkstreamId
);

/// The bare ULID of an optional id.
pub(crate) fn opt_text<T: IdText>(id: Option<&T>) -> Option<String> {
    id.map(IdText::text)
}

/// A unit enum's serde name, e.g. `TaskStatus::InProgress` → `in_progress`.
pub(crate) fn enum_text<T: Serialize>(value: &T) -> Result<String, serde_json::Error> {
    match serde_json::to_value(value)? {
        serde_json::Value::String(s) => Ok(s),
        other => Err(<serde_json::Error as serde::ser::Error>::custom(format!(
            "expected a unit enum, got {other}"
        ))),
    }
}

/// Parses a unit enum from its serde name.
pub(crate) fn parse_enum<T: DeserializeOwned>(text: &str) -> Result<T, ValueError> {
    T::deserialize(IntoDeserializer::<ValueError>::into_deserializer(text))
}

/// JSON text of a value, for JSON columns.
pub(crate) fn json<T: Serialize>(value: &T) -> Result<String, serde_json::Error> {
    serde_json::to_string(value)
}

/// JSON text of an optional value; `None` stays SQL NULL.
pub(crate) fn opt_json<T: Serialize>(
    value: Option<&T>,
) -> Result<Option<String>, serde_json::Error> {
    value.map(json).transpose()
}

/// A revision as SQL stores it.
pub(crate) fn sql_rev(rev: u64) -> i64 {
    i64::try_from(rev).unwrap_or(i64::MAX)
}

fn conversion(idx: usize, e: impl std::error::Error + Send + Sync + 'static) -> sql::Error {
    sql::Error::FromSqlConversionFailure(idx, Type::Text, Box::new(e))
}

/// Reads a text column and parses it with `FromStr` (ids, keys).
pub(crate) fn col<T>(row: &Row<'_>, idx: usize) -> sql::Result<T>
where
    T: FromStr,
    T::Err: std::error::Error + Send + Sync + 'static,
{
    let text: String = row.get(idx)?;
    text.parse().map_err(|e| conversion(idx, e))
}

/// Like [`col`], for a nullable column.
pub(crate) fn opt_col<T>(row: &Row<'_>, idx: usize) -> sql::Result<Option<T>>
where
    T: FromStr,
    T::Err: std::error::Error + Send + Sync + 'static,
{
    let text: Option<String> = row.get(idx)?;
    text.map(|t| t.parse().map_err(|e| conversion(idx, e)))
        .transpose()
}

/// Reads a unit enum column.
pub(crate) fn enum_col<T: DeserializeOwned>(row: &Row<'_>, idx: usize) -> sql::Result<T> {
    let text: String = row.get(idx)?;
    parse_enum(&text).map_err(|e| conversion(idx, e))
}

/// Reads a nullable unit enum column.
pub(crate) fn opt_enum_col<T: DeserializeOwned>(
    row: &Row<'_>,
    idx: usize,
) -> sql::Result<Option<T>> {
    let text: Option<String> = row.get(idx)?;
    text.map(|t| parse_enum(&t).map_err(|e| conversion(idx, e)))
        .transpose()
}

/// Reads a JSON column.
pub(crate) fn json_col<T: DeserializeOwned>(row: &Row<'_>, idx: usize) -> sql::Result<T> {
    let text: String = row.get(idx)?;
    serde_json::from_str(&text).map_err(|e| conversion(idx, e))
}

/// Reads a nullable JSON column.
pub(crate) fn opt_json_col<T: DeserializeOwned>(
    row: &Row<'_>,
    idx: usize,
) -> sql::Result<Option<T>> {
    let text: Option<String> = row.get(idx)?;
    text.map(|t| serde_json::from_str(&t).map_err(|e| conversion(idx, e)))
        .transpose()
}

#[cfg(test)]
mod tests {
    use super::*;
    use pitcrew_protocol::model::{AskKind, TaskStatus};

    #[test]
    fn enums_round_trip_through_their_serde_names() {
        assert_eq!(enum_text(&TaskStatus::InProgress).unwrap(), "in_progress");
        assert_eq!(
            parse_enum::<TaskStatus>("in_progress").unwrap(),
            TaskStatus::InProgress
        );
        assert_eq!(
            parse_enum::<AskKind>("approval").unwrap(),
            AskKind::Approval
        );
        assert!(parse_enum::<TaskStatus>("finished").is_err());
    }

    #[test]
    fn ids_are_stored_bare() {
        let id: TaskId = "tsk_01JB000000000000000TSK0004".parse().unwrap();
        assert_eq!(id.text(), "01JB000000000000000TSK0004");
    }
}
