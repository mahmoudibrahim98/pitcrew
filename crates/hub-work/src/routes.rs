//! The work routes of API v1 (`docs/build/contracts/api-v1.md`).
//!
//! - [`agent_routes`] are the routes marked **agent** in the contract: both token scopes may call
//!   them. Reads see the whole workspace; writes are limited to the agent's own tasks.
//! - [`device_routes`] need a person's device token. Mount them with `RouterParts::device`, which
//!   refuses agents before they get here; each handler also refuses agents itself, so mounting
//!   them in the wrong place still fails closed.
//! - [`routes`] is both.
//!
//! Handlers read the caller from `Extension<Caller>` (inserted by the API layer after
//! authentication) and the service from `Extension<Arc<WorkService>>` (added by the daemon). Every
//! failure is an `ApiError` body with its code's status. Database work runs on tokio's blocking
//! pool.
//!
//! Writes check what the path names and who may make them before they read the body: an unknown
//! task, workstream, project or ask is `404`, and an agent writing to a task that is not its own
//! gets `403`, even with a malformed or oversized body.

use crate::commands::{AnswerAsk, BriefEdit, NewAsk, NewComment, SessionLink, WorkstreamPatch};
use crate::dispatch::NewDispatch;
use crate::error::WorkError;
use crate::query::{AskFilter, SessionFilter, TaskFilter, TaskRef};
use crate::service::{WorkService, WorkspaceAt};
use axum::Json;
use axum::Router;
use axum::body::Body as RawBody;
use axum::extract::{FromRequest, FromRequestParts, Path, Query, Request};
use axum::http::StatusCode;
use axum::http::header::{CONTENT_TYPE, HeaderName};
use axum::http::request::Parts;
use axum::routing::{get, patch, post, put};
use pitcrew_protocol::api::{
    Caller, NewProject, NewTask, NewWorkstream, PersonaEdit, Setup, SetupDone, TeamEdit,
};
use pitcrew_protocol::events::{BriefTarget, Event};
use pitcrew_protocol::ids::{
    AskId, MemberId, PersonaId, ProjectId, SessionId, TeamId, WorkstreamId,
};
use pitcrew_protocol::model::{
    Ask, Brief, Dispatch, Machine, Member, Persona, Project, Session, Subtask, Task, TaskPatch,
    TaskStatus, Team, Workstream,
};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use std::str::FromStr;
use std::sync::Arc;

/// The largest request body accepted, as the contract says: 1 MiB.
pub const MAX_BODY: usize = 1024 * 1024;

type Reply<T> = Result<Json<T>, WorkError>;
type Created<T> = Result<(StatusCode, Json<T>), WorkError>;

/// Routes that agents may call too. See the [module docs](self).
pub fn agent_routes<S>() -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    Router::new()
        .route("/v1/me", get(me))
        .route("/v1/members", get(list_members))
        .route("/v1/tasks", get(list_tasks))
        .route("/v1/tasks/{id}", get(get_task))
        .route("/v1/tasks/{id}/move", post(move_task))
        .route("/v1/tasks/{id}/subtasks", put(replace_subtasks))
        .route("/v1/tasks/{id}/comments", post(post_comment))
        .route("/v1/asks", get(list_asks).post(raise_ask))
        .route("/v1/asks/{id}/answer", post(answer_ask))
}

