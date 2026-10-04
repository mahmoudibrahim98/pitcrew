//! Credentials (api-v1.md "Integrations", "Credentials"; G.5): a [`Secret`] that never shows
//! itself, kept in a private file of the state directory ([`SecretFiles`]), or read from
//! `gh auth token` on this machine at each sync ([`GhCli`]) and never kept.
//!
//! Nothing here logs a secret, puts one in an error, or hands one to anything but the transport's
//! `Authorization` header (through `pitcrew_sync_github::AuthToken` or
//! `pitcrew_sync_jira::JiraAuth`, which redact themselves too).

use pitcrew_protocol::ids::IntegrationId;
use pitcrew_protocol::integrations::MAX_SECRET_CHARS;
use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::io::{self, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// How long `gh auth token` may take.
const GH_TIMEOUT: Duration = Duration::from_secs(15);

/// A credential's value. No `Display`; `Debug` prints `Secret(***)`.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(***)")
    }
}

impl Secret {
    /// Checks and wraps a secret: 1 to [`MAX_SECRET_CHARS`] characters after trimming, with no
    /// whitespace or control characters inside. `None` otherwise (never saying what it was).
    #[must_use]
    pub fn new(value: &str) -> Option<Self> {
        let value = value.trim();
        let ok = !value.is_empty()
            && value.len() <= MAX_SECRET_CHARS * 4
            && value.chars().count() <= MAX_SECRET_CHARS
            && !value.chars().any(|c| c.is_whitespace() || c.is_control());
        ok.then(|| Self(value.to_owned()))
    }

    /// The value, for the transport's `Authorization` header only.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

/// Where the integrations' secrets are kept: one private file each in a private folder of the
/// state directory (`integrations/<id>.secret`; 0600 in 0700 on Unix, an owner-only DACL on
/// Windows).
#[derive(Debug, Clone)]
pub struct SecretFiles {
    dir: PathBuf,
}

impl SecretFiles {
    /// The secrets in `dir`.
    #[must_use]
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    fn path(&self, id: &IntegrationId) -> PathBuf {
        self.dir.join(format!("{}.secret", id.0))
    }

    /// Whether a secret is kept for `id`.
    #[must_use]
    pub fn has(&self, id: &IntegrationId) -> bool {
        fs::symlink_metadata(self.path(id)).is_ok_and(|m| m.is_file())
    }

    /// Keeps `secret` for `id`, replacing any kept before.
    ///
    /// # Errors
    /// The folder or file cannot be made private, written or renamed.
    pub fn save(&self, id: &IntegrationId, secret: &Secret) -> io::Result<()> {
        private_dir(&self.dir)?;
        let path = self.path(id);
        let tmp = self.dir.join(format!("{}.secret.tmp", id.0));
        let _ = fs::remove_file(&tmp);
        let written = create_private(&tmp).and_then(|mut file| {
            file.write_all(secret.expose().as_bytes())?;
            file.sync_all()?;
            drop(file);
            fs::rename(&tmp, &path)
        });
        if written.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        written
    }

    /// The secret kept for `id`, if any. On Unix the file must be ours and private.
    ///
    /// # Errors
    /// It is not a private regular file, cannot be read, or does not hold a secret.
    pub fn load(&self, id: &IntegrationId) -> io::Result<Option<Secret>> {
        let path = self.path(id);
        let meta = match fs::symlink_metadata(&path) {
            Ok(meta) => meta,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        if !meta.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "the integration's secret is not a regular file",
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;
            if meta.uid() != pitcrew_auth::euid() || meta.mode() & 0o077 != 0 {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "the integration's secret file must be ours and private (mode 600)",
                ));
            }
        }
        let mut text = String::new();
        let max = u64::try_from(MAX_SECRET_CHARS * 4 + 2).unwrap_or(u64::MAX);
        fs::File::open(&path)?.take(max).read_to_string(&mut text)?;
        Secret::new(&text).map(Some).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "the integration's secret file does not hold a secret",
            )
        })
    }

    /// Forgets the secret kept for `id`; none is fine.
    ///
    /// # Errors
    /// Removing it fails.
    pub fn remove(&self, id: &IntegrationId) -> io::Result<()> {
        crate::state::remove(&self.path(id))
    }
}

