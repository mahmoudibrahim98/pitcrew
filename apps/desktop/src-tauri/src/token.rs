//! Device tokens, and reading one from the daemon's private token file.
//!
//! A [`DeviceToken`] only ever goes into the `Authorization` header or the WebSocket subprotocol
//! of a request to the daemon it belongs to. It has no `Display` and no `Serialize`, and its
//! `Debug` hides it, so it cannot end up in a log line, an error or a command's result by
//! accident.

use std::fmt;
use std::io::{self, Read as _};
use std::path::Path;

/// The longest token accepted, in bytes. The daemon's are about 50.
pub const MAX_TOKEN_LEN: usize = 4096;

/// A device token: a secret.
#[derive(Clone, PartialEq, Eq)]
pub struct DeviceToken(String);

impl DeviceToken {
    /// Wraps `text`, which must look like a token: 1 to 4096 characters of `A-Z a-z 0-9 - . _ ~`.
    /// Anything else could not travel in a header or a subprotocol unchanged.
    ///
    /// # Errors
    /// The text is not a token. The error never repeats it.
    pub fn new(text: impl Into<String>) -> Result<Self, InvalidToken> {
        let text = text.into();
        let ok = (1..=MAX_TOKEN_LEN).contains(&text.len())
            && text
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~'));
        if ok {
            Ok(Self(text))
        } else {
            Err(InvalidToken)
        }
    }

    /// The secret itself, for the one header or subprotocol it goes into.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for DeviceToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("DeviceToken(…)")
    }
}

/// Text that is not a token.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("not a token: expected 1 to 4096 characters of A-Z a-z 0-9 - . _ ~")]
pub struct InvalidToken;

/// Reads the token in `path`, a file the daemon wrote: a regular file (not a link), and on Unix
/// ours and private (mode 600 or stricter). Read afresh on every connection, never copied.
///
/// # Errors
/// The file is missing, is not a private regular file of ours, or does not hold a token. The
/// error never holds the file's content.
pub fn read_token_file(path: &Path) -> io::Result<DeviceToken> {
    let file = open_private(path)?;
    let mut text = String::new();
    file.take(MAX_TOKEN_LEN as u64 + 2)
        .read_to_string(&mut text)
        .map_err(|e| io::Error::new(e.kind(), format!("cannot read {}: {e}", path.display())))?;
    DeviceToken::new(text.trim()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{} does not hold a token", path.display()),
        )
    })
}

#[cfg(unix)]
fn open_private(path: &Path) -> io::Result<std::fs::File> {
    use rustix::fs::{Mode, OFlags};
    use std::os::unix::fs::MetadataExt as _;
    let fd = rustix::fs::open(
        path,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
        Mode::empty(),
    )
    .map_err(|e| {
        let e = io::Error::from(e);
        io::Error::new(
            e.kind(),
            format!("cannot open {} (links are refused): {e}", path.display()),
        )
    })?;
    let file = std::fs::File::from(fd);
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} is not a regular file", path.display()),
        ));
    }
    if meta.uid() != rustix::process::geteuid().as_raw() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("{} is owned by another user", path.display()),
        ));
    }
    if meta.mode() & 0o077 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "{} is open to other users (mode {:o})",
                path.display(),
                meta.mode() & 0o777
            ),
        ));
    }
    Ok(file)
}

/// On Windows the state directory sits under the user's profile, whose ACL the file inherits
/// (`crates/daemon/README.md`); refuse links and anything but a regular file.
#[cfg(not(unix))]
fn open_private(path: &Path) -> io::Result<std::fs::File> {
    let meta = std::fs::symlink_metadata(path)?;
    if !meta.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{} is not a regular file", path.display()),
        ));
    }
    std::fs::File::open(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "pcd_0123456789abcdefABCDEF-_.~";

    #[test]
    fn tokens_are_checked_and_hidden() {
        let token = DeviceToken::new(SAMPLE).unwrap();
        assert_eq!(token.expose(), SAMPLE);
        assert_eq!(format!("{token:?}"), "DeviceToken(…)");
        for bad in [
            "",
            "a b",
            "a,b",
            "a\nb",
            "a\"b",
            "ä",
            &"x".repeat(MAX_TOKEN_LEN + 1),
        ] {
            assert_eq!(DeviceToken::new(bad), Err(InvalidToken), "{bad:?}");
        }
        assert!(!InvalidToken.to_string().contains(SAMPLE));
    }

    #[test]
    fn a_token_file_is_read_and_trimmed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("device.token");
        write_private(&path, &format!("{SAMPLE}\n"));
        assert_eq!(read_token_file(&path).unwrap().expose(), SAMPLE);
    }

    #[test]
    fn a_bad_token_file_is_refused_without_echoing_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("device.token");
        write_private(&path, "not a token, but secret-ish text");
        let err = read_token_file(&path).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(!err.to_string().contains("secret-ish"), "{err}");
        assert!(read_token_file(&dir.path().join("missing")).is_err());
        assert!(read_token_file(dir.path()).is_err(), "a directory");
    }

    #[cfg(unix)]
    #[test]
    fn open_or_linked_token_files_are_refused() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("device.token");
        write_private(&path, SAMPLE);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
        let err = read_token_file(&path).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::PermissionDenied, "{err}");

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let link = dir.path().join("link.token");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert!(read_token_file(&link).is_err());
    }

    fn write_private(path: &Path, text: &str) {
        std::fs::write(path, text).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
    }
}
