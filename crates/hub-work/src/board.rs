//! Board drafts: an agent drafts a workstream's board from its history, and a person reviews it
//! (api-v1.md, "Board drafts"). **Nothing is created until the person accepts it.**
//!
//! 1. **Preview** ([`WorkService::draft_preview`], person): the workstream's facts (its sessions,
//!    as its routes would list them, without sub-agents and drafting sessions; their recap lines
//!    and counts, their files relative to their folders; its tasks) become the office's bounded,
//!    redacted summary inside the versioned prompt (`pitcrew_office::board`). Nothing is sent or
//!    stored; the prompt is kept in memory for [`PREVIEW_KEPT_MS`], the workstream's latest only.
//!    The answer holds the summary, its sizes, the estimate, and the prompt's digest.
//! 2. **Start** ([`WorkService::start_draft`], person): with the digest the person confirmed. The
//!    workstream's latest preview, while it is kept and has that digest, is what is sent, exactly;
//!    else the prompt is made again, and if it differs (the workstream changed), `409`. Then,
//!    under the command lock: the agent is one of the caller's own (`403` otherwise; the back
//!    office, `@office`, when none is named), no other draft of the workstream is running or
//!    waiting for review (`409`), and the hub's own machine is live (`503` otherwise). It appends
//!    `session_discovered` (state `starting`, the agent, linked to the workstream by hand, in the
//!    folder the dispatcher names) and `board_draft_started`, then asks the
//!    [`Dispatcher`](crate::Dispatcher) to start the agent's CLI **confined**
//!    ([`Confinement`]: a fresh private folder, never the workstream's, with the prompt in
//!    [`PROMPT_FILE`](crate::PROMPT_FILE); the CLI's read-mostly launch shape, whatever its
//!    persona's mode; only `pitcrew board submit` and writing [`PROPOSAL_FILE`] pre-approved; a
//!    session token in place of the agent's; at most [`DRAFT_MAX_RUNTIME`]). A start that fails
//!    ends the session, and the draft with it.
//! 3. **Propose** ([`WorkService::propose_board`], the draft's session token only): a
//!    [`BoardProposal`] of at most `MAX_PROPOSAL_BYTES` (the route's bound), every text checked
//!    against its bound and redacted, every evidence session one of the workstream's. Once per
//!    draft: `board_proposed`. Then the dispatcher [finishes](crate::Dispatcher::finish_session)
//!    the session: its token stops working at once, and its CLI is ended.
//! 4. **Review** ([`WorkService::review_draft`], person): the accepted items become tasks in one
//!    append with the review: `task_created` for each (in the workstream, at its proposed status,
//!    labelled [`DRAFTED_LABEL`]), `session_linked` (by hand) for each evidence session not yet
//!    linked to a task, and `board_draft_reviewed`. Rejected items create nothing. Once per
//!    draft.
//!
//! Drafts are read from the log itself (their three event types, by the store's type index):
//! there are few, and they need no table of their own; an event that cannot be read is logged and
//! skipped. A running draft whose session has ended is [`DraftState::Ended`]: when its CLI exits,
//! when it runs past [`DRAFT_MAX_RUNTIME`], or when the hub restarts ([`WorkService::end_running_drafts`]:
//! its session token is gone).

use crate::dispatch::{Confinement, DispatchError, SessionRequest, refused, require_owner};
use crate::error::{Result, WorkError};
use crate::office::OFFICE_HANDLE;
use crate::query::{self, SessionFilter, TaskFilter};
use crate::recap::{BlockFilter, RecapIndex};
use crate::service::WorkService;
use pitcrew_office::board::{DraftFacts, SessionFacts, TaskFacts, draft_prompt};
use pitcrew_office::redact;
use pitcrew_protocol::api::{Caller, NewTask, TokenScope};
use pitcrew_protocol::board::{
    BoardDraft, BoardProposal, DRAFTED_LABEL, DraftPreview, DraftReview, DraftReviewed, DraftState,
    DraftedTask, MAX_EVIDENCE, MAX_NOTE, MAX_PROPOSED_DESCRIPTION, MAX_PROPOSED_TASKS,
    MAX_PROPOSED_TITLE, ProposedTask, StartDraft,
};
use pitcrew_protocol::events::{Event, EventBody};
use pitcrew_protocol::ids::{DraftId, MemberId, SessionId, TaskId, WorkstreamId};
use pitcrew_protocol::model::{
    Engine, LinkBasis, Liveness, Machine, MemberKind, PermissionMode, Session, SessionState,
    TaskStatus, TimestampMs, Workstream,
};
use pitcrew_store::sql::{Connection, params};
use sha2::{Digest as _, Sha256};
use std::collections::{HashMap, HashSet};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::time::Duration;

