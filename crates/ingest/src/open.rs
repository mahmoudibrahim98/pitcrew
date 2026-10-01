//! Opening a transcript without following a final link.
//!
//! Discovery lists regular files only, but a read comes later: by then the file may have been
//! swapped for a symbolic link (to `~/.ssh/id_ed25519`, say), a named pipe that would block the
//! read, or a directory. So the read opens the path without following a link in its last
//! component and checks what it opened, on the opened handle, so nothing can change between the
//! check and the read:
//! - **Unix:** `O_NOFOLLOW` (a link fails the open), with `O_NONBLOCK` so a named pipe cannot
//!   block it, then `fstat` must say a regular file.
//! - **Windows:** `FILE_FLAG_OPEN_REPARSE_POINT` (the link or junction itself is opened, not its
//!   target), then the handle's attributes must say neither a reparse point nor a directory.
//!
//! Only the last component is checked: folders above it may be links (a home on another drive),
//! as discovery already allows. A refused file is a [`NotRegularFile`] error, which says what was
//! found and never what is in it.

use pitcrew_interfaces::source::SourceError;
use std::fmt;
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};

/// What was found where a transcript should be.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum FileKind {
    /// A symbolic link, or a Windows junction.
    Link,
    /// Another Windows reparse point (a cloud-file placeholder, for instance).
    ReparsePoint,
    /// A directory.
    Directory,
    /// A named pipe (FIFO).
    Fifo,
    /// A Unix domain socket.
    Socket,
    /// A block or character device.
    Device,
    /// Something else that is not a regular file.
    Other,
}

impl fmt::Display for FileKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Link => "a symbolic link",
            Self::ReparsePoint => "a reparse point",
            Self::Directory => "a directory",
            Self::Fifo => "a named pipe",
            Self::Socket => "a socket",
            Self::Device => "a device",
            Self::Other => "something else",
        })
    }
}

/// A transcript path that names something other than a regular file. It is not read. Carried
/// inside [`SourceError::Io`]; find it with [`refusal`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NotRegularFile {
    /// The transcript's path.
    pub path: PathBuf,
    /// What is there instead.
    pub kind: FileKind,
}

impl fmt::Display for NotRegularFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} is {}, not a regular file; it is not read",
            self.path.display(),
            self.kind
        )
    }
}

impl std::error::Error for NotRegularFile {}

/// The refusal inside `error`, if the read failed because the transcript is not a regular file.
#[must_use]
pub fn refusal(error: &SourceError) -> Option<&NotRegularFile> {
    match error {
        SourceError::Io(e) => refusal_in(e),
        SourceError::Unreadable { .. } => None,
    }
}

pub(crate) fn refusal_in(error: &io::Error) -> Option<&NotRegularFile> {
    error.get_ref()?.downcast_ref::<NotRegularFile>()
}

pub(crate) fn refused(path: &Path, kind: FileKind) -> io::Error {
    io::Error::other(NotRegularFile {
        path: path.to_path_buf(),
        kind,
    })
}

/// Opens the transcript at `path` for reading, if it is a regular file and not a link to one.
///
/// # Errors
///
/// A [`NotRegularFile`] (see [`refusal`]) for a link, a named pipe, a directory or anything else
/// that is not a regular file; otherwise the open's own error.
pub(crate) fn open_transcript(path: &Path) -> io::Result<File> {
    imp::open(path, false)
}

/// [`open_transcript`], for a file someone else will open again by its path while this handle is
/// held: on Windows the file then cannot be renamed or deleted, so the path keeps naming the file
/// that was checked (SQLite opens an OpenCode store by its path). On Unix it is the same as
/// [`open_transcript`].
pub(crate) fn hold_transcript(path: &Path) -> io::Result<File> {
    imp::open(path, true)
}

#[cfg(unix)]
mod imp {
    use super::{FileKind, refused};
    use rustix::fs::OFlags;
    use rustix::io::Errno;
    use std::fs::File;
    use std::io;
    use std::os::unix::fs::FileTypeExt;
    use std::path::Path;

