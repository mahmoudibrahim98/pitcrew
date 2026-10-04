//! GitHub and Jira, wired in (api-v1.md "Integrations"; brief G-sync-wiring): connections, their
//! credentials, the read-only sync loop and its routes.
//!
//! - [`Integrations`] keeps the connections (`saved.rs`: in the state directory, never the event
//!   log), their credentials (`secret.rs`: a private file each, or `gh auth token` read at each
//!   sync), and each one's sync state and status.
//! - **The loop** ([`Integrations::spawn`]) syncs each connection on its interval and on demand,
//!   one at a time: it reads upstream with `pitcrew-sync-github` or `pitcrew-sync-jira` through the
//!   [`http::Upstream`] transport (HTTPS, or recorded fixtures in tests), applies what changed
//!   through hub-work's `SyncCommands` (`apply.rs`), then keeps the new sync state and status. A
//!   rate limit waits until it lifts. A sync only ever reads upstream.
//! - **Outward writes** (`writes.rs`, api-v1.md "Outward writes"): the loop also proposes what
//!   people's changes imply upstream (each one an approval ask), and sends what a person approved,
//!   one write at a time with the syncs. Nothing is sent without an answered approval ask.
//! - **The routes** (`routes.rs`) are device-only. No route returns a credential, and nothing here
//!   logs one.

mod apply;
pub mod http;
mod routes;
mod saved;
pub mod secret;
mod validate;
mod writes;

pub use routes::routes;

use anyhow::Context as _;
use apply::{Applied, Applier};
use http::Upstream;
use pitcrew_hub_work::WorkService;
use pitcrew_hub_work::links::{LinkScope, scope_of};
use pitcrew_protocol::api::{Caller, ErrorCode};
use pitcrew_protocol::ids::{AskId, IntegrationId, MemberId};
use pitcrew_protocol::integrations::{
    CredentialInfo, CredentialSource, Integration, IntegrationCheck, IntegrationLink,
    IntegrationSettings, JiraDeployment, NewIntegration, ScopeCheck, SyncCounts, SyncProblem,
    SyncStatus,
};
use pitcrew_protocol::model::{ExternalSystem, TimestampMs, Workstream};
use pitcrew_sync_github::probe::CheckOutcome;
use saved::{Files, Record, Saved};
use secret::{GhCli, Secret, SecretFiles};
use std::collections::{BTreeMap, HashSet};
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::Duration;
use tokio::sync::{Notify, watch};
use tokio::task::JoinHandle;

/// How long after the hub starts the first sync of a connection that is due waits.
const START_DELAY_MS: i64 = 30_000;
/// The longest the loop sleeps before looking again.
const LOOK_AGAIN: Duration = Duration::from_secs(60);

/// A refusal for a route: its code and a sentence for people. Never holds a credential.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub code: ErrorCode,
    pub message: String,
}

impl Refusal {
    fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    fn not_found(id: &IntegrationId) -> Self {
        Self::new(ErrorCode::NotFound, format!("No integration {}.", id.0))
    }

    fn internal(what: &str) -> Self {
        Self::new(
            ErrorCode::Internal,
            format!("{what} failed; see the hub's log."),
        )
    }
}

impl From<pitcrew_hub_work::WorkError> for Refusal {
    fn from(e: pitcrew_hub_work::WorkError) -> Self {
        Self::new(e.code(), e.to_string())
    }
}

