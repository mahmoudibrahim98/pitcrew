//! Confined runs: what this daemon does for a session PitCrew starts on its own behalf (a board
//! draft's now, the Orchestrator's later), hub-work's `Confinement` carried out. Shared plumbing:
//! every such run gets the same shape, whatever its CLI, and whatever the person's own settings
//! for that CLI say.
//!
//! - **A fresh private folder** ([`ConfinedRuns::folder_of`]): `scratch/<session>` in PitCrew's
//!   cache folder (`~/.cache/pitcrew` on Linux, `~/Library/Caches/PitCrew` on macOS,
//!   `%LOCALAPPDATA%\PitCrew\cache` on Windows), never in the state directory nor the person's
//!   repository, and refused if the two would nest. The scratch folder is 0700 (an owner-only
//!   ACL on Windows); each run's folder is made new for its start (an old one of the same name is
//!   removed first), owner-only too, and removed when the run ends. At each start, anything in
//!   the scratch folder that is not a running run's folder is removed (an injected `CLAUDE.md`
//!   there would otherwise be read by every later run, as the CLIs read their folders' parents);
//!   and all of it when the daemon starts, since no run outlives the daemon's tokens.
//! - **Its files**: `prompt.md` (the request's prompt, which the CLI's argv only names, so it
//!   passes Windows `.cmd` shims and never shows on `/proc/<pid>/cmdline`), and the CLI's
//!   settings ([`claude_settings`], [`opencode_settings`]); Codex has none it would read from an
//!   untrusted folder, and runs read-only instead (see the runner's `start_spec`). All 0600.
//! - **Its token**: a session token (`TokenScope::Session`, `pcs_…`) bound to the session,
//!   minted into the in-memory registry ([`HubTokens`]) and written to `sessions/<session>.token`
//!   in the state directory (0600), which the CLI's `PITCREW_TOKEN_FILE` names instead of its
//!   agent's token file ([`crate::dispatch::AgentEnv`]). It is revoked, and its file removed, as
//!   soon as the run has done its one thing ([`ConfinedRuns::revoke`]), when the run ends, and with
//!   the daemon (it was never on disk).
//! - **Its end** ([`Ender`]): at most the confinement's running time, then its CLI is ended
//!   (gracefully, else killed) and its session with it; when its session ends otherwise (its CLI
//!   exits, a person ends it), its folder and token go within [`POLL`].

use crate::runner::Attached;
use pitcrew_auth::{FileTokenStore, SecretToken, TokenError, TokenId, TokenInfo, TokenStore};
use pitcrew_hub_work::{Confinement, PROMPT_FILE, SessionRequest, WorkService};
use pitcrew_protocol::api::{Caller, TokenScope};
use pitcrew_protocol::ids::{CommandId, SessionId};
use pitcrew_protocol::model::{Engine, SessionState};
use pitcrew_protocol::runner::{CommandOutcome, EndMode, RunnerCommand};
use std::collections::HashMap;
use std::fmt;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex, OnceLock, PoisonError, Weak};
use std::time::{Duration, Instant};

/// How often a run's watch looks at whether its session has ended.
pub const POLL: Duration = Duration::from_secs(5);
/// How long a finished run's CLI is left to say it is done before it is ended.
pub const GRACE: Duration = Duration::from_secs(3);

/// The hub's tokens: the registry's (devices and agents, on disk), and the session tokens of
/// confined runs, in memory only, so none outlives the daemon. What the API verifies against.
#[derive(Debug)]
pub struct HubTokens {
    registry: Arc<FileTokenStore>,
    sessions: Arc<FileTokenStore>,
}

impl HubTokens {
    /// `registry`'s tokens, and the session tokens minted into `sessions` (an in-memory store).
    #[must_use]
    pub fn new(registry: Arc<FileTokenStore>, sessions: Arc<FileTokenStore>) -> Self {
        Self { registry, sessions }
    }
}

impl TokenStore for HubTokens {
    fn verify(&self, token: &str) -> Option<Caller> {
        match pitcrew_auth::token::claimed_prefix(token)? {
            pitcrew_auth::token::SESSION_PREFIX => self.sessions.verify(token),
            _ => self.registry.verify(token),
        }
    }