/// The event types a draft is made of.
const TYPES: [&str; 3] = [
    "board_draft_started",
    "board_proposed",
    "board_draft_reviewed",
];

/// The newest blocks of work read per session, for its counts and files.
const BLOCKS_PER_SESSION: usize = 50;

/// What the preview's prompt names as its draft: as long as a real id, so the sizes are exact.
const PLACEHOLDER_ID: &str = "drf_00000000000000000000000000";

/// The longest a draft's session runs: then its CLI is ended, and the draft with it, so an idle
/// CLI, or one that is not signed in, never blocks the workstream's next draft for long.
pub const DRAFT_MAX_RUNTIME: Duration = Duration::from_secs(30 * 60);

/// The file a draft's CLI may write its proposal to, in its folder, for `pitcrew board submit
/// <draft> --file proposal.json`.
pub const PROPOSAL_FILE: &str = "proposal.json";

/// How long a preview's prompt is kept for its start, in milliseconds.
pub const PREVIEW_KEPT_MS: i64 = 10 * 60 * 1000;

/// The most workstreams whose latest preview is kept.
const PREVIEWS_KEPT: usize = 64;

/// The prompt for a workstream, as made at a preview or now.
#[derive(Clone)]
struct Made {
    prompt: pitcrew_office::board::DraftPrompt,
    digest: String,
}

/// Each workstream's latest previewed prompt, while it is kept ([`PREVIEW_KEPT_MS`]): a start
/// with its digest sends exactly it, however the workstream's live facts moved since.
#[derive(Default)]
pub(crate) struct Previews(Vec<(WorkstreamId, TimestampMs, Made)>);

impl Previews {
    fn keep(&mut self, workstream: WorkstreamId, at: TimestampMs, made: Made) {
        self.0.retain(|(w, ..)| *w != workstream);
        self.0.push((workstream, at, made));
        if self.0.len() > PREVIEWS_KEPT {
            self.0.remove(0);
        }
    }

    fn take(&mut self, workstream: WorkstreamId, digest: &str, now: TimestampMs) -> Option<Made> {
        self.0
            .retain(|(_, at, _)| now.saturating_sub(*at) <= PREVIEW_KEPT_MS);
        let i = self
            .0
            .iter()
            .position(|(w, _, made)| *w == workstream && made.digest == digest)?;
        Some(self.0.remove(i).2)
    }
}

/// The confinement of a draft's run: `pitcrew board submit`, the proposal's file, and
/// [`DRAFT_MAX_RUNTIME`].
fn draft_confinement() -> Confinement {
    Confinement {
        commands: vec!["board submit".to_owned()],
        writes: vec![PROPOSAL_FILE.to_owned()],
        max_runtime: DRAFT_MAX_RUNTIME,
    }
}

impl WorkService {
    /// `GET /v1/workstreams/{id}/board-draft`: what a draft of `workstream` would send its agent,
    /// and its estimate. Nothing is sent or stored. People only.
    ///
    /// # Errors
    ///
    /// `forbidden` for an agent; `not_found` for an unknown workstream; database errors.
    pub fn draft_preview(
        &self,
        caller: &Caller,
        workstream: &WorkstreamId,
    ) -> Result<DraftPreview> {
        crate::commands::require_person(caller, "Previewing a board draft")?;
        let workstream = self.workstream(workstream)?;
        let made = self.make_prompt(&workstream)?;
        self.previews
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .keep(workstream.id, self.now(), made.clone());
        Ok(DraftPreview {
            workstream: workstream.id,
            prompt: pitcrew_office::prompts::DRAFT_BOARD.id(),
            cost: made.prompt.cost,
            summary: made.prompt.summary.text,
            digest: made.digest,
        })
    }

    /// The workstream's facts, the prompt they make (naming [`PLACEHOLDER_ID`]) and its digest.
    fn make_prompt(&self, workstream: &Workstream) -> Result<Made> {
        let facts = self.draft_facts(workstream)?;
        let prompt = draft_prompt(&facts, PLACEHOLDER_ID);
        let digest = hex(&Sha256::digest(prompt.text.as_bytes()));
        Ok(Made { prompt, digest })
    }