fn now_ms() -> TimestampMs {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

/// `YYYY-MM-DDTHH:MM:SSZ` for Unix seconds (GitHub's timestamp shape).
fn utc_rfc3339(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let rest = secs.rem_euclid(86_400);
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    )
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The host `gh auth token --hostname` takes for a GitHub API root.
fn github_host(api_base: Option<&str>) -> String {
    api_base
        .and_then(|base| url::Url::parse(base).ok())
        .and_then(|u| u.host_str().map(str::to_owned))
        .unwrap_or_else(|| "github.com".to_owned())
}

/// The repositories or Jira projects a connection syncs.
fn containers(settings: &IntegrationSettings) -> Vec<String> {
    match settings {
        IntegrationSettings::Github { repos, .. } => repos.clone(),
        IntegrationSettings::Jira { projects, .. } => projects.clone(),
    }
}

fn system_of(settings: &IntegrationSettings) -> ExternalSystem {
    match settings {
        IntegrationSettings::Github { .. } => ExternalSystem::Github,
        IntegrationSettings::Jira { .. } => ExternalSystem::Jira,
    }
}

/// The workstream links of `workstreams` inside a connection's containers, by container.
fn links_by_container(
    settings: &IntegrationSettings,
    workstreams: &[Workstream],
) -> BTreeMap<String, Vec<(Workstream, pitcrew_protocol::model::ExternalRef)>> {
    let system = system_of(settings);
    let mut out: BTreeMap<String, Vec<_>> = BTreeMap::new();
    for container in containers(settings) {
        out.insert(container, Vec::new());
    }
    for w in workstreams {
        for link in w.external.iter().filter(|l| l.system == system) {
            let Some(scope) = scope_of(link) else {
                continue;
            };
            let matched = out.keys().find(|c| match &scope {
                LinkScope::GithubRepo { .. } | LinkScope::GithubMilestone { .. } => {
                    c.eq_ignore_ascii_case(scope.container())
                }
                _ => *c == scope.container(),
            });
            if let Some(container) = matched.cloned()
                && let Some(entry) = out.get_mut(&container)
            {
                entry.push((w.clone(), link.clone()));
            }
        }
    }
    out
}

/// GitHub and Jira, wired in. See the [module docs](self). Share it in an `Arc`.
pub struct Integrations {
    work: Weak<WorkService>,
    files: Files,
    secrets: SecretFiles,
    gh: GhCli,
    /// `None` when the HTTPS transport could not be set up; syncs then report why.
    upstream: Result<Upstream, String>,
    saved: Mutex<Saved>,
    /// Requested or under way.
    running: Mutex<HashSet<IntegrationId>>,
    /// Asked for with `POST …/sync`.
    requested: Mutex<HashSet<IntegrationId>>,
    /// Failed writes a person asked to send again (`POST /v1/writes/{id}/retry`).
    retries: Mutex<HashSet<AskId>>,
    wake: Notify,
}

impl std::fmt::Debug for Integrations {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Integrations")
            .field("files", &self.files)
            .finish_non_exhaustive()
    }
}

impl Integrations {
    /// The integrations of the hub whose state directory is `root`, syncing through `upstream`
    /// (`Err` saying why there is no transport), reading `gh` from `gh`.
    ///
    /// # Errors
    /// `integrations.json` exists but cannot be read.
    pub fn open(
        root: &Path,
        work: &Arc<WorkService>,
        upstream: Result<Upstream, String>,
        gh: GhCli,
    ) -> anyhow::Result<Self> {
        let files = Files::new(root);
        let mut saved = files
            .load()
            .context("cannot read the integrations (integrations.json)")?;
        // A sync that was due while the hub was down runs soon after it starts, not at once.
        let soon = now_ms().saturating_add(START_DELAY_MS);
        for record in &mut saved.integrations {
            record.status.running = false;
            if record.status.next_at.is_none_or(|at| at < soon) {
                record.status.next_at = Some(soon);
            }
        }
        Ok(Self {
            work: Arc::downgrade(work),
            secrets: SecretFiles::new(files.dir().to_path_buf()),
            files,
            gh,
            upstream,
            saved: Mutex::new(saved),
            running: Mutex::new(HashSet::new()),
            requested: Mutex::new(HashSet::new()),
            retries: Mutex::new(HashSet::new()),
            wake: Notify::new(),
        })
    }

    fn work(&self) -> Result<Arc<WorkService>, Refusal> {
        self.work
            .upgrade()
            .ok_or_else(|| Refusal::new(ErrorCode::Unavailable, "The hub is stopping."))
    }

    fn record(&self, id: &IntegrationId) -> Result<Record, Refusal> {
        lock(&self.saved)
            .integrations
            .iter()
            .find(|r| r.id == *id)
            .cloned()
            .ok_or_else(|| Refusal::not_found(id))
    }

    fn save(&self, saved: &Saved) -> Result<(), Refusal> {
        self.files.save(saved).map_err(|e| {
            tracing::warn!(error = %e, "cannot save the integrations");
            Refusal::internal("Saving the integrations")
        })
    }

    fn view(&self, record: &Record, workstreams: &[Workstream]) -> Integration {
        let mut status = record.status.clone();
        status.running = lock(&self.running).contains(&record.id);
        let links = links_by_container(&record.settings, workstreams)
            .into_values()
            .flatten()
            .map(|(w, scope)| IntegrationLink {
                workstream: w.id,
                title: record.titles.get(&scope.key).cloned(),
                scope,
            })
            .collect();
        Integration {
            id: record.id,
            name: record.name.clone(),
            settings: record.settings.clone(),
            credential: CredentialInfo {
                source: record.credential,
                stored: record.credential == CredentialSource::Stored
                    && self.secrets.has(&record.id),
            },
            interval_minutes: record.interval_minutes,
            added_by: record.added_by,
            added_at: record.added_at,
            status,
            links,
        }
    }

