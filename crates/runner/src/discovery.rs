//! A bounded directory-layout snapshot for the daemon's file-based adapters.
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

const MAX_DIRS: usize = 4096;

#[derive(Debug, PartialEq, Eq)]
struct Stamp {
    directory: bool,
    modified: SystemTime,
    created: Option<SystemTime>,
    len: u64,
    #[cfg(unix)]
    identity: (u64, u64),
    #[cfg(unix)]
    changed: (i64, i64),
}

fn stamp(path: &Path) -> Option<Stamp> {
    let meta = fs::symlink_metadata(path).ok()?;
    if !(meta.is_dir() || meta.is_file()) || meta.file_type().is_symlink() {
        return None;
    }
    Some(Stamp {
        directory: meta.is_dir(),
        modified: meta.modified().ok()?,
        created: meta.created().ok(),
        len: meta.len(),
        #[cfg(unix)]
        identity: {
            use std::os::unix::fs::MetadataExt as _;
            (meta.dev(), meta.ino())
        },
        #[cfg(unix)]
        changed: {
            use std::os::unix::fs::MetadataExt as _;
            (meta.ctime(), meta.ctime_nsec())
        },
    })
}

#[derive(Debug)]
pub(crate) struct Layout(Vec<(PathBuf, Stamp)>);

impl Layout {
    pub fn capture(home: &Path) -> Option<Self> {
        let mut pending = vec![home.to_path_buf()];
        let mut dirs = Vec::new();
        while let Some(path) = pending.pop() {
            if dirs.len() + pending.len() >= MAX_DIRS {
                return None;
            }
            // Stamp before listing: a directory changed during discovery invalidates next time.
            let before = stamp(&path).filter(|s| s.directory)?;
            for entry in fs::read_dir(&path).ok()? {
                let entry = entry.ok()?;
                let kind = entry.file_type().ok()?;
                // Fall back rather than infer the layout through links or unreadable folders.
                if kind.is_symlink() {
                    return None;
                }
                if kind.is_dir() {
                    if dirs.len() + pending.len() >= MAX_DIRS {
                        return None;
                    }
                    pending.push(entry.path());
                }
            }
            dirs.push((path, before));
        }
        Some(Self(dirs))
    }

    /// Quiet OpenCode databases have no WAL, SHM or rollback journal. Stamp before the adapter
    /// reads them; writes or side-file creation during discovery invalidate the next snapshot.
    /// Used only on Unix, where ctime catches writes that restore mtime.
    pub fn capture_databases(home: &Path) -> Option<Self> {
        let mut layout = Self::capture(home)?;
        for entry in fs::read_dir(home).ok()? {
            let entry = entry.ok()?;
            let name = entry.file_name();
            let name = name.to_str()?;
            if !name.starts_with("opencode") {
                continue;
            }
            if [".db-wal", ".db-shm", ".db-journal"]
                .iter()
                .any(|suffix| name.ends_with(suffix))
            {
                return None;
            }
            if name.ends_with(".db") {
                if layout.0.len() >= MAX_DIRS {
                    return None;
                }
                let path = entry.path();
                let before = stamp(&path).filter(|s| !s.directory)?;
                layout.0.push((path, before));
            }
        }
        Some(layout)
    }

    pub fn unchanged(&self, home: &Path) -> bool {
        self.0.first().is_some_and(|(root, _)| root == home)
            && self
                .0
                .iter()
                .all(|(path, was)| stamp(path).as_ref() == Some(was))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_contents_do_not_change_names_but_nested_creation_and_deletion_do() {
        let home = tempfile::tempdir().unwrap();
        let nested = home.path().join("a/b");
        fs::create_dir_all(&nested).unwrap();
        let file = nested.join("s.jsonl");
        fs::write(&file, b"x").unwrap();
        let layout = Layout::capture(home.path()).unwrap();
        fs::write(&file, b"more lines").unwrap();
        assert!(layout.unchanged(home.path()));
        fs::write(nested.join("new.jsonl"), b"x").unwrap();
        assert!(!layout.unchanged(home.path()));
        let layout = Layout::capture(home.path()).unwrap();
        fs::remove_file(&file).unwrap();
        assert!(!layout.unchanged(home.path()));
        assert!(!layout.unchanged(&nested));
    }

    #[cfg(unix)]
    #[test]
    fn quiet_database_writes_and_sqlite_side_files_disable_skips() {
        let home = tempfile::tempdir().unwrap();
        let db = home.path().join("opencode.db");
        fs::write(&db, b"database").unwrap();
        let layout = Layout::capture_databases(home.path()).unwrap();
        assert!(layout.unchanged(home.path()));
        // A same-size write restoring mtime must still invalidate through ctime.
        let modified = fs::metadata(&db).unwrap().modified().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));
        fs::write(&db, b"changed!").unwrap();
        fs::File::options()
            .write(true)
            .open(&db)
            .unwrap()
            .set_modified(modified)
            .unwrap();
        assert!(!layout.unchanged(home.path()));
        for suffix in ["-wal", "-shm", "-journal"] {
            let side = home.path().join(format!("opencode.db{suffix}"));
            fs::write(&side, b"side").unwrap();
            assert!(Layout::capture_databases(home.path()).is_none());
            fs::remove_file(side).unwrap();
        }
        let layout = Layout::capture_databases(home.path()).unwrap();
        fs::write(home.path().join("opencode-new.db"), b"new").unwrap();
        assert!(
            !layout.unchanged(home.path()),
            "new databases are discovered"
        );
    }

    #[cfg(unix)]
    #[test]
    fn links_disable_caching_and_are_never_followed() {
        let home = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink("missing", home.path().join("link")).unwrap();
        assert!(Layout::capture(home.path()).is_none());
    }
}