    /// What the summary is made from: see the [module docs](self).
    fn draft_facts(&self, workstream: &Workstream) -> Result<DraftFacts> {
        let choice = self.import_choice();
        let (project, tasks, sessions, drafting) = self.read(|c| {
            let project = query::project(c, &workstream.project)?;
            let tasks = query::tasks(
                c,
                &TaskFilter {
                    workstream: Some(workstream.id),
                    ..TaskFilter::default()
                },
            )?;
            let sessions = query::sessions(
                c,
                &SessionFilter {
                    workstream: Some(workstream.id),
                    ..SessionFilter::default()
                },
            )?;
            let drafting: HashSet<SessionId> =
                load_drafts(c)?.into_iter().map(|d| d.session).collect();
            Ok((project, tasks, sessions, drafting))
        })?;
        let keys: HashMap<TaskId, String> =
            tasks.iter().map(|t| (t.id, t.key.to_string())).collect();
        let mut facts = DraftFacts {
            workstream: workstream.name.clone(),
            project: project.map(|p| p.name).unwrap_or_default(),
            tasks: tasks
                .iter()
                .map(|t| TaskFacts {
                    key: t.key.to_string(),
                    status: t.status,
                    title: t.title.clone(),
                })
                .collect(),
            sessions: Vec::new(),
        };
        for session in sessions {
            if session.parent.is_some()
                || drafting.contains(&session.id)
                || !choice.includes(&session)
            {
                continue;
            }
            let facts_of = self.session_facts(&session, &keys)?;
            facts.sessions.push(facts_of);
        }
        Ok(facts)
    }

    /// One session's facts, from the recap index: its newest blocks' counts, files and lines.
    fn session_facts(
        &self,
        session: &Session,
        keys: &HashMap<TaskId, String>,
    ) -> Result<SessionFacts> {
        let page = self.recap_blocks(
            &BlockFilter {
                session: Some(session.id),
                ..BlockFilter::default()
            },
            None,
            Some(BLOCKS_PER_SESSION),
        )?;
        let mut facts = SessionFacts {
            id: session.id,
            engine: session.engine,
            title: session.title.clone(),
            state: session.state,
            branch: session.branch.clone(),
            started: session.started,
            last_activity: session.last_activity,
            task: session.task.and_then(|t| keys.get(&t).cloned()),
            turns: 0,
            tools: 0,
            tools_failed: 0,
            edits: 0,
            files: Vec::new(),
            recaps: Vec::new(),
        };
        let mut files: Vec<(String, u32)> = Vec::new();
        for recap in &page.blocks {
            let counts = &recap.block.counts;
            facts.turns = facts.turns.saturating_add(counts.turns);
            facts.tools = facts.tools.saturating_add(counts.tools_run);
            facts.tools_failed = facts.tools_failed.saturating_add(counts.tools_failed);
            facts.edits = facts.edits.saturating_add(counts.file_edits);
            for touch in &recap.block.files {
                match files.iter_mut().find(|(path, _)| *path == touch.path) {
                    Some((_, edits)) => *edits = edits.saturating_add(touch.edits),
                    None => files.push((touch.path.clone(), touch.edits)),
                }
            }
            if facts.recaps.len() < pitcrew_office::board::MAX_RECAP_LINES
                && !recap.line.text.trim().is_empty()
            {
                facts.recaps.push(recap.line.text.clone());
            }
        }
        // Most edited first; a stable order among equals.
        files.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        facts.files = files.into_iter().map(|(path, _)| path).collect();
        Ok(facts)
    }

