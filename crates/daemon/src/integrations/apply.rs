//! Applying what a sync read upstream to the hub (api-v1.md, "Integrations", "What a sync does").
//!
//! The sync crates turn what changed upstream into [`Intent`]s (`plan`, `plan_workstream`);
//! this module decides which changes are the hub's business at all (**a linked scope**), routes
//! each new issue to the workstream that links its milestone or epic, else its repository or
//! project, and applies the intents through `pitcrew_hub_work::SyncCommands` (the sync's own
//! member, `Mover::Sync`). Conflicts become asks. Everything here runs on the blocking pool.
//!
//! - An issue no task mirrors is created from its "opened" change, in a linked scope, and not
//!   when it was already closed the first time it was seen. It is also created when a later read
//!   moves it, open, into a milestone or epic (or under any parent) that routes to a workstream:
//!   from the snapshot that read took ([`github_openings`], [`jira_openings`]), since the move
//!   carries none of its fields. Its other changes are skipped.
//! - A milestone or epic first seen closed in this read does not ship anything: only a close the
//!   sync sees happen does.

use pitcrew_hub_work::links::{LinkScope, scope_of};
use pitcrew_hub_work::{SyncCommands, SyncOutcome};
use pitcrew_protocol::integrations::{SyncCounts, SyncProblem};
use pitcrew_protocol::model::{ExternalRef, ExternalSystem, Receipt, Task, Workstream};
use pitcrew_sync_github::ownership::{Intent, LinkedWorkstream};
use std::collections::{BTreeMap, HashMap, HashSet};

/// What applying one sync's changes did.
#[derive(Debug, Default)]
pub struct Applied {
    pub counts: SyncCounts,
    pub problems: Vec<SyncProblem>,
    /// Upstream titles of milestones and epics seen, by key.
    pub titles: BTreeMap<String, String>,
}

/// The hub's side of one sync: the commands, the workstreams as they were when it began, and
/// what it did.
pub struct Applier<'a> {
    sync: SyncCommands<'a>,
    system: ExternalSystem,
    tracker: &'static str,
    workstreams: Vec<Workstream>,
    applied: Applied,
}

/// Where a change's item is, for routing and reporting.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Place {
    /// The repository or Jira project.
    container: String,
    /// Its milestone or epic key, if any.
    parent: Option<String>,
}

/// An issue's repository (`owner/repo#12` → `owner/repo`) or Jira project (`DEMO-12` → `DEMO`).
fn container_of(system: ExternalSystem, key: &str) -> String {
    match system {
        ExternalSystem::Jira => key.rsplit_once('-').map_or(key, |(p, _)| p).to_owned(),
        _ => key.split_once('#').map_or(key, |(r, _)| r).to_owned(),
    }
}

impl<'a> Applier<'a> {
    /// An applier for one sync of `system` through `sync`.
    ///
    /// # Errors
    /// The workstreams cannot be read.
    pub fn new(sync: SyncCommands<'a>, system: ExternalSystem) -> pitcrew_hub_work::Result<Self> {
        let workstreams = sync.workstreams()?;
        Ok(Self {
            sync,
            system,
            tracker: if system == ExternalSystem::Jira {
                "Jira"
            } else {
                "GitHub"
            },
            workstreams,
            applied: Applied::default(),
        })
    }

    /// What it did.
    #[must_use]
    pub fn finish(self) -> Applied {
        self.applied
    }

    fn problem(&mut self, scope: &str, message: impl Into<String>) {
        self.applied.problems.push(SyncProblem {
            scope: scope.to_owned(),
            message: message.into(),
        });
    }

    fn scopes(&self, w: &Workstream) -> Vec<LinkScope> {
        w.external
            .iter()
            .filter(|link| link.system == self.system)
            .filter_map(scope_of)
            .collect()
    }