    fn mint(&self, caller: Caller) -> Result<(TokenInfo, SecretToken), TokenError> {
        match caller.scope {
            TokenScope::Session(_) => self.sessions.mint(caller),
            TokenScope::Device | TokenScope::Agent | TokenScope::Reader => {
                self.registry.mint(caller)
            }
        }
    }

    fn revoke(&self, id: TokenId) -> Result<(), TokenError> {
        match self.sessions.revoke(id) {
            Err(TokenError::NotFound(_)) => self.registry.revoke(id),
            other => other,
        }
    }

    fn rotate(&self, id: TokenId) -> Result<(TokenInfo, SecretToken), TokenError> {
        match self.sessions.rotate(id) {
            Err(TokenError::NotFound(_)) => self.registry.rotate(id),
            other => other,
        }
    }

    fn list(&self) -> Vec<TokenInfo> {
        let mut all = self.registry.list();
        all.extend(self.sessions.list());
        all
    }
}

/// One confined run while it runs.
struct Run {
    folder: PathBuf,
    token: Option<TokenId>,
    token_file: PathBuf,
    /// Dropped when the run is finished: its watch stops.
    _stop: mpsc::Sender<()>,
}

/// The confined runs of this daemon. See the [module docs](self).
pub struct ConfinedRuns {
    /// The scratch folder, or why there is none (no cache folder known, or one that would nest
    /// with the state directory): then no confined run starts.
    root: Result<PathBuf, String>,
    /// The state directory: the session tokens' files are in `sessions/` there, and the CLIs'
    /// settings deny their file tools all of it.
    state: PathBuf,
    /// The session tokens' registry, in memory.
    tokens: Arc<FileTokenStore>,
    runs: Mutex<HashMap<SessionId, Run>>,
    /// One run prepared at a time: a run's tidying never removes another's folder half made.
    preparing: Mutex<()>,
}

impl fmt::Debug for ConfinedRuns {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConfinedRuns")
            .field("root", &self.root)
            .field("state", &self.state)
            .finish_non_exhaustive()
    }
}

/// PitCrew's scratch folder: `scratch` in this user's cache folder for PitCrew.
#[must_use]
pub fn default_root() -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", "PitCrew").map(|dirs| dirs.cache_dir().join("scratch"))
}

impl ConfinedRuns {
    /// Runs in folders under `root` (see [`default_root`]), for the state directory `state`, with
    /// their session tokens in `tokens` (in memory). A `root` that is missing, relative, or nests
    /// with `state` starts no run, and says why.
    #[must_use]
    pub fn new(root: Option<PathBuf>, state: &Path, tokens: Arc<FileTokenStore>) -> Self {
        let state = std::path::absolute(state).unwrap_or_else(|_| state.to_path_buf());
        let root = match root {
            None => Err("this user's cache folder is not known".to_owned()),
            Some(root) if !root.is_absolute() => Err(format!(
                "the scratch folder {} is not an absolute path",
                root.display()
            )),
            Some(root) if root.starts_with(&state) || state.starts_with(&root) => Err(format!(
                "the scratch folder {} and the state directory {} nest; a confined run's folder \
                 must be outside the state directory",
                root.display(),
                state.display()
            )),
            Some(root) => Ok(root),
        };
        if let Err(why) = &root {
            tracing::warn!(why, "confined runs (board drafts) cannot start");
        }
        Self {
            root,
            state,
            tokens,
            runs: Mutex::new(HashMap::new()),
            preparing: Mutex::new(()),
        }
    }

    /// The folder `session`'s run has, if runs can start.
    #[must_use]
    pub fn folder_of(&self, session: &SessionId) -> Option<PathBuf> {
        self.root
            .as_ref()
            .ok()
            .map(|root| root.join(session.0.to_string()))
    }

    /// The token file a running run's CLI gets, if `session` is one.
    #[must_use]
    pub fn token_file(&self, session: &SessionId) -> Option<PathBuf> {
        self.runs().get(session).map(|run| run.token_file.clone())
    }

    /// Whether `session` is a running confined run.
    #[cfg(test)]
    #[must_use]
    pub fn is_running(&self, session: &SessionId) -> bool {
        self.runs().contains_key(session)
    }