/// Routes that only a person's device may call. See the [module docs](self).
pub fn device_routes<S>() -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    Router::new()
        .route("/v1/workspace", get(get_workspace))
        .route("/v1/me/profile", put(put_profile))
        .route("/v1/me/cursors", get(get_cursors))
        .route("/v1/me/cursors/{scope}", put(put_cursor))
        .route("/v1/setup", post(post_setup))
        .route("/v1/machines", get(list_machines))
        .route("/v1/machines/{id}", put(put_machine))
        .route("/v1/personas", get(list_personas).post(create_persona))
        .route("/v1/personas/{id}", put(edit_persona))
        .route("/v1/teams", get(list_teams).post(create_team))
        .route("/v1/teams/{id}", put(edit_team))
        .route("/v1/projects", get(list_projects).post(create_project))
        .route("/v1/projects/{id}", get(get_project))
        .route(
            "/v1/workstreams",
            get(list_workstreams).post(create_workstream),
        )
        .route(
            "/v1/workstreams/{id}",
            get(get_workstream).patch(patch_workstream),
        )
        .route("/v1/tasks", post(create_task))
        .route("/v1/tasks/{id}", patch(patch_task))
        .route("/v1/tasks/{id}/assign", post(assign_task))
        .route("/v1/tasks/{id}/dispatch", post(dispatch_task))
        .route("/v1/safety", get(get_safety).put(put_safety))
        .route("/v1/import", get(get_import).put(put_import))
        .route("/v1/import/dry-run", post(dry_run_import))
        .route("/v1/sessions", get(list_sessions))
        .route("/v1/sessions/{id}", get(get_session))
        .route("/v1/sessions/{id}/link", post(link_session))
        .route("/v1/briefs", get(list_briefs))
        .route("/v1/briefs/{kind}/{id}", put(put_brief))
}

/// Every work route: [`agent_routes`] and [`device_routes`].
pub fn routes<S>() -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    agent_routes().merge(device_routes())
}

// ─── Extractors ──────────────────────────────────────────────────────────────────────────────────

/// The service, from the request's extensions.
struct Work(Arc<WorkService>);

impl<S: Send + Sync> FromRequestParts<S> for Work {
    type Rejection = WorkError;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, WorkError> {
        parts
            .extensions
            .get::<Arc<WorkService>>()
            .cloned()
            .map(Self)
            .ok_or_else(|| WorkError::internal("The work service is not wired into the API."))
    }
}

fn caller(parts: &Parts) -> Result<Caller, WorkError> {
    // Only reachable if a route is mounted outside the API layer's authentication.
    parts.extensions.get::<Caller>().copied().ok_or_else(|| {
        WorkError::new(
            pitcrew_protocol::api::ErrorCode::Unauthorized,
            "This route needs a bearer token.",
        )
    })
}

/// The caller, of either scope.
struct Who(Caller);

impl<S: Send + Sync> FromRequestParts<S> for Who {
    type Rejection = WorkError;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, WorkError> {
        caller(parts).map(Self)
    }
}

/// The caller, who must be a person (a device token).
struct Person(Caller);

impl<S: Send + Sync> FromRequestParts<S> for Person {
    type Rejection = WorkError;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, WorkError> {
        let caller = caller(parts)?;
        if caller.is_person() {
            Ok(Self(caller))
        } else {
            Err(WorkError::forbidden(format!(
                "{} {} needs a device token.",
                parts.method,
                parts.uri.path()
            )))
        }
    }
}

/// Path parameters. A segment that does not decode names nothing, so it is a `404`.
struct Segments<T>(T);

impl<S: Send + Sync, T: DeserializeOwned + Send> FromRequestParts<S> for Segments<T> {
    type Rejection = WorkError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, WorkError> {
        Path::<T>::from_request_parts(parts, state)
            .await
            .map(|Path(t)| Self(t))
            .map_err(|e| WorkError::not_found(format!("No such resource: {e}.")))
    }
}

/// The query string as pairs, so a key may repeat (`status=todo&status=review`).
struct Params(Vec<(String, String)>);

impl<S: Send + Sync> FromRequestParts<S> for Params {
    type Rejection = WorkError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, WorkError> {
        Query::<Vec<(String, String)>>::from_request_parts(parts, state)
            .await
            .map(|Query(pairs)| Self(pairs))
            .map_err(|e| WorkError::invalid(format!("Malformed query: {e}.")))
    }
}

impl Params {
    /// The first non-empty value of `key`, parsed. An empty value counts as absent.
    fn one<T: FromStr>(&self, key: &str) -> Result<Option<T>, WorkError> {
        self.values(key)
            .next()
            .map(|v| {
                v.parse()
                    .map_err(|_| WorkError::invalid(format!("{key}: {v:?} is not a valid id.")))
            })
            .transpose()
    }

    /// Every non-empty value of `key`, each a value of the enum `T`.
    fn all<T: DeserializeOwned>(&self, key: &str) -> Result<Vec<T>, WorkError> {
        self.values(key)
            .map(|v| {
                crate::codec::parse_enum(v)
                    .map_err(|_| WorkError::invalid(format!("{key}: unknown value {v:?}.")))
            })
            .collect()
    }