    /// `POST /v1/workstreams/{id}/board-drafts`: starts drafting `workstream`'s board with the
    /// preview the person confirmed. See the [module docs](self).
    ///
    /// # Errors
    ///
    /// `forbidden` for an agent, or an agent the caller does not own; `not_found` for an unknown
    /// workstream; `invalid` for an unknown agent, a person as the agent, or no agent named when
    /// there is no back office; `conflict` when the workstream changed since the preview (the
    /// digest differs), or another of its drafts is running or waiting for review, or the runner
    /// refused; `unavailable` when no session can start (no runner, no live machine).
    pub fn start_draft(
        &self,
        caller: &Caller,
        workstream: &WorkstreamId,
        start: StartDraft,
    ) -> Result<BoardDraft> {
        crate::commands::require_person(caller, "Drafting a board")?;
        let workstream = self.workstream(workstream)?;
        let previewed = self
            .previews
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take(workstream.id, &start.digest, self.now());
        let made = match previewed {
            Some(made) => made,
            None => self.make_prompt(&workstream)?,
        };
        let (draft, request, dispatcher) = {
            let _guard = self.lock();
            let (agent, machine, persona) = self.read(|c| {
                let agent = match &start.agent {
                    Some(id) => query::member(c, id)?
                        .ok_or_else(|| WorkError::invalid(format!("agent: no member {id}.")))?,
                    None => query::member_with_handle(c, OFFICE_HANDLE)?
                        .filter(|m| m.owner == Some(caller.member))
                        .ok_or_else(|| {
                            WorkError::invalid(
                                "Name an agent to draft the board: this hub has no back office \
                                 of yours.",
                            )
                        })?,
                };
                if agent.kind != MemberKind::Agent {
                    return Err(WorkError::invalid(format!(
                        "agent must be an agent; {} is a person.",
                        agent.handle
                    )));
                }
                require_owner(caller, &agent)?;
                if made.digest != start.digest {
                    return Err(WorkError::conflict(
                        "The workstream has changed since its preview: preview it again, and \
                         confirm what will be sent.",
                    ));
                }
                if let Some(open) = load_drafts(c)?.into_iter().find(|d| {
                    d.workstream == workstream.id
                        && matches!(d.state, DraftState::Running | DraftState::Proposed)
                }) {
                    return Err(WorkError::conflict(format!(
                        "{} already has a board draft {}: {}",
                        workstream.name,
                        if open.state == DraftState::Running {
                            "running"
                        } else {
                            "waiting for review"
                        },
                        if open.state == DraftState::Running {
                            "wait for it, or end its session."
                        } else {
                            "review it first (accepting none is fine)."
                        }
                    )));
                }
                let machine = self.draft_machine(c)?;
                let persona = match &agent.persona {
                    Some(id) => query::persona(c, id)?,
                    None => None,
                };
                Ok((agent, machine, persona))
            })?;
            let dispatcher = self.dispatcher().ok_or_else(|| {
                WorkError::unavailable("This hub cannot start sessions: it has no runner link.")
            })?;
            let ready = catch_unwind(AssertUnwindSafe(|| dispatcher.can_start(&machine.id)))
                .unwrap_or_else(|_| Err(DispatchError::Failed("the runner link panicked".into())));
            if let Err(error) = ready {
                return Err(refused(&error));
            }
            let engine = start
                .engine
                .or_else(|| persona.as_ref().map(|p| p.engine))
                .unwrap_or(Engine::Claude);
            let id = DraftId::new();
            let now = self.now();
            let session_id = SessionId::new();
            // Its own private folder, never the workstream's: the summary is all it needs.
            let cwd = catch_unwind(AssertUnwindSafe(|| dispatcher.confined_folder(&session_id)))
                .ok()
                .flatten()
                .unwrap_or_default();
            let session = Session {
                id: session_id,
                engine,
                native_id: String::new(),
                machine: machine.id,
                cwd: cwd.clone(),
                branch: None,
                title: Some(format!("Drafting the board of {}", workstream.name)),
                agent: Some(agent.id),
                workstream: Some(workstream.id),
                task: None,
                link_basis: Some(LinkBasis::Manual),
                state: SessionState::Starting,
                status_line: None,
                started: now,
                last_activity: now,
                terminal: None,
                parent: None,
            };
            let request = SessionRequest {
                session: session.id,
                agent: agent.id,
                owner: agent.owner,
                machine: machine.id,
                cwd,
                branch: None,
                engine,
                persona: persona.as_ref().map(|p| p.id),
                // A persona's model is for its own CLI; another CLI chosen here takes its default.
                model: persona
                    .as_ref()
                    .filter(|p| p.engine == engine)
                    .and_then(|p| p.model.clone()),
                // Never the persona's mode: a draft runs confined, whatever its agent may do
                // elsewhere.
                permission_mode: PermissionMode::Default,
                name: format!("Draft board {}", workstream.name),
                brief: made
                    .prompt
                    .text
                    .replacen(PLACEHOLDER_ID, &id.to_string(), 1),
                confinement: Some(draft_confinement()),
            };
            let started = EventBody::BoardDraftStarted {
                draft: id,
                workstream: workstream.id,
                agent: agent.id,
                engine,
                session: session.id,
                prompt: pitcrew_office::prompts::DRAFT_BOARD.id(),
                cost: made.prompt.cost,
            };
            self.append(&[
                self.by(caller, EventBody::SessionDiscovered { session }),
                self.by(caller, started),
            ])?;
            (id, request, dispatcher)
        };

        let started = catch_unwind(AssertUnwindSafe(|| dispatcher.start_session(&request)))
            .unwrap_or_else(|_| Err(DispatchError::Failed("the runner link panicked".into())));
        if let Err(error) = started {
            tracing::warn!(%draft, session = %request.session, error = %error, "a board draft's session could not start");
            if let Err(e) = self.abandon_session(&request.session, &error.to_string()) {
                tracing::warn!(%draft, session = %request.session, error = %e, "cannot end a board draft's session that did not start");
            }
            return Err(refused(&error));
        }
        self.board_draft(&draft)
    }

    /// Where a draft runs: the hub's own machine, which must be live. Never the workstream's
    /// folder: a draft runs in a private folder of its own there, from the summary alone.
    fn draft_machine(&self, c: &Connection) -> Result<Machine> {
        let none = || {
            WorkError::unavailable(
                "No machine can run the draft: this hub has no machine of its own.",
            )
        };
        let id = self.hub_machine().ok_or_else(none)?;
        let machine = query::machine(c, &id)?.ok_or_else(none)?;
        if machine.liveness != Liveness::Live {
            let liveness = crate::codec::enum_text(&machine.liveness).unwrap_or_default();
            return Err(WorkError::unavailable(format!(
                "{} is {liveness}; its runner cannot be reached.",
                machine.name
            )));
        }
        Ok(machine)
    }