    pub(super) fn open(path: &Path, _hold: bool) -> io::Result<File> {
        let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
        let fd = match rustix::fs::open(path, flags, rustix::fs::Mode::empty()) {
            Ok(fd) => fd,
            // A link in the last component: `ELOOP` (Linux, macOS), `EMLINK` (FreeBSD).
            Err(Errno::LOOP | Errno::MLINK) => return Err(refused(path, FileKind::Link)),
            // A socket, or a device with nothing behind it.
            Err(Errno::NXIO) => return Err(refused(path, FileKind::Other)),
            Err(e) => return Err(e.into()),
        };
        let file = File::from(fd);
        let kind = file.metadata()?.file_type();
        if !kind.is_file() {
            let what = if kind.is_dir() {
                FileKind::Directory
            } else if kind.is_fifo() {
                FileKind::Fifo
            } else if kind.is_socket() {
                FileKind::Socket
            } else if kind.is_block_device() || kind.is_char_device() {
                FileKind::Device
            } else {
                FileKind::Other
            };
            return Err(refused(path, what));
        }
        // `O_NONBLOCK` only mattered for the open: reads of a regular file ignore it, but it is
        // cleared so the handle is an ordinary one.
        let now = rustix::fs::fcntl_getfl(&file)?;
        rustix::fs::fcntl_setfl(&file, now.difference(OFlags::NONBLOCK))?;
        Ok(file)
    }
}

#[cfg(windows)]
mod imp {
    use super::{FileKind, refused};
    use std::fs::{File, OpenOptions};
    use std::io;
    use std::os::windows::fs::{MetadataExt as _, OpenOptionsExt as _};
    use std::path::Path;

    // Win32 values (`winbase.h`, `winnt.h`).
    const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
    /// Needed to open a directory at all, so that a directory (or a link to one) is refused as
    /// what it is rather than failing with "access denied".
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    const FILE_SHARE_READ: u32 = 0x1;
    const FILE_SHARE_WRITE: u32 = 0x2;
    const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;

    pub(super) fn open(path: &Path, hold: bool) -> io::Result<File> {
        let mut options = OpenOptions::new();
        options
            .read(true)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS);
        if hold {
            // Without `FILE_SHARE_DELETE`, nobody can rename, delete or replace the file while
            // this handle is open; reading and writing it stay allowed.
            options.share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE);
        }
        let file = options.open(path)?;
        let meta = file.metadata()?;
        let attributes = meta.file_attributes();
        if attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            // Symbolic links and junctions both read as links here.
            let kind = if meta.file_type().is_symlink() {
                FileKind::Link
            } else {
                FileKind::ReparsePoint
            };
            return Err(refused(path, kind));
        }
        if attributes & FILE_ATTRIBUTE_DIRECTORY != 0 {
            return Err(refused(path, FileKind::Directory));
        }
        if !meta.is_file() {
            return Err(refused(path, FileKind::Other));
        }
        Ok(file)
    }
}

#[cfg(not(any(unix, windows)))]
mod imp {
    use std::fs::File;
    use std::io;
    use std::path::Path;

    pub(super) fn open(path: &Path, _hold: bool) -> io::Result<File> {
        File::open(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read as _;

    fn kind_of(result: io::Result<File>) -> FileKind {
        match result {
            Ok(_) => panic!("opened"),
            Err(e) => {
                refusal_in(&e)
                    .unwrap_or_else(|| panic!("not a refusal: {e}"))
                    .kind
            }
        }
    }

    fn contents(result: io::Result<File>) -> String {
        let mut text = String::new();
        result
            .expect("open")
            .read_to_string(&mut text)
            .expect("read");
        text
    }

    #[test]
    fn a_regular_file_opens_and_reads() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("t.jsonl");
        std::fs::write(&path, "{}\n").expect("write");
        assert_eq!(contents(open_transcript(&path)), "{}\n");
        assert_eq!(contents(hold_transcript(&path)), "{}\n");
    }

    #[test]
    fn a_missing_file_is_not_a_refusal() {
        let dir = tempfile::tempdir().expect("tempdir");
        let err = open_transcript(&dir.path().join("gone.jsonl")).expect_err("missing");
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
        assert!(refusal_in(&err).is_none());
    }

    #[test]
    fn a_directory_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("t.jsonl");
        std::fs::create_dir(&path).expect("mkdir");
        assert_eq!(kind_of(open_transcript(&path)), FileKind::Directory);
    }

