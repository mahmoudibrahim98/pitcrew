//! Board drafts: an agent drafts a workstream's board from its history, and a person reviews it
//! (api-v1.md, "Board drafts"). **Nothing is created until the person accepts it.**
//!
//! 1. **Preview** ([`WorkService::draft_preview`], person): the workstream's facts (its sessions,
//!    as its routes would list them, without sub-agents and drafting sessions; their recap lines
//!    and counts; its tasks) become the office's bounded, redacted summary inside the versioned
//!    prompt (`pitcrew_office::board`). Nothing is sent or stored. The answer holds the summary, its
//!    sizes, the estimate, and the prompt's digest.
//! 2. **Start** ([`WorkService::start_draft`], person): with the digest the person confirmed. The
//!    prompt is made again; if it differs (the workstream changed), `409`. Then, under the command
//!    lock: the agent is one of the caller's own (`403` otherwise; the back office, `@office`,
//!    when none is named), no other draft of the workstream is running or waiting for review
//!    (`409`), and a session can start where the workstream's folder is (its first location,
//!    else its project's root, else the hub's own machine in `~`; `503` when none can). It
//!    appends `session_discovered` (state `starting`, the agent, linked to the workstream by hand)
//!    and `board_draft_started`, then asks the [`Dispatcher`](crate::Dispatcher) to start the
//!    agent's CLI with the prompt ([`SessionRequest`]). A start that fails ends the session, and
//!    the draft with it.
//! 3. **Propose** ([`WorkService::propose_board`], the draft's agent only): a [`BoardProposal`] of
//!    at most `MAX_PROPOSAL_BYTES` (the route's bound), every text checked against its bound and
//!    redacted, every evidence session one of the workstream's. Once per draft: `board_proposed`.
//! 4. **Review** ([`WorkService::review_draft`], person): the accepted items become tasks in one
//!    append with the review: `task_created` for each (in the workstream, at its proposed status,
//!    labelled [`DRAFTED_LABEL`]), `session_linked` (by hand) for each evidence session not yet
//!    linked to a task, and `board_draft_reviewed`. Rejected items create nothing. Once per
//!    draft.
//!
//! Drafts are read from the log itself (their three event types, by the store's type index):
//! there are few, and they need no table of their own. A running draft whose session has ended is
//! [`DraftState::Ended`].

use crate::dispatch::{DispatchError, SessionRequest, refused, require_owner};
use crate::error::{Result, WorkError};
use crate::office::OFFICE_HANDLE;
use crate::query::{self, SessionFilter, TaskFilter};
use crate::recap::{BlockFilter, RecapIndex};
use crate::service::WorkService;
use pitcrew_office::board::{DraftFacts, SessionFacts, TaskFacts, draft_prompt};
use pitcrew_office::redact;
use pitcrew_protocol::api::Caller;
use pitcrew_protocol::board::{
    BoardDraft, BoardProposal, DRAFTED_LABEL, DraftPreview, DraftReview, DraftReviewed, DraftState,
    DraftedTask, MAX_EVIDENCE, MAX_NOTE, MAX_PROPOSED_DESCRIPTION, MAX_PROPOSED_TASKS,
    MAX_PROPOSED_TITLE, ProposedTask, StartDraft,
};
use pitcrew_protocol::events::{Event, EventBody};
use pitcrew_protocol::ids::{DraftId, MemberId, SessionId, TaskId, TaskKey, WorkstreamId};
use pitcrew_protocol::model::{
    Engine, LinkBasis, Liveness, Machine, MemberKind, Session, SessionState, Task, TaskStatus,
    Workstream,
};
use pitcrew_store::sql::{Connection, params};
use sha2::{Digest as _, Sha256};
use std::collections::{HashMap, HashSet};
use std::panic::{AssertUnwindSafe, catch_unwind};

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

/// The prompt for a workstream, as made now.
struct Made {
    prompt: pitcrew_office::board::DraftPrompt,
    digest: String,
}

/// Where a draft's agent runs.
struct Place {
    machine: Machine,
    cwd: String,
    branch: Option<String>,
}

