//! Editing tasks (`PATCH /v1/tasks/{id-or-key}`) and creating projects and workstreams
//! (`POST /v1/projects`, `POST /v1/workstreams`), with api-v1's rules and error codes. People only.
//!
//! Each command checks the whole request before it changes anything: every `400` comes before the
//! one `409` (a `blocked_by` cycle, or a project key in use), so a malformed request is a `400` even
//! when it would also conflict.
//!
//! **Bounded work.** A body may be 1 MiB, so lists in it may be long. The checks that need no table
//! (trimming, lengths, deduplication) run before the command lock, in one pass with a hash set
//! that stops at the first label past the limit; the checks against the tables take one query per
//! list (`json_each` over the ids), however long it is. Nothing is quadratic in a list's length.

use crate::commands::{known_member, not_empty, require_person};
use crate::error::{Result, WorkError};
use crate::query::{self, TaskRef};
use crate::service::{WorkService, no_task};
use pitcrew_protocol::api::{Caller, NewProject, NewWorkstream};
use pitcrew_protocol::events::EventBody;
use pitcrew_protocol::ids::{ProjectId, WorkstreamId};
use pitcrew_protocol::model::{
    Date, Health, Location, Project, ProjectStatus, Task, TaskPatch, Workstream, WorkstreamStatus,
};
use pitcrew_store::sql::Connection;
use std::collections::HashSet;
use std::hash::Hash;

/// Longest task title, in characters (Unicode code points), after trimming.
pub const TITLE_CHARS: usize = 500;
/// Longest label, in characters, after trimming.
pub const LABEL_CHARS: usize = 64;
/// Most labels on a task.
pub const MAX_LABELS: usize = 32;

fn well_formed(date: &Date, field: &str) -> Result<()> {
    if date.is_well_formed() {
        Ok(())
    } else {
        Err(WorkError::invalid(format!(
            "{field} must be a date written YYYY-MM-DD; {:?} is not.",
            date.0
        )))
    }
}

/// `start` is not after `due`, when both are set. Dates are `YYYY-MM-DD`, so text order is date
/// order.
fn start_before_due(start: Option<&Date>, due: Option<&Date>) -> Result<()> {
    match (start, due) {
        (Some(start), Some(due)) if start.0 > due.0 => Err(WorkError::invalid(format!(
            "start ({}) must not be after due ({}).",
            start.0, due.0
        ))),
        _ => Ok(()),
    }
}

/// A location on a machine the workspace knows, with a path that is not blank.
fn known_location(conn: &Connection, location: &Location, field: &str) -> Result<()> {
    if query::machine(conn, &location.machine)?.is_none() {
        return Err(WorkError::invalid(format!(
            "{field}.machine: no machine {}.",
            location.machine
        )));
    }
    not_empty(&location.path, &format!("{field}.path"))
}

/// The list without repeats, the first of each kept, in one pass.
fn deduplicated<T: Copy + Eq + Hash>(mut list: Vec<T>) -> Vec<T> {
    let mut seen = HashSet::with_capacity(list.len().min(1024));
    list.retain(|item| seen.insert(*item));
    list
}

/// Labels trimmed and deduplicated (the first stays), each 1 to [`LABEL_CHARS`] characters, at most
/// [`MAX_LABELS`]. One pass, which stops at the first bad label or at the first distinct label past
/// the limit, so a long list costs no more than reading it.
fn checked_labels(labels: &[String]) -> Result<Vec<String>> {
    let mut seen: HashSet<&str> = HashSet::with_capacity(MAX_LABELS + 1);
    let mut out = Vec::new();
    for label in labels {
        let label = label.trim();
        // `len` first: a label of more bytes than 4 per character allowed is too long anyway.
        if label.is_empty() || label.len() > LABEL_CHARS * 4 || label.chars().count() > LABEL_CHARS
        {
            return Err(WorkError::invalid(format!(
                "Each label must be 1 to {LABEL_CHARS} characters after trimming; {:?} is not.",
                label.chars().take(LABEL_CHARS + 1).collect::<String>()
            )));
        }
        if seen.insert(label) {
            out.push(label.to_owned());
            if out.len() > MAX_LABELS {
                return Err(WorkError::invalid(format!(
                    "A task has at most {MAX_LABELS} labels; this one would have more."
                )));
            }
        }
    }
    Ok(out)
}