    #[test]
    fn the_refusal_names_the_path_and_the_kind_only() {
        let err = refused(Path::new("/x/t.jsonl"), FileKind::Link);
        assert_eq!(
            err.to_string(),
            "/x/t.jsonl is a symbolic link, not a regular file; it is not read"
        );
        let source = SourceError::Io(err);
        assert_eq!(
            refusal(&source).map(|r| r.kind),
            Some(FileKind::Link),
            "found through the trait's error"
        );
        assert!(
            refusal(&SourceError::Unreadable {
                path: PathBuf::from("x"),
                reason: "bad".into()
            })
            .is_none()
        );
        assert!(refusal(&SourceError::Io(io::Error::other("other"))).is_none());
    }

    #[cfg(unix)]
    mod unix {
        use super::*;
        use std::os::unix::fs::symlink;
        use std::sync::mpsc;
        use std::time::Duration;

        #[test]
        fn a_link_to_a_file_is_refused_and_its_target_never_shows() {
            let dir = tempfile::tempdir().expect("tempdir");
            let secret = dir.path().join("secret");
            std::fs::write(&secret, "PRIVATE KEY").expect("write");
            let path = dir.path().join("t.jsonl");
            symlink(&secret, &path).expect("link");
            for result in [open_transcript(&path), hold_transcript(&path)] {
                let err = result.expect_err("a link");
                assert_eq!(refusal_in(&err).map(|r| r.kind), Some(FileKind::Link));
                assert!(!err.to_string().contains("PRIVATE"), "{err}");
            }
        }

        #[test]
        fn a_link_to_a_directory_or_to_nothing_is_refused() {
            let dir = tempfile::tempdir().expect("tempdir");
            let path = dir.path().join("t.jsonl");
            symlink(dir.path(), &path).expect("link");
            assert_eq!(kind_of(open_transcript(&path)), FileKind::Link);
            let dangling = dir.path().join("d.jsonl");
            symlink(dir.path().join("nowhere"), &dangling).expect("link");
            assert_eq!(kind_of(open_transcript(&dangling)), FileKind::Link);
        }

        #[test]
        fn a_named_pipe_is_refused_without_blocking() {
            let dir = tempfile::tempdir().expect("tempdir");
            let path = dir.path().join("t.jsonl");
            let made = std::process::Command::new("mkfifo")
                .arg(&path)
                .status()
                .expect("mkfifo");
            assert!(made.success(), "mkfifo failed");
            let (tx, rx) = mpsc::channel();
            let p = path.clone();
            std::thread::spawn(move || {
                let _ = tx.send(open_transcript(&p).map(|_| ()));
            });
            let Ok(result) = rx.recv_timeout(Duration::from_secs(10)) else {
                // Unblock the reader before failing, so nothing is left behind.
                let _ = std::fs::OpenOptions::new().write(true).open(&path);
                panic!("opening a named pipe blocked");
            };
            let err = result.expect_err("a pipe");
            assert_eq!(refusal_in(&err).map(|r| r.kind), Some(FileKind::Fifo));
        }