/// Makes `dir` if it is not there, private to this user: 0700 on Unix; on Windows a new folder
/// gets an owner-only DACL.
///
/// # Errors
/// Creating fails, or an existing folder is not ours and private (Unix).
pub fn private_dir(dir: &Path) -> io::Result<()> {
    #[cfg(windows)]
    {
        if !dir.exists() {
            if let Some(parent) = dir.parent() {
                fs::create_dir_all(parent)?;
            }
            if let Err(e) = pitcrew_trust::windows::create_private_directory(dir)
                && e.kind() != io::ErrorKind::AlreadyExists
            {
                return Err(e);
            }
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        pitcrew_auth::create_private_dir(dir)
    }
}

/// A new file only this user can read and write.
fn create_private(path: &Path) -> io::Result<fs::File> {
    #[cfg(windows)]
    {
        pitcrew_trust::windows::create_private_file(path)
    }
    #[cfg(not(windows))]
    {
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        options.open(path)
    }
}

/// Why `gh auth token` gave no credential. Never holds what `gh` printed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GhError {
    /// No `gh` on `PATH`.
    #[error("the GitHub CLI (gh) is not installed on the hub's machine, or not on its PATH")]
    Missing,
    /// `gh` was found where someone else could have put it.
    #[error("the GitHub CLI (gh) found on PATH is not trusted: {0}")]
    Untrusted(String),
    /// `gh` could not be run, or took too long.
    #[error("the GitHub CLI (gh) could not be run: {0}")]
    Failed(String),
    /// `gh` ran and has no token (not signed in, for that host).
    #[error("the GitHub CLI (gh) is not signed in{0}; run `gh auth login` on the hub's machine")]
    SignedOut(String),
}

/// `gh auth token` on this machine: found on `PATH` (absolute folders only), checked with
/// `pitcrew_trust::check_trusted`, run without a terminal, and read once per sync.
#[derive(Debug, Clone)]
pub struct GhCli {
    path: Option<OsString>,
}

impl GhCli {
    /// `gh` from this process's `PATH`.
    #[must_use]
    pub fn from_env() -> Self {
        Self {
            path: std::env::var_os("PATH"),
        }
    }

    /// `gh` from the given `PATH` value (tests). Only the Unix tests run a stand-in `gh`, so on
    /// Windows nothing calls it.
    #[cfg(all(test, unix))]
    #[must_use]
    pub fn with_path(path: OsString) -> Self {
        Self { path: Some(path) }
    }

    fn find(&self) -> Option<PathBuf> {
        let name = if cfg!(windows) { "gh.exe" } else { "gh" };
        let path = self.path.as_ref()?;
        std::env::split_paths(path)
            .filter(|dir| dir.is_absolute())
            .map(|dir| dir.join(name))
            .find(|candidate| candidate.is_file())
    }

    /// The token `gh` holds for `host` (`github.com`, or an Enterprise host).
    ///
    /// # Errors
    /// See [`GhError`].
    pub async fn token(&self, host: &str) -> Result<Secret, GhError> {
        let program = self.find().ok_or(GhError::Missing)?;
        pitcrew_trust::check_trusted(&program).map_err(GhError::Untrusted)?;
        let mut command = tokio::process::Command::new(&program);
        command.args(["auth", "token"]);
        if host != "github.com" {
            command.args(["--hostname", host]);
        }
        command
            .env("GH_PROMPT_DISABLED", "1")
            .env("NO_COLOR", "1")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);
        #[cfg(windows)]
        {
            // CREATE_NO_WINDOW: no console flashes up for a daemon without one.
            command.creation_flags(0x0800_0000);
        }
        let output = tokio::time::timeout(GH_TIMEOUT, command.output())
            .await
            .map_err(|_| GhError::Failed("it did not answer in time".into()))?
            .map_err(|e| GhError::Failed(e.kind().to_string()))?;
        let host_note = if host == "github.com" {
            String::new()
        } else {
            format!(" for {host}")
        };
        if !output.status.success() {
            return Err(GhError::SignedOut(host_note));
        }
        let text = String::from_utf8(output.stdout).unwrap_or_default();
        Secret::new(&text).ok_or(GhError::SignedOut(host_note))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_secret_never_shows_itself() {
        let secret = Secret::new("  synthetic-value-0001\n").unwrap();
        assert_eq!(secret.expose(), "synthetic-value-0001");
        assert_eq!(format!("{secret:?}"), "Secret(***)");
        for bad in [
            "",
            "   ",
            "two words",
            "tab\there",
            "nul\0",
            &"x".repeat(5000),
        ] {
            assert!(Secret::new(bad).is_none());
        }
    }

