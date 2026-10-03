//! Dispatch in this process: hub-work's `Dispatcher` over the runner's commands
//! ([`RunnerLink`]), the agent token a CLI started for an agent gets ([`AgentEnv`]), the runner's
//! reports followed by the work model ([`FollowingSink`]), and the reconciliation of the sessions
//! the hub stored ahead of the runner ([`reconcile`]).
//!
//! - **The dispatcher** is built over the [`Attached`] runner, made before the `WorkService` (in
//!   `serve`'s `open_with`), so the service's dispatcher is fixed when the service is made, and
//!   it sees the same runner the session routes see, attached at start or once the workspace is
//!   set up. Without one attached (before setup, `--no-runner`, a runner that could not start),
//!   or for a machine other than the runner's, a dispatch answers `503` with the reason and
//!   nothing is recorded (`Dispatcher::can_start`).
//! - **Starting** runs the dispatch's `StartSession` (it names the dispatch's session, which the
//!   runner adopts for the CLI's transcript) in the folder resolved and checked as
//!   `POST /v1/sessions` checks one (`~` is this user's home). The runner refusing (a folder that
//!   is not one, a permission mode it does not allow, a second start in a folder where a CLI
//!   matched by folder still waits) is `409`; a start that fails (no terminal runtime, one that
//!   does not answer) is `503`.
//! - **The CLI's token** ([`AgentEnv`], the runner's `SessionEnv`): a session the hub stored with
//!   an agent gets `PITCREW_TOKEN_FILE`, a private file (`agents/<agent>.token` in the state
//!   directory, 0600) holding an **agent** token bound to that agent and its owner, minted once
//!   and reused while it verifies as exactly that; and where this daemon listens
//!   (`PITCREW_SOCKET`, `PITCREW_PIPE` or `PITCREW_URL`). Never the person's device token, and no
//!   token in the environment itself. **Nothing inherited wins over them:** the terminal runtime
//!   passes the daemon's own environment on, and the CLI reads `PITCREW_TOKEN` before
//!   `PITCREW_TOKEN_FILE`, and a platform's endpoint before `PITCREW_URL`; so `PITCREW_TOKEN` and
//!   the endpoint variables this daemon does not listen on are set empty, which the CLI reads as
//!   unset. A session without an agent gets nothing; one whose agent has no owner, or is not
//!   known, is not started.
//! - **Following** ([`FollowingSink`]): every batch the runner's `StoreSink` stores is handed to
//!   `WorkService::follow_sessions`, which moves the dispatched task on the session's first
//!   `working` or finished transcript turn and finishes the dispatch when its session ends
//!   (see hub-work's "Dispatch").
//! - **Reconciling** ([`reconcile`]): at start, once the runner attaches, and after each start of
//!   a session the hub stored ahead of the runner, the sessions on the runner's machine still
//!   `starting` with no CLI id, plus sessions with active dispatches after adoption, are looked
//!   at, then again with a growing pause (1 s up to a
//!   minute) while any waits. One whose start is under way (this hub is starting it,
//!   [`Attached::starting`], or the runner's command for it has not returned), whose CLI runs, or
//!   whose transcript the runner has, is left to be reported. One the runner has no terminal and
//!   no transcript for (a crash between the dispatch and its start), or whose program ended
//!   before its transcript appeared, twice in a row with a rescan between, is abandoned: its
//!   dispatch fails, it ends, and the runner retires its terminal, so no later transcript is
//!   taken for it. So is one matched by folder whose transcript did not appear within the
//!   runner's claim window (15 minutes): none can be matched to it any more.
//!   A reported CLI that exits without an end hook ends its session and cancels its dispatch;
//!   an adoption still on its way to the hub is allowed to arrive first.

use crate::agents::HubAgents;
use crate::runner::{Attached, Parts};
use crate::state::{read_token, write_token};
use pitcrew_auth::TokenStore;
use pitcrew_hub_work::{DispatchError, DispatchRequest, Dispatcher, WorkService};
use pitcrew_protocol::api::{Caller, TokenScope};
use pitcrew_protocol::events::Event;
use pitcrew_protocol::ids::{CommandId, MachineId, MemberId, SessionId};
use pitcrew_protocol::runner::{CommandOutcome, RunnerCommand};
use pitcrew_runner::{
    EventSink, RunnerCommands, SessionAgent, SessionAgents as _, SessionEnv, SinkError, Started,
    StoreSink,
};
use std::collections::HashSet;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, PoisonError, Weak};
use std::time::Duration;

/// The first pause between looks at sessions waiting for their CLI.
const FIRST_LOOK: Duration = Duration::from_secs(1);
/// The longest pause between looks while a session waits.
const LONGEST_LOOK: Duration = Duration::from_secs(60);
/// The reason an abandoned session's dispatch fails with.
const DID_NOT_START: &str = "its CLI did not start here, or ended before its transcript appeared";
/// The reason a session matched by folder whose transcript came too late fails with.
const TOO_LATE: &str = "its transcript did not appear within 15 minutes of its start, so it can \
                        no longer be told apart from another CLI's in its folder";
/// The variables that say where a daemon listens; the CLI reads the platform's own first.
const ENDPOINTS: [&str; 3] = ["PITCREW_SOCKET", "PITCREW_PIPE", "PITCREW_URL"];