    async fn workstreams(&self) -> Result<Vec<Workstream>, Refusal> {
        let work = self.work()?;
        tokio::task::spawn_blocking(move || work.workstreams(None))
            .await
            .map_err(|_| Refusal::internal("Reading the workstreams"))?
            .map_err(Refusal::from)
    }

    /// `GET /v1/integrations`.
    ///
    /// # Errors
    /// The workstreams cannot be read.
    pub async fn list(&self) -> Result<Vec<Integration>, Refusal> {
        let workstreams = self.workstreams().await?;
        let records = lock(&self.saved).integrations.clone();
        Ok(records.iter().map(|r| self.view(r, &workstreams)).collect())
    }

    /// `GET /v1/integrations/{id}`.
    ///
    /// # Errors
    /// `not_found`; the workstreams cannot be read.
    pub async fn get(&self, id: &IntegrationId) -> Result<Integration, Refusal> {
        let record = self.record(id)?;
        let workstreams = self.workstreams().await?;
        Ok(self.view(&record, &workstreams))
    }

    /// `POST /v1/integrations`.
    ///
    /// # Errors
    /// `invalid` for a malformed connection or a caller who is not a person of the workspace;
    /// `conflict` for a repository or project another connection syncs.
    pub async fn add(&self, caller: &Caller, new: NewIntegration) -> Result<Integration, Refusal> {
        let checked = validate::check(new).map_err(|m| Refusal::new(ErrorCode::Invalid, m))?;
        let wanted: HashSet<String> = validate::scope_keys(&checked.settings)
            .into_iter()
            .collect();
        if lock(&self.saved).integrations.iter().any(|r| {
            validate::scope_keys(&r.settings)
                .iter()
                .any(|k| wanted.contains(k))
        }) {
            return Err(Refusal::new(
                ErrorCode::Conflict,
                "Another integration already syncs one of these repositories or projects.",
            ));
        }
        let work = self.work()?;
        let owner = caller.member;
        let member = tokio::task::spawn_blocking(move || work.ensure_sync_member(owner))
            .await
            .map_err(|_| Refusal::internal("Adding the sync's member"))??;
        let now = now_ms();
        let record = Record {
            id: IntegrationId::new(),
            name: checked.name,
            settings: checked.settings,
            credential: checked.credential,
            interval_minutes: checked.interval_minutes,
            added_by: caller.member,
            added_at: now,
            status: SyncStatus {
                next_at: Some(now),
                ..SyncStatus::default()
            },
            titles: BTreeMap::new(),
            linked: BTreeMap::new(),
        };
        {
            let mut saved = lock(&self.saved);
            saved.sync_member = Some(member.id);
            saved.integrations.push(record.clone());
            self.save(&saved)?;
        }
        tracing::info!(integration = %record.id, by = %caller.member, "integration added");
        self.wake.notify_one();
        self.get(&record.id).await
    }

    /// `DELETE /v1/integrations/{id}`: the connection, its secret and its sync state.
    ///
    /// # Errors
    /// `not_found`; the files cannot be changed.
    pub fn remove(&self, caller: &Caller, id: &IntegrationId) -> Result<(), Refusal> {
        {
            let mut saved = lock(&self.saved);
            let before = saved.integrations.len();
            saved.integrations.retain(|r| r.id != *id);
            if saved.integrations.len() == before {
                return Err(Refusal::not_found(id));
            }
            self.save(&saved)?;
        }
        if let Err(e) = self.secrets.remove(id) {
            tracing::warn!(integration = %id, error = %e, "cannot remove the integration's secret");
        }
        if let Err(e) = self.files.remove_state(id) {
            tracing::warn!(integration = %id, error = %e, "cannot remove the integration's sync state");
        }
        lock(&self.requested).remove(id);
        tracing::info!(integration = %id, by = %caller.member, "integration removed");
        Ok(())
    }