/// The checks of a patch that need no table, before the command lock: the title trimmed and
/// bounded, labels checked ([`checked_labels`]), dates well formed and in order when the patch
/// sets both, blockers deduplicated.
fn normalized_patch(patch: TaskPatch) -> Result<TaskPatch> {
    let title = match patch.title {
        Some(title) => {
            let trimmed = title.trim();
            let too_long = trimmed.len() > TITLE_CHARS * 4 || trimmed.chars().count() > TITLE_CHARS;
            if trimmed.is_empty() || too_long {
                return Err(WorkError::invalid(format!(
                    "title must be 1 to {TITLE_CHARS} characters after trimming."
                )));
            }
            Some(trimmed.to_owned())
        }
        None => None,
    };
    let labels = patch.labels.as_deref().map(checked_labels).transpose()?;
    for (date, field) in [(&patch.start, "start"), (&patch.due, "due")] {
        if let Some(Some(date)) = date {
            well_formed(date, field)?;
        }
    }
    if let (Some(Some(start)), Some(Some(due))) = (&patch.start, &patch.due) {
        start_before_due(Some(start), Some(due))?;
    }
    Ok(TaskPatch {
        title,
        labels,
        blocked_by: patch.blocked_by.map(deduplicated),
        ..patch
    })
}

/// The checks of a normalised patch against the tables, for `task`: the workstream belongs to the
/// task's project; blockers exist and are not the task; start and due are in order as the task
/// will be; and last, so every `400` comes first, `blocked_by` closes no cycle (`409`).
fn checked_patch(conn: &Connection, task: &Task, patch: TaskPatch) -> Result<TaskPatch> {
    if let Some(Some(id)) = &patch.workstream {
        let found = query::workstream(conn, id)?
            .ok_or_else(|| WorkError::invalid(format!("workstream: no workstream {id}.")))?;
        if found.project != task.project {
            return Err(WorkError::invalid(format!(
                "workstream \"{}\" belongs to another project than {}.",
                found.name, task.key
            )));
        }
    }
    // The rule holds for the task as it will be: a new start against the current due, and the
    // other way round.
    let will_start = patch
        .start
        .as_ref()
        .map_or(task.start.as_ref(), Option::as_ref);
    let will_be_due = patch.due.as_ref().map_or(task.due.as_ref(), Option::as_ref);
    start_before_due(will_start, will_be_due)?;
    if let Some(blockers) = &patch.blocked_by {
        if let Some(i) = blockers.iter().position(|id| *id == task.id) {
            return Err(WorkError::invalid(format!(
                "blocked_by[{i}]: {} cannot be blocked by itself.",
                task.key
            )));
        }
        if let Some((i, id)) = query::first_unknown_task(conn, blockers)? {
            return Err(WorkError::invalid(format!(
                "blocked_by[{i}]: no task {id}."
            )));
        }
        if let Some(blocker) = query::first_waiting_on(conn, &task.id, blockers)? {
            let key = query::task(conn, &TaskRef::Id(blocker))?
                .map_or_else(|| blocker.to_string(), |t| t.key.to_string());
            return Err(WorkError::conflict(format!(
                "{key} already waits on {}, so {} cannot wait on it.",
                task.key, task.key
            )));
        }
    }
    Ok(patch)
}

/// The fields of `wanted` whose values differ from the task's (lists compared in order).
pub(crate) fn changed_fields(task: &Task, wanted: TaskPatch) -> TaskPatch {
    fn differs<T: PartialEq>(wanted: Option<T>, current: &T) -> Option<T> {
        wanted.filter(|w| w != current)
    }
    TaskPatch {
        workstream: differs(wanted.workstream, &task.workstream),
        title: differs(wanted.title, &task.title),
        description: differs(wanted.description, &task.description),
        priority: differs(wanted.priority, &task.priority),
        labels: differs(wanted.labels, &task.labels),
        start: differs(wanted.start, &task.start),
        due: differs(wanted.due, &task.due),
        blocked_by: differs(wanted.blocked_by, &task.blocked_by),
        accept_auto: differs(wanted.accept_auto, &task.accept_auto),
    }
}

