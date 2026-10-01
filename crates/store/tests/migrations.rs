use pitcrew_store::migrations::{self, Migration};
use pitcrew_store::{Error, Store, StoreOptions};
use std::borrow::Cow;
use std::path::Path;
use std::sync::{Arc, Barrier};

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
fn embedded_matches_the_migrations_directory() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations");
    let on_disk = migrations::load_dir(&dir).expect("load");
    assert_eq!(migrations::embedded(), on_disk.as_slice());
}

#[test]
fn concurrent_opens_of_a_fresh_file_both_succeed() {
    const OPENERS: usize = 4;
    for _ in 0..5 {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = Arc::new(dir.path().join("store.db"));
        let barrier = Arc::new(Barrier::new(OPENERS));
        let handles: Vec<_> = (0..OPENERS)
            .map(|_| {
                let path = Arc::clone(&path);
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    Store::open(path.as_path(), StoreOptions::default())
                        .map(|store| store.log_id().to_owned())
                })
            })
            .collect();
        let ids: Vec<String> = handles
            .into_iter()
            .map(|h| h.join().expect("thread").expect("open"))
            .collect();
        let versions: Vec<u32> = applied(&path).iter().map(|a| a.0).collect();
        let expected: Vec<u32> = migrations::embedded().iter().map(|m| m.version).collect();
        assert_eq!(versions, expected);
        // One log id, whichever opener wrote it.
        let stored: String = raw(&path)
            .query_row("SELECT value FROM meta WHERE key = 'log_id'", [], |r| {
                r.get(0)
            })
            .expect("log id");
        assert!(ids.iter().all(|id| *id == stored), "{ids:?} vs {stored}");
    }
}

const PARENT_CHILD: &str = "
    CREATE TABLE parent (id INTEGER PRIMARY KEY, name TEXT NOT NULL) STRICT;
    CREATE TABLE child (
      id     INTEGER PRIMARY KEY,
      parent INTEGER NOT NULL REFERENCES parent (id) ON DELETE CASCADE
    ) STRICT;
    INSERT INTO parent (id, name) VALUES (1, 'a'), (2, 'b');
    INSERT INTO child (id, parent) VALUES (10, 1), (11, 1), (12, 2);";

fn count(path: &Path, table: &str) -> i64 {
    raw(path)
        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
        .expect("count")
}

#[test]
fn a_table_rebuild_keeps_cascading_children() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("store.db");
    let list = [
        migration(1, "init", INIT_SQL),
        migration(150, "parent_child", PARENT_CHILD),
        // The documented rebuild: new table, copy, drop old, rename.
        migration(
            151,
            "rebuild_parent",
            "CREATE TABLE parent_new (id INTEGER PRIMARY KEY, name TEXT NOT NULL, note TEXT) STRICT;
             INSERT INTO parent_new (id, name) SELECT id, name FROM parent;
             DROP TABLE parent;
             ALTER TABLE parent_new RENAME TO parent;",
        ),
    ];
    let store = Store::open_with_migrations(&path, StoreOptions::default(), &list, Vec::new())
        .expect("open");
    assert_eq!(store.schema_version().expect("version"), Some(151));
    drop(store);
    assert_eq!(count(&path, "parent"), 2);
    assert_eq!(count(&path, "child"), 3);

    // Foreign keys are back on afterwards: the cascade still works.
    let conn = raw(&path);
    conn.execute_batch("PRAGMA foreign_keys = ON; DELETE FROM parent WHERE id = 1;")
        .expect("delete");
    drop(conn);
    assert_eq!(count(&path, "child"), 1);
}

#[test]
fn a_migration_leaving_a_dangling_reference_fails() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("store.db");
    let list = [
        migration(1, "init", INIT_SQL),
        migration(150, "parent_child", PARENT_CHILD),
        migration(
            151,
            "orphan",
            "INSERT INTO child (id, parent) VALUES (13, 99);",
        ),
    ];
    let err = Store::open_with_migrations(&path, StoreOptions::default(), &list, Vec::new())
        .expect_err("fail");
    assert!(
        matches!(err, Error::Migration { version: 151, .. }),
        "{err:?}"
    );
    let cause = std::error::Error::source(&err).expect("source").to_string();
    assert!(cause.contains("foreign key check failed"), "{cause}");
    assert_eq!(count(&path, "child"), 3);
    assert_eq!(
        applied(&path).iter().map(|a| a.0).collect::<Vec<_>>(),
        vec![1, 150]
    );
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
    drop(
        Store::open_with_migrations(&path, StoreOptions::default(), &a, Vec::new())
            .expect("open a"),
    );
    let b = [
        migration(1, "init", INIT_SQL),
        migration(200, "later", "CREATE TABLE c (x INTEGER) STRICT;"),
    ];
    let err = Store::open_with_migrations(&path, StoreOptions::default(), &b, Vec::new())
        .expect_err("refuse");
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
    let err = Store::open_with_migrations(&path, StoreOptions::default(), &list, Vec::new())
        .expect_err("fail");
    assert!(
        matches!(err, Error::Migration { version: 101, .. }),
        "{err:?}"
    );
    assert!(err.to_string().contains("0101_broken"), "{err}");
    // The cause is the source, not repeated in the message.
    let cause = std::error::Error::source(&err).expect("source").to_string();
    assert!(cause.contains("syntax error"), "{cause}");
    assert!(!err.to_string().contains(&cause), "{err}");

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
    let store = Store::open_with_migrations(&path, StoreOptions::default(), &list, Vec::new())
        .expect("open");
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
    drop(
        Store::open_with_migrations(&path, StoreOptions::default(), &list, Vec::new())
            .expect("reopen"),
    );
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