    /// The workstream an item at `place` belongs to: the first that links its milestone or epic,
    /// else the first that links its repository or project.
    fn route(&self, place: &Place) -> Option<&Workstream> {
        let names_parent = |scope: &LinkScope| match (scope, &place.parent) {
            (LinkScope::GithubMilestone { repo, number }, Some(parent)) => {
                format!("{repo}#milestone:{number}") == *parent
            }
            (LinkScope::JiraEpic { key, .. }, Some(parent)) => key == parent,
            _ => false,
        };
        let names_container = |scope: &LinkScope| match scope {
            LinkScope::GithubRepo { repo } => repo.eq_ignore_ascii_case(&place.container),
            LinkScope::JiraProject { project } => *project == place.container,
            _ => false,
        };
        self.workstreams
            .iter()
            .find(|w| self.scopes(w).iter().any(names_parent))
            .or_else(|| {
                self.workstreams
                    .iter()
                    .find(|w| self.scopes(w).iter().any(names_container))
            })
    }

    /// The workstreams that link the milestone or epic `key`.
    fn linking(&self, key: &str) -> Vec<Workstream> {
        self.workstreams
            .iter()
            .filter(|w| {
                w.external
                    .iter()
                    .any(|link| link.system == self.system && link.key == key)
            })
            .cloned()
            .collect()
    }

    fn place(&self, source: &ExternalRef, parent: Option<&ExternalRef>) -> Place {
        Place {
            container: container_of(self.system, &source.key),
            parent: parent.map(|p| p.key.clone()),
        }
    }

    fn conflict(&mut self, task: Option<&Task>, source: &ExternalRef, reason: &str) {
        let title = match task {
            Some(task) => format!(
                "{}: {} changed, but {} cannot follow",
                self.tracker, source.key, task.key
            ),
            None => format!("{}: {} needs a decision", self.tracker, source.key),
        };
        let link = source.url.as_deref().unwrap_or_default();
        let body = format!(
            "{}. Nothing was changed in PitCrew; decide what to do here.\n\n{link}",
            reason.trim_end_matches('.')
        );
        match self
            .sync
            .raise_conflict(task.map(|t| t.id), &title, body.trim_end())
        {
            Ok(Some(_)) => self.applied.counts.conflicts += 1,
            Ok(None) => {}
            Err(e) => {
                let scope = container_of(self.system, &source.key);
                self.problem(&scope, format!("could not raise a conflict ask: {e}"));
            }
        }
    }

    /// Applies one intent about the item at `place`; a failure is a problem of that scope.
    fn intent(&mut self, intent: Intent, place: &Place) {
        if let Err(e) = self.try_intent(intent, place) {
            let scope = place.container.clone();
            self.problem(&scope, format!("could not apply an upstream change: {e}"));
        }
    }

