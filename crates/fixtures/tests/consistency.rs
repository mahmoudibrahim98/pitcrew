//! The demo workspace must be internally consistent: every reference resolves, every invariant in
//! the protocol holds, and nothing in the JSON is silently ignored by the types.

use pitcrew_fixtures::{DEMO_WORKSPACE_JSON, DemoWorkspace, data_dir, demo_workspace};
use pitcrew_protocol::events::EventBody;
use pitcrew_protocol::model::{BriefTarget, MemberKind, Mover, SubtaskSource};
use serde_json::Value;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::hash::Hash;

fn ws() -> DemoWorkspace {
    demo_workspace().expect("the demo workspace matches the protocol types")
}

fn unique<T: Eq + Hash + Copy>(what: &str, ids: impl IntoIterator<Item = T>) -> HashSet<T> {
    let mut set = HashSet::new();
    for id in ids {
        assert!(set.insert(id), "duplicate {what} id");
    }
    set
}

/// Every key and value in `original` must survive a parse and re-serialise. This catches typos
/// in field names, which serde would otherwise ignore.
fn assert_subset(path: &str, original: &Value, reserialised: &Value) {
    match (original, reserialised) {
        (Value::Object(a), Value::Object(b)) => {
            for (key, value) in a {
                let other = b
                    .get(key)
                    .unwrap_or_else(|| panic!("{path}.{key} is not part of the type"));
                assert_subset(&format!("{path}.{key}"), value, other);
            }
        }
        (Value::Array(a), Value::Array(b)) => {
            assert_eq!(a.len(), b.len(), "{path}: array length changed");
            for (i, (x, y)) in a.iter().zip(b).enumerate() {
                assert_subset(&format!("{path}[{i}]"), x, y);
            }
        }
        _ => assert_eq!(original, reserialised, "{path}: value changed"),
    }
}

#[test]
fn parses_and_round_trips_without_losing_fields() {
    let original: Value = serde_json::from_str(DEMO_WORKSPACE_JSON).unwrap();
    let reserialised = serde_json::to_value(ws()).unwrap();
    assert_subset("$", &original, &reserialised);
}

#[test]
fn ids_are_unique() {
    let w = ws();
    unique("machine", w.machines.iter().map(|m| m.id));
    unique("member", w.members.iter().map(|m| m.id));
    unique("persona", w.personas.iter().map(|p| p.id));
    unique("project", w.projects.iter().map(|p| p.id));
    unique("workstream", w.workstreams.iter().map(|x| x.id));
    unique("task", w.tasks.iter().map(|t| t.id));
    unique(
        "subtask",
        w.tasks.iter().flat_map(|t| t.subtasks.iter().map(|s| s.id)),
    );
    unique("session", w.sessions.iter().map(|s| s.id));
    unique("dispatch", w.dispatches.iter().map(|d| d.id));
    unique("ask", w.asks.iter().map(|a| a.id));
    unique("event", w.events.iter().map(|e| e.id));
    unique("brief target", w.briefs.iter().map(|b| b.target));
    let handles: Vec<&str> = w.members.iter().map(|m| m.handle.as_str()).collect();
    assert_eq!(
        handles.len(),
        handles.iter().collect::<BTreeSet<_>>().len(),
        "duplicate handle"
    );
    let task_keys: Vec<String> = w.tasks.iter().map(|t| t.key.to_string()).collect();
    assert_eq!(
        task_keys.len(),
        task_keys.iter().collect::<BTreeSet<_>>().len(),
        "duplicate task key"
    );
}

#[test]
fn members_follow_the_ownership_rule() {
    let w = ws();
    let members: HashMap<_, _> = w.members.iter().map(|m| (m.id, m)).collect();
    let personas = unique("persona", w.personas.iter().map(|p| p.id));
    assert!(w.members.iter().any(|m| m.kind == MemberKind::Human));
    for m in &w.members {
        assert!(m.is_valid(), "{} breaks the ownership rule", m.handle);
        assert!(m.handle.starts_with('@'), "{} has no @", m.handle);
        if let Some(owner) = m.owner {
            assert_eq!(members[&owner].kind, MemberKind::Human, "owners are people");
        }
        if let Some(p) = m.persona {
            assert!(personas.contains(&p));
        }
    }
    for t in &w.teams {
        assert!(t.members.contains(&t.lead));
        assert!(t.members.iter().all(|m| members.contains_key(m)));
    }
}