impl WorkService {
    /// Edits a task's fields (`PATCH /v1/tasks/{id-or-key}`). People only.
    ///
    /// The whole patch is checked first (see [`TaskPatch`] and api-v1's "Editing a task"); then
    /// `task_updated` carries only the fields that differ from the task's, with `null` for a field
    /// it clears. A patch that changes nothing, `{}` included, returns the task and appends
    /// nothing.
    ///
    /// # Errors
    ///
    /// `forbidden` for an agent; `not_found` for an unknown task; `invalid` for a title or label
    /// out of bounds, a workstream of another project or unknown, an unknown blocker or the task
    /// itself, a malformed date or a start after the due date; `conflict` when `blocked_by` would
    /// close a cycle.
    pub fn patch_task(&self, caller: &Caller, task: &TaskRef, patch: TaskPatch) -> Result<Task> {
        require_person(caller, "Editing a task")?;
        // Checked before the lock; its error waits until the task is known to exist (404 first).
        let normalized = normalized_patch(patch);
        let _guard = self.lock();
        let (task, changes) = self.read(|c| {
            let task = query::task(c, task)?.ok_or_else(|| no_task(task))?;
            let wanted = checked_patch(c, &task, normalized?)?;
            let changes = changed_fields(&task, wanted);
            Ok((task, changes))
        })?;
        if changes.is_empty() {
            return Ok(task);
        }
        self.append(&[self.by(
            caller,
            EventBody::TaskUpdated {
                task: task.id,
                patch: changes,
            },
        )])?;
        self.reload_task(task.id)
    }

    /// Creates a project (`POST /v1/projects`). People only.
    ///
    /// The lead defaults to the caller and is always a member (put first when `members` leaves it
    /// out); repeated members are dropped. The status defaults to `in_progress`; `external` starts
    /// empty.
    ///
    /// # Errors
    ///
    /// `forbidden` for an agent; `invalid` for a blank name, an unknown lead or member, a malformed
    /// date or a start after the due date, a root on an unknown machine or with a blank path;
    /// `conflict` when another project has the key (also when another writer took it first; see
    /// "One writer" on [`WorkService`]).
    pub fn create_project(&self, caller: &Caller, new: NewProject) -> Result<Project> {
        require_person(caller, "Creating a project")?;
        not_empty(&new.name, "name")?;
        for (date, field) in [(&new.start, "start"), (&new.due, "due")] {
            if let Some(date) = date {
                well_formed(date, field)?;
            }
        }
        start_before_due(new.start.as_ref(), new.due.as_ref())?;
        let lead = new.lead.unwrap_or(caller.member);
        let mut members = deduplicated(new.members.unwrap_or_default());
        let _guard = self.lock();
        self.read(|c| {
            known_member(c, &lead, "lead")?;
            if let Some((i, id)) = query::first_unknown_member(c, &members)? {
                return Err(WorkError::invalid(format!("members[{i}]: no member {id}.")));
            }
            if let Some(root) = &new.root {
                known_location(c, root, "root")?;
            }
            match query::project_with_key(c, &new.key)? {
                Some(holder) => Err(key_in_use(&holder)),
                None => Ok(()),
            }
        })?;
        if !members.contains(&lead) {
            members.insert(0, lead);
        }
        let project = Project {
            id: ProjectId::new(),
            key: new.key,
            name: new.name,
            status: new.status.unwrap_or(ProjectStatus::InProgress),
            lead,
            members,
            start: new.start,
            due: new.due,
            root: new.root,
            external: Vec::new(),
        };
        let (id, key) = (project.id, project.key.clone());
        self.append(&[self.by(caller, EventBody::ProjectCreated { project })])?;
        match self.read(|c| Ok((query::project(c, &id)?, query::project_with_key(c, &key)?)))? {
            (Some(project), _) => Ok(project),
            // Only a second writer can take the key between the check and the append; the
            // projection then kept its project.
            (None, Some(holder)) => Err(key_in_use(&holder)),
            (None, None) => Err(WorkError::internal(format!(
                "project {id} is missing after it was created"
            ))),
        }
    }