    fn try_intent(&mut self, intent: Intent, place: &Place) -> pitcrew_hub_work::Result<()> {
        match intent {
            Intent::CreateTask {
                source,
                title,
                body,
                labels,
                ..
            } => {
                let Some(workstream) = self.route(place).map(|w| w.id) else {
                    self.applied.counts.skipped += 1;
                    return Ok(());
                };
                self.sync
                    .create_task(&workstream, source, &title, &body, &labels)?;
                self.applied.counts.applied += 1;
            }
            Intent::UpdateOwnedFields {
                task,
                title,
                body,
                labels,
                milestone,
            } => {
                let current = self.sync.task_by_id(&task)?;
                let mut workstream = None;
                if let Some(parent) = milestone {
                    let moved = Place {
                        container: place.container.clone(),
                        parent: parent.map(|p| p.key),
                    };
                    let target = self
                        .route(&moved)
                        .map(|t| (t.id, t.project, t.name.clone()));
                    if let Some((id, project, name)) = target {
                        if project != current.project {
                            let reason = format!(
                                "upstream moved {} to a scope linked to \"{name}\", in another \
                                 project",
                                current.key
                            );
                            if let Some(source) = current.source.clone() {
                                self.conflict(Some(&current), &source, &reason);
                            }
                        } else if current.workstream != Some(id) {
                            workstream = Some(id);
                        }
                    }
                }
                if let SyncOutcome::Changed(_) = self.sync.update_task(
                    &task,
                    title.as_deref(),
                    body.as_deref(),
                    labels.as_deref(),
                    workstream,
                )? {
                    self.applied.counts.applied += 1;
                }
            }
            Intent::ProposeMove { task, to, .. } => match self.sync.move_task(&task, to)? {
                SyncOutcome::Changed(_) => self.applied.counts.applied += 1,
                SyncOutcome::Unchanged => {}
                SyncOutcome::Refused(reason) => {
                    let current = self.sync.task_by_id(&task)?;
                    if let Some(source) = current.source.clone() {
                        self.conflict(Some(&current), &source, &reason);
                    }
                }
            },
            Intent::AttachReceipt { task, receipt } => {
                if let Receipt::PullRequest { url } = receipt
                    && !url.is_empty()
                    && self.sync.note(&task, &format!("Merged upstream: {url}"))?
                {
                    self.applied.counts.applied += 1;
                }
            }
            Intent::ConflictAsk {
                task,
                source,
                reason,
            } => {
                let current = match task {
                    Some(id) => Some(self.sync.task_by_id(&id)?),
                    None => None,
                };
                self.conflict(current.as_ref(), &source, &reason);
            }
            Intent::ProposeWorkstreamStatus { workstream, to } => {
                match self.sync.set_workstream_status(&workstream, to)? {
                    SyncOutcome::Changed(_) => self.applied.counts.applied += 1,
                    SyncOutcome::Unchanged => {}
                    SyncOutcome::Refused(reason) => {
                        let source = ExternalRef {
                            system: self.system,
                            key: place.parent.clone().unwrap_or_default(),
                            url: None,
                        };
                        self.conflict(None, &source, &reason);
                    }
                }
            }
            Intent::WorkstreamConflictAsk { source, reason, .. } => {
                self.conflict(None, &source, &reason);
            }
        }
        Ok(())
    }

    /// Applies a list of intents about the item at `place`.
    fn intents(&mut self, intents: Vec<Intent>, place: &Place) {
        for intent in intents {
            self.intent(intent, place);
        }
    }

    fn mirrored(&mut self, source: &ExternalRef) -> Option<Task> {
        match self.sync.task_by_source(source) {
            Ok(task) => task,
            Err(e) => {
                let scope = container_of(self.system, &source.key);
                self.problem(&scope, format!("could not read the hub's tasks: {e}"));
                None
            }
        }
    }

    fn linked_workstreams(&mut self, key: &str) -> Vec<(Workstream, bool)> {
        let mut out = Vec::new();
        for w in self.linking(key) {
            match self.sync.work_in_progress(&w.id) {
                Ok(busy) => out.push((w, busy)),
                Err(e) => self.problem(
                    &container_of(self.system, key),
                    format!("could not read the hub's tasks: {e}"),
                ),
            }
        }
        out
    }
}

/// For each issue, whether this read first saw it and whether it ends the read closed.
#[derive(Default)]
struct Seen {
    first: HashSet<String>,
    closed: HashMap<String, bool>,
}

impl Seen {
    fn first_seen_closed(&self, key: &str) -> bool {
        self.first.contains(key) && self.closed.get(key).copied().unwrap_or(false)
    }
}

/// For each issue a GitHub read moved into a milestone, the issue as a first read would report
/// it, from the state that read returned, when it is open: what [`apply_github`] creates a task
/// from when no task mirrors the issue yet.
#[must_use]
pub fn github_openings(
    changes: &[pitcrew_sync_github::UpstreamChange],
    state: &pitcrew_sync_github::SyncState,
) -> HashMap<String, pitcrew_sync_github::UpstreamChange> {
    changes
        .iter()
        .filter_map(|change| match change {
            pitcrew_sync_github::UpstreamChange::IssueMilestoned {
                source,
                milestone: Some(milestone),
                ..
            } => state
                .opened_from_snapshot(source, Some(milestone))
                .map(|opened| (source.key.clone(), opened)),
            _ => None,
        })
        .collect()
}