#[test]
fn work_structure_references_resolve() {
    let w = ws();
    let machines = unique("machine", w.machines.iter().map(|m| m.id));
    let members = unique("member", w.members.iter().map(|m| m.id));
    let projects: HashMap<_, _> = w.projects.iter().map(|p| (p.id, p)).collect();
    let workstreams: HashMap<_, _> = w.workstreams.iter().map(|x| (x.id, x)).collect();
    let tasks = unique("task", w.tasks.iter().map(|t| t.id));

    for p in &w.projects {
        assert!(members.contains(&p.lead));
        assert!(p.members.iter().all(|m| members.contains(m)));
        assert!(p.root.iter().all(|r| machines.contains(&r.machine)));
        assert!(p.start.iter().chain(&p.due).all(|d| d.is_well_formed()));
    }
    for x in &w.workstreams {
        assert!(projects.contains_key(&x.project));
        assert!(x.locations.iter().all(|l| machines.contains(&l.machine)));
    }
    for t in &w.tasks {
        let project = projects[&t.project];
        assert_eq!(t.key.project, project.key, "{} key/project mismatch", t.key);
        if let Some(ws) = t.workstream {
            assert_eq!(
                workstreams[&ws].project, t.project,
                "{} crosses projects",
                t.key
            );
        }
        assert!(t.assignee.iter().all(|a| members.contains(a)));
        assert!(t.blocked_by.iter().all(|b| tasks.contains(b) && *b != t.id));
        assert!(t.start.iter().chain(&t.due).all(|d| d.is_well_formed()));
        for s in &t.subtasks {
            if let SubtaskSource::AgentPlan { agent } = s.source {
                assert_eq!(
                    t.assignee,
                    Some(agent),
                    "{}: plan from a non-assignee",
                    t.key
                );
            }
        }
    }
}

#[test]
fn sessions_dispatches_asks_and_briefs_resolve() {
    let w = ws();
    let machines = unique("machine", w.machines.iter().map(|m| m.id));
    let members = unique("member", w.members.iter().map(|m| m.id));
    let projects = unique("project", w.projects.iter().map(|p| p.id));
    let workstreams = unique("workstream", w.workstreams.iter().map(|x| x.id));
    let tasks: HashMap<_, _> = w.tasks.iter().map(|t| (t.id, t)).collect();
    let sessions = unique("session", w.sessions.iter().map(|s| s.id));

    for s in &w.sessions {
        assert!(machines.contains(&s.machine));
        assert!(s.agent.iter().all(|a| members.contains(a)));
        assert!(s.workstream.iter().all(|x| workstreams.contains(x)));
        if let Some(t) = s.task {
            assert_eq!(
                tasks[&t].workstream, s.workstream,
                "session/task workstream mismatch"
            );
        }
        assert_eq!(
            s.link_basis.is_some(),
            s.workstream.is_some() || s.task.is_some(),
            "a link needs a basis and a basis needs a link"
        );
        assert!(s.started <= s.last_activity);
    }
    for d in &w.dispatches {
        assert!(tasks.contains_key(&d.task));
        assert!(members.contains(&d.agent));
        assert!(d.session.iter().all(|s| sessions.contains(s)));
        assert_eq!(d.ended.is_some(), d.outcome.is_some());
    }
    for a in &w.asks {
        assert!(members.contains(&a.from) && members.contains(&a.to));
        assert!(a.task.iter().all(|t| tasks.contains_key(t)));
        assert!(a.session.iter().all(|s| sessions.contains(s)));
        if let Some(answer) = &a.answer {
            assert!(members.contains(&answer.by));
            assert!(answer.option.is_none_or(|i| i < a.options.len()));
        }
    }
    for b in &w.briefs {
        match b.target {
            BriefTarget::Project(p) => assert!(projects.contains(&p)),
            BriefTarget::Workstream(x) => assert!(workstreams.contains(&x)),
        }
    }
}

#[test]
fn events_are_ordered_authored_and_legal() {
    let w = ws();
    let members: HashMap<_, _> = w.members.iter().map(|m| (m.id, m)).collect();
    let tasks: HashMap<_, _> = w.tasks.iter().map(|t| (t.id, t)).collect();

    for pair in w.events.windows(2) {
        assert!(pair[0].id < pair[1].id, "event ids must increase");
        assert!(pair[0].at <= pair[1].at, "event times must not go back");
    }
    for e in &w.events {
        assert_eq!(e.workspace, w.workspace.id);
        let author = members[&e.author];
        match author.kind {
            MemberKind::Agent => {
                assert_eq!(e.on_behalf_of, author.owner, "agents act for their owner");
            }
            MemberKind::Human => assert!(e.on_behalf_of.is_none()),
        }
        if let EventBody::TaskMoved {
            task,
            from,
            to,
            mover,
        } = &e.body
        {
            assert!(from.can_move(*to, *mover), "illegal move in {}", e.id);
            if let Mover::Agent { on_own_task: true } = mover {
                assert_eq!(
                    tasks[task].assignee,
                    Some(e.author),
                    "not the agent's own task"
                );
            }
        }
    }
}

#[test]
fn transcript_samples_exist_and_are_valid_json_lines() {
    let dir = data_dir().join("transcripts");
    for file in ["claude/demo-session.jsonl", "codex/rollout-demo.jsonl"] {
        let text = std::fs::read_to_string(dir.join(file)).unwrap();
        assert!(!text.is_empty(), "{file} is empty");
        for (n, line) in text.lines().enumerate() {
            serde_json::from_str::<Value>(line).unwrap_or_else(|e| panic!("{file}:{}: {e}", n + 1));
        }
    }
    for file in ["opencode/schema.sql", "opencode/seed.sql"] {
        assert!(dir.join(file).is_file(), "{file} missing");
    }
}