/// hub-work's `Dispatcher` over the runner attached in this process. See the [module docs](self).
#[derive(Debug)]
pub struct RunnerLink {
    attached: Arc<Attached>,
}

impl RunnerLink {
    /// Starts dispatched sessions with the runner `attached` has, once it has one.
    #[must_use]
    pub fn new(attached: Arc<Attached>) -> Self {
        Self { attached }
    }

    /// The runner, if one is attached and runs on `machine`.
    fn runner(&self, machine: &MachineId) -> Result<Parts, DispatchError> {
        let runner = self.attached.get().ok_or_else(|| {
            DispatchError::Unavailable(
                "no runner is attached to this hub (it starts once the workspace is set up, and \
                 not with --no-runner), so it cannot start sessions"
                    .into(),
            )
        })?;
        if runner.machine != *machine {
            return Err(DispatchError::Unavailable(format!(
                "machine {machine} is not this hub's, and this hub cannot reach other machines' \
                 runners yet"
            )));
        }
        Ok(runner.clone())
    }
}

impl Dispatcher for RunnerLink {
    fn can_start(&self, machine: &MachineId) -> Result<(), DispatchError> {
        self.runner(machine).map(|_| ())
    }

    fn start(&self, request: &DispatchRequest) -> Result<(), DispatchError> {
        // From here until the runner answers, the reconciliation leaves the session alone.
        let _starting = self.attached.starting(request.session);
        let runner = self.runner(&request.machine)?;
        let folder = folder(&request.cwd).map_err(DispatchError::Rejected)?;
        let mut command = request.start_command();
        if let RunnerCommand::StartSession { cwd, .. } = &mut command {
            *cwd = folder;
        }
        match runner.commands.run(CommandId::new(), &command) {
            CommandOutcome::Ok { .. } => {
                tracing::info!(dispatch = %request.dispatch, session = %request.session, "started a dispatched session's CLI");
                self.attached.started();
                Ok(())
            }
            CommandOutcome::Rejected { reason } => Err(DispatchError::Rejected(reason)),
            CommandOutcome::Failed { error } => Err(DispatchError::Unavailable(error)),
        }
    }
}

/// A dispatch's folder, as the CLI starts in it: `~` (or `~/…`) in this user's home, then checked
/// and resolved as `POST /v1/sessions` checks a `cwd`. Otherwise why not.
fn folder(cwd: &str) -> Result<String, String> {
    let home = || {
        directories::BaseDirs::new()
            .map(|dirs| dirs.home_dir().to_path_buf())
            .ok_or_else(|| "this user's home folder is not known".to_owned())
    };
    let path = match cwd.strip_prefix('~') {
        Some("") => home()?,
        Some(rest) if rest.starts_with(['/', '\\']) => home()?.join(&rest[1..]),
        _ => PathBuf::from(cwd),
    };
    let text = path
        .to_str()
        .ok_or_else(|| "the folder is not UTF-8".to_owned())?;
    let folder = crate::sessions::checked_cwd(text)?;
    if !folder.group_writable.is_empty() {
        tracing::info!(
            cwd = %folder.path,
            group_writable = ?folder.group_writable,
            "a dispatched session starts in a folder that members of its group can change"
        );
    }
    Ok(folder.path)
}

/// The environment of a CLI the runner starts for a session the hub stored: the agent's token
/// file and where this daemon listens. See the [module docs](self).
pub struct AgentEnv {
    /// Weak: the runner's commands hold this, and the work model holds the runner link, which
    /// reaches those commands.
    work: Weak<WorkService>,
    tokens: Arc<dyn TokenStore>,
    /// `agents/` in the state directory.
    dir: PathBuf,
    /// The variable that says where this daemon listens, once it does.
    endpoint: OnceLock<(&'static str, String)>,
    /// One token file written at a time.
    writing: Mutex<()>,
}

impl fmt::Debug for AgentEnv {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AgentEnv")
            .field("dir", &self.dir)
            .field("endpoint", &self.endpoint.get())
            .finish_non_exhaustive()
    }
}

impl AgentEnv {
    /// Tokens minted in `tokens`, written to files in `dir`, for the agents `work` says sessions
    /// run as.
    #[must_use]
    pub fn new(work: &Arc<WorkService>, tokens: Arc<dyn TokenStore>, dir: PathBuf) -> Self {
        Self {
            work: Arc::downgrade(work),
            tokens,
            dir,
            endpoint: OnceLock::new(),
            writing: Mutex::new(()),
        }
    }

    /// This daemon listens there now: `variable` (`PITCREW_SOCKET`, `PITCREW_PIPE` or
    /// `PITCREW_URL`) is `value` for every CLI started from now on.
    pub fn listening(&self, variable: &'static str, value: String) {
        let _ = self.endpoint.set((variable, value));
    }