/// For each issue a Jira read moved under an epic, the issue as a first read would report it,
/// from the state that read returned, when it is not done: what [`apply_jira`] creates a task
/// from when no task mirrors the issue yet.
#[must_use]
pub fn jira_openings(
    changes: &[pitcrew_sync_jira::UpstreamChange],
    state: &pitcrew_sync_jira::SyncState,
) -> HashMap<String, pitcrew_sync_jira::UpstreamChange> {
    changes
        .iter()
        .filter_map(|change| match change {
            pitcrew_sync_jira::UpstreamChange::IssueReparented {
                source,
                epic: Some(epic),
                ..
            } => state
                .created_from_snapshot(source, Some(epic))
                .map(|created| (source.key.clone(), created)),
            _ => None,
        })
        .collect()
}

/// Applies a GitHub sync's changes. `openings` is [`github_openings`] of them.
pub fn apply_github(
    applier: &mut Applier<'_>,
    changes: &[pitcrew_sync_github::UpstreamChange],
    openings: &HashMap<String, pitcrew_sync_github::UpstreamChange>,
) {
    use pitcrew_sync_github::UpstreamChange as C;
    applier.applied.counts.changes = u32::try_from(changes.len()).unwrap_or(u32::MAX);
    let mut seen = Seen::default();
    let mut milestones_first_seen = HashSet::new();
    for change in changes {
        let key = change.source().key.clone();
        match change {
            C::IssueOpened { .. } => {
                seen.first.insert(key.clone());
                seen.closed.insert(key, false);
            }
            C::IssueClosed { .. } => {
                seen.closed.insert(key, true);
            }
            C::IssueReopened { .. } => {
                seen.closed.insert(key, false);
            }
            C::MilestoneCreated { .. } => {
                milestones_first_seen.insert(key);
            }
            _ => {}
        }
    }
    let mut skipped_sources = HashSet::new();
    for change in changes {
        let source = change.source();
        match change {
            C::MilestoneCreated { title, .. } | C::MilestoneRenamed { title, .. } => {
                applier
                    .applied
                    .titles
                    .insert(source.key.clone(), title.clone());
            }
            C::MilestoneClosed { .. } => {
                if milestones_first_seen.contains(&source.key) {
                    continue;
                }
                let place = Place {
                    container: container_of(applier.system, &source.key),
                    parent: Some(source.key.clone()),
                };
                for (w, busy) in applier.linked_workstreams(&source.key) {
                    let linked = LinkedWorkstream {
                        workstream: &w,
                        work_in_progress: busy,
                    };
                    let intents = pitcrew_sync_github::plan_workstream(change, &linked);
                    applier.intents(intents, &place);
                }
            }
            C::PullRequestMerged { closes, .. } => {
                let mut any = false;
                for issue in closes {
                    if let Some(task) = applier.mirrored(issue) {
                        any = true;
                        let place = applier.place(issue, None);
                        let intents = pitcrew_sync_github::plan(change, Some(&task));
                        applier.intents(intents, &place);
                    }
                }
                if !any {
                    applier.applied.counts.skipped += 1;
                }
            }
            C::PullRequestOpened { .. } | C::PullRequestClosed { .. } => {
                applier.applied.counts.skipped += 1;
            }
            C::IssueOpened { milestone, .. } => {
                let task = applier.mirrored(source);
                if task.is_none() && seen.first_seen_closed(&source.key) {
                    skipped_sources.insert(source.key.clone());
                    applier.applied.counts.skipped += 1;
                    continue;
                }
                let place = applier.place(source, milestone.as_ref());
                let intents = pitcrew_sync_github::plan(change, task.as_ref());
                applier.intents(intents, &place);
            }
            _ => {
                // Every other issue change: only for an issue a task mirrors, or one moved, open,
                // into a milestone that routes to a workstream (created from its snapshot).
                if skipped_sources.contains(&source.key) {
                    applier.applied.counts.skipped += 1;
                    continue;
                }
                let Some(task) = applier.mirrored(source) else {
                    if let C::IssueMilestoned {
                        milestone: Some(milestone),
                        ..
                    } = change
                        && let Some(opened) = openings.get(&source.key)
                    {
                        let place = applier.place(source, Some(milestone));
                        let intents = pitcrew_sync_github::plan(opened, None);
                        applier.intents(intents, &place);
                    } else {
                        applier.applied.counts.skipped += 1;
                    }
                    continue;
                };
                let place = applier.place(source, None);
                let intents = pitcrew_sync_github::plan(change, Some(&task));
                applier.intents(intents, &place);
            }
        }
    }
}