    /// `PUT /v1/integrations/{id}/credential`.
    ///
    /// # Errors
    /// `not_found`; `invalid` for a malformed secret; `conflict` for a `gh_cli` connection; the
    /// file cannot be written.
    pub fn set_credential(
        &self,
        caller: &Caller,
        id: &IntegrationId,
        secret: &str,
    ) -> Result<(), Refusal> {
        let record = self.record(id)?;
        let secret = Secret::new(secret).ok_or_else(|| {
            Refusal::new(
                ErrorCode::Invalid,
                "secret must be 1 to 4096 characters, without whitespace or control characters.",
            )
        })?;
        if record.credential == CredentialSource::GhCli {
            return Err(Refusal::new(
                ErrorCode::Conflict,
                "This integration reads `gh auth token` on the hub's machine and keeps no secret.",
            ));
        }
        self.secrets.save(id, &secret).map_err(|e| {
            tracing::warn!(integration = %id, error = %e, "cannot store the integration's secret");
            Refusal::internal("Storing the secret")
        })?;
        tracing::info!(integration = %id, by = %caller.member, "integration credential stored");
        self.request(id);
        Ok(())
    }

    fn request(&self, id: &IntegrationId) {
        lock(&self.requested).insert(*id);
        lock(&self.running).insert(*id);
        self.wake.notify_one();
    }

    /// `POST /v1/integrations/{id}/sync`.
    ///
    /// # Errors
    /// `not_found`; the workstreams cannot be read.
    pub async fn sync_now(&self, id: &IntegrationId) -> Result<Integration, Refusal> {
        self.record(id)?;
        self.request(id);
        self.get(id).await
    }

    /// The credential of `record`, or a problem to report.
    async fn credential(&self, record: &Record) -> Result<Secret, SyncProblem> {
        let problem = |message: String| SyncProblem {
            scope: String::new(),
            message,
        };
        match record.credential {
            CredentialSource::GhCli => {
                let host = match &record.settings {
                    IntegrationSettings::Github { api_base, .. } => {
                        github_host(api_base.as_deref())
                    }
                    IntegrationSettings::Jira { .. } => "github.com".to_owned(),
                };
                self.gh
                    .token(&host)
                    .await
                    .map_err(|e| problem(e.to_string()))
            }
            CredentialSource::Stored => match self.secrets.load(&record.id) {
                Ok(Some(secret)) => Ok(secret),
                Ok(None) => Err(problem(
                    "No credential yet: add one for this integration in the desktop app.".into(),
                )),
                Err(e) => {
                    tracing::warn!(integration = %record.id, error = %e, "cannot read the integration's secret");
                    Err(problem(
                        "The stored credential cannot be read; add it again.".into(),
                    ))
                }
            },
        }
    }

    fn upstream(&self) -> Result<&Upstream, SyncProblem> {
        self.upstream.as_ref().map_err(|why| SyncProblem {
            scope: String::new(),
            message: why.clone(),
        })
    }

    /// `POST /v1/integrations/{id}/test`.
    ///
    /// # Errors
    /// `not_found`.
    pub async fn check(&self, id: &IntegrationId) -> Result<IntegrationCheck, Refusal> {
        let record = self.record(id)?;
        let at = now_ms();
        let failed = |message: String| IntegrationCheck {
            ok: false,
            at,
            checks: vec![ScopeCheck {
                scope: String::new(),
                ok: false,
                message,
            }],
            warnings: Vec::new(),
        };
        let secret = match self.credential(&record).await {
            Ok(secret) => secret,
            Err(problem) => return Ok(failed(problem.message)),
        };
        let upstream = match self.upstream() {
            Ok(upstream) => upstream.clone(),
            Err(problem) => return Ok(failed(problem.message)),
        };
        Ok(match &record.settings {
            IntegrationSettings::Github { repos, api_base } => {
                let config = github_config(repos, api_base.clone(), &secret);
                let report = pitcrew_sync_github::probe::probe(&upstream, &config).await;
                github_check(&report, at)
            }
            IntegrationSettings::Jira { .. } => {
                let Some(config) = jira_config(&record.settings, &secret) else {
                    return Ok(failed("The integration's settings are incomplete.".into()));
                };
                let report = pitcrew_sync_jira::probe::probe(&upstream, &config).await;
                jira_check(&report, at)
            }
        })
    }

