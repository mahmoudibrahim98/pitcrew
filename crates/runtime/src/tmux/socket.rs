//! The private directory that holds the server's socket.

use std::io;
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, PermissionsExt};
use std::path::Path;

/// macOS limits a socket path to 104 bytes including the NUL (Linux: 108).
const SOCKET_PATH_MAX: usize = 103;

/// Makes sure `socket`'s directory exists, belongs to this user, is a real directory (not a
/// link) and is closed to everyone else, creating it (0700) if missing (its parent must exist);
/// and that `socket` itself is either absent or a socket of this user's. Every directory above
/// it must belong to root or this user, and only a sticky one (like `/tmp`) may be writable by
/// others: otherwise someone else could swap the directory. Checked before every connection,
/// since the directory can be replaced while PitCrew runs.
///
/// Anything else is refused, never repaired: anyone who can reach the socket controls every
/// terminal.
pub(crate) fn ensure_private(socket: &Path) -> Result<(), String> {
    if socket.as_os_str().len() > SOCKET_PATH_MAX {
        return Err(format!(
            "the socket path {} is longer than {SOCKET_PATH_MAX} bytes",
            socket.display()
        ));
    }
    if !socket.is_absolute() {
        return Err(format!(
            "the socket path {} is not absolute",
            socket.display()
        ));
    }
    let Some(dir) = socket.parent() else {
        return Err(format!("{} has no directory", socket.display()));
    };
    let me = rustix::process::getuid().as_raw();
    let Some(parent) = dir.parent() else {
        return Err(format!("{} has no parent directory", dir.display()));
    };
    let real = std::fs::canonicalize(parent)
        .map_err(|e| format!("cannot resolve {}: {e}", parent.display()))?;
    for above in real.ancestors() {
        let meta = std::fs::metadata(above)
            .map_err(|e| format!("cannot inspect {}: {e}", above.display()))?;
        let mode = meta.permissions().mode();
        if meta.uid() != 0 && meta.uid() != me {
            return Err(format!(
                "{} belongs to uid {}, neither root nor this user",
                above.display(),
                meta.uid()
            ));
        }
        if mode & 0o022 != 0 && mode & 0o1000 == 0 {
            return Err(format!(
                "{} can be changed by other users (mode {:03o}) and is not sticky",
                above.display(),
                mode & 0o7777
            ));
        }
    }
    match std::fs::DirBuilder::new().mode(0o700).create(dir) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(format!("cannot create {}: {e}", dir.display())),
    }
    let meta = std::fs::symlink_metadata(dir)
        .map_err(|e| format!("cannot inspect {}: {e}", dir.display()))?;
    if !meta.file_type().is_dir() {
        return Err(format!("{} is not a directory", dir.display()));
    }
    if meta.uid() != me {
        return Err(format!(
            "{} belongs to uid {}, not to this user ({me})",
            dir.display(),
            meta.uid()
        ));
    }
    let mode = meta.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
        return Err(format!(
            "{} is open to other users (mode {mode:03o}); it must be 0700",
            dir.display()
        ));
    }

    match std::fs::symlink_metadata(socket) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("cannot inspect {}: {e}", socket.display())),
        Ok(meta) if meta.file_type().is_socket() && meta.uid() == me => Ok(()),
        Ok(_) => Err(format!(
            "{} is not a socket of this user's",
            socket.display()
        )),
    }
}

/// True if `dir` is a real directory of this user's that nobody else can enter or change.
pub(crate) fn is_private_dir(dir: &Path) -> bool {
    std::fs::symlink_metadata(dir).is_ok_and(|meta| {
        meta.file_type().is_dir()
            && meta.uid() == rustix::process::getuid().as_raw()
            && meta.permissions().mode() & 0o077 == 0
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "pc-sock-{}-{name}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        ));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&dir)
            .expect("scratch dir");
        dir
    }

    #[test]
    fn creates_a_closed_directory_and_refuses_open_or_odd_ones() {
        let base = scratch("modes");
        let socket = base.join("fresh").join("s");
        ensure_private(&socket).expect("created");
        let mode = std::fs::metadata(base.join("fresh"))
            .expect("meta")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o700);
        // Again: an existing private directory is fine.
        ensure_private(&socket).expect("existing");

        let open = base.join("open");
        std::fs::create_dir(&open).expect("dir");
        std::fs::set_permissions(&open, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        let why = ensure_private(&open.join("s")).expect_err("open dir");
        assert!(why.contains("open to other users"), "{why}");

        let file = base.join("file");
        std::fs::write(&file, b"x").expect("file");
        let why = ensure_private(&file.join("s")).expect_err("file");
        assert!(why.contains("not a directory"), "{why}");

        let link = base.join("link");
        std::os::unix::fs::symlink(base.join("fresh"), &link).expect("symlink");
        let why = ensure_private(&link.join("s")).expect_err("symlink");
        assert!(why.contains("not a directory"), "{why}");

        // The socket itself must be absent or a socket.
        std::fs::write(base.join("fresh").join("s"), b"not a socket").expect("file");
        let why = ensure_private(&socket).expect_err("a file at the socket path");
        assert!(why.contains("not a socket"), "{why}");
        std::fs::remove_file(base.join("fresh").join("s")).expect("remove");
        assert!(is_private_dir(&base.join("fresh")));
        assert!(!is_private_dir(&open));
        assert!(!is_private_dir(&link));

        // A directory above that others can write to is refused, unless it is sticky.
        let shared = base.join("shared");
        std::fs::create_dir(&shared).expect("dir");
        std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o777)).expect("chmod");
        let below = shared.join("pc").join("s");
        let why = ensure_private(&below).expect_err("writable ancestor");
        assert!(why.contains("not sticky"), "{why}");
        std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o1777)).expect("chmod");
        ensure_private(&below).expect("sticky ancestor");

        assert!(ensure_private(Path::new("relative/s")).is_err());
        let long = format!("/tmp/{}/s", "x".repeat(120));
        assert!(
            ensure_private(Path::new(&long))
                .expect_err("long")
                .contains("longer")
        );
        std::fs::remove_dir_all(&base).expect("cleanup");
    }
}
