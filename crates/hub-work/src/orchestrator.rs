//! The Orchestrator panel's conversations (api-v1.md, "Orchestrator").
//!
//! A person asks about their work; an agent CLI they already use answers, in a session the hub
//! starts for them, with a token that can only read. Its answer is read back from the session's
//! transcript. The hub keeps each person's conversations here, outside the event log, in
//! `orchestrator.json` ([`WorkService::with_orchestrator_file`]), so a person can clear them.
//!
//! 1. **Asking** ([`WorkService::ask_orchestrator`], people only), under the command lock, then
//!    the conversations' own lock:
//!    - one answer at a time per person (`409` while one is under way), at most
//!      [`MAX_TURNS`] questions per conversation (`409`);
//!    - a new conversation runs as the caller's own agent ([`own_agent`]: the back office by
//!      default), in the engine asked for, else the remembered one, else Claude Code; its CLI must
//!      be installed on the hub's own machine (`409` when the runner link says it is not);
//!    - a follow-up whose session still lives is typed into it (`SendText`, after
//!      [`clean_question`] made it one line and [`typed`] made it never a CLI command); one whose
//!      session ended starts a new session, whose prompt carries the conversation so far;
//!    - a new session is stored first (`session_discovered`: titled [`SESSION_TITLE`], the agent
//!      named, linked to nothing), in the caller's scratch folder on the hub's machine
//!      ([`Dispatcher::scratch`]), with the CLI's own default permission mode, and the prompt
//!      (`pitcrew_office::orchestrator::prompt`); then the [`Dispatcher`] starts it. Its CLI gets
//!      a reader token: the daemon asks [`WorkService::reads_only`] which sessions are these.
//!    - starting a new conversation ends the caller's other Orchestrator session (one at a time).
//!
//!    A start or a typing that fails marks the turn `failed` and answers why (`409`, `503` or
//!    `500`, as a dispatch does).
//! 2. **Following** ([`WorkService::follow_orchestrator`], about once a second, by the daemon):
//!    each answering turn's transcript is read back to its prompt ([`progress`]): the first user
//!    prompt after the turn's start (for a follow-up, the one with its text); its answer is the
//!    assistant's text after that, until the transcript's turn end (`answered`). Past
//!    [`MAX_ANSWER_BYTES`] it is cut (`too_long`), past [`MAX_ANSWER_SECONDS`] it is `timed_out`
//!    (both send the CLI Esc), and a session that ends first leaves it `failed`. Its references and
//!    suggestions are found in the text (`pitcrew_office::orchestrator::scan`) and kept only when
//!    the hub knows what they name.
//! 3. **Cancel** ([`WorkService::cancel_answer`]): Esc, then `canceled`. **Clear**
//!    ([`WorkService::clear_conversations`]): the caller's conversations are forgotten and their
//!    session is ended.
//!
//! Nothing here acts on the work: a suggestion is data the panel shows, and only a person's click
//! makes a request, as that person.

use crate::commands::require_person;
use crate::dispatch::{DispatchError, SessionRequest, own_agent, refused};
use crate::error::{Result, WorkError};
use crate::query::{self, TaskRef};
use crate::service::WorkService;
use pitcrew_office::orchestrator::{
    Cited, Earlier, PromptFacts, RecapOf, Scan, Suggested, clean_question, prompt, scan, typed,
};
use pitcrew_protocol::api::Caller;
use pitcrew_protocol::events::EventBody;
use pitcrew_protocol::ids::{ConversationId, MachineId, MemberId, SessionId};
use pitcrew_protocol::model::{
    Engine, Liveness, PermissionMode, Session, SessionState, TimestampMs,
};
use pitcrew_protocol::orchestrator::{
    AnswerReference, AnswerSuggestion, AnswerUsage, Conversation, EngineStatus, MAX_ANSWER_BYTES,
    MAX_ANSWER_SECONDS, MAX_CONVERSATIONS, MAX_QUESTION_CHARS, MAX_REFERENCES, MAX_TURNS,
    Orchestrator, OrchestratorLimits, OrchestratorTurn, Question, ReferenceTarget, TurnState,
};
use pitcrew_protocol::runner::{EndMode, RunnerCommand};
use pitcrew_protocol::transcript::TranscriptItem;
use pitcrew_store::sql::Connection;
use serde::{Deserialize, Serialize};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::{MutexGuard, PoisonError};

/// The title, and the terminal's name, of every Orchestrator session.
pub const SESSION_TITLE: &str = "Orchestrator";
/// The engines the panel offers, in its order.
pub const ENGINES: [Engine; 3] = [Engine::Claude, Engine::Codex, Engine::OpenCode];
/// The file's format.
const FILE_VERSION: u32 = 1;
/// Items read per transcript page.
const PAGE_LIMIT: usize = 500;
/// The most pages read back for one turn in one look.
const MAX_PAGES: usize = 20;

/// The conversations, as the hub keeps them.
#[derive(Debug, Default)]
pub(crate) struct OrchestratorState {
    file: Option<PathBuf>,
    people: Vec<Person>,
}

/// `orchestrator.json`.
#[derive(Debug, Serialize, Deserialize)]
struct Saved {
    version: u32,
    people: Vec<Person>,
}

/// One person's.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Person {
    member: MemberId,
    /// The engine they chose last for a new conversation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    engine: Option<Engine>,
    /// Oldest first.
    #[serde(default)]
    conversations: Vec<Stored>,
}

