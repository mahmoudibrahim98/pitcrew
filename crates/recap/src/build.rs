//! Building blocks from events, all at once ([`blocks`]) or as they arrive ([`BlockBuilder`]).
//!
//! **Grouping.** Each event is routed to a [`BlockKey`]:
//! - events that name a session (tool runs, edits, turns, state changes, dispatches, asks) go to
//!   that session;
//! - events that name a task (moves, plans, assignments, comments) go to the task's session while
//!   that session has an open block, and otherwise to the task's workstream (or project);
//! - workstream events (health, briefs, decisions) go to the workstream;
//! - machine liveness, project creation and brief proposals are not activity and are skipped, and
//!   so are events the hub ignores: a stale move, or a link that would replace a firm one (see
//!   [`Directory`]).
//!
//! **Closing.** Before each event is placed, every open block whose last event is more than the
//! gap older than it is closed. The next event for that key then starts a new block. Closing
//! depends only on the events seen so far, never on the wall clock or on how events were batched,
//! so [`blocks`] and a [`BlockBuilder`] fed in any batches give the same blocks.

use crate::block::{Block, BlockKey, Config, Counts, Fact, FactKind, FileTouch};
use crate::checks::{classify, mentions_divergence};
use crate::directory::Directory;
use crate::hash::IdMap;
use crate::text::{
    JOB_CHARS, PATH_CHARS, TITLE_CHARS, clean, clean_tail, is_plain_path, push_first, push_latest,
    push_small,
};
use pitcrew_protocol::events::{Event, EventBody};
use pitcrew_protocol::ids::{EventId, MemberId, SessionId, TaskId};
use pitcrew_protocol::model::{BriefTarget, Receipt, SessionState, TaskStatus, TimestampMs};
use std::collections::BTreeSet;

/// Builds the blocks for a run of events in log order. `directory` is what was known before the
/// first event; it is updated from the events as they are read.
#[must_use]
pub fn blocks(events: &[Event], directory: &Directory, config: &Config) -> Vec<Block> {
    let mut builder = BlockBuilder::new(config.clone(), directory.clone());
    // Nobody asks this builder for changes.
    builder.track = false;
    for event in events {
        builder.push(event);
    }
    builder.finish()
}

/// Blocks changed since the last [`BlockBuilder::take_changes`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BlockChanges {
    /// Blocks that closed. They will not change again.
    pub closed: Vec<Block>,
    /// Blocks still open that changed, as they are now.
    pub open: Vec<Block>,
}

/// Builds blocks incrementally. Push events in log order, and take the changes after each batch.
#[derive(Debug)]
pub struct BlockBuilder {
    config: Config,
    directory: Directory,
    open: IdMap<BlockKey, Block>,
    /// `(end, key)` of every open block, oldest first, to find blocks to close.
    idle: BTreeSet<(TimestampMs, BlockKey)>,
    closed: Vec<Block>,
    touched: BTreeSet<BlockKey>,
    /// Whether to record which open blocks changed, for [`BlockBuilder::take_changes`].
    track: bool,
    skipped: u64,
}

/// Where an event goes, and the task it concerns.
struct Route {
    key: BlockKey,
    task: Option<TaskId>,
}

impl Route {
    fn new(key: BlockKey, task: Option<TaskId>) -> Self {
        Self { key, task }
    }
}

impl BlockBuilder {
    /// A builder with the given config and what is known before the first event.
    #[must_use]
    pub fn new(config: Config, directory: Directory) -> Self {
        Self {
            config: config.normalized(),
            directory,
            open: IdMap::default(),
            idle: BTreeSet::new(),
            closed: Vec::new(),
            touched: BTreeSet::new(),
            track: true,
            skipped: 0,
        }
    }

    /// The directory, as updated by the events so far. Use it to name things in summaries.
    #[must_use]
    pub fn directory(&self) -> &Directory {
        &self.directory
    }

    /// Events that belonged to no block: no session, task or workstream could be found, or the
    /// hub ignores them (a stale move, or a link that would replace a firm one; see
    /// [`Directory`]).
    #[must_use]
    pub fn skipped(&self) -> u64 {
        self.skipped
    }