    /// Whether `session` is a board draft's (whatever the draft's state): its CLI is only ever
    /// given its own session token, never its agent's.
    ///
    /// # Errors
    ///
    /// Database errors.
    pub fn is_draft_session(&self, session: &SessionId) -> Result<bool> {
        Ok(self
            .read(load_drafts)?
            .iter()
            .any(|d| d.session == *session))
    }

    /// The sessions of the drafts still running: what the daemon ends when it starts
    /// ([`Self::end_running_drafts`]).
    ///
    /// # Errors
    ///
    /// Database errors.
    pub fn running_draft_sessions(&self) -> Result<Vec<SessionId>> {
        Ok(self
            .read(load_drafts)?
            .into_iter()
            .filter(|d| d.state == DraftState::Running)
            .map(|d| d.session)
            .collect())
    }

    /// Ends every draft still running, with its session: called when the daemon starts, since a
    /// draft's session token lived in the daemon's memory, so its CLI could never propose. Each
    /// session is ended in the log ([`Self::end_confined_session`]) and handed to the dispatcher
    /// to finish (its CLI, if it still runs, is ended). How many were.
    ///
    /// # Errors
    ///
    /// Database errors.
    pub fn end_running_drafts(&self, reason: &str) -> Result<usize> {
        let sessions = self.running_draft_sessions()?;
        for session in &sessions {
            self.end_confined_session(session, reason)?;
            self.finish_confined(session);
        }
        Ok(sessions.len())
    }

    /// Asks the dispatcher to finish a confined session: its token stops at once, and its CLI is
    /// ended. A failure is logged.
    pub(crate) fn finish_confined(&self, session: &SessionId) {
        let Some(dispatcher) = self.dispatcher() else {
            return;
        };
        let done = catch_unwind(AssertUnwindSafe(|| dispatcher.finish_session(session)))
            .unwrap_or_else(|_| Err(DispatchError::Failed("the runner link panicked".into())));
        if let Err(error) = done {
            tracing::warn!(%session, error = %error, "cannot finish a confined session");
        }
    }

    /// Every board draft, newest first; only `workstream`'s when given.
    ///
    /// # Errors
    ///
    /// Database errors.
    pub fn board_drafts(&self, workstream: Option<&WorkstreamId>) -> Result<Vec<BoardDraft>> {
        let mut drafts = self.read(load_drafts)?;
        drafts.retain(|d| workstream.is_none_or(|w| d.workstream == *w));
        drafts.reverse();
        Ok(drafts)
    }

    /// One board draft.
    ///
    /// # Errors
    ///
    /// `not_found` for an unknown draft; database errors.
    pub fn board_draft(&self, id: &DraftId) -> Result<BoardDraft> {
        self.read(|c| find(c, id))
    }

    /// Whether `caller` may propose for draft `id`: `not_found` for an unknown draft, `forbidden`
    /// for anyone but the session token made for its session (its agent's own token and a person
    /// included). The route asks before it reads the body.
    ///
    /// # Errors
    ///
    /// `not_found`, `forbidden`, or database errors.
    pub fn check_draft_proposer(&self, caller: &Caller, id: &DraftId) -> Result<()> {
        proposer(caller, &self.board_draft(id)?)
    }

    /// `POST /v1/board-drafts/{id}/proposal`: the drafting agent's proposal. See the
    /// [module docs](self).
    ///
    /// # Errors
    ///
    /// `not_found` for an unknown draft; `forbidden` for anyone but its agent; `invalid` for a
    /// proposal past its bounds, an empty title, a `canceled` status, or evidence that is not one
    /// of the workstream's sessions; `conflict` once the draft has a proposal, or has ended.
    pub fn propose_board(
        &self,
        caller: &Caller,
        id: &DraftId,
        proposal: BoardProposal,
    ) -> Result<BoardDraft> {
        self.check_draft_proposer(caller, id)?;
        let shape = checked_shape(proposal)?;
        let choice = self.import_choice();
        let _guard = self.lock();
        let (draft, proposal) = self.read(|c| {
            let draft = find(c, id)?;
            proposer(caller, &draft)?;
            let drafting: HashSet<SessionId> =
                load_drafts(c)?.into_iter().map(|d| d.session).collect();
            for (i, task) in shape.tasks.iter().enumerate() {
                for session in &task.evidence {
                    let known = query::session(c, session)?.filter(|s| {
                        s.workstream == Some(draft.workstream)
                            && s.parent.is_none()
                            && !drafting.contains(&s.id)
                            && choice.includes(s)
                    });
                    if known.is_none() {
                        return Err(WorkError::invalid(format!(
                            "tasks[{i}].evidence: {} is not one of the workstream's sessions.",
                            session.0
                        )));
                    }
                }
            }
            match draft.state {
                DraftState::Running => Ok((draft, shape)),
                DraftState::Proposed | DraftState::Reviewed => {
                    Err(WorkError::conflict(format!("{id} already has a proposal.")))
                }
                DraftState::Ended => Err(WorkError::conflict(format!(
                    "{id} has ended: its session ended without a proposal."
                ))),
            }
        })?;
        self.append(&[self.by(
            caller,
            EventBody::BoardProposed {
                draft: draft.id,
                workstream: draft.workstream,
                tasks: proposal.tasks,
                note: proposal.note,
            },
        )])?;
        // Its one thing is done: its token stops now, and its CLI is ended.
        self.finish_confined(&draft.session);
        self.board_draft(id)
    }