/// A conversation, with what following its turns needs.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Stored {
    id: ConversationId,
    engine: Engine,
    agent: MemberId,
    started: TimestampMs,
    /// Its newest session, live or not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    session: Option<SessionId>,
    turns: Vec<StoredTurn>,
}

/// A turn, and where it is in its session's transcript.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct StoredTurn {
    turn: OrchestratorTurn,
    /// What was typed for it, for a follow-up in a live session: its prompt has this text. `None`
    /// for a session's first turn, whose prompt is the session's first.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    typed: Option<String>,
    /// Its prompt is at or after this offset.
    #[serde(default)]
    after: u64,
    /// Its prompt's offset, once found.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    prompt_at: Option<u64>,
}

impl StoredTurn {
    /// Where the next turn in the same session starts.
    fn next_after(&self) -> u64 {
        self.prompt_at.map_or(self.after, |at| at.saturating_add(1))
    }
}

/// What a turn's transcript says so far: see [`progress`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Progress {
    /// The offset of its prompt, once found.
    pub prompt_at: Option<u64>,
    /// The assistant's text after it, so far.
    pub answer: String,
    /// The tools run after it.
    pub tool_runs: u32,
    /// When its turn ended, by the transcript.
    pub ended: Option<TimestampMs>,
}

/// What `items` (a session's transcript from `after` on, oldest first) say of the turn whose
/// prompt is the first user prompt at or after `after` (with `typed`'s text, when given): the
/// assistant's text after it, the tools run, and whether its turn ended, up to the next user
/// prompt.
pub(crate) fn progress(items: &[TranscriptItem], after: u64, typed: Option<&str>) -> Progress {
    let is_prompt = |text: &str| typed.is_none_or(|typed| text.trim() == typed.trim());
    let Some(start) = items.iter().position(|item| {
        matches!(item, TranscriptItem::UserPrompt { offset, text, .. }
            if *offset >= after && is_prompt(text))
    }) else {
        return Progress::default();
    };
    let mut out = Progress {
        prompt_at: Some(items[start].offset()),
        ..Progress::default()
    };
    for item in &items[start + 1..] {
        match item {
            TranscriptItem::UserPrompt { .. } => break,
            TranscriptItem::AssistantText { text, .. } => {
                let text = text.trim();
                if !text.is_empty() {
                    if !out.answer.is_empty() {
                        out.answer.push_str("\n\n");
                    }
                    out.answer.push_str(text);
                }
            }
            TranscriptItem::ToolUse { .. } => out.tool_runs = out.tool_runs.saturating_add(1),
            TranscriptItem::TurnEnded { at, .. } => {
                out.ended = Some(*at);
                break;
            }
            _ => {}
        }
    }
    out
}