    fn values<'a>(&'a self, key: &'a str) -> impl Iterator<Item = &'a str> + 'a {
        self.0
            .iter()
            .filter(move |(k, v)| k == key && !v.is_empty())
            .map(|(_, v)| v.as_str())
    }
}

/// Reads a JSON body of at most [`MAX_BODY`] bytes. Anything malformed is `400 invalid`.
async fn json_value(body: RawBody) -> Result<serde_json::Value, WorkError> {
    let bytes = axum::body::to_bytes(body, MAX_BODY)
        .await
        .map_err(|_| WorkError::invalid("The body is larger than 1 MiB, or could not be read."))?;
    serde_json::from_slice(&bytes).map_err(|e| WorkError::invalid(format!("Malformed body: {e}.")))
}

fn decode<T: DeserializeOwned>(value: serde_json::Value) -> Result<T, WorkError> {
    T::deserialize(value).map_err(|e| WorkError::invalid(format!("Malformed body: {e}.")))
}

/// A JSON object body (see [`json_value`]); unknown fields are ignored. Anything but an object is
/// `400`: serde would otherwise read an array as a struct's fields by position.
async fn json<T: DeserializeOwned>(body: RawBody) -> Result<T, WorkError> {
    let value = json_value(body).await?;
    if !value.is_object() {
        return Err(WorkError::invalid("The body must be a JSON object."));
    }
    decode(value)
}

/// A JSON array body (see [`json_value`]).
async fn json_list<T: DeserializeOwned>(body: RawBody) -> Result<Vec<T>, WorkError> {
    let value = json_value(body).await?;
    if !value.is_array() {
        return Err(WorkError::invalid("The body must be a JSON array."));
    }
    decode(value)
}

/// A JSON object body, read as soon as the handler runs. For routes whose path names nothing to
/// look up and whose caller is already known to be allowed. See [`json`].
struct Body<T>(T);

impl<S: Send + Sync, T: DeserializeOwned> FromRequest<S> for Body<T> {
    type Rejection = WorkError;

    async fn from_request(request: Request, _: &S) -> Result<Self, WorkError> {
        json(request.into_body()).await.map(Self)
    }
}

/// Runs `f` on the blocking pool, where the service's SQLite work belongs.
async fn blocking<T, F>(work: Arc<WorkService>, f: F) -> Result<T, WorkError>
where
    T: Send + 'static,
    F: FnOnce(&WorkService) -> Result<T, WorkError> + Send + 'static,
{
    tokio::task::spawn_blocking(move || f(&work))
        .await
        .map_err(|e| WorkError::internal(format!("The work task failed: {e}.")))?
}

fn task_ref(id: &str) -> Result<TaskRef, WorkError> {
    TaskRef::parse(id).ok_or_else(|| WorkError::not_found(format!("No task {id}.")))
}

fn path_id<T: FromStr>(id: &str, what: &str) -> Result<T, WorkError> {
    id.parse()
        .map_err(|_| WorkError::not_found(format!("No {what} {id}.")))
}

/// Checks that `caller` may write to `task` (`404`, then `403`), before the body is read.
async fn may_write(w: &Arc<WorkService>, caller: Caller, task: &TaskRef) -> Result<(), WorkError> {
    let task = task.clone();
    blocking(Arc::clone(w), move |w| w.check_task_write(&caller, &task)).await
}

/// Checks that what the path names exists (`404`, from `find`), before the body is read.
async fn exists<T, F>(w: &Arc<WorkService>, find: F) -> Result<(), WorkError>
where
    T: Send + 'static,
    F: FnOnce(&WorkService) -> Result<T, WorkError> + Send + 'static,
{
    blocking(Arc::clone(w), find).await.map(|_| ())
}

// ─── Workspace, members, machines, personas, teams ───────────────────────────────────────────────

async fn get_workspace(Work(w): Work, Person(_): Person) -> Reply<WorkspaceAt> {
    Ok(Json(blocking(w, WorkService::workspace_at).await?))
}