    /// Starts the loop on the current runtime. See the [module docs](self). Every append to the
    /// event log wakes it, so an answered approval is acted on at once.
    pub fn spawn(self: &Arc<Self>) -> Running {
        let (stop, stopped) = watch::channel(false);
        let appended = self.work().ok().map(|w| w.store().subscribe());
        let task = tokio::spawn(run(Arc::clone(self), stopped));
        let waker = appended.map(|mut appended| {
            let me = Arc::downgrade(self);
            tokio::spawn(async move {
                loop {
                    match appended.recv().await {
                        Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                            match me.upgrade() {
                                Some(integrations) => integrations.wake.notify_one(),
                                None => return,
                            }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                    }
                }
            })
        });
        Running { stop, task, waker }
    }

    /// The connections due now (or asked for), in order.
    fn due(&self) -> Vec<IntegrationId> {
        let now = now_ms();
        let requested = lock(&self.requested).clone();
        lock(&self.saved)
            .integrations
            .iter()
            .filter(|r| {
                requested.contains(&r.id)
                    || (r.status.next_at.is_some_and(|at| at <= now)
                        && r.status.rate_limited_until.is_none_or(|until| until <= now))
            })
            .map(|r| r.id)
            .collect()
    }

    /// How long until the next connection is due, at most [`LOOK_AGAIN`].
    fn until_next(&self) -> Duration {
        let now = now_ms();
        lock(&self.saved)
            .integrations
            .iter()
            .filter_map(|r| r.status.next_at)
            .map(|at| Duration::from_millis(u64::try_from(at.saturating_sub(now)).unwrap_or(0)))
            .min()
            .unwrap_or(LOOK_AGAIN)
            .min(LOOK_AGAIN)
    }

    /// Syncs one connection (see the [module docs](self)), and keeps its status.
    async fn sync_one(&self, id: IntegrationId) {
        lock(&self.requested).remove(&id);
        lock(&self.running).insert(id);
        let Ok(record) = self.record(&id) else {
            lock(&self.running).remove(&id);
            return;
        };
        let started = now_ms();
        let result = self.sync_record(&record).await;
        let ended = now_ms();
        let interval = i64::from(record.interval_minutes).saturating_mul(60_000);
        {
            let mut saved = lock(&self.saved);
            if let Some(kept) = saved.integrations.iter_mut().find(|r| r.id == id) {
                let status = &mut kept.status;
                status.last_attempt_at = Some(started);
                status.next_at = Some(ended.saturating_add(interval));
                match result {
                    Ok(done) => {
                        status.rate_limited_until = done.rate_limited_until;
                        if let Some(until) = done.rate_limited_until {
                            status.next_at = status.next_at.map(|at| at.max(until));
                        }
                        if done.problems.is_empty() && done.rate_limited_until.is_none() {
                            status.last_success_at = Some(ended);
                        }
                        status.problems = done.problems;
                        status.last_run = Some(done.counts);
                        kept.titles.extend(done.titles);
                        kept.linked = done.linked;
                    }
                    Err(problem) => {
                        status.problems = vec![problem];
                        status.last_run = None;
                    }
                }
                status.running = false;
                if let Err(e) = self.files.save(&saved) {
                    tracing::warn!(error = %e, "cannot save the integrations");
                }
            }
        }
        lock(&self.running).remove(&id);
        if lock(&self.requested).contains(&id) {
            lock(&self.running).insert(id);
        }
    }