    /// `POST /v1/board-drafts/{id}/review`: a person accepts some of a proposal's tasks (any, all
    /// or none); see the [module docs](self).
    ///
    /// # Errors
    ///
    /// `forbidden` for an agent; `not_found` for an unknown draft; `invalid` for an index past the
    /// proposal's tasks, or one given twice; `conflict` when the draft has no proposal yet, was
    /// reviewed already, or another writer took a task key meanwhile.
    pub fn review_draft(
        &self,
        caller: &Caller,
        id: &DraftId,
        review: DraftReview,
    ) -> Result<DraftReviewed> {
        crate::commands::require_person(caller, "Reviewing a board draft")?;
        let _guard = self.lock();
        let ((draft, rejected), tasks, links) = self.read(|c| {
            let draft = find(c, id)?;
            let proposal = match (&draft.state, &draft.proposal) {
                (DraftState::Proposed, Some(proposal)) => proposal.clone(),
                (DraftState::Reviewed, _) => {
                    return Err(WorkError::conflict(format!("{id} was reviewed already.")));
                }
                _ => {
                    return Err(WorkError::conflict(format!(
                        "{id} has no proposal to review yet."
                    )));
                }
            };
            let mut accepted: Vec<(u32, usize)> = Vec::with_capacity(review.accept.len());
            let mut seen = HashSet::new();
            for &item in &review.accept {
                let Some(index) = usize::try_from(item)
                    .ok()
                    .filter(|i| *i < proposal.tasks.len())
                else {
                    return Err(WorkError::invalid(format!(
                        "accept: {item} is not one of the proposal's {} tasks.",
                        proposal.tasks.len()
                    )));
                };
                if !seen.insert(item) {
                    return Err(WorkError::invalid(format!(
                        "accept: {item} is given twice."
                    )));
                }
                accepted.push((item, index));
            }
            accepted.sort_unstable();
            let workstream = query::workstream(c, &draft.workstream)?
                .ok_or_else(|| WorkError::conflict("The draft's workstream is no longer known."))?;
            let mut taken: Option<u32> = None;
            let mut tasks = Vec::with_capacity(accepted.len());
            let mut links: Vec<(SessionId, TaskId)> = Vec::new();
            let mut linked: HashSet<SessionId> = HashSet::new();
            for (item, index) in &accepted {
                let proposed = &proposal.tasks[*index];
                // Made as `POST /v1/tasks` makes a task, numbered on from the one before.
                let task = crate::commands::plan_task(
                    c,
                    NewTask {
                        project: workstream.project,
                        workstream: Some(workstream.id),
                        title: proposed.title.clone(),
                        description: proposed.description.clone(),
                        status: Some(proposed.status),
                        priority: None,
                        assignee: None,
                        labels: Some(vec![DRAFTED_LABEL.to_owned()]),
                        due: None,
                    },
                    taken,
                )?;
                taken = Some(task.key.number);
                for session in &proposed.evidence {
                    let free = query::session(c, session)?
                        .is_some_and(|s| s.task.is_none() && s.workstream == Some(workstream.id));
                    if free && linked.insert(*session) {
                        links.push((*session, task.id));
                    }
                }
                tasks.push((*item, task));
            }
            let rejected: Vec<u32> = (0..proposal.tasks.len())
                .filter_map(|i| u32::try_from(i).ok())
                .filter(|i| !seen.contains(i))
                .collect();
            Ok(((draft, rejected), tasks, links))
        })?;
        let mut events: Vec<Event> = Vec::with_capacity(tasks.len() + links.len() + 1);
        for (_, task) in &tasks {
            events.push(self.by(caller, EventBody::TaskCreated { task: task.clone() }));
        }
        for (session, task) in &links {
            events.push(self.by(
                caller,
                EventBody::SessionLinked {
                    session: *session,
                    workstream: Some(draft.workstream),
                    task: Some(*task),
                    basis: LinkBasis::Manual,
                },
            ));
        }
        events.push(
            self.by(
                caller,
                EventBody::BoardDraftReviewed {
                    draft: draft.id,
                    workstream: draft.workstream,
                    accepted: tasks
                        .iter()
                        .map(|(item, task)| DraftedTask {
                            item: *item,
                            task: task.id,
                        })
                        .collect(),
                    rejected,
                },
            ),
        );
        self.append(&events)?;
        let created = self.read(|c| {
            let mut out = Vec::with_capacity(tasks.len());
            for (_, task) in &tasks {
                match query::task(c, &crate::query::TaskRef::Id(task.id))? {
                    Some(stored) => out.push(stored),
                    // Only a second writer can take a key between the check and the append.
                    None => {
                        return Err(WorkError::conflict(format!(
                            "{} was taken by another change at the same moment; the other \
                             accepted tasks were created.",
                            task.key
                        )));
                    }
                }
            }
            Ok(out)
        })?;
        Ok(DraftReviewed {
            draft: self.board_draft(id)?,
            tasks: created,
        })
    }
}