    fn runs(&self) -> std::sync::MutexGuard<'_, HashMap<SessionId, Run>> {
        self.runs.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Removes the whole scratch folder: at the daemon's start, when no run can be running (their
    /// tokens were in the last daemon's memory).
    pub fn sweep(&self) {
        let Ok(root) = &self.root else {
            return;
        };
        match std::fs::remove_dir_all(root) {
            Ok(()) => {
                tracing::info!(root = %root.display(), "removed the last daemon's confined runs' folders")
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => {
                tracing::warn!(root = %root.display(), error = %e, "cannot remove the scratch folder")
            }
        }
    }

    /// Prepares `request`'s run: its fresh private folder with its prompt and settings files, and
    /// its session token's file. Its folder, for the CLI's start; or why not (the start is
    /// refused). A run that is prepared is running until [`Self::finish`].
    ///
    /// # Errors
    ///
    /// No scratch folder, an agent without an owner, or a folder or file that cannot be made.
    pub fn prepare(
        &self,
        request: &SessionRequest,
        confinement: &Confinement,
    ) -> Result<(PathBuf, mpsc::Receiver<()>), String> {
        let root = self
            .root
            .as_ref()
            .map_err(|why| format!("confined runs cannot start on this machine: {why}"))?;
        let owner = request.owner.ok_or_else(|| {
            "the agent has no owner, so its run cannot be given a token".to_owned()
        })?;
        let folder = root.join(request.session.0.to_string());
        let _preparing = self
            .preparing
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        self.tidy(root)
            .map_err(|e| format!("cannot prepare the scratch folder {}: {e}", root.display()))?;
        fresh_private_dir(&folder)
            .map_err(|e| format!("cannot make the run's folder {}: {e}", folder.display()))?;
        let written = write_files(&folder, request, confinement, &self.state);
        if let Err(e) = written {
            let _ = std::fs::remove_dir_all(&folder);
            return Err(format!("cannot write the run's files: {e}"));
        }
        let caller = Caller {
            member: request.agent,
            scope: TokenScope::Session(request.session),
            on_behalf_of: Some(owner),
        };
        let token_file = self
            .state
            .join("sessions")
            .join(format!("{}.token", request.session.0));
        let minted = crate::dispatch::private_dir(&self.state.join("sessions"))
            .map_err(|e| e.to_string())
            .and_then(|()| self.tokens.mint(caller).map_err(|e| e.to_string()))
            .and_then(|(info, token)| {
                crate::state::write_token(&token_file, &token)
                    .map(|()| info)
                    .map_err(|e| e.to_string())
            });
        let info = match minted {
            Ok(info) => info,
            Err(e) => {
                let _ = std::fs::remove_dir_all(&folder);
                return Err(format!("cannot give the run its token: {e}"));
            }
        };
        tracing::info!(session = %request.session, token = %info.id, folder = %folder.display(), "prepared a confined run");
        let (stop, stopped) = mpsc::channel();
        self.runs().insert(
            request.session,
            Run {
                folder: folder.clone(),
                token: Some(info.id),
                token_file,
                _stop: stop,
            },
        );
        Ok((folder, stopped))
    }

    /// Removes from the scratch folder whatever is not a running run's folder, making it (0700)
    /// first if it is not there.
    fn tidy(&self, root: &Path) -> io::Result<()> {
        private_root(root)?;
        let keep: Vec<String> = self.runs().keys().map(|s| s.0.to_string()).collect();
        for entry in std::fs::read_dir(root)? {
            let entry = entry?;
            let name = entry.file_name();
            if name.to_str().is_some_and(|n| keep.iter().any(|k| k == n)) {
                continue;
            }
            let path = entry.path();
            let removed = if entry.file_type()?.is_dir() {
                std::fs::remove_dir_all(&path)
            } else {
                std::fs::remove_file(&path)
            };
            match removed {
                Ok(()) => {
                    tracing::debug!(path = %path.display(), "removed a leftover from the scratch folder")
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    /// Revokes `session`'s token now and removes its file: the run has done its one thing. Its
    /// folder stays until [`Self::finish`].
    pub fn revoke(&self, session: &SessionId) {
        let (token, file) = {
            let mut runs = self.runs();
            let Some(run) = runs.get_mut(session) else {
                return;
            };
            (run.token.take(), run.token_file.clone())
        };
        if let Some(id) = token {
            match self.tokens.revoke(id) {
                Ok(()) | Err(TokenError::NotFound(_)) => {
                    tracing::info!(%session, token = %id, "revoked a confined run's token");
                }
                Err(e) => {
                    tracing::warn!(%session, error = %e, "cannot revoke a confined run's token")
                }
            }
        }
        if let Err(e) = crate::state::remove(&file) {
            tracing::warn!(%session, error = %e, "cannot remove a confined run's token file");
        }
    }

    /// Ends `session`'s run here: its token revoked, its token file and folder removed, its watch
    /// stopped. Nothing for a session that is not a running run.
    pub fn finish(&self, session: &SessionId) {
        self.revoke(session);
        let Some(run) = self.runs().remove(session) else {
            return;
        };
        match std::fs::remove_dir_all(&run.folder) {
            Ok(()) => tracing::info!(%session, "removed a confined run's folder"),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => {
                tracing::warn!(%session, folder = %run.folder.display(), error = %e, "cannot remove a confined run's folder")
            }
        }
    }
}

/// Makes the scratch folder if it is not there, and keeps it private: on Unix it must be this
/// user's own real folder (not a link), and is set to 0700 if it is open; on Windows a new one
/// gets an owner-only ACL.
fn private_root(root: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _, PermissionsExt as _};
        if let Some(parent) = root.parent() {
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(parent)?;
        }
        match std::fs::DirBuilder::new().mode(0o700).create(root) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
        let meta = std::fs::symlink_metadata(root)?;
        if !meta.is_dir() || meta.uid() != pitcrew_auth::euid() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("{} is not this user's own folder", root.display()),
            ));
        }
        if meta.mode() & 0o077 != 0 {
            std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o700))?;
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        crate::integrations::secret::private_dir(root)
    }
}