    /// Adds one event.
    pub fn push(&mut self, event: &Event) {
        let activity = self.directory.learn(event);
        self.close_before(event.at);
        let route = if activity { self.route(event) } else { None };
        let Some(route) = route else {
            self.skipped = self.skipped.saturating_add(1);
            return;
        };
        let key = route.key;
        if !self.open.contains_key(&key) {
            while self.open.len() >= self.config.max_open {
                let Some((_, oldest)) = self.idle.pop_first() else {
                    break;
                };
                self.close(oldest);
            }
            self.open.insert(key, new_block(key, event));
            self.idle.insert((event.at, key));
        }
        if let Some(block) = self.open.get_mut(&key) {
            let old_end = block.end;
            apply(block, event, route.task, &self.directory, &self.config);
            if block.end != old_end {
                self.idle.remove(&(old_end, key));
                self.idle.insert((block.end, key));
            }
        }
        if self.track {
            self.touched.insert(key);
        }
    }

    /// Adds a batch of events and returns what changed.
    pub fn push_batch(&mut self, events: &[Event]) -> BlockChanges {
        for event in events {
            self.push(event);
        }
        self.take_changes()
    }

    /// Blocks closed, and open blocks changed, since the last call.
    pub fn take_changes(&mut self) -> BlockChanges {
        let closed = std::mem::take(&mut self.closed);
        let touched = std::mem::take(&mut self.touched);
        let open = touched
            .iter()
            .filter_map(|key| self.open.get(key).cloned())
            .collect();
        BlockChanges { closed, open }
    }

    /// Closes every block with no event in the gap before `now`, e.g. on a timer when the log has
    /// gone quiet. An event that arrives later with an older time than `now` then starts a new
    /// block, where [`blocks`] would have added it to the old one.
    pub fn close_idle(&mut self, now: TimestampMs) {
        self.close_before(now);
    }

    /// The open blocks, in order.
    #[must_use]
    pub fn open_blocks(&self) -> Vec<Block> {
        let mut out: Vec<Block> = self.open.values().cloned().collect();
        sort(&mut out);
        out
    }

    /// Every block not yet taken as closed, plus the open ones, in order.
    #[must_use]
    pub fn finish(mut self) -> Vec<Block> {
        let mut out = std::mem::take(&mut self.closed);
        out.extend(self.open.into_values());
        sort(&mut out);
        out
    }

    fn close_before(&mut self, at: TimestampMs) {
        let limit = at.saturating_sub(self.config.gap_ms);
        while let Some(&(end, key)) = self.idle.first() {
            if end >= limit {
                break;
            }
            self.idle.pop_first();
            self.close(key);
        }
    }

    fn close(&mut self, key: BlockKey) {
        if let Some(block) = self.open.remove(&key) {
            self.closed.push(block);
        }
    }

    fn route(&self, event: &Event) -> Option<Route> {
        use BlockKey::{Project, Session, Workstream};
        let session = |s: SessionId| Some(Route::new(Session(s), None));
        match &event.body {
            EventBody::ToolRan { session: s, .. }
            | EventBody::FileEdited { session: s, .. }
            | EventBody::TurnEnded { session: s, .. }
            | EventBody::SessionStateChanged { session: s, .. }
            | EventBody::SessionLinked { session: s, .. }
            | EventBody::SessionEnded { session: s } => session(*s),
            EventBody::SessionDiscovered { session: s } => session(s.id),
            EventBody::DispatchStarted { dispatch } => match dispatch.session {
                Some(s) => Some(Route::new(Session(s), Some(dispatch.task))),
                None => self.by_task(dispatch.task),
            },
            EventBody::DispatchFinished { dispatch, .. } => {
                let info = self.directory.dispatch(*dispatch)?;
                match info.session {
                    Some(s) => Some(Route::new(Session(s), Some(info.task))),
                    None => self.by_task(info.task),
                }
            }
            EventBody::AskRaised { ask } => match (ask.session, ask.task) {
                (Some(s), task) => Some(Route::new(Session(s), task)),
                (None, Some(task)) => self.by_task(task),
                (None, None) => None,
            },
            EventBody::AskAnswered { ask, .. } => {
                let info = self.directory.ask_info(*ask)?;
                match (info.session, info.task) {
                    (Some(s), task) if self.open.contains_key(&Session(s)) => {
                        Some(Route::new(Session(s), task))
                    }
                    (_, Some(task)) => self.by_task(task),
                    (Some(s), None) => Some(self.by_session_workstream(s)),
                    (None, None) => None,
                }
            }
            EventBody::TaskCreated { task } => self.by_task(task.id),
            EventBody::TaskMoved { task, .. }
            | EventBody::TaskAssigned { task, .. }
            | EventBody::SubtasksReplaced { task, .. } => self.by_task(*task),
            EventBody::CommentPosted {
                task, workstream, ..
            } => match (task, workstream) {
                (Some(task), _) => self.by_task(*task),
                (None, Some(w)) => Some(Route::new(Workstream(*w), None)),
                (None, None) => None,
            },
            EventBody::WorkstreamCreated { workstream } => {
                Some(Route::new(Workstream(workstream.id), None))
            }
            EventBody::WorkstreamChanged { workstream, .. } => {
                Some(Route::new(Workstream(*workstream), None))
            }
            EventBody::BriefAccepted { target, .. } => Some(Route::new(
                match target {
                    BriefTarget::Workstream(w) => Workstream(*w),
                    BriefTarget::Project(p) => Project(*p),
                },
                None,
            )),
            EventBody::DecisionRecorded {
                workstream: Some(w),
                ..
            } => Some(Route::new(Workstream(*w), None)),
            _ => None,
        }
    }

