//! Private directories and files: owned by the current user and closed to everyone else.
//!
//! On Unix every check is explicit: owner equals the effective uid, no group or other access, no
//! symlinks. On Windows, files inherit the ACL of their directory, so a daemon's state and run
//! directories must be under the user's profile (e.g. `%LOCALAPPDATA%`), which only that user
//! (and administrators) can open.

use std::fs;
use std::io;
use std::path::Path;

/// The effective user id of this process.
#[cfg(unix)]
#[must_use]
pub fn euid() -> u32 {
    rustix::process::geteuid().as_raw()
}

/// Creates `dir` (and missing parents) with mode 0700, then checks it with
/// [`check_private_dir`]. An existing directory is never re-permissioned: if it is not already
/// private, this fails.
///
/// # Errors
/// Creating fails, or the directory is not private.
pub fn create_private_dir(dir: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)?;
    }
    #[cfg(not(unix))]
    fs::create_dir_all(dir)?;
    check_private_dir(dir)
}

/// Checks that `dir` is a real directory (not a symlink) and, on Unix, that it is owned by the
/// effective user and has no group or other permissions.
///
/// Clients use this before trusting a daemon's socket directory with a token.
///
/// # Errors
/// `PermissionDenied` (or `InvalidInput` for a non-directory) naming what is wrong.
pub fn check_private_dir(dir: &Path) -> io::Result<()> {
    let meta = fs::symlink_metadata(dir)?;
    if !meta.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} is not a directory", dir.display()),
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        check_owner_and_mode(dir, meta.uid(), meta.mode(), 0o077)?;
    }
    Ok(())
}

/// Fails unless `uid` is ours and `mode` has none of the `forbidden` bits.
#[cfg(unix)]
pub(crate) fn check_owner_and_mode(
    path: &Path,
    uid: u32,
    mode: u32,
    forbidden: u32,
) -> io::Result<()> {
    if uid != euid() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{} is owned by another user (uid {uid})", path.display()),
        ));
    }
    if mode & forbidden != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "{} has mode {:o}; it must not grant {:o}",
                path.display(),
                mode & 0o7777,
                forbidden
            ),
        ));
    }
    Ok(())
}

/// Opens an existing file for reading without following a symlink, and checks that it is a
/// regular file owned by us that nobody else can write. `Ok(None)` if it does not exist.
pub(crate) fn open_private_file(path: &Path) -> io::Result<Option<fs::File>> {
    #[cfg(unix)]
    {
        use rustix::fs::{Mode, OFlags};
        use std::os::unix::fs::MetadataExt as _;
        let fd = match rustix::fs::open(
            path,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        ) {
            Ok(fd) => fd,
            Err(rustix::io::Errno::NOENT) => return Ok(None),
            Err(e) => return Err(io::Error::from(e)),
        };
        let file = fs::File::from(fd);
        let meta = file.metadata()?;
        if !meta.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{} is not a regular file", path.display()),
            ));
        }
        check_owner_and_mode(path, meta.uid(), meta.mode(), 0o022)?;
        Ok(Some(file))
    }
    #[cfg(not(unix))]
    {
        match fs::symlink_metadata(path) {
            Ok(meta) if !meta.is_file() => Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{} is not a regular file", path.display()),
            )),
            Ok(_) => fs::File::open(path).map(Some),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }
}

/// Creates a new file (never an existing one) that only we can read and write.
pub(crate) fn create_new_private_file(path: &Path) -> io::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    options.open(path)
}

/// An exclusive lock on a file, held until dropped. Fails at once if another holder exists, in
/// this process or another.
#[derive(Debug)]
pub(crate) struct ExclusiveLock {
    _file: fs::File,
}

impl ExclusiveLock {
    pub(crate) fn acquire(path: &Path) -> io::Result<Self> {
        let mut options = fs::OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            #[allow(clippy::cast_possible_wrap)] // O_NOFOLLOW is a small positive flag.
            let nofollow = rustix::fs::OFlags::NOFOLLOW.bits() as i32;
            options.mode(0o600).custom_flags(nofollow);
            let file = options.open(path)?;
            rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive)
                .map_err(|e| {
                    if e == rustix::io::Errno::WOULDBLOCK {
                        io::Error::new(io::ErrorKind::WouldBlock, "locked by another holder")
                    } else {
                        io::Error::from(e)
                    }
                })?;
            Ok(Self { _file: file })
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt as _;
            // No sharing: a second open fails with a sharing violation while this one is held.
            options.share_mode(0);
            match options.open(path) {
                Ok(file) => Ok(Self { _file: file }),
                Err(e) if e.raw_os_error() == Some(32) => Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "locked by another holder",
                )),
                Err(e) => Err(e),
            }
        }
        #[cfg(not(any(unix, windows)))]
        {
            Ok(Self {
                _file: options.open(path)?,
            })
        }
    }
}

#[cfg(unix)]
impl Drop for ExclusiveLock {
    /// Unlocks before the file is closed: a process another thread is starting holds a copy of
    /// every descriptor until it runs its program, and an flock lasts while any copy is open.
    /// `LOCK_UN` releases the lock of the open file every copy shares, so a child forked to keep
    /// the lock would lose it here; none is meant to.
    fn drop(&mut self) {
        let _ = rustix::fs::flock(&self._file, rustix::fs::FlockOperation::Unlock);
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    #[test]
    fn a_new_directory_is_0700_and_passes() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("a/b");
        create_private_dir(&dir).unwrap();
        let mode = fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
        create_private_dir(&dir).unwrap();
    }

    #[test]
    fn an_open_directory_is_refused_not_repaired() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("open");
        fs::create_dir(&dir).unwrap();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o770)).unwrap();
        let err = create_private_dir(&dir).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);
        let mode = fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o770);
    }

    #[test]
    fn a_symlinked_directory_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().join("real");
        create_private_dir(&real).unwrap();
        let link = tmp.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert!(check_private_dir(&link).is_err());
    }

    #[test]
    fn private_files_must_not_be_writable_by_others_or_symlinks() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("f");
        fs::write(&file, "x").unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(open_private_file(&file).unwrap().is_some());
        fs::set_permissions(&file, fs::Permissions::from_mode(0o664)).unwrap();
        assert!(open_private_file(&file).is_err());

        let link = tmp.path().join("link");
        std::os::unix::fs::symlink(&file, &link).unwrap();
        assert!(open_private_file(&link).is_err());
        assert!(
            open_private_file(&tmp.path().join("none"))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn a_lock_has_one_holder() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("lock");
        let first = ExclusiveLock::acquire(&path).unwrap();
        let err = ExclusiveLock::acquire(&path).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::WouldBlock);
        drop(first);
        ExclusiveLock::acquire(&path).unwrap();
    }
}