/// Makes `folder` new and private: an old one of that name is removed first, and the new one is
/// made by this call alone (0700; an owner-only ACL on Windows).
fn fresh_private_dir(folder: &Path) -> io::Result<()> {
    match std::fs::symlink_metadata(folder) {
        Ok(meta) if meta.is_dir() => std::fs::remove_dir_all(folder)?,
        Ok(_) => std::fs::remove_file(folder)?,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    #[cfg(windows)]
    {
        pitcrew_trust::windows::create_private_directory(folder)
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        std::fs::DirBuilder::new().mode(0o700).create(folder)
    }
    #[cfg(not(any(unix, windows)))]
    {
        std::fs::create_dir(folder)
    }
}

/// A new file only this user can read and write, with `text`.
fn write_new_private(path: &Path, text: &str) -> io::Result<()> {
    #[cfg(windows)]
    let mut file = pitcrew_trust::windows::create_private_file(path)?;
    #[cfg(not(windows))]
    let mut file = {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        options.open(path)?
    };
    file.write_all(text.as_bytes())?;
    file.sync_all()
}

/// The run's files in its folder: its prompt, and its CLI's settings.
fn write_files(
    folder: &Path,
    request: &SessionRequest,
    confinement: &Confinement,
    state: &Path,
) -> io::Result<()> {
    write_new_private(&folder.join(PROMPT_FILE), &request.brief)?;
    match request.engine {
        Engine::Claude => {
            let settings = folder.join(".claude");
            #[cfg(windows)]
            pitcrew_trust::windows::create_private_directory(&settings)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt as _;
                std::fs::DirBuilder::new().mode(0o700).create(&settings)?;
            }
            #[cfg(not(any(unix, windows)))]
            std::fs::create_dir(&settings)?;
            write_new_private(
                &settings.join("settings.json"),
                &claude_settings(confinement, state),
            )
        }
        Engine::OpenCode => write_new_private(
            &folder.join("opencode.json"),
            &opencode_settings(confinement),
        ),
        _ => Ok(()),
    }
}

/// `path` as Claude Code's permission rules name an absolute path: `//` and the path with `/`
/// separators; on Windows, `C:\Users\…` as `//c/Users/…`.
#[must_use]
pub fn claude_absolute(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    let text = text
        .strip_prefix("//?/")
        .or_else(|| text.strip_prefix("//./"))
        .unwrap_or(&text);
    let bytes = text.as_bytes();
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        let drive = (bytes[0] as char).to_ascii_lowercase();
        return format!("//{drive}{}", &text[2..]);
    }
    format!("/{}", text)
}