/// `POST /v1/setup`: the first run of a fresh hub. Device tokens only (`Person` rejects an agent
/// before the body is read, as the contract's "once only" rules require).
async fn post_setup(
    Work(w): Work,
    Person(caller): Person,
    Body(setup): Body<Setup>,
) -> Reply<SetupDone> {
    Ok(Json(blocking(w, move |w| w.set_up(&caller, setup)).await?))
}

async fn me(Work(w): Work, Who(caller): Who) -> Reply<Member> {
    Ok(Json(blocking(w, move |w| w.member(&caller.member)).await?))
}

async fn list_members(Work(w): Work, Who(_): Who) -> Reply<Vec<Member>> {
    Ok(Json(blocking(w, WorkService::members).await?))
}

async fn list_machines(Work(w): Work, Person(_): Person) -> Reply<Vec<Machine>> {
    Ok(Json(blocking(w, WorkService::machines).await?))
}

async fn put_machine(
    Work(w): Work,
    Person(caller): Person,
    Segments(id): Segments<pitcrew_protocol::ids::MachineId>,
    body: RawBody,
) -> Reply<Machine> {
    blocking(Arc::clone(&w), move |w| {
        w.require_owner(&caller)?;
        w.read(|c| crate::query::machine(c, &id))?
            .ok_or_else(|| WorkError::not_found("Unknown machine."))?;
        Ok(())
    })
    .await?;
    let input: pitcrew_protocol::settings::SaveMachine = json(body).await?;
    Ok(Json(
        blocking(w, move |w| w.rename_machine(&caller, id, &input.name)).await?,
    ))
}

async fn list_personas(Work(w): Work, Person(_): Person) -> Reply<Vec<Persona>> {
    Ok(Json(blocking(w, WorkService::personas).await?))
}

async fn put_profile(
    Work(w): Work,
    Person(caller): Person,
    Body(input): Body<pitcrew_protocol::settings::SaveProfile>,
) -> Reply<Member> {
    Ok(Json(
        blocking(w, move |w| w.save_profile(&caller, input)).await?,
    ))
}

async fn list_teams(Work(w): Work, Person(_): Person) -> Reply<Vec<Team>> {
    Ok(Json(blocking(w, WorkService::teams).await?))
}

// ─── Projects and workstreams ────────────────────────────────────────────────────────────────────

async fn list_projects(Work(w): Work, Person(_): Person) -> Reply<Vec<Project>> {
    Ok(Json(blocking(w, WorkService::projects).await?))
}

async fn get_project(
    Work(w): Work,
    Person(_): Person,
    Segments(id): Segments<String>,
) -> Reply<Project> {
    let id: ProjectId = path_id(&id, "project")?;
    Ok(Json(blocking(w, move |w| w.project(&id)).await?))
}

async fn create_project(
    Work(w): Work,
    Person(caller): Person,
    Body(new): Body<NewProject>,
) -> Created<Project> {
    let project = blocking(w, move |w| w.create_project(&caller, new)).await?;
    Ok((StatusCode::CREATED, Json(project)))
}

async fn create_workstream(
    Work(w): Work,
    Person(caller): Person,
    Body(new): Body<NewWorkstream>,
) -> Created<Workstream> {
    let workstream = blocking(w, move |w| w.create_workstream(&caller, new)).await?;
    Ok((StatusCode::CREATED, Json(workstream)))
}

async fn list_workstreams(
    Work(w): Work,
    Person(_): Person,
    params: Params,
) -> Reply<Vec<Workstream>> {
    let project: Option<ProjectId> = params.one("project")?;
    Ok(Json(
        blocking(w, move |w| w.workstreams(project.as_ref())).await?,
    ))
}

async fn get_workstream(
    Work(w): Work,
    Person(_): Person,
    Segments(id): Segments<String>,
) -> Reply<Workstream> {
    let id: WorkstreamId = path_id(&id, "workstream")?;
    Ok(Json(blocking(w, move |w| w.workstream(&id)).await?))
}

async fn patch_workstream(
    Work(w): Work,
    Person(caller): Person,
    Segments(id): Segments<String>,
    body: RawBody,
) -> Reply<Workstream> {
    let id: WorkstreamId = path_id(&id, "workstream")?;
    exists(&w, move |w| w.workstream(&id)).await?;
    let patch: WorkstreamPatch = json(body).await?;
    Ok(Json(
        blocking(w, move |w| w.patch_workstream(&caller, &id, patch)).await?,
    ))
}

