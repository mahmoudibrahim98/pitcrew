use pitcrew_store::migrations::{self, Migration};
use pitcrew_store::{Error, Store, StoreOptions};
use std::borrow::Cow;
use std::path::Path;

const INIT_SQL: &str = include_str!("../migrations/0001_init.sql");

fn raw(path: &Path) -> rusqlite::Connection {
    rusqlite::Connection::open(path).expect("open raw connection")
}

fn applied(path: &Path) -> Vec<(u32, String, i64)> {
    let conn = raw(path);
    let mut stmt = conn
        .prepare("SELECT version, name, applied_at FROM schema_migrations ORDER BY version")
        .expect("prepare");
    stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .expect("query")
        .collect::<rusqlite::Result<_>>()
        .expect("rows")
}

fn migration(version: u32, name: &'static str, sql: &'static str) -> Migration {
    Migration {
        version,
        name: Cow::Borrowed(name),
        sql: Cow::Borrowed(sql),
    }
}

#[test]
fn embedded_includes_init() {
    let all = migrations::embedded();
    assert_eq!(all.first().map(|m| m.version), Some(1));
    assert_eq!(all[0].name, "init");
    assert!(all.windows(2).all(|w| w[0].version < w[1].version));
}

#[test]
fn opening_twice_applies_nothing_new() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("store.db");
    let store = Store::open(&path, StoreOptions::default()).expect("first open");
    let latest = migrations::embedded().last().map(|m| m.version);
    assert_eq!(store.schema_version().expect("version"), latest);
    drop(store);
    let first = applied(&path);
    assert_eq!(first.len(), migrations::embedded().len());

    let store = Store::open(&path, StoreOptions::default()).expect("second open");
    drop(store);
    assert_eq!(applied(&path), first);
}

#[test]
fn newer_schema_is_refused() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("store.db");
    drop(Store::open(&path, StoreOptions::default()).expect("open"));
    raw(&path)
        .execute(
            "INSERT INTO schema_migrations (version, name, applied_at) VALUES (9999, 'future', 0)",
            [],
        )
        .expect("insert future version");

    let err = Store::open(&path, StoreOptions::default()).expect_err("must refuse");
    let supported = migrations::embedded().last().map_or(0, |m| m.version);
    assert!(
        matches!(err, Error::SchemaTooNew { found: 9999, supported: s } if s == supported),
        "{err:?}"
    );
    let msg = err.to_string();
    assert!(msg.contains("9999") && msg.contains("upgrade"), "{msg}");
}

#[test]
fn unknown_older_migration_is_refused() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("store.db");
    let a = [
        migration(1, "init", INIT_SQL),
        migration(150, "other_branch", "CREATE TABLE b (x INTEGER) STRICT;"),
    ];
    drop(Store::open_with_migrations(&path, StoreOptions::default(), &a).expect("open a"));
    let b = [
        migration(1, "init", INIT_SQL),
        migration(200, "later", "CREATE TABLE c (x INTEGER) STRICT;"),
    ];
    let err = Store::open_with_migrations(&path, StoreOptions::default(), &b).expect_err("refuse");
    assert!(
        matches!(err, Error::UnknownMigration { version: 150 }),
        "{err:?}"
    );
}

#[test]
fn a_failed_migration_leaves_nothing_behind() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("store.db");
    let list = [
        migration(1, "init", INIT_SQL),
        migration(
            101,
            "broken",
            "CREATE TABLE half (x INTEGER) STRICT; THIS IS NOT SQL;",
        ),
    ];
    let err = Store::open_with_migrations(&path, StoreOptions::default(), &list).expect_err("fail");
    assert!(
        matches!(err, Error::Migration { version: 101, .. }),
        "{err:?}"
    );
    assert!(err.to_string().contains("0101_broken"), "{err}");

    let conn = raw(&path);
    let half: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE name = 'half'",
            [],
            |r| r.get(0),
        )
        .expect("count");
    assert_eq!(half, 0);
    drop(conn);
    assert_eq!(
        applied(&path).iter().map(|a| a.0).collect::<Vec<_>>(),
        vec![1]
    );
}

#[test]
fn migrations_in_a_directory_are_picked_up() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mdir = dir.path().join("migrations");
    std::fs::create_dir(&mdir).expect("mkdir");
    std::fs::write(mdir.join("0001_init.sql"), INIT_SQL).expect("write");
    // Out of order on disk and interleaved across streams.
    std::fs::write(
        mdir.join("0301_recaps.sql"),
        "CREATE TABLE recaps (id TEXT PRIMARY KEY) STRICT;",
    )
    .expect("write");
    std::fs::write(
        mdir.join("0142_widgets.sql"),
        "CREATE TABLE widgets (id INTEGER PRIMARY KEY) STRICT;",
    )
    .expect("write");
    std::fs::write(mdir.join("README.md"), "not a migration").expect("write");

    let list = migrations::load_dir(&mdir).expect("load");
    let versions: Vec<u32> = list.iter().map(|m| m.version).collect();
    assert_eq!(versions, vec![1, 142, 301]);
    assert_eq!(list[1].name, "widgets");

    let path = dir.path().join("store.db");
    let store = Store::open_with_migrations(&path, StoreOptions::default(), &list).expect("open");
    assert_eq!(store.schema_version().expect("version"), Some(301));
    drop(store);
    let tables: i64 = raw(&path)
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE name IN ('widgets', 'recaps')",
            [],
            |r| r.get(0),
        )
        .expect("count");
    assert_eq!(tables, 2);

    // A new file added later is applied on the next open, and only it.
    let before = applied(&path);
    std::fs::write(
        mdir.join("0205_labels.sql"),
        "CREATE TABLE labels (id INTEGER PRIMARY KEY) STRICT;",
    )
    .expect("write");
    let list = migrations::load_dir(&mdir).expect("reload");
    drop(Store::open_with_migrations(&path, StoreOptions::default(), &list).expect("reopen"));
    let after = applied(&path);
    assert_eq!(after.len(), before.len() + 1);
    assert!(after.iter().any(|a| a.0 == 205 && a.1 == "labels"));
}

#[test]
fn bad_directories_are_rejected() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("0142_a.sql"), "").expect("write");
    std::fs::write(dir.path().join("0142_b.sql"), "").expect("write");
    let err = migrations::load_dir(dir.path()).expect_err("duplicate");
    assert!(
        err.to_string().contains("duplicate migration number 0142"),
        "{err}"
    );

    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("142_short.sql"), "").expect("write");
    let err = migrations::load_dir(dir.path()).expect_err("bad name");
    assert!(err.to_string().contains("bad migration file name"), "{err}");
}