/// A JSON string, quoted and escaped.
fn quoted(text: &str) -> String {
    serde_json::Value::String(text.to_owned()).to_string()
}

/// Claude Code's project settings for a confined run (`.claude/settings.json`; with
/// `--setting-sources=project`, the only settings file it reads). In its `default` mode, every
/// tool use not allowed here needs the person's approval in the run's terminal; these are
/// allowed without asking: `pitcrew <command> …` for each of the confinement's commands, and
/// writing each of its files in the folder. Denied outright: web fetch and search, and reading or
/// changing the PitCrew state directory. Bypass mode is disabled.
#[must_use]
pub fn claude_settings(confinement: &Confinement, state: &Path) -> String {
    let mut allow: Vec<String> = confinement
        .commands
        .iter()
        .map(|c| format!("Bash(pitcrew {c}:*)"))
        .collect();
    for file in &confinement.writes {
        allow.push(format!("Write(./{file})"));
        allow.push(format!("Edit(./{file})"));
    }
    let state = format!("{}/**", claude_absolute(state));
    let deny = [
        "WebFetch".to_owned(),
        "WebSearch".to_owned(),
        format!("Read({state})"),
        format!("Edit({state})"),
        format!("Write({state})"),
    ];
    let list = |items: &[String]| {
        items
            .iter()
            .map(|i| format!("      {}", quoted(i)))
            .collect::<Vec<_>>()
            .join(",\n")
    };
    format!(
        "{{\n  \"permissions\": {{\n    \"defaultMode\": \"default\",\n    \
         \"disableBypassPermissionsMode\": \"disable\",\n    \"allow\": [\n{}\n    ],\n    \
         \"deny\": [\n{}\n    ]\n  }},\n  \"enableAllProjectMcpServers\": false\n}}\n",
        list(&allow),
        list(&deny)
    )
}

/// OpenCode's project config for a confined run (`opencode.json`). Its rules are matched last to
/// first, so the order is the point: everything denied, then reading, listing and searching its
/// own folder allowed (`external_directory` denies the rest of the disk), and `pitcrew <command>
/// …` for each of the confinement's commands. Edits, web fetch and search stay denied: it passes
/// its proposal on standard input. The person's global OpenCode config is merged under it.
#[must_use]
pub fn opencode_settings(confinement: &Confinement) -> String {
    let mut bash = vec![format!("      {}: \"deny\"", quoted("*"))];
    for command in &confinement.commands {
        bash.push(format!(
            "      {}: \"allow\"",
            quoted(&format!("pitcrew {command} *"))
        ));
    }
    format!(
        "{{\n  \"$schema\": \"https://opencode.ai/config.json\",\n  \"permission\": {{\n    \
         \"*\": \"deny\",\n    \"read\": \"allow\",\n    \"list\": \"allow\",\n    \
         \"glob\": \"allow\",\n    \"grep\": \"allow\",\n    \
         \"external_directory\": \"deny\",\n    \"edit\": \"deny\",\n    \
         \"webfetch\": \"deny\",\n    \"websearch\": \"deny\",\n    \"bash\": {{\n{}\n    }}\n  \
         }}\n}}\n",
        bash.join(",\n")
    )
}

/// What ends confined runs: the runner's commands, the runs, and the work model (once it is
/// made). Cheap to clone; its threads hold only this.
#[derive(Clone)]
pub struct Ender {
    pub(crate) attached: Arc<Attached>,
    pub(crate) runs: Arc<ConfinedRuns>,
    pub(crate) work: Arc<OnceLock<Weak<WorkService>>>,
}

impl fmt::Debug for Ender {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Ender")
            .field("runs", &self.runs)
            .finish_non_exhaustive()
    }
}