    #[test]
    fn secrets_are_kept_privately_and_forgotten() {
        let tmp = tempfile::tempdir().unwrap();
        let files = SecretFiles::new(tmp.path().join("integrations"));
        let id = IntegrationId::new();
        assert!(!files.has(&id));
        assert_eq!(files.load(&id).unwrap(), None);
        let first = Secret::new("synthetic-value-0001").unwrap();
        files.save(&id, &first).unwrap();
        assert!(files.has(&id));
        assert_eq!(files.load(&id).unwrap(), Some(first));
        let second = Secret::new("synthetic-value-0002").unwrap();
        files.save(&id, &second).unwrap();
        assert_eq!(files.load(&id).unwrap(), Some(second));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(&tmp.path().join("integrations")), 0o700);
            let path = tmp.path().join(format!("integrations/{}.secret", id.0));
            assert_eq!(mode(&path), 0o600);
            fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
            assert_eq!(
                files.load(&id).unwrap_err().kind(),
                io::ErrorKind::PermissionDenied
            );
            fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        }
        files.remove(&id).unwrap();
        assert!(!files.has(&id));
        files.remove(&id).unwrap();
        let names: Vec<_> = fs::read_dir(tmp.path().join("integrations"))
            .unwrap()
            .collect();
        assert!(names.is_empty());
    }

    /// A stand-in `gh` in a private temporary folder: never the machine's own.
    #[cfg(unix)]
    fn stand_in(script: &str) -> (tempfile::TempDir, GhCli) {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let gh = dir.path().join("gh");
        fs::write(&gh, script).unwrap();
        fs::set_permissions(&gh, fs::Permissions::from_mode(0o700)).unwrap();
        let cli = GhCli::with_path(dir.path().as_os_str().to_owned());
        (dir, cli)
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn gh_auth_token_is_read_from_a_stand_in() {
        let (_dir, gh) = stand_in(
            "#!/bin/sh\n[ \"$1 $2\" = 'auth token' ] || exit 3\n\
             if [ \"$3\" = --hostname ]; then echo \"synthetic-$4\"; else echo synthetic-github; fi\n",
        );
        assert_eq!(
            gh.token("github.com").await.unwrap().expose(),
            "synthetic-github"
        );
        assert_eq!(
            gh.token("ghe.example.com").await.unwrap().expose(),
            "synthetic-ghe.example.com"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_signed_out_or_missing_gh_says_so_without_its_output() {
        let (_dir, gh) = stand_in("#!/bin/sh\necho 'synthetic-noise' >&2\nexit 1\n");
        let err = gh.token("github.com").await.unwrap_err();
        assert_eq!(err, GhError::SignedOut(String::new()));
        assert!(!err.to_string().contains("synthetic-noise"));

        let empty = tempfile::tempdir().unwrap();
        let none = GhCli::with_path(empty.path().as_os_str().to_owned());
        assert_eq!(
            none.token("github.com").await.unwrap_err(),
            GhError::Missing
        );
        // A relative PATH entry is never searched.
        let relative = GhCli::with_path(OsString::from("."));
        assert_eq!(
            relative.token("github.com").await.unwrap_err(),
            GhError::Missing
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_gh_others_can_change_is_not_run() {
        use std::os::unix::fs::PermissionsExt as _;
        let (dir, gh) = stand_in("#!/bin/sh\necho synthetic-github\n");
        fs::set_permissions(dir.path().join("gh"), fs::Permissions::from_mode(0o777)).unwrap();
        assert!(matches!(
            gh.token("github.com").await.unwrap_err(),
            GhError::Untrusted(_)
        ));
    }
}
