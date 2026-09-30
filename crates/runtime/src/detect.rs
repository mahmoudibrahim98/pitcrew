//! Version detection before using the control-mode command formatter.

use std::fmt;
use std::io;
use std::path::Path;
use std::process::{Command, Stdio};
use std::str::FromStr;

/// Minimum portable tmux release supported by this crate.
pub const MIN_TMUX_VERSION: (u32, u32) = (3, 2);

/// OpenBSD version numbers refer to the OS, not portable tmux releases.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VersionKind {
    Release,
    Next,
    OpenBsd,
}

/// Parsed output of `tmux -V`, retaining release suffixes such as `a`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TmuxVersion {
    pub kind: VersionKind,
    pub major: u32,
    pub minor: u32,
    pub suffix: String,
}

impl TmuxVersion {
    /// OpenBSD 6.9 documents the required quoting and control-flow notifications.
    /// Older OS releases are rejected, not compared to portable tmux's numbering.
    pub fn is_supported(&self) -> bool {
        (self.major, self.minor)
            >= match self.kind {
                VersionKind::Release | VersionKind::Next => MIN_TMUX_VERSION,
                VersionKind::OpenBsd => (6, 9),
            }
    }
}

impl FromStr for TmuxVersion {
    type Err = DetectError;

    fn from_str(output: &str) -> Result<Self, Self::Err> {
        let invalid = || DetectError::InvalidVersion(output.to_owned());
        let text = output.trim().strip_prefix("tmux ").ok_or_else(invalid)?;
        let (kind, number) = if let Some(number) = text.strip_prefix("next-") {
            (VersionKind::Next, number)
        } else if let Some(number) = text.strip_prefix("openbsd-") {
            (VersionKind::OpenBsd, number)
        } else {
            (VersionKind::Release, text)
        };
        let (major, rest) = number.split_once('.').ok_or_else(invalid)?;
        let end = rest.bytes().take_while(u8::is_ascii_digit).count();
        let (minor, suffix) = rest.split_at(end);
        if major.is_empty() || !major.bytes().all(|b| b.is_ascii_digit()) || minor.is_empty() {
            return Err(invalid());
        }
        let valid_suffix = match kind {
            VersionKind::OpenBsd => suffix.is_empty() || suffix == "-current",
            _ => {
                suffix.is_empty()
                    || (suffix.len() == 1 && suffix.as_bytes()[0].is_ascii_lowercase())
            }
        };
        if !valid_suffix {
            return Err(invalid());
        }
        Ok(Self {
            kind,
            major: major.parse().map_err(|_| invalid())?,
            minor: minor.parse().map_err(|_| invalid())?,
            suffix: suffix.to_owned(),
        })
    }
}

impl fmt::Display for TmuxVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let prefix = match self.kind {
            VersionKind::Release => "",
            VersionKind::Next => "next-",
            VersionKind::OpenBsd => "openbsd-",
        };
        write!(f, "{prefix}{}.{}{}", self.major, self.minor, self.suffix)
    }
}

/// Reasons to fall back to the PTY runtime instead of attempting control mode.
#[derive(Debug)]
pub enum DetectError {
    Io(io::Error),
    ProbeFailed { code: Option<i32>, stderr: String },
    InvalidVersion(String),
    Unsupported(TmuxVersion),
}

impl fmt::Display for DetectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "tmux version probe: {error}"),
            Self::ProbeFailed { code, stderr } => {
                write!(f, "tmux -V exited with {code:?}: {stderr}")
            }
            Self::InvalidVersion(output) => write!(f, "unrecognized tmux version: {output:?}"),
            Self::Unsupported(version) => write!(
                f,
                "tmux {version} is unsupported; requires tmux >= 3.2 or OpenBSD >= 6.9"
            ),
        }
    }
}

impl std::error::Error for DetectError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

/// Probe the executable directly (no shell). Older or unrecognized versions are
/// errors so the caller can select the PTY runtime. This does not establish a
/// connection or verify a separately running server's version.
pub fn detect_tmux(path: impl AsRef<Path>) -> Result<TmuxVersion, DetectError> {
    let output = Command::new(path.as_ref())
        .arg("-V")
        .stdin(Stdio::null())
        .output()
        .map_err(DetectError::Io)?;
    if !output.status.success() {
        return Err(DetectError::ProbeFailed {
            code: output.status.code(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }
    let text = std::str::from_utf8(&output.stdout).map_err(|_| {
        DetectError::InvalidVersion(String::from_utf8_lossy(&output.stdout).into_owned())
    })?;
    let version: TmuxVersion = text.parse()?;
    if !version.is_supported() {
        return Err(DetectError::Unsupported(version));
    }
    Ok(version)
}
