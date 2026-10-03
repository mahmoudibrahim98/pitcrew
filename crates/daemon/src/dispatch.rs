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
//!   token in the environment itself. A session without an agent gets nothing; one whose agent
//!   has no owner, or is not known, is not started.
//! - **Following** ([`FollowingSink`]): every batch the runner's `StoreSink` stores is handed to
//!   `WorkService::follow_sessions`, which moves the dispatched task on the session's first
//!   `working` and finishes the dispatch when its session ends (see hub-work's "Dispatch").
//! - **Reconciling** ([`reconcile`]): at start, once the runner attaches, and after each start of
//!   a session the hub stored ahead of the runner, the sessions on the runner's machine still
//!   `starting` with no CLI id are looked at, then again with a growing pause (1 s up to a
//!   minute) while any waits. One whose CLI runs, or whose transcript the runner has, is left to
//!   be reported; one the runner has no terminal and no transcript for (a crash between the
//!   dispatch and its start), or whose program ended before its transcript appeared, twice in a
//!   row with a rescan between, is abandoned: its dispatch fails and it ends.

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
    EventSink, SessionAgent, SessionAgents as _, SessionEnv, SinkError, Started, StoreSink,
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
        let mut env = vec![("PITCREW_TOKEN_FILE".to_owned(), file)];
        if let Some((variable, value)) = self.endpoint.get() {
            env.push(((*variable).to_owned(), value.clone()));
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

/// One look: `None` once the work model is gone (the daemon stops); else whether a session still
/// waits for its CLI. `gone` holds the sessions found gone at the last look.
async fn look(
    work: &Weak<WorkService>,
    attached: &Attached,
    gone: &mut HashSet<SessionId>,
) -> Option<bool> {
    let work = work.upgrade()?;
    let Some(runner) = attached.get().cloned() else {
        return Some(false);
    };
    let machine = runner.machine;
    let reading = Arc::clone(&work);
    let sessions =
        match tokio::task::spawn_blocking(move || reading.unreported_sessions(&machine)).await {
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
        let (commands, id) = (runner.commands.clone(), session.id);
        let started = tokio::task::spawn_blocking(move || commands.started(id))
            .await
            .unwrap_or_else(|e| Started::Unknown(e.to_string()));
        match started {
            Started::Gone if gone.remove(&id) => {
                let abandoning = Arc::clone(&work);
                let abandoned = tokio::task::spawn_blocking(move || {
                    abandoning.abandon_session(&id, DID_NOT_START)
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
            Started::Gone => {
                // Its transcript may be there, not found yet: looked for before the next look.
                gone.insert(id);
                waiting = true;
                rescan = true;
            }
            // Reported: its re-statement is on its way to the store.
            Started::Running | Started::Reported => waiting = true,
            Started::Unknown(why) => {
                tracing::debug!(session = %id, why, "cannot tell yet whether a session's CLI started");
                waiting = true;
            }
        }
    }
    if rescan {
        let commands = runner.commands.clone();
        drop(tokio::task::spawn_blocking(move || {
            commands.run(CommandId::new(), &RunnerCommand::Scan { roots: Vec::new() })
        }));
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
        assert_eq!(first.len(), 1, "not listening yet: {first:?}");
        assert_eq!(first[0].0, "PITCREW_TOKEN_FILE");
        let file = PathBuf::from(&first[0].1);
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

        // Reused while it verifies; with where the daemon listens once it does.
        env.listening("PITCREW_URL", "http://127.0.0.1:47317".into());
        let again = env.env_for(mine.id).unwrap();
        assert_eq!(again[0], first[0]);
        assert_eq!(read_token(&file).unwrap().unwrap(), token);
        assert_eq!(
            again[1],
            (
                "PITCREW_URL".to_owned(),
                "http://127.0.0.1:47317".to_owned()
            )
        );
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

    /// `~` is this user's home.
    #[test]
    fn a_dispatch_without_a_folder_runs_in_the_home() {
        let home = directories::BaseDirs::new()
            .unwrap()
            .home_dir()
            .to_path_buf();
        if !home.is_dir() || crate::sessions::checked_cwd(home.to_str().unwrap()).is_err() {
            eprintln!("skipped: this user's home is not a folder a session may start in");
            return;
        }
        let real = std::fs::canonicalize(&home).unwrap();
        assert_eq!(folder("~").unwrap(), real.to_str().unwrap());
        assert!(folder("relative").is_err());
    }
}