/// A proposal with every text trimmed, checked against its bound and redacted, and its evidence
/// without repeats; or why not (`invalid`).
fn checked_shape(proposal: BoardProposal) -> Result<BoardProposal> {
    if proposal.tasks.len() > MAX_PROPOSED_TASKS {
        return Err(WorkError::invalid(format!(
            "tasks: at most {MAX_PROPOSED_TASKS}, and this proposal has {}.",
            proposal.tasks.len()
        )));
    }
    let mut tasks = Vec::with_capacity(proposal.tasks.len());
    for (i, task) in proposal.tasks.into_iter().enumerate() {
        let title = task.title.trim();
        let chars = title.chars().count();
        if chars == 0 || chars > MAX_PROPOSED_TITLE {
            return Err(WorkError::invalid(format!(
                "tasks[{i}].title must be 1 to {MAX_PROPOSED_TITLE} characters."
            )));
        }
        if task.status == TaskStatus::Canceled {
            return Err(WorkError::invalid(format!(
                "tasks[{i}].status: a draft proposes work, never a canceled task."
            )));
        }
        let description = task
            .description
            .as_deref()
            .map(str::trim)
            .filter(|d| !d.is_empty());
        if description.is_some_and(|d| d.chars().count() > MAX_PROPOSED_DESCRIPTION) {
            return Err(WorkError::invalid(format!(
                "tasks[{i}].description must be at most {MAX_PROPOSED_DESCRIPTION} characters."
            )));
        }
        let mut evidence = Vec::new();
        for session in task.evidence {
            if !evidence.contains(&session) {
                evidence.push(session);
            }
        }
        if evidence.len() > MAX_EVIDENCE {
            return Err(WorkError::invalid(format!(
                "tasks[{i}].evidence: at most {MAX_EVIDENCE} sessions."
            )));
        }
        let title = redact::line(title, MAX_PROPOSED_TITLE).text;
        if title.is_empty() {
            return Err(WorkError::invalid(format!(
                "tasks[{i}].title must be 1 to {MAX_PROPOSED_TITLE} characters."
            )));
        }
        tasks.push(ProposedTask {
            title,
            status: task.status,
            description: description
                .map(|d| redact::line(d, MAX_PROPOSED_DESCRIPTION).text)
                .filter(|d| !d.is_empty()),
            evidence,
        });
    }
    let note = proposal
        .note
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty());
    if note.is_some_and(|n| n.chars().count() > MAX_NOTE) {
        return Err(WorkError::invalid(format!(
            "note must be at most {MAX_NOTE} characters."
        )));
    }
    Ok(BoardProposal {
        tasks,
        note: note
            .map(|n| redact::line(n, MAX_NOTE).text)
            .filter(|n| !n.is_empty()),
    })
}

/// Whether `caller` may propose for `draft`: only the session token made for its session, which
/// acts as its agent. Its agent's own token, another session's, and people are `forbidden`.
fn proposer(caller: &Caller, draft: &BoardDraft) -> Result<()> {
    if caller.scope == TokenScope::Session(draft.session) && caller.member == draft.agent {
        return Ok(());
    }
    Err(WorkError::forbidden(format!(
        "Only the session drafting {} may propose its board, with the token it was given.",
        draft.id
    )))
}

/// Lowercase hex.
fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(out, "{b:02x}");
    }
    out
}

/// Draft `id`, or `not_found`.
fn find(c: &Connection, id: &DraftId) -> Result<BoardDraft> {
    load_drafts(c)?
        .into_iter()
        .find(|d| d.id == *id)
        .ok_or_else(|| WorkError::not_found(format!("No board draft {id}.")))
}