// ─── Tasks ───────────────────────────────────────────────────────────────────────────────────────

/// The task list is sent as stored (see [`WorkService::tasks_json`]), so a long list is not
/// decoded and encoded again.
async fn list_tasks(
    Work(w): Work,
    Who(_): Who,
    params: Params,
) -> Result<([(HeaderName, &'static str); 1], String), WorkError> {
    let filter = TaskFilter {
        project: params.one("project")?,
        workstream: params.one("workstream")?,
        assignee: params.one("assignee")?,
        statuses: params.all("status")?,
    };
    let body = blocking(w, move |w| w.tasks_json(&filter)).await?;
    Ok(([(CONTENT_TYPE, "application/json")], body))
}

async fn get_task(Work(w): Work, Who(_): Who, Segments(id): Segments<String>) -> Reply<Task> {
    let task = task_ref(&id)?;
    Ok(Json(blocking(w, move |w| w.task(&task)).await?))
}

async fn create_task(
    Work(w): Work,
    Person(caller): Person,
    Body(new): Body<NewTask>,
) -> Created<Task> {
    let task = blocking(w, move |w| w.create_task(&caller, new)).await?;
    Ok((StatusCode::CREATED, Json(task)))
}

async fn patch_task(
    Work(w): Work,
    Person(caller): Person,
    Segments(id): Segments<String>,
    body: RawBody,
) -> Reply<Task> {
    let task = task_ref(&id)?;
    may_write(&w, caller, &task).await?;
    let patch: TaskPatch = json(body).await?;
    Ok(Json(
        blocking(w, move |w| w.patch_task(&caller, &task, patch)).await?,
    ))
}

#[derive(Deserialize)]
struct MoveTask {
    to: TaskStatus,
}

async fn move_task(
    Work(w): Work,
    Who(caller): Who,
    Segments(id): Segments<String>,
    body: RawBody,
) -> Reply<Task> {
    let task = task_ref(&id)?;
    may_write(&w, caller, &task).await?;
    let body: MoveTask = json(body).await?;
    Ok(Json(
        blocking(w, move |w| w.move_task(&caller, &task, body.to)).await?,
    ))
}

async fn assign_task(
    Work(w): Work,
    Person(caller): Person,
    Segments(id): Segments<String>,
    body: RawBody,
) -> Reply<Task> {
    let task = task_ref(&id)?;
    may_write(&w, caller, &task).await?;
    let body: serde_json::Map<String, serde_json::Value> = json(body).await?;
    let assignee: Option<MemberId> = match body.get("assignee") {
        None => {
            return Err(WorkError::invalid(
                "assignee is required; send null to unassign.",
            ));
        }
        Some(value) => serde_json::from_value(value.clone())
            .map_err(|e| WorkError::invalid(format!("assignee: {e}.")))?,
    };
    Ok(Json(
        blocking(w, move |w| w.assign_task(&caller, &task, assignee)).await?,
    ))
}

async fn replace_subtasks(
    Work(w): Work,
    Who(caller): Who,
    Segments(id): Segments<String>,
    body: RawBody,
) -> Reply<Task> {
    let task = task_ref(&id)?;
    may_write(&w, caller, &task).await?;
    let subtasks: Vec<Subtask> = json_list(body).await?;
    Ok(Json(
        blocking(w, move |w| w.replace_subtasks(&caller, &task, subtasks)).await?,
    ))
}

async fn post_comment(
    Work(w): Work,
    Who(caller): Who,
    Segments(id): Segments<String>,
    body: RawBody,
) -> Created<Event> {
    let task = task_ref(&id)?;
    may_write(&w, caller, &task).await?;
    let comment: NewComment = json(body).await?;
    let event = blocking(w, move |w| w.post_comment(&caller, &task, comment)).await?;
    Ok((StatusCode::CREATED, Json(event)))
}

/// `202 Accepted`: the dispatch is recorded and its session is starting.
async fn dispatch_task(
    Work(w): Work,
    Person(caller): Person,
    Segments(id): Segments<String>,
    body: RawBody,
) -> Result<(StatusCode, Json<Dispatch>), WorkError> {
    let task = task_ref(&id)?;
    may_write(&w, caller, &task).await?;
    let new: NewDispatch = json(body).await?;
    let dispatch = blocking(w, move |w| w.dispatch_task(&caller, &task, new)).await?;
    Ok((StatusCode::ACCEPTED, Json(dispatch)))
}

// ─── Sessions ────────────────────────────────────────────────────────────────────────────────────

async fn list_sessions(Work(w): Work, Person(_): Person, params: Params) -> Reply<Vec<Session>> {
    let filter = SessionFilter {
        machine: params.one("machine")?,
        workstream: params.one("workstream")?,
        task: params.one("task")?,
        states: params.all("state")?,
    };
    Ok(Json(
        blocking(w, move |w| {
            let choice = w.import_choice();
            Ok(w.sessions(&filter)?
                .into_iter()
                .filter(|s| choice.includes(s))
                .collect())
        })
        .await?,
    ))
}

async fn get_session(
    Work(w): Work,
    Person(_): Person,
    Segments(id): Segments<String>,
) -> Reply<Session> {
    let id: SessionId = path_id(&id, "session")?;
    Ok(Json(
        blocking(w, move |w| {
            if !w.session_included(&id)? {
                return Err(WorkError::not_found("Session is excluded."));
            }
            w.session(&id)
        })
        .await?,
    ))
}

async fn link_session(
    Work(w): Work,
    Person(caller): Person,
    Segments(id): Segments<String>,
    body: RawBody,
) -> Reply<Session> {
    let id: SessionId = path_id(&id, "session")?;
    blocking(Arc::clone(&w), move |w| w.session(&id)).await?;
    let link: SessionLink = json(body).await?;
    Ok(Json(
        blocking(w, move |w| w.link_session(&caller, &id, link)).await?,
    ))
}

// ─── Asks and briefs ─────────────────────────────────────────────────────────────────────────────

async fn list_asks(Work(w): Work, Who(_): Who, params: Params) -> Reply<Vec<Ask>> {
    let filter = AskFilter {
        to: params.one("to")?,
        states: params.all("state")?,
    };
    Ok(Json(blocking(w, move |w| w.asks(&filter)).await?))
}

async fn raise_ask(Work(w): Work, Who(caller): Who, Body(new): Body<NewAsk>) -> Created<Ask> {
    let ask = blocking(w, move |w| w.raise_ask(&caller, new)).await?;
    Ok((StatusCode::CREATED, Json(ask)))
}

async fn answer_ask(
    Work(w): Work,
    Who(caller): Who,
    Segments(id): Segments<String>,
    body: RawBody,
) -> Reply<Ask> {
    let id: AskId = path_id(&id, "ask")?;
    blocking(Arc::clone(&w), move |w| w.check_answer(&caller, &id)).await?;
    let answer: AnswerAsk = json(body).await?;
    Ok(Json(
        blocking(w, move |w| w.answer_ask(&caller, &id, answer)).await?,
    ))
}

async fn list_briefs(Work(w): Work, Person(_): Person) -> Reply<Vec<Brief>> {
    Ok(Json(blocking(w, WorkService::briefs).await?))
}

async fn get_cursors(
    Work(w): Work,
    Person(caller): Person,
) -> Reply<Vec<pitcrew_protocol::api::ReadCursor>> {
    Ok(Json(blocking(w, move |w| w.cursors(&caller)).await?))
}

async fn put_cursor(
    Work(w): Work,
    Person(caller): Person,
    Segments(scope): Segments<String>,
    Body(request): Body<pitcrew_protocol::api::MoveCursor>,
) -> Reply<pitcrew_protocol::api::ReadCursor> {
    Ok(Json(
        blocking(w, move |w| w.move_cursor(&caller, &scope, request.rev)).await?,
    ))
}

async fn put_brief(
    Work(w): Work,
    Person(caller): Person,
    Segments((kind, id)): Segments<(String, String)>,
    body: RawBody,
) -> Reply<Brief> {
    let target = match kind.as_str() {
        "project" => BriefTarget::Project(path_id(&id, "project")?),
        "workstream" => BriefTarget::Workstream(path_id(&id, "workstream")?),
        _ => {
            return Err(WorkError::not_found(format!(
                "Briefs belong to a project or a workstream, not to \"{kind}\"."
            )));
        }
    };
    match target {
        BriefTarget::Project(id) => exists(&w, move |w| w.project(&id)).await?,
        BriefTarget::Workstream(id) => exists(&w, move |w| w.workstream(&id)).await?,
    }
    let edit: BriefEdit = json(body).await?;
    Ok(Json(
        blocking(w, move |w| w.put_brief(&caller, target, edit)).await?,
    ))
}

async fn get_import(
    Work(w): Work,
    Person(_): Person,
) -> Reply<pitcrew_protocol::import::ImportChoice> {
    Ok(Json(w.import_choice()))
}
async fn dry_run_import(
    Work(w): Work,
    Person(_): Person,
    Body(filter): Body<pitcrew_protocol::import::ImportFilter>,
) -> Reply<pitcrew_protocol::import::ImportDryRun> {
    Ok(Json(pitcrew_protocol::import::ImportDryRun {
        count: blocking(w, move |w| w.import_dry_run(filter)).await?,
    }))
}
async fn put_import(
    Work(w): Work,
    Person(_): Person,
    Body(filter): Body<pitcrew_protocol::import::ImportFilter>,
) -> Reply<pitcrew_protocol::import::ImportResult> {
    Ok(Json(pitcrew_protocol::import::ImportResult {
        imported: blocking(w, move |w| w.commit_import(filter)).await?,
    }))
}

async fn create_persona(
    Work(w): Work,
    Person(caller): Person,
    Body(edit): Body<PersonaEdit>,
) -> Created<Persona> {
    Ok((
        StatusCode::CREATED,
        Json(blocking(w, move |w| w.save_persona(&caller, None, edit)).await?),
    ))
}
async fn edit_persona(
    Work(w): Work,
    Person(caller): Person,
    Segments(id): Segments<String>,
    body: RawBody,
) -> Reply<Persona> {
    let id: PersonaId = path_id(&id, "persona")?;
    exists(&w, move |w| {
        w.personas()?
            .into_iter()
            .find(|p| p.id == id)
            .ok_or_else(|| WorkError::not_found("No such persona."))
    })
    .await?;
    let input: pitcrew_protocol::settings::SavePersona = json(body).await?;
    let edit = PersonaEdit {
        name: input.name,
        engine: input.engine,
        model: input.model,
        instructions: input.instructions,
        permission_mode: input.permission_mode,
    };
    Ok(Json(
        blocking(w, move |w| w.save_persona(&caller, Some(id), edit)).await?,
    ))
}
async fn create_team(
    Work(w): Work,
    Person(caller): Person,
    Body(edit): Body<TeamEdit>,
) -> Created<Team> {
    Ok((
        StatusCode::CREATED,
        Json(blocking(w, move |w| w.save_team(&caller, None, edit)).await?),
    ))
}
async fn edit_team(
    Work(w): Work,
    Person(caller): Person,
    Segments(id): Segments<String>,
    body: RawBody,
) -> Reply<Team> {
    let id: TeamId = path_id(&id, "team")?;
    exists(&w, move |w| {
        w.teams()?
            .into_iter()
            .find(|t| t.id == id)
            .ok_or_else(|| WorkError::not_found("No such team."))
    })
    .await?;
    let edit: TeamEdit = json(body).await?;
    Ok(Json(
        blocking(w, move |w| w.save_team(&caller, Some(id), edit)).await?,
    ))
}

async fn get_safety(Work(w): Work, Person(_): Person) -> Reply<serde_json::Value> {
    Ok(Json(
        blocking(w, |w| {
            let mut value = serde_json::to_value(w.safety()?)?;
            if !w.safety_saved()? {
                value["saved"] = serde_json::json!(false);
            }
            Ok(value)
        })
        .await?,
    ))
}
async fn put_safety(
    Work(w): Work,
    Person(caller): Person,
    Body(settings): Body<pitcrew_protocol::onboarding::SaveSafety>,
) -> Reply<pitcrew_protocol::onboarding::SafetySettings> {
    Ok(Json(
        blocking(w, move |w| w.save_safety(&caller, settings.into())).await?,
    ))
}