/// Applies a Jira sync's changes. `openings` is [`jira_openings`] of them.
pub fn apply_jira(
    applier: &mut Applier<'_>,
    changes: &[pitcrew_sync_jira::UpstreamChange],
    openings: &HashMap<String, pitcrew_sync_jira::UpstreamChange>,
) {
    use pitcrew_sync_jira::UpstreamChange as C;
    applier.applied.counts.changes = u32::try_from(changes.len()).unwrap_or(u32::MAX);
    let mut seen = Seen::default();
    let mut epics_first_seen = HashSet::new();
    for change in changes {
        let key = change.source().key.clone();
        match change {
            C::IssueCreated { .. } => {
                seen.first.insert(key.clone());
                seen.closed.insert(key, false);
            }
            C::IssueDone { .. } => {
                seen.closed.insert(key, true);
            }
            C::IssueReopened { .. } => {
                seen.closed.insert(key, false);
            }
            C::EpicCreated { .. } => {
                epics_first_seen.insert(key);
            }
            _ => {}
        }
    }
    let mut skipped_sources = HashSet::new();
    for change in changes {
        let source = change.source();
        match change {
            C::EpicCreated { title, .. } | C::EpicRenamed { title, .. } => {
                applier
                    .applied
                    .titles
                    .insert(source.key.clone(), title.clone());
            }
            C::EpicClosed { .. } => {
                if epics_first_seen.contains(&source.key) {
                    continue;
                }
                let place = Place {
                    container: container_of(applier.system, &source.key),
                    parent: Some(source.key.clone()),
                };
                for (w, busy) in applier.linked_workstreams(&source.key) {
                    let linked = LinkedWorkstream {
                        workstream: &w,
                        work_in_progress: busy,
                    };
                    let intents = pitcrew_sync_jira::plan_workstream(change, &linked);
                    applier.intents(intents, &place);
                }
            }
            C::IssueCreated { epic, .. } => {
                let task = applier.mirrored(source);
                if task.is_none() && seen.first_seen_closed(&source.key) {
                    skipped_sources.insert(source.key.clone());
                    applier.applied.counts.skipped += 1;
                    continue;
                }
                let place = applier.place(source, epic.as_ref());
                let intents = pitcrew_sync_jira::plan(change, task.as_ref());
                applier.intents(intents, &place);
            }
            _ => {
                if skipped_sources.contains(&source.key) {
                    applier.applied.counts.skipped += 1;
                    continue;
                }
                let Some(task) = applier.mirrored(source) else {
                    if let C::IssueReparented {
                        epic: Some(epic), ..
                    } = change
                        && let Some(created) = openings.get(&source.key)
                    {
                        let place = applier.place(source, Some(epic));
                        let intents = pitcrew_sync_jira::plan(created, None);
                        applier.intents(intents, &place);
                    } else {
                        applier.applied.counts.skipped += 1;
                    }
                    continue;
                };
                let place = applier.place(source, None);
                let intents = pitcrew_sync_jira::plan(change, Some(&task));
                applier.intents(intents, &place);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn containers_are_the_repository_or_the_project() {
        assert_eq!(
            container_of(ExternalSystem::Github, "example-org/demo-repo#12"),
            "example-org/demo-repo"
        );
        assert_eq!(
            container_of(ExternalSystem::Github, "example-org/demo-repo#milestone:1"),
            "example-org/demo-repo"
        );
        assert_eq!(container_of(ExternalSystem::Jira, "DEMO-12"), "DEMO");
    }
}