    /// The file holding a token for `agent`, acting for `owner`: the one there while it verifies
    /// as exactly that, else a new token minted into it. Only its id is logged.
    fn token_file(&self, agent: MemberId, owner: MemberId) -> anyhow::Result<PathBuf> {
        let want = Caller {
            member: agent,
            scope: TokenScope::Agent,
            on_behalf_of: Some(owner),
        };
        let path = self.dir.join(format!("{}.token", agent.0));
        let _writing = self.writing.lock().unwrap_or_else(PoisonError::into_inner);
        if let Ok(Some(raw)) = read_token(&path)
            && self.tokens.verify(&raw) == Some(want)
        {
            return Ok(path);
        }
        private_dir(&self.dir)?;
        let (info, token) = self.tokens.mint(want)?;
        write_token(&path, &token)?;
        tracing::info!(token = %info.id, %agent, path = %path.display(), "minted an agent token for the sessions it runs");
        Ok(path)
    }
}

impl SessionEnv for AgentEnv {
    fn env_for(&self, session: SessionId) -> Result<Vec<(String, String)>, String> {
        let work = self
            .work
            .upgrade()
            .ok_or_else(|| "the hub is stopping".to_owned())?;
        let (agent, owner) = match HubAgents::new(work).agent_of(session) {
            SessionAgent::NoAgent => return Ok(Vec::new()),
            SessionAgent::Unknown => {
                return Err(format!(
                    "who session {session} runs as is not known, so its CLI cannot be given a \
                     token"
                ));
            }
            SessionAgent::Agent { owner: None, .. } => {
                return Err(format!(
                    "the agent session {session} runs as has no owner, so it cannot be given a \
                     token"
                ));
            }
            SessionAgent::Agent {
                agent,
                owner: Some(owner),
            } => (agent, owner),
        };
        let file = self.token_file(agent, owner).map_err(|e| {
            tracing::error!(%agent, error = %format!("{e:#}"), "cannot write an agent's token file");
            "the agent's token file cannot be written".to_owned()
        })?;
        let file = file
            .into_os_string()
            .into_string()
            .map_err(|_| "the state directory's path is not UTF-8".to_owned())?;
        // Empty is unset to the CLI: an inherited token or endpoint must not win over these.
        let mut env = vec![
            ("PITCREW_TOKEN".to_owned(), String::new()),
            ("PITCREW_TOKEN_FILE".to_owned(), file),
        ];
        let listening = self.endpoint.get();
        for variable in ENDPOINTS {
            let value = match listening {
                Some((at, value)) if *at == variable => value.clone(),
                _ => String::new(),
            };
            env.push((variable.to_owned(), value));
        }
        Ok(env)
    }
}

/// Makes `dir` if it is not there, private to this user (0700 on Unix).
fn private_dir(dir: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)?;
        let mode = std::fs::metadata(dir)?.permissions().mode();
        if mode & 0o077 != 0 {
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        // Files take the ACL of the state directory, which is under the user's profile.
        std::fs::create_dir_all(dir)
    }
}

/// The runner's `StoreSink`, followed by the work model: each batch the store took is handed to
/// `WorkService::follow_sessions`. A failure there is logged; the batch stays stored.
pub struct FollowingSink {
    inner: StoreSink,
    work: Arc<WorkService>,
}

impl FollowingSink {
    /// `inner`'s batches, followed by `work`.
    #[must_use]
    pub fn new(inner: StoreSink, work: Arc<WorkService>) -> Self {
        Self { inner, work }
    }
}

impl fmt::Debug for FollowingSink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FollowingSink")
            .field("inner", &self.inner)
            .finish_non_exhaustive()
    }
}

impl EventSink for FollowingSink {
    fn accept(&self, events: &[Event]) -> Result<(), SinkError> {
        self.inner.accept(events)?;
        if let Err(e) = self.work.follow_sessions(events) {
            tracing::warn!(error = %e, "the work model could not follow what the runner reported");
        }
        Ok(())
    }
}

/// Looks at the sessions the hub stored ahead of the runner until the daemon stops. See the
/// [module docs](self). Holds the work model only while it looks.
pub async fn reconcile(work: Weak<WorkService>, attached: Arc<Attached>) {
    let mut gone: HashSet<SessionId> = HashSet::new();
    let mut pause = FIRST_LOOK;
    loop {
        let Some(waiting) = look(&work, &attached, &mut gone).await else {
            return;
        };
        if waiting {
            tokio::select! {
                () = tokio::time::sleep(pause) => pause = (pause * 2).min(LONGEST_LOOK),
                () = attached.next_start() => pause = FIRST_LOOK,
            }
        } else {
            attached.next_start().await;
            pause = FIRST_LOOK;
        }
    }
}

/// What the reconciliation asks of the runner. [`pitcrew_runner::RunnerCommands`] here; tests
/// stand in for it.
trait Starts: Send + Sync + 'static {
    /// Where `session` stands on the runner (`RunnerCommands::started`). Blocking.
    fn started(&self, session: SessionId) -> Started;
    /// Look for new transcripts now.
    fn rescan(&self);
    /// The hub gave up on `session`: forget its terminal if its program ended
    /// (`RunnerCommands::retire`). Blocking.
    fn retire(&self, session: SessionId);
}

impl Starts for RunnerCommands {
    fn started(&self, session: SessionId) -> Started {
        RunnerCommands::started(self, session)
    }

    fn rescan(&self) {
        self.run(CommandId::new(), &RunnerCommand::Scan { roots: Vec::new() });
    }