    async fn sync_record(&self, record: &Record) -> Result<Done, SyncProblem> {
        let secret = self.credential(record).await?;
        let upstream = self.upstream()?.clone();
        let work = self.work().map_err(|r| SyncProblem {
            scope: String::new(),
            message: r.message,
        })?;
        let member = lock(&self.saved).sync_member.ok_or_else(|| SyncProblem {
            scope: String::new(),
            message: "The sync's member is missing; remove and add the integration again.".into(),
        })?;
        let workstreams = self.workstreams().await.map_err(|r| SyncProblem {
            scope: String::new(),
            message: r.message,
        })?;
        let linked: BTreeMap<String, Vec<String>> =
            links_by_container(&record.settings, &workstreams)
                .into_iter()
                .map(|(container, links)| {
                    let mut keys: Vec<String> = links.into_iter().map(|(_, l)| l.key).collect();
                    keys.sort();
                    keys.dedup();
                    (container, keys)
                })
                .collect();
        let changed: Vec<String> = linked
            .iter()
            .filter(|(container, keys)| {
                record.linked.get(*container).map_or(&[][..], Vec::as_slice) != keys.as_slice()
            })
            .map(|(container, _)| container.clone())
            .collect();
        let now_unix = now_ms() / 1000;
        let mut done = Done {
            linked,
            ..Done::default()
        };
        match &record.settings {
            IntegrationSettings::Github { repos, api_base } => {
                let mut state: pitcrew_sync_github::SyncState =
                    self.files.load_state(&record.id).unwrap_or_default();
                for repo in &changed {
                    // Issues and pull requests are read again (a merged pull request seen
                    // before its issue was in scope is noted now); milestones are not, so a
                    // milestone closed long ago does not ship a workstream just linked to it.
                    if let Some(repo_state) = state.repos.get_mut(repo) {
                        repo_state.issues = pitcrew_sync_github::state::ListCache::default();
                        repo_state.issue_snapshots.clear();
                        repo_state.pulls = pitcrew_sync_github::state::ListCache::default();
                        repo_state.pull_snapshots.clear();
                    }
                }
                let mut config = github_config(repos, api_base.clone(), &secret);
                config.now_unix = now_unix;
                config.now = pitcrew_sync_github::GithubTimestamp::new(utc_rfc3339(now_unix));
                let outcome = pitcrew_sync_github::sync::sync(state, &upstream, &config).await;
                done.rate_limited_until =
                    outcome.rate_limited.map(|rl| rl.until.saturating_mul(1000));
                done.problems
                    .extend(outcome.errors.iter().map(|e| SyncProblem {
                        scope: e.repo.clone(),
                        message: e.message.clone(),
                    }));
                let changes = outcome.changes;
                let applied = apply_blocking(work, member, ExternalSystem::Github, move |a| {
                    apply::apply_github(a, &changes);
                })
                .await?;
                done.merge(applied, outcome.malformed_skipped);
                self.keep_state(&record.id, &outcome.state)?;
            }
            IntegrationSettings::Jira { deployment, .. } => {
                let mut config =
                    jira_config(&record.settings, &secret).ok_or_else(|| SyncProblem {
                        scope: String::new(),
                        message: "The integration's settings are incomplete.".into(),
                    })?;
                config.now_unix = now_unix;
                let mut state: pitcrew_sync_jira::SyncState =
                    self.files.load_state(&record.id).unwrap_or_default();
                for project in &changed {
                    if let Some(project_state) = state.projects.get_mut(project) {
                        project_state.cursor = None;
                        project_state.issue_snapshots.clear();
                        project_state.resume_without_margin = false;
                    }
                }
                let outcome = match deployment {
                    JiraDeployment::Cloud => {
                        pitcrew_sync_jira::sync::sync(
                            state,
                            &upstream,
                            &pitcrew_sync_jira::JiraCloud,
                            &config,
                        )
                        .await
                    }
                    JiraDeployment::DataCenter => {
                        pitcrew_sync_jira::sync::sync(
                            state,
                            &upstream,
                            &pitcrew_sync_jira::JiraDataCenter,
                            &config,
                        )
                        .await
                    }
                };
                done.rate_limited_until =
                    outcome.rate_limited.map(|rl| rl.until.saturating_mul(1000));
                done.problems
                    .extend(outcome.errors.iter().map(|e| SyncProblem {
                        scope: e.project.clone(),
                        message: e.message.clone(),
                    }));
                let changes = outcome.changes;
                let applied = apply_blocking(work, member, ExternalSystem::Jira, move |a| {
                    apply::apply_jira(a, &changes);
                })
                .await?;
                done.merge(applied, outcome.malformed_skipped);
                self.keep_state(&record.id, &outcome.state)?;
            }
        }
        Ok(done)
    }

    fn keep_state<T: serde::Serialize>(
        &self,
        id: &IntegrationId,
        state: &T,
    ) -> Result<(), SyncProblem> {
        self.files.save_state(id, state).map_err(|e| {
            tracing::warn!(integration = %id, error = %e, "cannot save the sync state");
            SyncProblem {
                scope: String::new(),
                message: "The sync state could not be saved; see the hub's log.".into(),
            }
        })
    }
}

/// What one sync did, for its status.
#[derive(Debug, Default)]
struct Done {
    counts: SyncCounts,
    problems: Vec<SyncProblem>,
    titles: BTreeMap<String, String>,
    linked: BTreeMap<String, Vec<String>>,
    rate_limited_until: Option<TimestampMs>,
}

impl Done {
    fn merge(&mut self, applied: Applied, malformed: u32) {
        self.counts = applied.counts;
        self.counts.malformed = malformed;
        self.problems.extend(applied.problems);
        self.titles = applied.titles;
    }
}