impl WorkService {
    /// `GET /v1/workstreams/{id}/board-draft`: what a draft of `workstream` would send its agent,
    /// and its estimate. Nothing is sent or stored. People only.
    ///
    /// # Errors
    ///
    /// `forbidden` for an agent; `not_found` for an unknown workstream; database errors.
    pub fn draft_preview(&self, caller: &Caller, workstream: &WorkstreamId) -> Result<DraftPreview> {
        crate::commands::require_person(caller, "Previewing a board draft")?;
        let workstream = self.workstream(workstream)?;
        let made = self.make_prompt(&workstream)?;
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
        let made = self.make_prompt(&workstream)?;
        let (draft, request, dispatcher) = {
            let _guard = self.lock();
            let (agent, place, persona) = self.read(|c| {
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
                let place = self.draft_place(c, &workstream)?;
                let persona = match &agent.persona {
                    Some(id) => query::persona(c, id)?,
                    None => None,
                };
                Ok((agent, place, persona))
            })?;
            let dispatcher = self.dispatcher().ok_or_else(|| {
                WorkError::unavailable("This hub cannot start sessions: it has no runner link.")
            })?;
            let ready = catch_unwind(AssertUnwindSafe(|| dispatcher.can_start(&place.machine.id)))
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
            let session = Session {
                id: SessionId::new(),
                engine,
                native_id: String::new(),
                machine: place.machine.id,
                cwd: place.cwd.clone(),
                branch: place.branch.clone(),
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
                machine: place.machine.id,
                cwd: place.cwd,
                branch: place.branch,
                engine,
                persona: persona.as_ref().map(|p| p.id),
                // A persona's model is for its own CLI; another CLI chosen here takes its default.
                model: persona
                    .as_ref()
                    .filter(|p| p.engine == engine)
                    .and_then(|p| p.model.clone()),
                permission_mode: persona
                    .as_ref()
                    .map(|p| p.permission_mode)
                    .unwrap_or_default(),
                name: format!("Draft board {}", workstream.name),
                brief: made
                    .prompt
                    .text
                    .replacen(PLACEHOLDER_ID, &id.to_string(), 1),
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

    /// Where a draft of `workstream` runs: its first location, else its project's root, else the
    /// hub's own machine in `~`. The machine must be live.
    fn draft_place(&self, c: &Connection, workstream: &Workstream) -> Result<Place> {
        let mut locations = workstream.locations.clone();
        if let Some(root) = query::project(c, &workstream.project)?.and_then(|p| p.root) {
            locations.push(root);
        }
        let place = match locations.first() {
            Some(location) => {
                let machine = query::machine(c, &location.machine)?.ok_or_else(|| {
                    WorkError::unavailable(format!(
                        "The workstream's folder is on machine {}, which this hub does not know.",
                        location.machine
                    ))
                })?;
                Place {
                    machine,
                    cwd: location.path.clone(),
                    branch: location.branch.clone(),
                }
            }
            None => {
                let none = || {
                    WorkError::unavailable(
                        "No machine can run the draft: the workstream has no folder and this hub \
                         has no machine of its own.",
                    )
                };
                let id = self.hub_machine().ok_or_else(none)?;
                Place {
                    machine: query::machine(c, &id)?.ok_or_else(none)?,
                    cwd: "~".to_owned(),
                    branch: None,
                }
            }
        };
        if place.machine.liveness != Liveness::Live {
            let liveness = crate::codec::enum_text(&place.machine.liveness).unwrap_or_default();
            return Err(WorkError::unavailable(format!(
                "{} is {liveness}; its runner cannot be reached.",
                place.machine.name
            )));
        }
        Ok(place)
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
    /// for anyone but its agent. The route asks before it reads the body.
    ///
    /// # Errors
    ///
    /// `not_found`, `forbidden`, or database errors.
    pub fn check_draft_proposer(&self, caller: &Caller, id: &DraftId) -> Result<()> {
        let draft = self.board_draft(id)?;
        if caller.is_person() || caller.member != draft.agent {
            return Err(WorkError::forbidden(format!(
                "Only the agent drafting {id} may propose its board."
            )));
        }
        Ok(())
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
            if caller.is_person() || caller.member != draft.agent {
                return Err(WorkError::forbidden(format!(
                    "Only the agent drafting {id} may propose its board."
                )));
            }
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
                DraftState::Proposed | DraftState::Reviewed => Err(WorkError::conflict(format!(
                    "{id} already has a proposal."
                ))),
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
                    return Err(WorkError::invalid(format!("accept: {item} is given twice.")));
                }
                accepted.push((item, index));
            }
            accepted.sort_unstable();
            let workstream = query::workstream(c, &draft.workstream)?.ok_or_else(|| {
                WorkError::conflict("The draft's workstream is no longer known.")
            })?;
            let project = query::project(c, &workstream.project)?.ok_or_else(|| {
                WorkError::conflict("The draft's project is no longer known.")
            })?;
            let mut number = query::highest_task_number(c, &project.key)?;
            let mut tasks = Vec::with_capacity(accepted.len());
            let mut links: Vec<(SessionId, TaskId)> = Vec::new();
            let mut linked: HashSet<SessionId> = HashSet::new();
            for (item, index) in &accepted {
                let proposed = &proposal.tasks[*index];
                number = number.checked_add(1).ok_or_else(|| {
                    WorkError::conflict(format!("{} has no task numbers left.", project.key))
                })?;
                let key = TaskKey::new(project.key.clone(), number)
                    .map_err(|e| WorkError::internal(e.to_string()))?;
                let task = Task {
                    id: TaskId::new(),
                    key,
                    project: project.id,
                    workstream: Some(workstream.id),
                    title: proposed.title.clone(),
                    description: proposed.description.clone().unwrap_or_default(),
                    status: proposed.status,
                    priority: pitcrew_protocol::model::Priority::default(),
                    assignee: None,
                    labels: vec![DRAFTED_LABEL.to_owned()],
                    start: None,
                    due: None,
                    blocked_by: Vec::new(),
                    source: None,
                    accept_auto: false,
                    subtasks: Vec::new(),
                };
                for session in &proposed.evidence {
                    let free = query::session(c, session)?.is_some_and(|s| {
                        s.task.is_none() && s.workstream == Some(workstream.id)
                    });
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
        events.push(self.by(
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
        ));
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
        let data: serde_json::Value = serde_json::from_str(&data)?;
        let body: EventBody =
            serde_json::from_value(serde_json::json!({ "type": kind, "data": data }))?;
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
        assert_eq!(shape.tasks[0].title, "Rotate [redacted]");
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