    /// A task's event goes to its session while that session is at work, and otherwise to where
    /// the task lives.
    fn by_task(&self, task: TaskId) -> Option<Route> {
        use BlockKey::{Project, Session, Workstream};
        let session = self.directory.task_session(task);
        if let Some(s) = session
            && self.open.contains_key(&Session(s))
        {
            return Some(Route::new(Session(s), Some(task)));
        }
        let key = match self.directory.task(task) {
            Some(info) => match info.workstream {
                Some(w) => Workstream(w),
                None => Project(info.project),
            },
            None => Session(session?),
        };
        Some(Route::new(key, Some(task)))
    }

    fn by_session_workstream(&self, s: SessionId) -> Route {
        let key = match self.directory.session(s).and_then(|i| i.workstream) {
            Some(w) => BlockKey::Workstream(w),
            None => BlockKey::Session(s),
        };
        Route::new(key, None)
    }
}

/// Sorts blocks by start, then id, keeping the given order for ties. Blocks are large, so this
/// sorts their positions and then swaps each block into place along the permutation's cycles.
fn sort(blocks: &mut [Block]) {
    let mut keys: Vec<(TimestampMs, EventId, usize)> = blocks
        .iter()
        .enumerate()
        .map(|(i, b)| (b.start, b.id, i))
        .collect();
    keys.sort_unstable();
    // `from[k]` is the current position of the block that belongs at `k`.
    let mut from: Vec<usize> = keys.into_iter().map(|(_, _, i)| i).collect();
    for start in 0..from.len() {
        let mut here = start;
        loop {
            let src = from[here];
            from[here] = here;
            if src == start || src == here {
                break;
            }
            blocks.swap(here, src);
            here = src;
        }
    }
}

fn new_block(key: BlockKey, event: &Event) -> Block {
    let (session, workstream, project) = match key {
        BlockKey::Session(s) => (Some(s), None, None),
        BlockKey::Workstream(w) => (None, Some(w), None),
        BlockKey::Project(p) => (None, None, Some(p)),
    };
    Block {
        id: event.id,
        last: event.id,
        key,
        start: event.at,
        end: event.at,
        session,
        workstream,
        project,
        tasks: Vec::new(),
        agent: None,
        actors: Vec::new(),
        counts: Counts::default(),
        files: Vec::new(),
        files_omitted: 0,
        facts: Vec::new(),
        facts_omitted: 0,
        tool_receipts: Vec::new(),
        turn_receipts: Vec::new(),
    }
}

fn inc(n: &mut u32) {
    *n = n.saturating_add(1);
}

fn push_unique<T: PartialEq + Copy>(list: &mut Vec<T>, item: T, cap: usize) {
    if list.len() < cap && !list.contains(&item) {
        push_small(list, item);
    }
}

