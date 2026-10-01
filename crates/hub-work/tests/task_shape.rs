//! `work.tasks` stores each task as the protocol's `Task` serialized to JSON, so a change to
//! `Task`'s shape must come with a bump of `Tasks::VERSION` (the store then rebuilds the documents
//! from the log). This test pins the serialized shape to the version: when `Task`, `Subtask` or
//! `ExternalRef` change, it fails until both the version and the shape below are updated.

use pitcrew_hub_work::projection::Tasks;
use pitcrew_protocol::ids::{MemberId, ProjectId, SubtaskId, TaskId, WorkstreamId};
use pitcrew_protocol::model::{
    Date, ExternalRef, ExternalSystem, Priority, Subtask, SubtaskSource, Task, TaskStatus,
};
use serde_json::Value;
use std::collections::BTreeSet;

/// The version of `work.tasks`, and every field path of a fully populated `Task` at that version.
const PINNED_VERSION: u32 = 2;
const PINNED_SHAPE: &[&str] = &[
    "accept_auto",
    "assignee",
    "blocked_by[]",
    "description",
    "due",
    "id",
    "key",
    "labels[]",
    "priority",
    "project",
    "source",
    "source.key",
    "source.system",
    "source.url",
    "start",
    "status",
    "subtasks[]",
    "subtasks[].done",
    "subtasks[].id",
    "subtasks[].source",
    "subtasks[].source.agent",
    "subtasks[].source.kind",
    "subtasks[].text",
    "title",
    "workstream",
];

/// Every field path in `value`: `a`, `a.b`, and `a[]` for array items.
fn paths(prefix: &str, value: &Value, out: &mut BTreeSet<String>) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                let path = if prefix.is_empty() {
                    key.clone()
                } else {
                    format!("{prefix}.{key}")
                };
                out.insert(path.clone());
                paths(&path, child, out);
            }
        }
        Value::Array(items) => {
            let path = format!("{prefix}[]");
            out.insert(path.clone());
            for item in items {
                paths(&path, item, out);
            }
        }
        _ => {}
    }
}

/// A task with every optional field set, so every field is serialized.
fn full_task() -> Task {
    let agent = MemberId::new();
    Task {
        id: TaskId::new(),
        key: "PAP-1".parse().expect("key"),
        project: ProjectId::new(),
        workstream: Some(WorkstreamId::new()),
        title: "Title".into(),
        description: "Description".into(),
        status: TaskStatus::Todo,
        priority: Priority::High,
        assignee: Some(agent),
        labels: vec!["label".into()],
        start: Some(Date("2026-10-01".into())),
        due: Some(Date("2026-10-31".into())),
        blocked_by: vec![TaskId::new()],
        source: Some(full_external()),
        accept_auto: true,
        subtasks: vec![
            Subtask {
                id: SubtaskId::new(),
                text: "Mine".into(),
                done: false,
                source: SubtaskSource::AgentPlan { agent },
            },
            Subtask {
                id: SubtaskId::new(),
                text: "A person's".into(),
                done: true,
                source: SubtaskSource::Human,
            },
        ],
    }
}

fn full_external() -> ExternalRef {
    ExternalRef {
        system: ExternalSystem::Github,
        key: "owner/repo#1".into(),
        url: Some("https://example.com/owner/repo/issues/1".into()),
    }
}

/// Struct literals above stop compiling when a field is added to `Task`, `Subtask` or
/// `ExternalRef`; a field that serializes differently changes the paths below.
#[test]
fn the_stored_task_shape_is_pinned_to_the_projection_version() {
    let mut shape = BTreeSet::new();
    paths("", &serde_json::to_value(full_task()).expect("json"), &mut shape);
    let pinned: BTreeSet<String> = PINNED_SHAPE.iter().map(|s| (*s).to_owned()).collect();
    assert_eq!(
        (Tasks::VERSION, shape),
        (PINNED_VERSION, pinned),
        "Task's serialized shape and Tasks::VERSION must change together: if Task changed, bump \
         Tasks::VERSION (crates/hub-work/src/projection/tasks.rs) and update PINNED_VERSION and \
         PINNED_SHAPE here"
    );
}