impl Ender {
    /// Ends `session`'s CLI (gracefully, else killed), its run here, and its session in the log
    /// (for a CLI that never reported one).
    pub fn end(&self, session: SessionId, reason: &str) {
        if let Some(runner) = self.attached.get() {
            let graceful = RunnerCommand::EndSession {
                session,
                mode: EndMode::Graceful,
            };
            if !matches!(
                runner.commands.run(CommandId::new(), &graceful),
                CommandOutcome::Ok { .. }
            ) {
                let kill = RunnerCommand::EndSession {
                    session,
                    mode: EndMode::Kill,
                };
                if let CommandOutcome::Failed { error }
                | CommandOutcome::Rejected { reason: error } =
                    runner.commands.run(CommandId::new(), &kill)
                {
                    tracing::debug!(%session, error, "a confined run's CLI was not ended (it may have no terminal)");
                }
            }
        }
        self.runs.finish(&session);
        if let Some(work) = self.work.get().and_then(Weak::upgrade)
            && let Err(e) = work.end_confined_session(&session, reason)
        {
            tracing::warn!(%session, error = %e, "cannot end a confined run's session");
        }
    }

    /// Ends `session` after `wait`, on a thread of its own.
    pub fn end_later(&self, session: SessionId, wait: Duration, reason: &'static str) {
        let ender = self.clone();
        let spawned = std::thread::Builder::new()
            .name("pitcrew-confined-end".into())
            .spawn(move || {
                std::thread::sleep(wait);
                ender.end(session, reason);
            });
        if let Err(e) = spawned {
            tracing::warn!(%session, error = %e, "cannot end a confined run later; ending it now");
            self.end(session, reason);
        }
    }