fn clamp_u32(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// Adds one event to its block.
fn apply(block: &mut Block, event: &Event, task: Option<TaskId>, dir: &Directory, cfg: &Config) {
    // Links change only with a block's first event, a task's event, or an event that relinks the
    // session; skipping the lookups otherwise keeps tool runs and edits cheap.
    let relink = block.counts.events == 0
        || task.is_some()
        || matches!(
            event.body,
            EventBody::SessionLinked { .. }
                | EventBody::SessionDiscovered { .. }
                | EventBody::DispatchStarted { .. }
        );
    inc(&mut block.counts.events);
    block.last = event.id;
    block.start = block.start.min(event.at);
    block.end = block.end.max(event.at);
    push_unique(&mut block.actors, event.author, cfg.max_actors);
    if relink {
        link(block, task, dir, cfg);
    }

    let ev = Receipt::Event { id: event.id };
    let mut facts = Facts {
        block,
        by: event.author,
        at: event.at,
        cfg,
    };
    match &event.body {
        EventBody::ToolRan {
            tool,
            target,
            outcome,
            failed,
            receipt,
            ..
        } => {
            let b = &mut *facts.block;
            inc(&mut b.counts.tools_run);
            if *failed {
                inc(&mut b.counts.tools_failed);
            }
            push_first(&mut b.tool_receipts, [&ev, receipt], cfg.max_receipts);
            b.agent = b.agent.or(Some(event.author));
            if let Some(check) = classify(tool, target) {
                facts.check(check, *failed, &[&ev, receipt]);
            }
            if mentions_divergence(outcome) {
                facts.diverged(&[], &[&ev, receipt]);
            }
        }
        EventBody::FileEdited {
            path,
            added,
            removed,
            ..
        } => {
            let b = &mut *facts.block;
            b.agent = b.agent.or(Some(event.author));
            edit_file(b, path, *added, *removed, &ev, cfg);
        }
        EventBody::TurnEnded { receipt, .. } => {
            let b = &mut *facts.block;
            b.agent = b.agent.or(Some(event.author));
            inc(&mut b.counts.turns);
            push_first(&mut b.turn_receipts, [&ev, receipt], cfg.max_receipts);
        }
        EventBody::SessionDiscovered { session } => facts.add(
            FactKind::SessionStarted {
                title: session.title.as_deref().map(|t| clean(t, TITLE_CHARS)),
            },
            &[&ev],
        ),
        EventBody::SessionStateChanged {
            to, status_line, ..
        } => match to {
            SessionState::Waiting => {
                let status_line = status_line.as_deref().map(|s| clean(s, TITLE_CHARS));
                facts.merge(
                    |k| matches!(k, FactKind::SessionWaiting { .. }),
                    FactKind::SessionWaiting { status_line },
                    &[&ev],
                );
            }
            SessionState::Ended => facts.merge(
                |k| matches!(k, FactKind::SessionEnded),
                FactKind::SessionEnded,
                &[&ev],
            ),
            _ => {}
        },
        EventBody::SessionEnded { .. } => facts.merge(
            |k| matches!(k, FactKind::SessionEnded),
            FactKind::SessionEnded,
            &[&ev],
        ),
        EventBody::SessionLinked {
            workstream, task, ..
        } => facts.merge(
            |k| matches!(k, FactKind::SessionLinked { .. }),
            FactKind::SessionLinked {
                workstream: *workstream,
                task: *task,
            },
            &[&ev],
        ),
        EventBody::DispatchStarted { dispatch } => facts.add(
            FactKind::DispatchStarted {
                task: dispatch.task,
                agent: dispatch.agent,
            },
            &[&ev],
        ),
        EventBody::DispatchFinished {
            dispatch,
            outcome,
            summary,
        } => {
            let summary = summary.as_deref();
            facts.add(
                FactKind::DispatchFinished {
                    task: dir.dispatch(*dispatch).map(|d| d.task),
                    outcome: *outcome,
                    summary: summary.map(|s| clean(s, TITLE_CHARS)),
                },
                &[&ev],
            );
            if summary.is_some_and(mentions_divergence) {
                facts.diverged(&[], &[&ev]);
            }
        }
        EventBody::TaskCreated { task } => {
            facts.add(FactKind::TaskCreated { task: task.id }, &[&ev]);
        }
        EventBody::TaskMoved { task, from, to, .. } => {
            inc(&mut facts.block.counts.task_moves);
            facts.moved(*task, *from, *to, &ev);
        }
        EventBody::TaskAssigned { task, assignee } => {
            let t = *task;
            facts.merge(
                |k| matches!(k, FactKind::TaskAssigned { task, .. } if *task == t),
                FactKind::TaskAssigned {
                    task: t,
                    assignee: *assignee,
                },
                &[&ev],
            );
        }
        EventBody::SubtasksReplaced { task, subtasks } => {
            let t = *task;
            let done = subtasks.iter().filter(|s| s.done).count();
            facts.merge(
                |k| matches!(k, FactKind::PlanUpdated { task, .. } if *task == t),
                FactKind::PlanUpdated {
                    task: t,
                    done: clamp_u32(done),
                    total: clamp_u32(subtasks.len()),
                },
                &[&ev],
            );
        }
        EventBody::AskRaised { ask } => {
            inc(&mut facts.block.counts.asks_raised);
            let mut evidence: Vec<&Receipt> = vec![&ev];
            evidence.extend(ask.receipts.iter().take(cfg.max_receipts));
            // The divergence comes first: it is why the ask was raised.
            if mentions_divergence(&ask.title) || mentions_divergence(&ask.body) {
                let jobs: Vec<String> = ask
                    .receipts
                    .iter()
                    .filter_map(|r| match r {
                        Receipt::Job { id, .. } => Some(clean(id, JOB_CHARS)),
                        _ => None,
                    })
                    .collect();
                facts.diverged(&jobs, &evidence);
            }
            facts.add(
                FactKind::AskRaised {
                    ask: ask.id,
                    ask_kind: ask.kind,
                    to: ask.to,
                    title: clean(&ask.title, TITLE_CHARS),
                },
                &evidence,
            );
        }
        EventBody::AskAnswered { ask, .. } => {
            inc(&mut facts.block.counts.asks_answered);
            facts.add(FactKind::AskAnswered { ask: *ask }, &[&ev]);
        }
        EventBody::CommentPosted {
            task,
            workstream,
            mentions,
            ..
        } => {
            inc(&mut facts.block.counts.comments);
            let mut kept = Vec::new();
            for m in mentions.iter().take(64) {
                push_unique(&mut kept, *m, 4);
            }
            facts.add(
                FactKind::Commented {
                    task: *task,
                    workstream: *workstream,
                    mentions: kept,
                },
                &[&ev],
            );
        }
        EventBody::DecisionRecorded { text, receipts, .. } => {
            let mut evidence: Vec<&Receipt> = vec![&ev];
            evidence.extend(receipts.iter().take(cfg.max_receipts));
            facts.add(
                FactKind::DecisionRecorded {
                    text: clean(text, TITLE_CHARS),
                },
                &evidence,
            );
        }
        EventBody::WorkstreamCreated { workstream } => facts.add(
            FactKind::WorkstreamCreated {
                workstream: workstream.id,
            },
            &[&ev],
        ),
        EventBody::WorkstreamChanged {
            workstream,
            status,
            health,
        } => {
            let w = *workstream;
            facts.merge(
                |k| matches!(k, FactKind::WorkstreamChanged { workstream, .. } if *workstream == w),
                FactKind::WorkstreamChanged {
                    workstream: w,
                    status: *status,
                    health: *health,
                },
                &[&ev],
            );
        }
        EventBody::BriefAccepted { target, pinned, .. } => {
            let t = *target;
            facts.merge(
                |k| matches!(k, FactKind::BriefAccepted { target, .. } if *target == t),
                FactKind::BriefAccepted {
                    target: t,
                    pinned: *pinned,
                },
                &[&ev],
            );
        }
        _ => {}
    }
}

/// Fills in the block's workstream, project, tasks and agent from what the directory knows.
fn link(block: &mut Block, task: Option<TaskId>, dir: &Directory, cfg: &Config) {
    if let Some(s) = block.session
        && let Some(info) = dir.session(s)
    {
        if info.workstream.is_some() {
            block.workstream = info.workstream;
        }
        block.agent = block.agent.or(info.agent);
        if let Some(t) = info.task {
            push_unique(&mut block.tasks, t, cfg.max_tasks);
            if block.workstream.is_none() {
                block.workstream = dir.task(t).and_then(|i| i.workstream);
            }
        }
    }
    if let Some(t) = task {
        push_unique(&mut block.tasks, t, cfg.max_tasks);
        if let Some(info) = dir.task(t) {
            block.workstream = block.workstream.or(info.workstream);
            block.project = block.project.or(Some(info.project));
        }
    }
    if let Some(w) = block.workstream {
        block.project = dir.workstream_project(w).or(block.project);
    }
}

fn edit_file(block: &mut Block, path: &str, added: u32, removed: u32, ev: &Receipt, cfg: &Config) {
    let c = &mut block.counts;
    inc(&mut c.file_edits);
    c.lines_added = c.lines_added.saturating_add(u64::from(added));
    c.lines_removed = c.lines_removed.saturating_add(u64::from(removed));
    // Most paths are already clean; only copy the ones that are kept.
    let cleaned;
    let path = if is_plain_path(path) {
        path
    } else {
        cleaned = clean_tail(path, PATH_CHARS);
        cleaned.as_str()
    };
    if let Some(file) = block.files.iter_mut().find(|f| f.path == path) {
        inc(&mut file.edits);
        file.added = file.added.saturating_add(u64::from(added));
        file.removed = file.removed.saturating_add(u64::from(removed));
        push_latest(&mut file.receipts, [ev], cfg.max_receipts);
    } else if block.files.len() < cfg.max_files {
        push_small(
            &mut block.files,
            FileTouch {
                path: path.to_owned(),
                edits: 1,
                added: u64::from(added),
                removed: u64::from(removed),
                receipts: vec![ev.clone()],
            },
        );
    } else {
        inc(&mut block.files_omitted);
    }
}

/// Adds and merges facts for one event.
struct Facts<'a> {
    block: &'a mut Block,
    by: MemberId,
    at: TimestampMs,
    cfg: &'a Config,
}

