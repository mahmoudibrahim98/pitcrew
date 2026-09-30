//! A toy projection shared by the tests: events counted by a key, with the last revision seen.

#![allow(dead_code)]

use pitcrew_store::migrations::{self, Migration};
use pitcrew_store::sql::{self, Transaction};
use pitcrew_store::{BoxError, Projection, StoredEvent, event_type};
use std::borrow::Cow;

/// Counts events per key, and the last revision seen per key.
pub struct CountBy {
    pub name: &'static str,
    pub version: u32,
    pub table: &'static str,
    pub key: fn(&StoredEvent) -> String,
    /// Fails `apply` for events with this key.
    pub fail_on: Option<&'static str>,
}

pub fn by_type(e: &StoredEvent) -> String {
    event_type(&e.event.body).expect("type")
}

pub fn by_author(e: &StoredEvent) -> String {
    e.event.author.to_string()
}

impl CountBy {
    pub fn types() -> Self {
        Self {
            name: "toy.by_type",
            version: 1,
            table: "toy_by_type",
            key: by_type,
            fail_on: None,
        }
    }

    pub fn authors() -> Self {
        Self {
            name: "toy.by_author",
            version: 1,
            table: "toy_by_author",
            key: by_author,
            fail_on: None,
        }
    }
}

impl Projection for CountBy {
    fn name(&self) -> &str {
        self.name
    }

    fn version(&self) -> u32 {
        self.version
    }

    fn reset(&self, tx: &Transaction<'_>) -> Result<(), BoxError> {
        tx.execute(&format!("DELETE FROM {}", self.table), [])?;
        Ok(())
    }

    fn apply(&self, tx: &Transaction<'_>, event: &StoredEvent) -> Result<(), BoxError> {
        let key = (self.key)(event);
        if self.fail_on == Some(key.as_str()) {
            return Err(format!("refusing {key}").into());
        }
        tx.prepare_cached(&format!(
            "INSERT INTO {} (key, n, last) VALUES (?1, 1, ?2)
             ON CONFLICT (key) DO UPDATE SET n = n + 1, last = excluded.last",
            self.table
        ))?
        .execute(sql::params![key, i64::try_from(event.rev)?])?;
        Ok(())
    }
}

pub const TOY_SQL: &str = "
    CREATE TABLE toy_by_type (key TEXT PRIMARY KEY, n INTEGER NOT NULL, last INTEGER NOT NULL) STRICT;
    CREATE TABLE toy_by_author (key TEXT PRIMARY KEY, n INTEGER NOT NULL, last INTEGER NOT NULL) STRICT;";

pub fn toy_migrations() -> Vec<Migration> {
    let mut list = migrations::embedded().to_vec();
    list.push(Migration {
        version: 190,
        name: Cow::Borrowed("toy"),
        sql: Cow::Borrowed(TOY_SQL),
    });
    list
}

pub fn both() -> Vec<Box<dyn Projection>> {
    vec![Box::new(CountBy::types()), Box::new(CountBy::authors())]
}