/// Every draft, oldest first, folded from its events in log order. A proposal or review that
/// does not follow the draft's state (none the hub appends) changes nothing.
fn load_drafts(c: &Connection) -> Result<Vec<BoardDraft>> {
    let mut stmt = c.prepare_cached(
        "SELECT at, author, type, data FROM events WHERE type IN (?1, ?2, ?3) ORDER BY rev",
    )?;
    let rows = stmt.query_map(params![TYPES[0], TYPES[1], TYPES[2]], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
        ))
    })?;
    let mut drafts: Vec<BoardDraft> = Vec::new();
    for row in rows {
        let (at, author, kind, data) = row?;
        // One event that cannot be read (written by a newer hub, or damaged) must not hide every
        // draft: it is logged and skipped.
        let body = serde_json::from_str::<serde_json::Value>(&data).and_then(|data| {
            serde_json::from_value::<EventBody>(serde_json::json!({ "type": kind, "data": data }))
        });
        let body = match body {
            Ok(body) => body,
            Err(error) => {
                tracing::warn!(kind, at, error = %error, "skipped a board-draft event that cannot be read");
                continue;
            }
        };
        match body {
            EventBody::BoardDraftStarted {
                draft,
                workstream,
                agent,
                engine,
                session,
                prompt,
                cost,
            } => {
                let Ok(by) = author.parse::<MemberId>() else {
                    continue;
                };
                drafts.push(BoardDraft {
                    id: draft,
                    workstream,
                    agent,
                    engine,
                    session,
                    by,
                    prompt,
                    cost,
                    started: at,
                    state: DraftState::Running,
                    proposal: None,
                    proposed: None,
                    reviewed: None,
                    accepted: Vec::new(),
                    rejected: Vec::new(),
                });
            }
            EventBody::BoardProposed {
                draft, tasks, note, ..
            } => {
                if let Some(d) = drafts
                    .iter_mut()
                    .find(|d| d.id == draft && d.proposal.is_none())
                {
                    d.proposal = Some(BoardProposal { tasks, note });
                    d.proposed = Some(at);
                    d.state = DraftState::Proposed;
                }
            }
            EventBody::BoardDraftReviewed {
                draft,
                accepted,
                rejected,
                ..
            } => {
                if let Some(d) = drafts
                    .iter_mut()
                    .find(|d| d.id == draft && d.state == DraftState::Proposed)
                {
                    d.accepted = accepted;
                    d.rejected = rejected;
                    d.reviewed = Some(at);
                    d.state = DraftState::Reviewed;
                }
            }
            _ => {}
        }
    }
    for draft in &mut drafts {
        if draft.state == DraftState::Running
            && query::session(c, &draft.session)?.is_none_or(|s| s.state == SessionState::Ended)
        {
            draft.state = DraftState::Ended;
        }
    }
    Ok(drafts)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_proposal_is_trimmed_bounded_and_redacted() {
        let session: SessionId = "01JB000000000000000SES0001".parse().unwrap_or_default();
        let shape = checked_shape(BoardProposal {
            tasks: vec![ProposedTask {
                title: "  Rotate ghp_16C7e42F292c6912E7710c838347Ae178B4a  ".into(),
                status: TaskStatus::Todo,
                description: Some("   ".into()),
                evidence: vec![session, session],
            }],
            note: Some(" mail sam@example.com ".into()),
        })
        .unwrap_or_else(|e| panic!("{e}"));
        // Compared without printing: on a failure the title would hold the synthetic token.
        assert!(
            shape.tasks[0].title == "Rotate [redacted]",
            "the title is not trimmed and redacted"
        );
        assert_eq!(shape.tasks[0].description, None);
        assert_eq!(shape.tasks[0].evidence, vec![session]);
        assert_eq!(shape.note.as_deref(), Some("mail [email]"));
        let task = |title: &str, status| ProposedTask {
            title: title.into(),
            status,
            description: None,
            evidence: Vec::new(),
        };
        for bad in [
            vec![task(" ", TaskStatus::Todo)],
            vec![task(&"x".repeat(MAX_PROPOSED_TITLE + 1), TaskStatus::Todo)],
            vec![task("Gone", TaskStatus::Canceled)],
            vec![task("x", TaskStatus::Todo); MAX_PROPOSED_TASKS + 1],
        ] {
            let refused = checked_shape(BoardProposal {
                tasks: bad,
                note: None,
            });
            assert!(refused.is_err());
        }
        assert!(
            checked_shape(BoardProposal {
                tasks: Vec::new(),
                note: Some("n".repeat(MAX_NOTE + 1)),
            })
            .is_err()
        );
        assert_eq!(hex(&[0, 15, 255]), "000fff");
        assert_eq!(PLACEHOLDER_ID.len(), DraftId::new().to_string().len());
    }
}