        #[test]
        fn a_file_under_a_linked_folder_opens() {
            let dir = tempfile::tempdir().expect("tempdir");
            let real = dir.path().join("real");
            std::fs::create_dir(&real).expect("mkdir");
            std::fs::write(real.join("t.jsonl"), "{}\n").expect("write");
            let home = dir.path().join("home");
            symlink(&real, &home).expect("link");
            assert_eq!(contents(open_transcript(&home.join("t.jsonl"))), "{}\n");
        }
    }

    #[cfg(windows)]
    mod windows {
        use super::*;

        /// Creating a symbolic link needs Developer Mode or an elevated process.
        fn link_or_skip(make: io::Result<()>, what: &str) -> bool {
            match make {
                Ok(()) => true,
                // ERROR_PRIVILEGE_NOT_HELD
                Err(e) if e.raw_os_error() == Some(1314) => {
                    eprintln!("skipped: this account may not create {what} ({e})");
                    false
                }
                Err(e) => panic!("cannot create {what}: {e}"),
            }
        }

        /// A junction needs no privilege, but std cannot make one.
        fn junction(link: &Path, target: &Path) {
            let status = std::process::Command::new("cmd")
                .arg("/C")
                .arg("mklink")
                .arg("/J")
                .arg(link)
                .arg(target)
                .stdout(std::process::Stdio::null())
                .status()
                .expect("mklink");
            assert!(status.success(), "mklink /J failed");
        }

        #[test]
        fn a_link_to_a_file_is_refused_where_links_can_be_made() {
            let dir = tempfile::tempdir().expect("tempdir");
            let secret = dir.path().join("secret");
            std::fs::write(&secret, "PRIVATE KEY").expect("write");
            let path = dir.path().join("t.jsonl");
            if !link_or_skip(
                std::os::windows::fs::symlink_file(&secret, &path),
                "a file symlink",
            ) {
                return;
            }
            for result in [open_transcript(&path), hold_transcript(&path)] {
                let err = result.expect_err("a link");
                assert_eq!(refusal_in(&err).map(|r| r.kind), Some(FileKind::Link));
                assert!(!err.to_string().contains("PRIVATE"), "{err}");
            }
        }

        #[test]
        fn a_link_to_a_directory_is_refused_where_links_can_be_made() {
            let dir = tempfile::tempdir().expect("tempdir");
            let path = dir.path().join("t.jsonl");
            if !link_or_skip(
                std::os::windows::fs::symlink_dir(dir.path(), &path),
                "a directory symlink",
            ) {
                return;
            }
            assert_eq!(kind_of(open_transcript(&path)), FileKind::Link);
        }

        #[test]
        fn a_junction_is_refused() {
            let dir = tempfile::tempdir().expect("tempdir");
            let target = dir.path().join("target");
            std::fs::create_dir(&target).expect("mkdir");
            let path = dir.path().join("t.jsonl");
            junction(&path, &target);
            assert_eq!(kind_of(open_transcript(&path)), FileKind::Link);
        }

        #[test]
        fn a_file_under_a_junction_opens() {
            let dir = tempfile::tempdir().expect("tempdir");
            let real = dir.path().join("real");
            std::fs::create_dir(&real).expect("mkdir");
            std::fs::write(real.join("t.jsonl"), "{}\n").expect("write");
            let home = dir.path().join("home");
            junction(&home, &real);
            assert_eq!(contents(open_transcript(&home.join("t.jsonl"))), "{}\n");
        }

        #[test]
        fn a_held_file_cannot_be_replaced_until_it_is_released() {
            let dir = tempfile::tempdir().expect("tempdir");
            let path = dir.path().join("opencode.db");
            std::fs::write(&path, "store").expect("write");
            let other = dir.path().join("other");
            std::fs::write(&other, "other").expect("write");
            let held = hold_transcript(&path).expect("hold");
            assert!(
                std::fs::rename(&other, &path).is_err(),
                "replaced while held"
            );
            assert!(std::fs::remove_file(&path).is_err(), "deleted while held");
            drop(held);
            std::fs::rename(&other, &path).expect("replaced once released");
        }
    }
}