impl Facts<'_> {
    /// Adds a new fact, or counts it as omitted when the block is full.
    fn add(&mut self, kind: FactKind, evidence: &[&Receipt]) {
        if self.block.facts.len() >= self.cfg.max_facts {
            inc(&mut self.block.facts_omitted);
            return;
        }
        let mut receipts = Vec::with_capacity(evidence.len().min(self.cfg.max_receipts));
        push_first(
            &mut receipts,
            evidence.iter().copied(),
            self.cfg.max_receipts,
        );
        push_small(
            &mut self.block.facts,
            Fact {
                by: self.by,
                at: self.at,
                kind,
                receipts,
            },
        );
    }

    /// Replaces the fact that `same` matches with `kind`, keeping its first receipts and adding
    /// the new ones; or adds `kind` when there is none.
    fn merge(&mut self, same: impl Fn(&FactKind) -> bool, kind: FactKind, evidence: &[&Receipt]) {
        if let Some(fact) = self.block.facts.iter_mut().find(|f| same(&f.kind)) {
            fact.kind = kind;
            push_latest(
                &mut fact.receipts,
                evidence.iter().copied(),
                self.cfg.max_receipts,
            );
        } else {
            self.add(kind, evidence);
        }
    }

    fn moved(&mut self, task: TaskId, from: TaskStatus, to: TaskStatus, ev: &Receipt) {
        let existing = self
            .block
            .facts
            .iter_mut()
            .find(|f| matches!(f.kind, FactKind::TaskMoved { task: t, .. } if t == task));
        if let Some(fact) = existing {
            if let FactKind::TaskMoved { to: last, .. } = &mut fact.kind {
                *last = to;
            }
            push_latest(&mut fact.receipts, [ev], self.cfg.max_receipts);
        } else {
            self.add(FactKind::TaskMoved { task, from, to }, &[ev]);
        }
    }

    /// Records a check run. Once a check fails, its receipts start at the first failure and end at
    /// the latest run, which is the evidence for "failed then passed".
    fn check(&mut self, check: crate::checks::Check, failed: bool, evidence: &[&Receipt]) {
        let cap = self.cfg.max_receipts;
        let existing = self
            .block
            .facts
            .iter_mut()
            .find(|f| matches!(f.kind, FactKind::Checks { check: c, .. } if c == check));
        let Some(fact) = existing else {
            self.add(
                FactKind::Checks {
                    check,
                    runs: 1,
                    failures: u32::from(failed),
                    last_failed: failed,
                },
                evidence,
            );
            return;
        };
        if let FactKind::Checks {
            runs,
            failures,
            last_failed,
            ..
        } = &mut fact.kind
        {
            inc(runs);
            if failed && *failures == 0 {
                fact.receipts.clear();
            }
            if failed {
                inc(failures);
            }
            *last_failed = failed;
        }
        push_latest(&mut fact.receipts, evidence.iter().copied(), cap);
    }

    fn diverged(&mut self, jobs: &[String], evidence: &[&Receipt]) {
        let cap = self.cfg.max_receipts;
        let existing = self
            .block
            .facts
            .iter_mut()
            .find(|f| matches!(f.kind, FactKind::JobDiverged { .. }));
        let Some(fact) = existing else {
            self.add(
                FactKind::JobDiverged {
                    jobs: jobs.iter().take(4).cloned().collect(),
                },
                evidence,
            );
            return;
        };
        if let FactKind::JobDiverged { jobs: known } = &mut fact.kind {
            for job in jobs {
                if known.len() < 4 && !known.contains(job) {
                    known.push(job.clone());
                }
            }
        }
        push_first(&mut fact.receipts, evidence.iter().copied(), cap);
    }
}