    /// Watches a started run on a thread of its own: its folder and token go once its session has
    /// ended, and it is ended after `max_runtime`. The watch stops when the run is finished.
    pub fn watch(&self, session: SessionId, max_runtime: Duration, stop: mpsc::Receiver<()>) {
        let ender = self.clone();
        let spawned = std::thread::Builder::new()
            .name("pitcrew-confined-watch".into())
            .spawn(move || {
                let deadline = Instant::now() + max_runtime;
                loop {
                    let left = deadline.saturating_duration_since(Instant::now());
                    match stop.recv_timeout(left.min(POLL)) {
                        Ok(()) | Err(RecvTimeoutError::Disconnected) => return,
                        Err(RecvTimeoutError::Timeout) => {}
                    }
                    let ended = ender
                        .work
                        .get()
                        .and_then(Weak::upgrade)
                        .is_none_or(|work| {
                            work.session(&session)
                                .map_or(true, |s| s.state == SessionState::Ended)
                        });
                    if ended {
                        ender.runs.finish(&session);
                        return;
                    }
                    if Instant::now() >= deadline {
                        tracing::warn!(%session, minutes = max_runtime.as_secs() / 60, "a confined run ran past its time; ending it");
                        ender.end(session, "it ran past its time");
                        return;
                    }
                }
            });
        if let Err(e) = spawned {
            tracing::warn!(%session, error = %e, "cannot watch a confined run");
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use pitcrew_protocol::ids::MemberId;
    use pitcrew_protocol::model::PermissionMode;

    fn confinement() -> Confinement {
        Confinement {
            commands: vec!["board submit".into()],
            writes: vec!["proposal.json".into()],
            max_runtime: Duration::from_secs(30 * 60),
        }
    }

    fn request(engine: Engine) -> SessionRequest {
        SessionRequest {
            session: SessionId::new(),
            agent: MemberId::new(),
            owner: Some(MemberId::new()),
            machine: pitcrew_protocol::ids::MachineId::new(),
            cwd: String::new(),
            branch: None,
            engine,
            persona: None,
            model: None,
            permission_mode: PermissionMode::Default,
            name: "Draft board Synthetic".into(),
            brief: "Synthetic prompt: propose \"tasks\" (with | pipes) & more.\n".into(),
            confinement: Some(confinement()),
        }
    }

    /// A state directory and a scratch folder side by side in a temporary folder.
    fn runs(dir: &Path) -> (ConfinedRuns, PathBuf, PathBuf) {
        let state = dir.join("state");
        std::fs::create_dir_all(&state).unwrap();
        let root = dir.join("cache").join("scratch");
        let runs = ConfinedRuns::new(
            Some(root.clone()),
            &state,
            Arc::new(FileTokenStore::in_memory()),
        );
        (runs, state, root)
    }

    #[cfg(unix)]
    fn mode(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn a_run_gets_a_fresh_private_folder_outside_the_state_directory() {
        let dir = tempfile::tempdir().unwrap();
        let (runs, state, root) = runs(dir.path());
        let request = request(Engine::Claude);
        let folder = runs.folder_of(&request.session).unwrap();
        assert!(folder.starts_with(&root));
        assert!(!folder.starts_with(&state));
        // A leftover of an earlier run of the same name, and an injected file in the scratch
        // folder, are gone before the run starts.
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("CLAUDE.md"), "Synthetic injected instructions").unwrap();
        std::fs::write(root.join("AGENTS.md"), "Synthetic injected instructions").unwrap();
        std::fs::create_dir_all(root.join(".claude")).unwrap();
        let (made, _stop) = runs.prepare(&request, &confinement()).unwrap();
        assert_eq!(made, folder);
        assert!(!folder.join("CLAUDE.md").exists());
        assert!(!root.join("AGENTS.md").exists());
        assert!(!root.join(".claude").exists());
        let mut names: Vec<String> = std::fs::read_dir(&folder)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        assert_eq!(names, [".claude", "prompt.md"]);
        // The prompt, whole, in its file.
        assert_eq!(
            std::fs::read_to_string(folder.join(PROMPT_FILE)).unwrap(),
            request.brief
        );
        #[cfg(unix)]
        {
            assert_eq!(mode(&root), 0o700);
            assert_eq!(mode(&folder), 0o700);
            assert_eq!(mode(&folder.join(PROMPT_FILE)), 0o600);
            assert_eq!(mode(&folder.join(".claude/settings.json")), 0o600);
        }
        // Its token file is in the state directory, private, and names a session token that
        // verifies as the session's agent, bound to the session.
        let file = runs.token_file(&request.session).unwrap();
        assert!(file.starts_with(state.join("sessions")));
        #[cfg(unix)]
        assert_eq!(mode(&file), 0o600);
        let raw = crate::state::read_token(&file).unwrap().unwrap();
        assert!(raw.starts_with("pcs_"));
        let caller = runs.tokens.verify(&raw).unwrap();
        assert_eq!(caller.member, request.agent);
        assert_eq!(caller.scope, TokenScope::Session(request.session));
        assert_eq!(caller.on_behalf_of, request.owner);

        // A second run in the same scratch folder keeps the first's folder.
        let other = super::tests::request(Engine::Codex);
        let (second, _stop2) = runs.prepare(&other, &confinement()).unwrap();
        assert!(folder.is_dir());
        let names: Vec<String> = std::fs::read_dir(&second)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert_eq!(
            names,
            ["prompt.md"],
            "Codex reads no settings file from its folder"
        );

        // Revoked: the token stops, its file goes; the folder stays until the run is finished.
        runs.revoke(&request.session);
        assert_eq!(runs.tokens.verify(&raw), None);
        assert!(!file.exists());
        assert!(folder.is_dir());
        runs.finish(&request.session);
        assert!(!folder.exists());
        assert!(!runs.is_running(&request.session));
        // Finishing twice, or a session that never ran, does nothing.
        runs.finish(&request.session);
        runs.finish(&SessionId::new());
        // A run's start is never in an old run's folder: the same session prepared again is new.
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("leftover"), "synthetic").unwrap();
        let (again, _stop3) = runs.prepare(&request, &confinement()).unwrap();
        assert_eq!(again, folder);
        assert!(!folder.join("leftover").exists());
        // At the daemon's start, all of it goes.
        runs.sweep();
        assert!(!root.exists());
    }

    #[test]
    fn a_scratch_folder_that_nests_with_the_state_directory_starts_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("state");
        for root in [state.join("scratch"), dir.path().to_path_buf()] {
            let runs = ConfinedRuns::new(Some(root), &state, Arc::new(FileTokenStore::in_memory()));
            let request = request(Engine::Claude);
            assert_eq!(runs.folder_of(&request.session), None);
            let error = runs.prepare(&request, &confinement()).unwrap_err();
            assert!(error.contains("nest"), "{error}");
        }
        let runs = ConfinedRuns::new(None, &state, Arc::new(FileTokenStore::in_memory()));
        assert!(
            runs.prepare(&request(Engine::Claude), &confinement())
                .is_err()
        );
    }

    #[test]
    fn an_agent_without_an_owner_gets_no_run() {
        let dir = tempfile::tempdir().unwrap();
        let (runs, ..) = runs(dir.path());
        let mut request = request(Engine::Claude);
        request.owner = None;
        assert!(
            runs.prepare(&request, &confinement())
                .unwrap_err()
                .contains("owner")
        );
        assert!(!runs.is_running(&request.session));
    }