    /// Creates a workstream in a project (`POST /v1/workstreams`). People only.
    ///
    /// The status defaults to `active`; the health starts `on_track` and `external` empty.
    ///
    /// # Errors
    ///
    /// `forbidden` for an agent; `invalid` for a blank name, or a location on an unknown machine or
    /// with a blank path; then `not_found` for an unknown project, although it is in the body (as
    /// api-v1 says).
    pub fn create_workstream(&self, caller: &Caller, new: NewWorkstream) -> Result<Workstream> {
        require_person(caller, "Creating a workstream")?;
        not_empty(&new.name, "name")?;
        let locations = new.locations.unwrap_or_default();
        for (i, location) in locations.iter().enumerate() {
            not_empty(&location.path, &format!("locations[{i}].path"))?;
        }
        let machines: Vec<_> = locations.iter().map(|l| l.machine).collect();
        let _guard = self.lock();
        self.read(|c| {
            if let Some((i, id)) = query::first_unknown_machine(c, &machines)? {
                return Err(WorkError::invalid(format!(
                    "locations[{i}].machine: no machine {id}."
                )));
            }
            match query::project(c, &new.project)? {
                Some(_) => Ok(()),
                None => Err(WorkError::not_found(format!("No project {}.", new.project))),
            }
        })?;
        let workstream = Workstream {
            id: WorkstreamId::new(),
            project: new.project,
            name: new.name,
            status: new.status.unwrap_or(WorkstreamStatus::Active),
            health: Health::OnTrack,
            locations,
            external: Vec::new(),
        };
        let id = workstream.id;
        self.append(&[self.by(caller, EventBody::WorkstreamCreated { workstream })])?;
        self.read(|c| query::workstream(c, &id))?.ok_or_else(|| {
            WorkError::internal(format!("workstream {id} is missing after it was created"))
        })
    }
}

fn key_in_use(holder: &Project) -> WorkError {
    WorkError::conflict(format!(
        "The key {} is already used by \"{}\".",
        holder.key, holder.name
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pitcrew_protocol::ids::TaskId;

    fn labels(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn labels_are_trimmed_deduplicated_and_bounded() {
        assert_eq!(
            checked_labels(&labels(&[" figures ", "paper", "figures"])).expect("ok"),
            labels(&["figures", "paper"])
        );
        assert!(checked_labels(&labels(&["ok", "  "])).is_err());
        assert!(checked_labels(&["y".repeat(LABEL_CHARS + 1)]).is_err());
        assert!(checked_labels(&["z".repeat(LABEL_CHARS)]).is_ok());
        // Code points, not bytes.
        assert!(checked_labels(&["é".repeat(LABEL_CHARS)]).is_ok());
        let many: Vec<String> = (0..=MAX_LABELS).map(|i| format!("l{i}")).collect();
        assert!(checked_labels(&many).is_err());
        assert!(checked_labels(&many[..MAX_LABELS]).is_ok());
        // Repeats do not count: the limit is on distinct labels.
        let mut repeated = many[..MAX_LABELS].to_vec();
        repeated.extend(std::iter::repeat_n(" l0 ".to_owned(), 100_000));
        assert_eq!(checked_labels(&repeated).expect("ok").len(), MAX_LABELS);
    }

    #[test]
    fn start_is_checked_against_due() {
        let d = |s: &str| Date(s.to_owned());
        assert!(start_before_due(Some(&d("2026-10-02")), Some(&d("2026-10-01"))).is_err());
        assert!(start_before_due(Some(&d("2026-10-01")), Some(&d("2026-10-01"))).is_ok());
        assert!(start_before_due(None, Some(&d("2026-10-01"))).is_ok());
        assert!(start_before_due(Some(&d("2026-10-01")), None).is_ok());
    }

    #[test]
    fn blockers_and_members_keep_the_first_of_each() {
        let (a, b) = (TaskId::new(), TaskId::new());
        assert_eq!(deduplicated(vec![a, b, a, a, b]), [a, b]);
    }
}