/// `text` cut to at most `max` bytes, at a character's end.
fn cut(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

/// One answering turn, as a look at it needs it.
struct Look {
    member: MemberId,
    conversation: ConversationId,
    index: usize,
    session: SessionId,
    asked: TimestampMs,
    after: u64,
    typed: Option<String>,
}

/// What a look decided.
struct Update {
    state: TurnState,
    answer: String,
    references: Vec<AnswerReference>,
    suggestions: Vec<AnswerSuggestion>,
    tool_runs: u32,
    prompt_at: Option<u64>,
    ended: Option<TimestampMs>,
    note: Option<String>,
}

/// What asking decided, under the locks.
enum Step {
    /// Type the question into the conversation's live session.
    Type {
        machine: MachineId,
        session: SessionId,
        text: String,
    },
    /// Start a session with the prompt.
    Start(Box<SessionRequest>),
}

impl OrchestratorState {
    fn person(&self, member: MemberId) -> Option<&Person> {
        self.people.iter().find(|p| p.member == member)
    }

    fn person_mut(&mut self, member: MemberId) -> &mut Person {
        if let Some(i) = self.people.iter().position(|p| p.member == member) {
            return &mut self.people[i];
        }
        self.people.push(Person {
            member,
            engine: None,
            conversations: Vec::new(),
        });
        let last = self.people.len() - 1;
        &mut self.people[last]
    }

    fn turn_mut(
        &mut self,
        member: MemberId,
        conversation: ConversationId,
        index: usize,
    ) -> Option<&mut StoredTurn> {
        self.people
            .iter_mut()
            .find(|p| p.member == member)?
            .conversations
            .iter_mut()
            .find(|c| c.id == conversation)?
            .turns
            .get_mut(index)
    }

    /// Writes the file, when there is one: atomically, private to this user.
    fn save(&self) -> Result<()> {
        let Some(file) = &self.file else {
            return Ok(());
        };
        let parent = file
            .parent()
            .ok_or_else(|| WorkError::internal("the Orchestrator's file has no folder"))?;
        let saved = Saved {
            version: FILE_VERSION,
            people: self.people.clone(),
        };
        let mut pending = tempfile::NamedTempFile::new_in(parent)
            .map_err(|e| WorkError::internal(format!("creating the Orchestrator's file: {e}")))?;
        use std::io::Write as _;
        pending
            .write_all(&serde_json::to_vec(&saved)?)
            .and_then(|()| pending.as_file().sync_all())
            .map_err(|e| WorkError::internal(format!("writing the Orchestrator's file: {e}")))?;
        pending
            .persist(file)
            .map_err(|e| WorkError::internal(format!("saving the Orchestrator's file: {e}")))?;
        Ok(())
    }
}

impl WorkService {
    /// Keeps the Orchestrator's conversations in `file` (`orchestrator.json` in the daemon's state
    /// directory), loading what it holds. Without it they live only as long as the service.
    ///
    /// # Errors
    ///
    /// An unreadable file, or one that does not parse.
    pub fn with_orchestrator_file(self, file: PathBuf) -> Result<Self> {
        let people = match std::fs::read(&file) {
            Ok(bytes) => {
                let saved: Saved = serde_json::from_slice(&bytes)?;
                if saved.version != FILE_VERSION {
                    return Err(WorkError::internal(format!(
                        "{} is version {}; this hub reads version {FILE_VERSION}",
                        file.display(),
                        saved.version
                    )));
                }
                saved.people
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => {
                return Err(WorkError::internal(format!(
                    "reading {}: {e}",
                    file.display()
                )));
            }
        };
        *self.conversations() = OrchestratorState {
            file: Some(file),
            people,
        };
        Ok(self)
    }

    fn conversations(&self) -> MutexGuard<'_, OrchestratorState> {
        self.orchestrator
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Whether `session` is an Orchestrator session: its CLI gets a token that may only read.
    #[must_use]
    pub fn reads_only(&self, session: &SessionId) -> bool {
        self.conversations().people.iter().any(|p| {
            p.conversations.iter().any(|c| {
                c.session == Some(*session) || c.turns.iter().any(|t| t.turn.session == *session)
            })
        })
    }

    /// `GET /v1/orchestrator`: the caller's own. People only.
    ///
    /// # Errors
    ///
    /// `forbidden` for an agent or a reader; database errors.
    pub fn orchestrator(&self, caller: &Caller) -> Result<Orchestrator> {
        require_person(caller, "The Orchestrator")?;
        let person = self.conversations().person(caller.member).cloned();
        let (engine, stored) = person.map_or((None, Vec::new()), |p| (p.engine, p.conversations));
        let conversations = self.read(|c| {
            stored
                .iter()
                .rev()
                .map(|s| view(c, s))
                .collect::<Result<Vec<_>>>()
        })?;
        Ok(Orchestrator {
            engines: self.engines(),
            engine,
            limits: OrchestratorLimits::CURRENT,
            conversations,
        })
    }

    /// Each engine, and whether its CLI is installed on the hub's own machine.
    fn engines(&self) -> Vec<EngineStatus> {
        let dispatcher = self.dispatcher();
        let machine = self.hub_machine();
        ENGINES
            .iter()
            .map(|&engine| EngineStatus {
                engine,
                installed: match (&dispatcher, &machine) {
                    (Some(d), Some(m)) => d.installed(m, engine).unwrap_or(false),
                    _ => false,
                },
            })
            .collect()
    }

    /// One of the caller's conversations, as the routes answer it.
    ///
    /// # Errors
    ///
    /// `not_found` for an unknown one (or another person's); database errors.
    pub fn conversation(&self, caller: &Caller, id: &ConversationId) -> Result<Conversation> {
        let stored = self
            .conversations()
            .person(caller.member)
            .and_then(|p| p.conversations.iter().find(|c| c.id == *id).cloned())
            .ok_or_else(|| no_conversation(id))?;
        self.read(|c| view(c, &stored))
    }

    /// `POST /v1/orchestrator/questions`: see the [module docs](self).
    ///
    /// # Errors
    ///
    /// `forbidden` for an agent or a reader, or an agent the caller does not own; `not_found` for
    /// an unknown conversation; `invalid` for an empty or overlong question, an unknown agent, a
    /// person as the agent, or no agent named without a back office; `conflict` while an answer is
    /// under way, at [`MAX_TURNS`], for an engine not installed, or when the runner refused;
    /// `unavailable` when no session can start; `internal` when the start failed.
    pub fn ask_orchestrator(&self, caller: &Caller, question: Question) -> Result<Conversation> {
        require_person(caller, "Asking the Orchestrator")?;
        let follow_up = question.conversation;
        let text = clean_question(&question.text, follow_up.is_some());
        let chars = text.chars().count();
        if chars == 0 || chars > MAX_QUESTION_CHARS {
            return Err(WorkError::invalid(format!(
                "text must be 1 to {MAX_QUESTION_CHARS} characters, once control and hidden \
                 characters are dropped."
            )));
        }
        let dispatcher = self.dispatcher();
        let (conversation, index, step, ending) = {
            let _guard = self.lock();
            let mut state = self.conversations();
            let person = state.person(caller.member).cloned();
            let existing = match follow_up {
                Some(id) => Some(
                    person
                        .as_ref()
                        .and_then(|p| p.conversations.iter().find(|c| c.id == id).cloned())
                        .ok_or_else(|| no_conversation(&id))?,
                ),
                None => None,
            };
            let engine = existing.as_ref().map_or_else(
                || {
                    question
                        .engine
                        .or_else(|| person.as_ref().and_then(|p| p.engine))
                        .unwrap_or(Engine::Claude)
                },
                |c| c.engine,
            );
            let now = self.now();
            // The agent; the live session a follow-up types into; the other live sessions, which
            // end when a new session starts (one at a time per person).
            let (live, ending, agent, persona) = self.read(|c| {
                let agent = match &existing {
                    Some(conv) => own_agent(c, caller, Some(&conv.agent), "ask the Orchestrator")?,
                    None => own_agent(c, caller, question.agent.as_ref(), "ask the Orchestrator")?,
                };
                let persona = match &agent.persona {
                    Some(id) => query::persona(c, id)?,
                    None => None,
                };
                let live = match existing.as_ref().and_then(|conv| conv.session) {
                    Some(id) => query::session(c, &id)?.filter(|s| s.state != SessionState::Ended),
                    None => None,
                };
                let mut ending = Vec::new();
                for conv in person.iter().flat_map(|p| &p.conversations) {
                    if let Some(id) = conv.session
                        && let Some(s) = query::session(c, &id)?
                        && s.state != SessionState::Ended
                        && live.as_ref().is_none_or(|l| l.id != s.id)
                    {
                        ending.push((s.machine, s.id));
                    }
                }
                Ok((live, ending, agent, persona))
            })?;
            if person.as_ref().is_some_and(|p| {
                p.conversations
                    .iter()
                    .any(|c| c.turns.iter().any(|t| t.turn.state == TurnState::Answering))
            }) {
                return Err(WorkError::conflict(
                    "An answer is under way: wait for it, or stop it, before asking again.",
                ));
            }
            if existing
                .as_ref()
                .is_some_and(|c| c.turns.len() >= MAX_TURNS)
            {
                return Err(WorkError::conflict(format!(
                    "A conversation holds at most {MAX_TURNS} questions: start a new one."
                )));
            }
            // Only a new session ends the others: a follow-up typed into its live session does not.
            let ending = if live.is_some() { Vec::new() } else { ending };

            let mut stored = existing.unwrap_or_else(|| Stored {
                id: ConversationId::new(),
                engine,
                agent: agent.id,
                started: now,
                session: None,
                turns: Vec::new(),
            });
            let (step, turn) = match live {
                Some(session) => {
                    let after = stored.turns.last().map_or(0, StoredTurn::next_after);
                    let typed = typed(&text);
                    (
                        Step::Type {
                            machine: session.machine,
                            session: session.id,
                            text: typed.clone(),
                        },
                        // Its prompt is the one with the text typed.
                        StoredTurn {
                            turn: new_turn(text, now, session.id),
                            typed: Some(typed),
                            after,
                            prompt_at: None,
                        },
                    )
                }
                None => {
                    let machine = self.orchestrator_machine(dispatcher.as_deref(), engine)?;
                    let d = dispatcher.as_ref().ok_or_else(no_runner)?;
                    let cwd = d
                        .scratch(&machine, &format!("orchestrator-{}", caller.member.0))
                        .map_err(|e| refused(&e))?;
                    let earlier: Vec<Earlier> = stored
                        .turns
                        .iter()
                        .map(|t| Earlier {
                            question: t.turn.question.clone(),
                            answer: t.turn.answer.clone(),
                        })
                        .collect();
                    let me = self.read(|c| query::member(c, &caller.member))?;
                    let person_name = me.map_or_else(
                        || caller.member.to_string(),
                        |m| format!("{} ({})", m.name, m.handle),
                    );
                    let workspace = self.workspace_at()?.workspace.name;
                    let today = pitcrew_recap::date_of(now, 0).0;
                    let brief = prompt(&PromptFacts {
                        person: &person_name,
                        workspace: &workspace,
                        today: &today,
                        question: &text,
                        earlier: &earlier,
                    });
                    let session = Session {
                        id: SessionId::new(),
                        engine,
                        native_id: String::new(),
                        machine,
                        cwd: cwd.clone(),
                        branch: None,
                        title: Some(SESSION_TITLE.to_owned()),
                        agent: Some(agent.id),
                        workstream: None,
                        task: None,
                        link_basis: None,
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
                        machine,
                        cwd,
                        branch: None,
                        engine,
                        persona: persona.as_ref().map(|p| p.id),
                        // A persona's model is for its own CLI; another CLI takes its default.
                        model: persona
                            .as_ref()
                            .filter(|p| p.engine == engine)
                            .and_then(|p| p.model.clone()),
                        // The CLI's own default, never a persona's: it only reads.
                        permission_mode: PermissionMode::Default,
                        name: SESSION_TITLE.to_owned(),
                        brief,
                    };
                    let turn = StoredTurn {
                        turn: new_turn(text, now, session.id),
                        typed: None,
                        after: 0,
                        prompt_at: None,
                    };
                    stored.session = Some(session.id);
                    // Stored before its CLI starts, so its CLI's token is a reader's.
                    self.append(&[self.by(caller, EventBody::SessionDiscovered { session })])?;
                    (Step::Start(Box::new(request)), turn)
                }
            };
            stored.turns.push(turn);
            let index = stored.turns.len() - 1;
            let id = stored.id;
            let person = state.person_mut(caller.member);
            if follow_up.is_none() {
                person.engine = Some(engine);
            }
            match person.conversations.iter_mut().find(|c| c.id == id) {
                Some(slot) => *slot = stored,
                None => person.conversations.push(stored),
            }
            let over = person.conversations.len().saturating_sub(MAX_CONVERSATIONS);
            person.conversations.drain(..over);
            state.save()?;
            (id, index, step, ending)
        };

        let dispatcher = dispatcher.ok_or_else(no_runner)?;
        for (machine, session) in ending {
            let end = RunnerCommand::EndSession {
                session,
                mode: EndMode::Kill,
            };
            if let Err(e) = guarded(|| dispatcher.command(&machine, &end)) {
                tracing::warn!(%session, error = %e, "cannot end an earlier Orchestrator session");
            }
        }
        let outcome = match &step {
            Step::Type {
                machine,
                session,
                text,
            } => guarded(|| {
                dispatcher.command(
                    machine,
                    &RunnerCommand::SendText {
                        session: *session,
                        text: text.clone(),
                    },
                )
            }),
            Step::Start(request) => guarded(|| dispatcher.start_session(request)),
        };
        if let Err(error) = outcome {
            tracing::warn!(conversation = %conversation, error = %error, "an Orchestrator question could not reach its CLI");
            if let Step::Start(request) = &step
                && let Err(e) = self.abandon_session(&request.session, &error.to_string())
            {
                tracing::warn!(session = %request.session, error = %e, "cannot end an Orchestrator session that did not start");
            }
            self.finish_turn(
                caller.member,
                conversation,
                index,
                TurnState::Failed,
                |turn| {
                    turn.note = Some(format!("Its CLI could not be reached: {error}."));
                },
            )?;
            return Err(refused(&error));
        }
        self.conversation(caller, &conversation)
    }

    /// The hub's own machine, where questions run, if a session of `engine` can start there.
    fn orchestrator_machine(
        &self,
        dispatcher: Option<&dyn crate::Dispatcher>,
        engine: Engine,
    ) -> Result<MachineId> {
        let dispatcher = dispatcher.ok_or_else(no_runner)?;
        let machine = self.hub_machine().ok_or_else(|| {
            WorkError::unavailable(
                "This hub has no machine of its own yet, so the Orchestrator cannot start.",
            )
        })?;
        let found = self
            .read(|c| query::machine(c, &machine))?
            .ok_or_else(|| WorkError::unavailable("This hub's own machine is not known."))?;
        if found.liveness != Liveness::Live {
            let liveness = crate::codec::enum_text(&found.liveness).unwrap_or_default();
            return Err(WorkError::unavailable(format!(
                "{} is {liveness}; its runner cannot be reached.",
                found.name
            )));
        }
        let ready = guarded(|| dispatcher.can_start(&machine));
        if let Err(error) = ready {
            return Err(refused(&error));
        }
        if dispatcher.installed(&machine, engine) == Some(false) {
            return Err(WorkError::conflict(format!(
                "{} is not installed on {}: install it, or choose another agent CLI.",
                engine_name(engine),
                found.name
            )));
        }
        Ok(machine)
    }

    /// Ends a turn that is still answering as `state`, with `edit` applied; saves.
    fn finish_turn(
        &self,
        member: MemberId,
        conversation: ConversationId,
        index: usize,
        state: TurnState,
        edit: impl FnOnce(&mut OrchestratorTurn),
    ) -> Result<()> {
        let now = self.now();
        let mut conversations = self.conversations();
        let Some(stored) = conversations.turn_mut(member, conversation, index) else {
            return Ok(());
        };
        if stored.turn.state != TurnState::Answering {
            return Ok(());
        }
        let turn = &mut stored.turn;
        turn.state = state;
        turn.ended = Some(now);
        turn.usage = Some(AnswerUsage {
            duration_ms: u64::try_from(now.saturating_sub(turn.asked)).unwrap_or(0),
            tool_runs: turn.usage.map_or(0, |u| u.tool_runs),
            answer_bytes: u32::try_from(turn.answer.len()).unwrap_or(u32::MAX),
        });
        edit(turn);
        conversations.save()
    }

    /// `POST /v1/orchestrator/conversations/{id}/cancel`: Esc for the answer under way, then
    /// `canceled`.
    ///
    /// # Errors
    ///
    /// `forbidden` for an agent or a reader; `not_found` for an unknown conversation; `conflict`
    /// when none of its turns answers; `unavailable` (or what the runner said) when its CLI cannot
    /// be reached, with nothing changed.
    pub fn cancel_answer(&self, caller: &Caller, id: &ConversationId) -> Result<Conversation> {
        require_person(caller, "Stopping an answer")?;
        let (index, session) = {
            let state = self.conversations();
            let stored = state
                .person(caller.member)
                .and_then(|p| p.conversations.iter().find(|c| c.id == *id))
                .ok_or_else(|| no_conversation(id))?;
            let index = stored
                .turns
                .iter()
                .position(|t| t.turn.state == TurnState::Answering)
                .ok_or_else(|| {
                    WorkError::conflict("No answer of this conversation is under way.")
                })?;
            (index, stored.turns[index].turn.session)
        };
        let found = self.read(|c| query::session(c, &session))?;
        if let Some(found) = found.filter(|s| s.state != SessionState::Ended) {
            let dispatcher = self.dispatcher().ok_or_else(no_runner)?;
            guarded(|| dispatcher.command(&found.machine, &RunnerCommand::Interrupt { session }))
                .map_err(|e| refused(&e))?;
        }
        self.finish_turn(caller.member, *id, index, TurnState::Canceled, |_| {})?;
        self.conversation(caller, id)
    }

    /// `DELETE /v1/orchestrator/conversations`: forgets the caller's conversations and ends their
    /// session. The remembered engine stays.
    ///
    /// # Errors
    ///
    /// `forbidden` for an agent or a reader; the file cannot be written; database errors.
    pub fn clear_conversations(&self, caller: &Caller) -> Result<()> {
        require_person(caller, "Clearing the Orchestrator's history")?;
        let sessions: Vec<SessionId> = {
            let mut state = self.conversations();
            let person = state.person_mut(caller.member);
            let sessions = person
                .conversations
                .iter()
                .filter_map(|c| c.session)
                .collect();
            person.conversations.clear();
            state.save()?;
            sessions
        };
        let live: Vec<Session> = self.read(|c| {
            let mut live = Vec::new();
            for id in &sessions {
                if let Some(s) = query::session(c, id)?
                    && s.state != SessionState::Ended
                {
                    live.push(s);
                }
            }
            Ok(live)
        })?;
        if let Some(dispatcher) = self.dispatcher() {
            for session in live {
                let end = RunnerCommand::EndSession {
                    session: session.id,
                    mode: EndMode::Kill,
                };
                if let Err(e) = guarded(|| dispatcher.command(&session.machine, &end)) {
                    tracing::warn!(session = %session.id, error = %e, "cannot end a cleared Orchestrator session");
                }
            }
        }
        Ok(())
    }

    /// Follows every answering turn once: see the [module docs](self). Answers how many still
    /// answer. The daemon calls it about once a second, off the async executor.
    ///
    /// # Errors
    ///
    /// The file cannot be written; database errors.
    pub fn follow_orchestrator(&self) -> Result<usize> {
        let looks: Vec<Look> = {
            let state = self.conversations();
            state
                .people
                .iter()
                .flat_map(|p| {
                    p.conversations.iter().flat_map(move |c| {
                        c.turns
                            .iter()
                            .enumerate()
                            .filter(|(_, t)| t.turn.state == TurnState::Answering)
                            .map(move |(index, t)| Look {
                                member: p.member,
                                conversation: c.id,
                                index,
                                session: t.turn.session,
                                asked: t.turn.asked,
                                after: t.after,
                                typed: t.typed.clone(),
                            })
                    })
                })
                .collect()
        };
        let mut answering = 0;
        for look in looks {
            if self.look(&look)? == TurnState::Answering {
                answering += 1;
            }
        }
        Ok(answering)
    }

    /// One look at one answering turn; answers where it stands now.
    fn look(&self, look: &Look) -> Result<TurnState> {
        let session = self.read(|c| query::session(c, &look.session))?;
        let dispatcher = self.dispatcher();
        let mut items = Vec::new();
        if let (Some(found), Some(dispatcher)) = (&session, &dispatcher) {
            match self.read_back(
                dispatcher.as_ref(),
                &found.machine,
                &look.session,
                look.after,
            ) {
                Ok(read) => items = read,
                Err(e) => {
                    tracing::debug!(session = %look.session, error = %e, "cannot read an Orchestrator answer's transcript yet");
                }
            }
        }
        let now = self.now();
        let progress = progress(&items, look.after, look.typed.as_deref());
        let ended_session = session
            .as_ref()
            .is_none_or(|s| s.state == SessionState::Ended);
        let too_long = progress.answer.len() > MAX_ANSWER_BYTES;
        let timed_out = now.saturating_sub(look.asked) > i64::from(MAX_ANSWER_SECONDS) * 1000;
        let (state, note) = if progress.ended.is_some() && !too_long {
            (TurnState::Answered, None)
        } else if too_long {
            (
                TurnState::TooLong,
                Some(format!(
                    "The answer passed {} KiB: it was cut there, and its CLI was stopped.",
                    MAX_ANSWER_BYTES / 1024
                )),
            )
        } else if timed_out {
            (
                TurnState::TimedOut,
                Some(format!(
                    "No answer within {MAX_ANSWER_SECONDS} seconds: its CLI was stopped. It may \
                     be waiting in its terminal (to sign in, or to trust its folder)."
                )),
            )
        } else if ended_session {
            (
                TurnState::Failed,
                Some("Its session ended before it answered.".to_owned()),
            )
        } else {
            (TurnState::Answering, None)
        };
        let answer = cut(&progress.answer, MAX_ANSWER_BYTES);
        let found = scan(&answer);
        let (references, suggestions, answer) =
            self.read(|c| resolve(c, &self.import_choice(), &found))?;
        let ended = match state {
            TurnState::Answering => None,
            TurnState::Answered => progress
                .ended
                .map(|at| at.clamp(look.asked, now.max(look.asked))),
            _ => Some(now),
        };
        let update = Update {
            state,
            answer,
            references,
            suggestions,
            tool_runs: progress.tool_runs,
            prompt_at: progress.prompt_at,
            ended,
            note,
        };
        let changed = self.apply(look, update)?;
        if changed
            && matches!(state, TurnState::TooLong | TurnState::TimedOut)
            && let (Some(found), Some(dispatcher)) = (&session, &dispatcher)
            && found.state != SessionState::Ended
        {
            let stop = RunnerCommand::Interrupt {
                session: look.session,
            };
            if let Err(e) = guarded(|| dispatcher.command(&found.machine, &stop)) {
                tracing::warn!(session = %look.session, error = %e, "cannot stop an Orchestrator answer past its bounds");
            }
        }
        Ok(state)
    }

    /// `session`'s transcript from `after` on, oldest first: the newest page, and older ones
    /// until one starts at or before `after` (at most [`MAX_PAGES`]).
    fn read_back(
        &self,
        dispatcher: &dyn crate::Dispatcher,
        machine: &MachineId,
        session: &SessionId,
        after: u64,
    ) -> std::result::Result<Vec<TranscriptItem>, DispatchError> {
        let mut pages = Vec::new();
        let mut before = None;
        for _ in 0..MAX_PAGES {
            let page = guarded(|| dispatcher.transcript(machine, session, before, PAGE_LIMIT))?;
            let (from, at_start) = (page.from, page.at_start);
            pages.push(page.items);
            if at_start || from <= after {
                break;
            }
            before = Some(from);
        }
        Ok(pages
            .into_iter()
            .rev()
            .flatten()
            .filter(|item| item.offset() >= after)
            .collect())
    }

    /// Writes what a look decided into its turn, if the turn still answers. Answers whether its
    /// state changed (and then saves).
    fn apply(&self, look: &Look, update: Update) -> Result<bool> {
        let mut state = self.conversations();
        let Some(stored) = state.turn_mut(look.member, look.conversation, look.index) else {
            return Ok(false);
        };
        if stored.turn.state != TurnState::Answering || stored.turn.session != look.session {
            return Ok(false);
        }
        stored.prompt_at = update.prompt_at.or(stored.prompt_at);
        let turn = &mut stored.turn;
        turn.answer = update.answer;
        turn.references = update.references;
        turn.suggestions = update.suggestions;
        // Kept as it goes (the routes show it once the turn has ended).
        turn.usage = Some(AnswerUsage {
            duration_ms: 0,
            tool_runs: update.tool_runs,
            answer_bytes: u32::try_from(turn.answer.len()).unwrap_or(u32::MAX),
        });
        if update.state == TurnState::Answering {
            return Ok(false);
        }
        turn.state = update.state;
        turn.ended = update.ended;
        turn.note = update.note;
        if let Some(usage) = &mut turn.usage {
            let ended = update.ended.unwrap_or(turn.asked);
            usage.duration_ms = u64::try_from(ended.saturating_sub(turn.asked)).unwrap_or(0);
        }
        state.save()?;
        Ok(true)
    }
}

/// A turn just asked.
fn new_turn(question: String, asked: TimestampMs, session: SessionId) -> OrchestratorTurn {
    OrchestratorTurn {
        question,
        asked,
        session,
        state: TurnState::Answering,
        answer: String::new(),
        references: Vec::new(),
        suggestions: Vec::new(),
        usage: None,
        ended: None,
        note: None,
    }
}

/// A conversation as the routes answer it: its session only while that lives, and a turn's
/// usage only once it has ended.
fn view(conn: &Connection, stored: &Stored) -> Result<Conversation> {
    let session = match stored.session {
        Some(id) => query::session(conn, &id)?
            .filter(|s| s.state != SessionState::Ended)
            .map(|s| s.id),
        None => None,
    };
    Ok(Conversation {
        id: stored.id,
        engine: stored.engine,
        agent: stored.agent,
        started: stored.started,
        session,
        turns: stored
            .turns
            .iter()
            .map(|t| {
                let mut turn = t.turn.clone();
                if turn.state == TurnState::Answering {
                    turn.usage = None;
                }
                turn
            })
            .collect(),
    })
}

/// The references and suggestions of `found` that name what the hub knows (sessions only as the
/// import choice shows them), at most [`MAX_REFERENCES`]; and the answer's text, with any
/// suggestion line that names nothing known put back.
fn resolve(
    conn: &Connection,
    choice: &pitcrew_protocol::import::ImportChoice,
    found: &Scan,
) -> Result<(Vec<AnswerReference>, Vec<AnswerSuggestion>, String)> {
    let mut references = Vec::new();
    for reference in &found.references {
        if references.len() >= MAX_REFERENCES {
            break;
        }
        if let Some((target, label)) = target(conn, choice, &reference.cited)? {
            references.push(AnswerReference {
                text: reference.text.clone(),
                target,
                label,
            });
        }
    }
    let mut suggestions = Vec::new();
    let mut text = found.text.clone();
    for suggested in &found.suggestions {
        let made = match suggested {
            Suggested::Move { task, to, .. } => match target(conn, choice, task)? {
                Some((ReferenceTarget::Task { id, key }, _)) => {
                    let current = query::task(conn, &TaskRef::Id(id))?.map(|t| t.status);
                    (current != Some(*to)).then(|| AnswerSuggestion::MoveTask {
                        label: format!(
                            "Move {key} to {}",
                            crate::codec::enum_text(to).unwrap_or_default()
                        ),
                        task: id,
                        key,
                        to: *to,
                    })
                }
                _ => None,
            },
            Suggested::Open { cited, .. } => {
                target(conn, choice, cited)?.map(|(target, label)| AnswerSuggestion::Open {
                    target,
                    label: format!("Open {label}"),
                })
            }
        };
        match made {
            Some(suggestion) => suggestions.push(suggestion),
            None => {
                let line = match suggested {
                    Suggested::Move { line, .. } | Suggested::Open { line, .. } => line,
                };
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str("Suggestion: ");
                text.push_str(line);
            }
        }
    }
    Ok((references, suggestions, text))
}

/// What `cited` names, and a short label for it, if the hub knows it.
fn target(
    conn: &Connection,
    choice: &pitcrew_protocol::import::ImportChoice,
    cited: &Cited,
) -> Result<Option<(ReferenceTarget, String)>> {
    Ok(match cited {
        Cited::Session(id) => query::session(conn, id)?
            .filter(|s| choice.includes(s))
            .map(|s| {
                let label = s
                    .title
                    .as_deref()
                    .map(str::trim)
                    .filter(|t| !t.is_empty())
                    .map_or_else(|| format!("Session {}", s.id), str::to_owned);
                (ReferenceTarget::Session { id: s.id }, label)
            }),
        Cited::TaskId(id) => task_target(conn, &TaskRef::Id(*id))?,
        Cited::TaskKey(key) => task_target(conn, &TaskRef::Key(key.clone()))?,
        Cited::Workstream(id) => query::workstream(conn, id)?.map(|w| {
            (
                ReferenceTarget::Workstream {
                    id: w.id,
                    project: w.project,
                },
                w.name,
            )
        }),
        Cited::Project(id) => {
            query::project(conn, id)?.map(|p| (ReferenceTarget::Project { id: p.id }, p.name))
        }
        Cited::Recap { of, date } => {
            let found = match of {
                RecapOf::Workstream(id) => {
                    query::workstream(conn, id)?.map(|w| (w.project, Some(w.id), w.name))
                }
                RecapOf::Project(id) => query::project(conn, id)?.map(|p| (p.id, None, p.name)),
            };
            found.map(|(project, workstream, name)| {
                let label = match date {
                    Some(day) => format!("Recap of {name}, {}", day.0),
                    None => format!("Recap of {name}"),
                };
                (
                    ReferenceTarget::Recap {
                        project,
                        workstream,
                        date: date.clone(),
                    },
                    label,
                )
            })
        }
    })
}

fn task_target(conn: &Connection, task: &TaskRef) -> Result<Option<(ReferenceTarget, String)>> {
    Ok(query::task(conn, task)?.map(|t| {
        (
            ReferenceTarget::Task {
                id: t.id,
                key: t.key.clone(),
            },
            format!("{} {}", t.key, t.title),
        )
    }))
}

fn no_conversation(id: &ConversationId) -> WorkError {
    WorkError::not_found(format!("No conversation {id}."))
}

fn no_runner() -> WorkError {
    WorkError::unavailable("This hub cannot start sessions: it has no runner link.")
}

/// A runner link's call, with a panic in it taken as a failure.
fn guarded<T>(
    call: impl FnOnce() -> std::result::Result<T, DispatchError>,
) -> std::result::Result<T, DispatchError> {
    catch_unwind(AssertUnwindSafe(call))
        .unwrap_or_else(|_| Err(DispatchError::Failed("the runner link panicked".into())))
}

/// An engine's name for people.
fn engine_name(engine: Engine) -> &'static str {
    match engine {
        Engine::Claude => "Claude Code",
        Engine::Codex => "Codex",
        Engine::OpenCode => "OpenCode",
        _ => "This agent CLI",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prompt_at(offset: u64, text: &str) -> TranscriptItem {
        TranscriptItem::UserPrompt {
            at: 1,
            text: text.into(),
            offset,
        }
    }

    fn said(offset: u64, text: &str) -> TranscriptItem {
        TranscriptItem::AssistantText {
            at: 2,
            text: text.into(),
            offset,
        }
    }

    fn tool(offset: u64) -> TranscriptItem {
        TranscriptItem::ToolUse {
            at: 2,
            call_id: "c".into(),
            tool: "Bash".into(),
            target: "pitcrew session list".into(),
            input: None,
            offset,
        }
    }

    fn end(offset: u64, at: TimestampMs) -> TranscriptItem {
        TranscriptItem::TurnEnded { at, offset }
    }

    #[test]
    fn a_turn_is_its_prompt_and_the_assistant_text_after_it() {
        let items = vec![
            prompt_at(0, "the whole first prompt"),
            said(10, "Looking."),
            tool(10),
            said(20, "Two sessions ran."),
            end(20, 9),
            prompt_at(30, "And yesterday?"),
            said(40, "One."),
        ];
        let first = progress(&items, 0, None);
        assert_eq!(first.prompt_at, Some(0));
        assert_eq!(first.answer, "Looking.\n\nTwo sessions ran.");
        assert_eq!(first.tool_runs, 1);
        assert_eq!(first.ended, Some(9));
        // A follow-up: the prompt with its text, after the earlier one; not ended yet.
        let next = progress(&items, 1, Some("And yesterday?"));
        assert_eq!(next.prompt_at, Some(30));
        assert_eq!(next.answer, "One.");
        assert_eq!(next.ended, None);
        // Its prompt is not there yet: nothing.
        assert_eq!(progress(&items, 1, Some("Not typed")), Progress::default());
        assert_eq!(progress(&[], 0, None), Progress::default());
    }

    #[test]
    fn answers_are_cut_at_a_character() {
        assert_eq!(cut("héllo", 2), "h");
        assert_eq!(cut("héllo", 3), "hé");
        assert_eq!(cut("abc", 10), "abc");
    }
}