    #[test]
    fn claude_settings_allow_only_the_runs_commands_and_files() {
        let state = if cfg!(windows) {
            PathBuf::from(r"C:\Users\sam\AppData\Local\PitCrew\data")
        } else {
            PathBuf::from("/home/sam/.local/share/pitcrew")
        };
        let text = claude_settings(&confinement(), &state);
        let settings: serde_json::Value = serde_json::from_str(&text).unwrap();
        let state_rule = if cfg!(windows) {
            "//c/Users/sam/AppData/Local/PitCrew/data/**"
        } else {
            "//home/sam/.local/share/pitcrew/**"
        };
        assert_eq!(
            settings["permissions"]["allow"],
            serde_json::json!([
                "Bash(pitcrew board submit:*)",
                "Write(./proposal.json)",
                "Edit(./proposal.json)"
            ])
        );
        assert_eq!(
            settings["permissions"]["deny"],
            serde_json::json!([
                "WebFetch",
                "WebSearch",
                format!("Read({state_rule})"),
                format!("Edit({state_rule})"),
                format!("Write({state_rule})")
            ])
        );
        assert_eq!(settings["permissions"]["defaultMode"], "default");
        assert_eq!(
            settings["permissions"]["disableBypassPermissionsMode"],
            "disable"
        );
        assert_eq!(settings["enableAllProjectMcpServers"], false);
    }

    #[test]
    fn claude_names_absolute_paths_its_own_way() {
        assert_eq!(claude_absolute(Path::new("/home/sam/x")), "//home/sam/x");
        assert_eq!(
            claude_absolute(Path::new(r"C:\Users\sam\x")),
            "//c/Users/sam/x"
        );
        assert_eq!(claude_absolute(Path::new(r"\\?\D:\data\p")), "//d/data/p");
    }

    #[test]
    fn opencode_settings_deny_all_but_reading_its_folder_and_the_runs_commands() {
        let text = opencode_settings(&confinement());
        let settings: serde_json::Value = serde_json::from_str(&text).unwrap();
        let permission = &settings["permission"];
        for (key, want) in [
            ("*", "deny"),
            ("read", "allow"),
            ("external_directory", "deny"),
            ("edit", "deny"),
            ("webfetch", "deny"),
            ("websearch", "deny"),
        ] {
            assert_eq!(permission[key], want, "{key}");
        }
        assert_eq!(permission["bash"]["*"], "deny");
        assert_eq!(permission["bash"]["pitcrew board submit *"], "allow");
        // OpenCode matches its rules last to first: the catch-all denials come first.
        let star = text.find("\"*\": \"deny\"").unwrap();
        assert!(star < text.find("\"read\"").unwrap());
        assert!(
            text.find("      \"*\": \"deny\"").unwrap()
                < text.find("pitcrew board submit").unwrap()
        );
    }

    #[test]
    fn session_tokens_live_in_memory_and_the_rest_in_the_registry() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("state");
        pitcrew_auth::create_private_dir(&state).unwrap();
        let registry = Arc::new(FileTokenStore::open(&state).unwrap());
        let sessions = Arc::new(FileTokenStore::in_memory());
        let hub = HubTokens::new(Arc::clone(&registry), Arc::clone(&sessions));
        let person = Caller {
            member: MemberId::new(),
            scope: TokenScope::Device,
            on_behalf_of: None,
        };
        let session = Caller {
            member: MemberId::new(),
            scope: TokenScope::Session(SessionId::new()),
            on_behalf_of: Some(person.member),
        };
        let (_, device) = hub.mint(person).unwrap();
        let (info, token) = hub.mint(session).unwrap();
        assert_eq!(hub.verify(device.expose()), Some(person));
        assert_eq!(hub.verify(token.expose()), Some(session));
        assert_eq!(registry.list().len(), 1, "a session token is never on disk");
        assert_eq!(sessions.list().len(), 1);
        assert_eq!(hub.list().len(), 2);
        hub.revoke(info.id).unwrap();
        assert_eq!(hub.verify(token.expose()), None);
        assert_eq!(hub.verify(device.expose()), Some(person));
    }
}