/// Applies changes through the sync's commands on the blocking pool.
async fn apply_blocking(
    work: Arc<WorkService>,
    member: MemberId,
    system: ExternalSystem,
    apply: impl FnOnce(&mut Applier<'_>) + Send + 'static,
) -> Result<Applied, SyncProblem> {
    let problem = |message: String| SyncProblem {
        scope: String::new(),
        message,
    };
    tokio::task::spawn_blocking(move || {
        let commands = work.sync_commands(member).map_err(|e| e.to_string())?;
        let mut applier = Applier::new(commands, system).map_err(|e| e.to_string())?;
        apply(&mut applier);
        Ok::<_, String>(applier.finish())
    })
    .await
    .map_err(|_| problem("Applying the changes failed; see the hub's log.".into()))?
    .map_err(problem)
}

fn github_config(
    repos: &[String],
    api_base: Option<String>,
    secret: &Secret,
) -> pitcrew_sync_github::SyncConfig {
    let now_unix = now_ms() / 1000;
    pitcrew_sync_github::SyncConfig {
        repos: repos
            .iter()
            .filter_map(|r| pitcrew_sync_github::RepoRef::new(r.clone()).ok())
            .collect(),
        token: pitcrew_sync_github::AuthToken::new(secret.expose()),
        now_unix,
        now: pitcrew_sync_github::GithubTimestamp::new(utc_rfc3339(now_unix)),
        api_base,
    }
}

fn jira_config(
    settings: &IntegrationSettings,
    secret: &Secret,
) -> Option<pitcrew_sync_jira::SyncConfig> {
    let IntegrationSettings::Jira {
        deployment,
        site,
        projects,
        email,
        epic_link_field,
    } = settings
    else {
        return None;
    };
    let (auth, version) = match deployment {
        JiraDeployment::Cloud => (
            pitcrew_sync_jira::JiraAuth::Basic {
                email: email.clone()?,
                api_token: secret.expose().to_owned(),
            },
            3,
        ),
        JiraDeployment::DataCenter => (
            pitcrew_sync_jira::JiraAuth::Bearer {
                token: secret.expose().to_owned(),
            },
            2,
        ),
    };
    Some(pitcrew_sync_jira::SyncConfig {
        projects: projects
            .iter()
            .filter_map(|p| pitcrew_sync_jira::ProjectRef::new(p.clone()).ok())
            .collect(),
        auth,
        api_base: format!("{site}/rest/api/{version}"),
        site_base: site.clone(),
        epic_link_field: epic_link_field.clone(),
        now_unix: now_ms() / 1000,
    })
}

fn outcome_text(outcome: &CheckOutcome, tracker: &str) -> (bool, String) {
    match outcome {
        CheckOutcome::Readable { can_write: false } => (true, "Readable.".into()),
        CheckOutcome::Readable { can_write: true } => (
            true,
            "Readable. This credential could also change it; PitCrew only reads.".into(),
        ),
        CheckOutcome::NotFound => (
            false,
            "Not found, or this credential cannot read it.".into(),
        ),
        CheckOutcome::Refused => (false, format!("{tracker} refused the credential.")),
        CheckOutcome::Forbidden => (false, format!("{tracker} refused this read.")),
        CheckOutcome::NotChecked => (false, "Not checked.".into()),
        CheckOutcome::Failed(why) => (false, why.clone()),
    }
}

fn rate_limit_note(until_unix: i64) -> String {
    format!(
        "Upstream asked to wait until {}; try again then.",
        utc_rfc3339(until_unix)
    )
}

fn github_check(
    report: &pitcrew_sync_github::probe::ProbeReport,
    at: TimestampMs,
) -> IntegrationCheck {
    let credential = if report.refused() {
        (false, "GitHub refused the credential.".to_owned())
    } else if let Some(rl) = report.rate_limited {
        (false, rate_limit_note(rl.until))
    } else {
        (true, "The credential works.".to_owned())
    };
    let mut checks = vec![ScopeCheck {
        scope: String::new(),
        ok: credential.0,
        message: credential.1,
    }];
    let mut warnings = Vec::new();
    for check in &report.repos {
        let (ok, message) = outcome_text(&check.outcome, "GitHub");
        if check.outcome == (CheckOutcome::Readable { can_write: true }) {
            warnings.push(format!(
                "The credential can change {}. PitCrew only reads: a fine-grained token with \
                 read-only access to these repositories is safer.",
                check.repo.as_str()
            ));
        }
        checks.push(ScopeCheck {
            scope: check.repo.as_str().to_owned(),
            ok,
            message,
        });
    }
    let broad = report.broad_scopes();
    if !broad.is_empty() {
        warnings.push(format!(
            "This token's scopes ({}) reach further than reading these repositories. A \
             fine-grained, read-only token for them is safer.",
            broad.join(", ")
        ));
    }
    IntegrationCheck {
        ok: report.ok() && checks.iter().all(|c| c.ok),
        at,
        checks,
        warnings,
    }
}

fn jira_check(report: &pitcrew_sync_jira::probe::ProbeReport, at: TimestampMs) -> IntegrationCheck {
    let account = match (&report.account, report.rate_limited) {
        (_, Some(rl)) if !matches!(report.account, CheckOutcome::Readable { .. }) => {
            (false, rate_limit_note(rl.until))
        }
        (CheckOutcome::Readable { .. }, _) => (true, "The credential works.".to_owned()),
        (other, _) => outcome_text(other, "Jira"),
    };
    let mut checks = vec![ScopeCheck {
        scope: String::new(),
        ok: account.0,
        message: account.1,
    }];
    for check in &report.projects {
        let (ok, message) = match (&check.outcome, report.rate_limited) {
            (CheckOutcome::NotChecked, Some(rl)) => (false, rate_limit_note(rl.until)),
            (outcome, _) => outcome_text(outcome, "Jira"),
        };
        checks.push(ScopeCheck {
            scope: check.project.as_str().to_owned(),
            ok,
            message,
        });
    }
    IntegrationCheck {
        ok: report.ok() && checks.iter().all(|c| c.ok),
        at,
        checks,
        warnings: Vec::new(),
    }
}

/// The loop: outward writes to propose and to send, then each connection due, one at a time,
/// until stopped.
async fn run(integrations: Arc<Integrations>, mut stopped: watch::Receiver<bool>) {
    loop {
        if *stopped.borrow() {
            return;
        }
        tokio::select! {
            () = async {
                integrations.plan_writes().await;
                integrations.settle_writes().await;
            } => {}
            _ = stopped.changed() => return,
        }
        for id in integrations.due() {
            if *stopped.borrow() {
                return;
            }
            tokio::select! {
                () = integrations.sync_one(id) => {}
                _ = stopped.changed() => return,
            }
        }
        let wait = integrations.until_next().max(Duration::from_millis(50));
        tokio::select! {
            () = tokio::time::sleep(wait) => {}
            () = integrations.wake.notified() => {}
            _ = stopped.changed() => return,
        }
    }
}

/// The loop, running.
#[derive(Debug)]
pub struct Running {
    stop: watch::Sender<bool>,
    task: JoinHandle<()>,
    /// Wakes the loop on each append to the event log.
    waker: Option<JoinHandle<()>>,
}

impl Running {
    /// Stops the loop and waits for it, at most `within`. A sync under way is dropped: its state
    /// is kept only when it ends, so the next start reads that much again. A write being sent is
    /// dropped too: the next start finishes it as failed, and never sends it again by itself.
    pub async fn stop(self, within: Duration) {
        if let Some(waker) = &self.waker {
            waker.abort();
        }
        let _ = self.stop.send(true);
        let mut task = self.task;
        if tokio::time::timeout(within, &mut task).await.is_err() {
            task.abort();
        }
    }
}

/// The transport the hub syncs through: recorded fixtures from `fixtures` (tests), else HTTPS.
pub fn upstream(fixtures: Option<&Path>) -> anyhow::Result<Result<Upstream, String>> {
    if let Some(dir) = fixtures {
        tracing::warn!(
            dir = %dir.display(),
            "integrations read recorded fixtures, not the network (--integration-fixtures)"
        );
        let fixtures = http::FixtureTransport::load(dir)
            .with_context(|| format!("cannot read the fixtures in {}", dir.display()))?;
        return Ok(Ok(Upstream::Fixtures(fixtures)));
    }
    Ok(match http::HttpsTransport::new() {
        Ok(https) => Ok(Upstream::Https(https)),
        Err(e) => {
            tracing::warn!(error = %e, "integrations cannot reach GitHub or Jira over HTTPS");
            Err(format!(
                "The hub cannot make HTTPS connections ({e}); see the hub's log."
            ))
        }
    })
}

// Unix only: the sync tests run a stand-in `gh`, a shell script.
#[cfg(all(test, unix))]
mod tests;
// Every platform: these keep a stored secret, and run no `gh`.
#[cfg(test)]
mod writes_tests;