    fn retire(&self, session: SessionId) {
        if let Err(e) = RunnerCommands::retire(self, session) {
            tracing::warn!(%session, error = %e, "cannot retire the terminal of a session whose CLI did not start");
        }
    }
}

/// One look: `None` once the work model is gone (the daemon stops); else whether a session still
/// waits for its CLI. `gone` holds the sessions found gone at the last look.
async fn look(
    work: &Weak<WorkService>,
    attached: &Attached,
    gone: &mut HashSet<SessionId>,
) -> Option<bool> {
    let Some(runner) = attached.get().cloned() else {
        return work.upgrade().map(|_| false);
    };
    let starts: Arc<dyn Starts> = Arc::new(runner.commands);
    look_at(work, attached, runner.machine, &starts, gone).await
}

/// [`look`], at the sessions of `machine`, whose runner `starts` answers for.
async fn look_at(
    work: &Weak<WorkService>,
    attached: &Attached,
    machine: MachineId,
    starts: &Arc<dyn Starts>,
    gone: &mut HashSet<SessionId>,
) -> Option<bool> {
    let work = work.upgrade()?;
    let reading = Arc::clone(&work);
    let sessions =
        match tokio::task::spawn_blocking(move || reading.reconciling_sessions(&machine)).await {
            Ok(Ok(sessions)) => sessions,
            Ok(Err(e)) => {
                tracing::warn!(error = %e, "cannot list the sessions waiting for their CLI");
                return Some(true);
            }
            Err(e) => {
                tracing::warn!(error = %e, "listing the sessions waiting for their CLI failed");
                return Some(true);
            }
        };
    gone.retain(|id| sessions.iter().any(|s| s.id == *id));
    let mut waiting = false;
    let mut rescan = false;
    for session in sessions {
        let id = session.id;
        // This hub is starting it: however long the runner takes, it is not one that did not
        // start. Asked again once the start returns.
        if attached.is_starting(id) {
            gone.remove(&id);
            waiting = true;
            continue;
        }
        let asking = Arc::clone(starts);
        let started = tokio::task::spawn_blocking(move || asking.started(id))
            .await
            .unwrap_or_else(|e| Started::Unknown(e.to_string()));
        let reason = match started {
            Started::Gone if !session.native_id.is_empty() => {
                // The hub may hold imported/demo sessions this runner never started. Only an
                // observed terminal exit (Exited) ends an adopted dispatch.
                gone.remove(&id);
                waiting = true;
                continue;
            }
            Started::Exited if !session.native_id.is_empty() => DID_NOT_START,
            Started::Exited => {
                // Adoption has reached the runner's index but its re-statement has not reached
                // the hub yet. Do not fail a dispatch for a transcript already found.
                gone.remove(&id);
                waiting = true;
                continue;
            }
            Started::Gone if gone.remove(&id) => DID_NOT_START,
            Started::TooLate => TOO_LATE,
            Started::Gone => {
                // Its transcript may be there, not found yet: looked for before the next look.
                gone.insert(id);
                waiting = true;
                rescan = true;
                continue;
            }
            // Reported: its re-statement is on its way to the store.
            Started::Running | Started::Reported => {
                gone.remove(&id);
                waiting = true;
                continue;
            }
            Started::Unknown(why) => {
                gone.remove(&id);
                tracing::debug!(session = %id, why, "cannot tell yet whether a session's CLI started");
                waiting = true;
                continue;
            }
        };
        let (abandoning, retiring) = (Arc::clone(&work), Arc::clone(starts));
        let abandoned = tokio::task::spawn_blocking(move || {
            let abandoned = if session.native_id.is_empty() {
                abandoning.abandon_session(&id, reason)
            } else {
                abandoning.dispatched_cli_exited(&id)
            };
            if abandoned.is_ok() {
                retiring.retire(id);
            }
            abandoned
        })
        .await;
        match abandoned {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                tracing::warn!(session = %id, error = %e, "cannot end a session whose CLI did not start")
            }
            Err(e) => {
                tracing::warn!(session = %id, error = %e, "ending a session whose CLI did not start failed")
            }
        }
    }
    if rescan {
        let starts = Arc::clone(starts);
        drop(tokio::task::spawn_blocking(move || starts.rescan()));
    }
    Some(waiting)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::Runner;
    use pitcrew_auth::FileTokenStore;
    use pitcrew_protocol::events::EventBody;
    use pitcrew_protocol::ids::{DispatchId, TaskId, TaskKey, WorkspaceId};
    use pitcrew_protocol::model::{
        Engine, Member, MemberKind, PermissionMode, Session, SessionState, Workspace,
    };
    use pitcrew_store::{Store, StoreOptions};

    fn member(kind: MemberKind, handle: &str, owner: Option<MemberId>) -> Member {
        Member {
            id: MemberId::new(),
            kind,
            handle: handle.into(),
            name: handle.trim_start_matches('@').into(),
            owner,
            persona: None,
        }
    }

    fn session(agent: Option<MemberId>) -> Session {
        Session {
            id: SessionId::new(),
            engine: Engine::Claude,
            native_id: String::new(),
            machine: MachineId::new(),
            cwd: "/w".into(),
            branch: None,
            title: None,
            agent,
            workstream: None,
            task: None,
            link_basis: None,
            state: SessionState::Starting,
            status_line: None,
            started: 1,
            last_activity: 1,
            terminal: None,
            parent: None,
        }
    }

    /// A work model over a store in `dir` holding `members` and `sessions`.
    fn work(dir: &Path, members: &[&Member], sessions: &[&Session]) -> Arc<WorkService> {
        let store = Arc::new(
            Store::open_with(
                dir.join("hub.db"),
                StoreOptions::default(),
                pitcrew_hub_work::projections(),
            )
            .unwrap(),
        );
        let workspace = Workspace {
            id: WorkspaceId::new(),
            name: "Lab".into(),
        };
        let mut bodies: Vec<EventBody> = members
            .iter()
            .map(|m| EventBody::MemberAdded {
                member: (*m).clone(),
            })
            .collect();
        bodies.extend(sessions.iter().map(|s| EventBody::SessionDiscovered {
            session: (*s).clone(),
        }));
        let author = members[0].id;
        let events: Vec<Event> = bodies
            .into_iter()
            .map(|b| Event::now(workspace.id, author, b))
            .collect();
        store.append(&events).unwrap();
        Arc::new(WorkService::new(store, workspace))
    }

    /// A CLI started for a session run as an agent gets a file holding an agent token for that
    /// agent and its owner (private, minted once and reused), and where the daemon listens; never
    /// the token itself. A session without an agent gets nothing; one whose agent has no owner, or
    /// is not known, is refused.
    #[test]
    fn an_agents_cli_gets_its_own_token_file() {
        let tmp = tempfile::tempdir().unwrap();
        let sam = member(MemberKind::Human, "@sam", None);
        let writer = member(MemberKind::Agent, "@writer", Some(sam.id));
        let orphan = member(MemberKind::Agent, "@orphan", None);
        let (mine, free, orphaned) = (
            session(Some(writer.id)),
            session(None),
            session(Some(orphan.id)),
        );
        let work = work(
            tmp.path(),
            &[&sam, &writer, &orphan],
            &[&mine, &free, &orphaned],
        );
        let tokens: Arc<dyn TokenStore> = Arc::new(FileTokenStore::in_memory());
        let device = tokens
            .mint(Caller {
                member: sam.id,
                scope: TokenScope::Device,
                on_behalf_of: None,
            })
            .unwrap()
            .1;
        let dir = tmp.path().join("agents");
        let env = AgentEnv::new(&work, Arc::clone(&tokens), dir.clone());

        let first = env.env_for(mine.id).unwrap();
        let var = |env: &[(String, String)], name: &str| {
            let values: Vec<&str> = env
                .iter()
                .filter(|(k, _)| k == name)
                .map(|(_, v)| v.as_str())
                .collect();
            assert_eq!(values.len(), 1, "{name} once: {env:?}");
            values[0].to_owned()
        };
        // Not listening yet: no endpoint, and none inherited either.
        for name in [
            "PITCREW_TOKEN",
            "PITCREW_SOCKET",
            "PITCREW_PIPE",
            "PITCREW_URL",
        ] {
            assert_eq!(var(&first, name), "", "{name} is set empty: {first:?}");
        }
        assert_eq!(first.len(), 5, "{first:?}");
        let file = PathBuf::from(var(&first, "PITCREW_TOKEN_FILE"));
        assert_eq!(file, dir.join(format!("{}.token", writer.id.0)));
        let token = read_token(&file).unwrap().unwrap();
        assert_ne!(token, device.expose());
        assert_eq!(
            tokens.verify(&token),
            Some(Caller {
                member: writer.id,
                scope: TokenScope::Agent,
                on_behalf_of: Some(sam.id),
            })
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(&file), 0o600);
            assert_eq!(mode(&dir), 0o700);
        }
        assert!(first.iter().all(|(_, v)| !v.contains(&token)));

        // Reused while it verifies; with where the daemon listens once it does, and the other
        // endpoints and the token variable still empty, so an inherited one never wins.
        env.listening("PITCREW_URL", "http://127.0.0.1:47317".into());
        let again = env.env_for(mine.id).unwrap();
        assert_eq!(
            var(&again, "PITCREW_TOKEN_FILE"),
            var(&first, "PITCREW_TOKEN_FILE")
        );
        assert_eq!(read_token(&file).unwrap().unwrap(), token);
        assert_eq!(var(&again, "PITCREW_URL"), "http://127.0.0.1:47317");
        for name in ["PITCREW_TOKEN", "PITCREW_SOCKET", "PITCREW_PIPE"] {
            assert_eq!(var(&again, name), "", "{name}: {again:?}");
        }
        // As the CLI reads them, over an environment that already had others.
        let inherited = [
            ("PITCREW_TOKEN", device.expose().to_owned()),
            ("PITCREW_SOCKET", "/home/sam/elsewhere".to_owned()),
            ("PITCREW_PIPE", r"\\.\pipe\elsewhere".to_owned()),
        ];
        let seen = |name: &str| {
            again
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.clone())
                .or_else(|| {
                    inherited
                        .iter()
                        .find(|(k, _)| *k == name)
                        .map(|(_, v)| v.clone())
                })
                .map(std::ffi::OsString::from)
        };
        let read = pitcrew_cli::config::token_from_env(&seen).unwrap();
        assert_eq!(read, token, "the agent's token, not the inherited one");
        assert!(matches!(
            pitcrew_cli::config::Endpoint::from_env(&seen).unwrap(),
            pitcrew_cli::config::Endpoint::Tcp { .. }
        ));
        assert_eq!(tokens.list().len(), 2, "the device's and one agent token");
        // A token that no longer verifies is replaced.
        std::fs::write(&file, "pc_not_a_token\n").unwrap();
        env.env_for(mine.id).unwrap();
        let replaced = read_token(&file).unwrap().unwrap();
        assert_ne!(replaced, token);
        assert_eq!(tokens.verify(&replaced).map(|c| c.member), Some(writer.id));

        assert_eq!(env.env_for(free.id).unwrap(), Vec::new());
        assert!(env.env_for(orphaned.id).unwrap_err().contains("no owner"));
        // Not stored: no agent yet, so nothing.
        assert_eq!(env.env_for(SessionId::new()).unwrap(), Vec::new());
    }

    fn request(machine: MachineId, cwd: &str) -> DispatchRequest {
        DispatchRequest {
            dispatch: DispatchId::new(),
            session: SessionId::new(),
            task: TaskId::new(),
            key: "PAP-5".parse::<TaskKey>().unwrap(),
            workstream: None,
            agent: MemberId::new(),
            owner: None,
            machine,
            cwd: cwd.into(),
            branch: None,
            engine: Engine::Claude,
            persona: None,
            model: None,
            permission_mode: PermissionMode::Default,
            name: "PAP-5 Seed runs".into(),
            brief: "Go".into(),
        }
    }

    /// Without a runner, or for another machine, a dispatch cannot start (`503`, nothing
    /// recorded); a folder that is not one is refused (`409`); one the runtime cannot start in
    /// is unavailable.
    #[test]
    fn the_runner_link_starts_only_where_its_runner_runs() {
        let tmp = tempfile::tempdir().unwrap();
        let attached = Arc::new(Attached::default());
        let link = RunnerLink::new(Arc::clone(&attached));
        let machine = MachineId::new();
        let unavailable = |r: Result<(), DispatchError>, says: &str| {
            assert!(
                matches!(&r, Err(DispatchError::Unavailable(why)) if why.contains(says)),
                "{r:?}"
            );
        };
        unavailable(link.can_start(&machine), "no runner");
        let runner = Runner::idle(&tmp.path().join("runner"));
        let here = runner.parts().machine;
        attached.set(runner.parts());
        unavailable(link.can_start(&machine), "not this hub's");
        assert_eq!(link.can_start(&here), Ok(()));
        unavailable(link.start(&request(machine, "/")), "not this hub's");
        let missing = tmp.path().join("missing");
        assert!(matches!(
            link.start(&request(here, missing.to_str().unwrap())),
            Err(DispatchError::Rejected(_))
        ));
        // The idle runner has no terminal runtime.
        let work = tmp.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        assert!(matches!(
            link.start(&request(here, work.to_str().unwrap())),
            Err(DispatchError::Unavailable(_))
        ));
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(runner.stop(Duration::from_secs(10)));
    }

    /// `~` is this user's home, checked as any folder is.
    #[test]
    fn a_dispatch_without_a_folder_runs_in_the_home() {
        let home = directories::BaseDirs::new()
            .unwrap()
            .home_dir()
            .to_path_buf();
        let Ok(checked) = crate::sessions::checked_cwd(home.to_str().unwrap()) else {
            eprintln!("skipped: this user's home is not a folder a session may start in");
            return;
        };
        assert_eq!(folder("~").unwrap(), checked.path);
        assert!(folder("relative").is_err());
    }

    /// Answers the reconciliation as told, and notes what it was asked.
    #[derive(Debug, Default)]
    struct Told {
        answers: Mutex<std::collections::HashMap<SessionId, Started>>,
        asked: Mutex<Vec<SessionId>>,
        retired: Mutex<Vec<SessionId>>,
    }

    impl Starts for Told {
        fn started(&self, session: SessionId) -> Started {
            self.asked.lock().unwrap().push(session);
            self.answers
                .lock()
                .unwrap()
                .get(&session)
                .cloned()
                .unwrap_or(Started::Gone)
        }
        fn rescan(&self) {}
        fn retire(&self, session: SessionId) {
            self.retired.lock().unwrap().push(session);
        }
    }

    #[test]
    fn an_unrelated_empty_transcript_does_not_block_dispatch_retirement() {
        use pitcrew_interfaces::source::{
            Cursor, ParseChunk, SourceAdapter, SourceError, TranscriptPage, TranscriptRef,
        };

        // Model an adapter that cannot read session metadata from the empty old transcript.
        struct MissingMetadata(pitcrew_ingest::claude::ClaudeAdapter);
        impl SourceAdapter for MissingMetadata {
            fn engine(&self) -> Engine {
                Engine::Claude
            }
            fn discover(&self, home: &Path) -> Result<Vec<TranscriptRef>, SourceError> {
                self.0.discover(home)
            }
            fn read_from(
                &self,
                transcript: &TranscriptRef,
                _: &Cursor,
            ) -> Result<ParseChunk, SourceError> {
                Err(SourceError::Unreadable {
                    path: transcript.path.clone(),
                    reason: "session metadata is missing".into(),
                })
            }
            fn read_page(
                &self,
                transcript: &TranscriptRef,
                before: Option<u64>,
                limit: usize,
            ) -> Result<TranscriptPage, SourceError> {
                self.0.read_page(transcript, before, limit)
            }
        }
        for engine in [Engine::Codex, Engine::Claude] {
            let tmp = tempfile::tempdir().unwrap();
            let sam = member(MemberKind::Human, "@sam", None);
            let writer = member(MemberKind::Agent, "@writer", Some(sam.id));
            let mut pending = session(Some(writer.id));
            pending.engine = engine;
            let folder = tmp.path().join("work");
            std::fs::create_dir_all(&folder).unwrap();
            pending.cwd = folder.to_str().unwrap().to_owned();
            let work = work(tmp.path(), &[&sam, &writer], &[&pending]);
            let dispatch = pitcrew_protocol::model::Dispatch {
                id: DispatchId::new(),
                task: TaskId::new(),
                agent: writer.id,
                session: Some(pending.id),
                brief: "Submit the seeds".into(),
                started: 1,
                ended: None,
                outcome: None,
                summary: None,
            };
            work.store()
                .append(&[Event::now(
                    work.workspace(),
                    sam.id,
                    EventBody::DispatchStarted {
                        dispatch: dispatch.clone(),
                    },
                )])
                .unwrap();
            let claude = tmp.path().join("claude");
            let codex = tmp.path().join("codex");
            let empty = claude.join("projects/old/empty.jsonl");
            std::fs::create_dir_all(empty.parent().unwrap()).unwrap();
            std::fs::write(empty, b"").unwrap();
            let config = pitcrew_runner::RunnerConfig::new(
                work.workspace(),
                pending.machine,
                sam.id,
                tmp.path().join("runner"),
            )
            .with_home(Engine::Claude, &claude)
            .with_home(Engine::Codex, &codex);
            let runner = pitcrew_runner::start(
                config,
                vec![
                    Arc::new(MissingMetadata(pitcrew_ingest::claude::ClaudeAdapter::new())),
                    Arc::new(pitcrew_ingest::codex::CodexAdapter::new()),
                ],
                Arc::new(pitcrew_runner::StoreSink::new(work.store().clone(), sam.id)),
            )
            .unwrap();
            let runtime = Arc::new(pitcrew_interfaces::fake::FakeRuntime::default());
            let terminals = runner.terminals(runtime.clone()).unwrap();
            let commands = runner.commands(&terminals);
            let command = |named| RunnerCommand::StartSession {
                engine,
                cwd: pending.cwd.clone(),
                name: "Seeds".into(),
                brief: None,
                persona: None,
                model: None,
                account: None,
                permission_mode: PermissionMode::Default,
                session: Some(named),
            };
            assert!(matches!(
                commands.run(CommandId::new(), &command(pending.id)),
                CommandOutcome::Ok { .. }
            ));
            let terminal = terminals.terminal_of(pending.id).unwrap().unwrap();
            pitcrew_interfaces::runtime::Runtime::kill(runtime.as_ref(), terminal).unwrap();
            assert_eq!(
                commands.started(pending.id),
                Started::Gone,
                "an unrelated empty transcript must not block the exited CLI's final scan"
            );
            let starts: Arc<dyn Starts> = Arc::new(commands.clone());
            let attached = Attached::default();
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async {
                let mut gone = HashSet::new();
                for _ in 0..2 {
                    look_at(
                        &Arc::downgrade(&work),
                        &attached,
                        pending.machine,
                        &starts,
                        &mut gone,
                    )
                    .await;
                }
            });
            assert_eq!(
                work.dispatch(&dispatch.id).unwrap().outcome,
                Some(pitcrew_protocol::model::DispatchOutcome::Failed)
            );
            assert_eq!(
                work.session(&pending.id).unwrap().state,
                SessionState::Ended
            );
            assert_eq!(terminals.terminal_of(pending.id).unwrap(), None);
            assert!(
                matches!(
                    commands.run(CommandId::new(), &command(SessionId::new())),
                    CommandOutcome::Ok { .. }
                ),
                "a new start must not be told to try again"
            );
            runner.stop();
        }
    }

    #[test]
    fn a_reported_dispatch_finishes_when_its_cli_exits_without_a_hook() {
        let tmp = tempfile::tempdir().unwrap();
        let sam = member(MemberKind::Human, "@sam", None);
        let writer = member(MemberKind::Agent, "@writer", Some(sam.id));
        let mut reported = session(Some(writer.id));
        reported.native_id = "synthetic-cli-id".into();
        reported.state = SessionState::Idle;
        let work = work(tmp.path(), &[&sam, &writer], &[&reported]);
        let dispatch = pitcrew_protocol::model::Dispatch {
            id: DispatchId::new(),
            task: TaskId::new(),
            agent: writer.id,
            session: Some(reported.id),
            brief: "Submit the seeds".into(),
            started: 1,
            ended: None,
            outcome: None,
            summary: None,
        };
        work.store()
            .append(&[Event::now(
                work.workspace(),
                sam.id,
                EventBody::DispatchStarted {
                    dispatch: dispatch.clone(),
                },
            )])
            .unwrap();
        let told = Arc::new(Told::default());
        let starts: Arc<dyn Starts> = told.clone();
        let attached = Attached::default();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let mut gone = HashSet::new();
            told.answers
                .lock()
                .unwrap()
                .insert(reported.id, Started::Reported);
            assert_eq!(
                look_at(
                    &Arc::downgrade(&work),
                    &attached,
                    reported.machine,
                    &starts,
                    &mut gone
                )
                .await,
                Some(true)
            );
            assert_eq!(work.dispatch(&dispatch.id).unwrap().outcome, None);
            told.answers
                .lock()
                .unwrap()
                .insert(reported.id, Started::Exited);
            for _ in 0..3 {
                look_at(
                    &Arc::downgrade(&work),
                    &attached,
                    reported.machine,
                    &starts,
                    &mut gone,
                )
                .await;
            }
        });
        assert_eq!(
            work.dispatch(&dispatch.id).unwrap().outcome,
            Some(pitcrew_protocol::model::DispatchOutcome::Canceled),
            "an exited reported CLI must finish its active dispatch"
        );
        assert_eq!(
            work.session(&reported.id).unwrap().state,
            SessionState::Ended
        );
        assert!(told.asked.lock().unwrap().contains(&reported.id));
    }

    #[test]
    fn an_exited_adoption_is_allowed_to_reach_the_hub_before_it_finishes() {
        let tmp = tempfile::tempdir().unwrap();
        let sam = member(MemberKind::Human, "@sam", None);
        let pending = session(None);
        let work = work(tmp.path(), &[&sam], &[&pending]);
        let attached = Attached::default();
        let told = Arc::new(Told::default());
        told.answers
            .lock()
            .unwrap()
            .insert(pending.id, Started::Exited);
        let starts: Arc<dyn Starts> = told;
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let mut gone = HashSet::new();
            for _ in 0..3 {
                assert_eq!(
                    look_at(
                        &Arc::downgrade(&work),
                        &attached,
                        pending.machine,
                        &starts,
                        &mut gone
                    )
                    .await,
                    Some(true)
                );
                assert_eq!(
                    work.session(&pending.id).unwrap().state,
                    SessionState::Starting
                );
            }
        });
    }

    /// The reconciliation never takes a start this hub is making for one whose CLI did not start,
    /// however long the runner takes: it is not even asked about it. Once the start is over, a
    /// session gone twice in a row (a runner answer between resets it) is abandoned, and its
    /// terminal retired; one matched by folder past the claim window is abandoned at once.
    #[test]
    fn the_reconciliation_leaves_starts_under_way_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let sam = member(MemberKind::Human, "@sam", None);
        let writer = member(MemberKind::Agent, "@writer", Some(sam.id));
        let machine = MachineId::new();
        let on = |agent| Session {
            machine,
            ..session(agent)
        };
        let (held, late, lost, flaky) = (on(Some(writer.id)), on(None), on(None), on(None));
        let work = work(tmp.path(), &[&sam, &writer], &[&held, &late, &lost, &flaky]);
        let attached = Arc::new(Attached::default());
        let told = Arc::new(Told::default());
        told.answers
            .lock()
            .unwrap()
            .insert(late.id, Started::TooLate);
        let starts: Arc<dyn Starts> = told.clone();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let weak = Arc::downgrade(&work);
        let mut gone = HashSet::new();
        let mut look = || {
            rt.block_on(look_at(&weak, &attached, machine, &starts, &mut gone))
                .unwrap()
        };
        let state = |s: &Session| work.session(&s.id).unwrap().state;

        let starting = attached.starting(held.id);
        assert!(look(), "sessions wait");
        assert_eq!(state(&late), SessionState::Ended, "too late: at once");
        assert_eq!(state(&lost), SessionState::Starting, "gone once");
        // A runner answer between two gones: not twice in a row.
        told.answers
            .lock()
            .unwrap()
            .insert(flaky.id, Started::Running);
        assert!(look());
        told.answers.lock().unwrap().remove(&flaky.id);
        assert_eq!(state(&lost), SessionState::Ended, "gone twice");
        assert!(look());
        assert_eq!(state(&flaky), SessionState::Starting);
        for _ in 0..3 {
            assert!(look());
        }
        assert_eq!(state(&held), SessionState::Starting, "under way");
        assert!(!told.asked.lock().unwrap().contains(&held.id));

        // The start is over and its CLI did not start: gone twice, abandoned.
        drop(starting);
        assert!(look());
        assert_eq!(state(&held), SessionState::Starting);
        assert!(!look(), "nothing waits any more");
        assert_eq!(state(&held), SessionState::Ended);
        assert_eq!(state(&flaky), SessionState::Ended);
        let mut retired = told.retired.lock().unwrap().clone();
        retired.sort_unstable();
        let mut abandoned = vec![held.id, late.id, lost.id, flaky.id];
        abandoned.sort_unstable();
        assert_eq!(
            retired, abandoned,
            "each abandoned session's terminal retired"
        );
    }
}
