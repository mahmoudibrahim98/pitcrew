//! Seeding: imports a [`DemoWorkspace`] as events, so a real hub can serve the demo data.

use crate::error::{Result, WorkError};
use crate::query;
use crate::service::WorkService;
use pitcrew_fixtures::DemoWorkspace;
use pitcrew_protocol::events::{Event, EventBody};
use pitcrew_protocol::ids::{EventId, MemberId};
use pitcrew_protocol::model::{BriefSource, MemberKind, TimestampMs};
use pitcrew_store::RevRange;

/// The events that rebuild `demo`: one event per thing it lists, then its own slice of the log.
///
/// - Everything is authored by `author` (the importing person), without `on_behalf_of`.
/// - Machines, personas, members, teams, projects, workstreams and tasks are stamped with the
///   demo's earliest time; sessions, dispatches, asks and briefs with their own times.
/// - A brief the back office wrote becomes a `brief_proposed` with its receipts, then a
///   `brief_accepted` of the same text (a person accepting the proposal). A person's brief is a
///   `brief_accepted` alone.
/// - The demo's slice of events comes last. It re-states things already listed (the demo's lists
///   are the state after it), so applying it changes nothing but the activity it records.
///
/// Brief `next` steps are not carried: `brief_accepted` has no field for them yet.
#[must_use]
pub fn demo_events(demo: &DemoWorkspace, author: MemberId) -> Vec<Event> {
    let start = earliest(demo);
    let ws = demo.workspace.id;
    let event = |at: TimestampMs, body: EventBody| Event {
        id: EventId::new(),
        at,
        workspace: ws,
        author,
        on_behalf_of: None,
        body,
    };
    let mut out = Vec::new();
    for machine in &demo.machines {
        let machine = machine.clone();
        out.push(event(start, EventBody::MachineAdded { machine }));
    }
    for persona in &demo.personas {
        let persona = persona.clone();
        out.push(event(start, EventBody::PersonaSaved { persona }));
    }
    for member in &demo.members {
        let member = member.clone();
        out.push(event(start, EventBody::MemberAdded { member }));
    }
    for team in &demo.teams {
        let team = team.clone();
        out.push(event(start, EventBody::TeamSaved { team }));
    }
    for project in &demo.projects {
        let project = project.clone();
        out.push(event(start, EventBody::ProjectCreated { project }));
    }
    for workstream in &demo.workstreams {
        let workstream = workstream.clone();
        out.push(event(start, EventBody::WorkstreamCreated { workstream }));
    }
    for task in &demo.tasks {
        let task = task.clone();
        out.push(event(start, EventBody::TaskCreated { task }));
    }
    for session in &demo.sessions {
        let at = session.started;
        let session = session.clone();
        out.push(event(at, EventBody::SessionDiscovered { session }));
    }
    for dispatch in &demo.dispatches {
        let at = dispatch.started;
        let dispatch = dispatch.clone();
        out.push(event(at, EventBody::DispatchStarted { dispatch }));
    }
    for ask in &demo.asks {
        let at = ask.created;
        let ask = ask.clone();
        out.push(event(at, EventBody::AskRaised { ask }));
    }
    for brief in &demo.briefs {
        if brief.source == BriefSource::BackOffice {
            out.push(event(
                brief.updated,
                EventBody::BriefProposed {
                    target: brief.target,
                    text: brief.text.clone(),
                    receipts: brief.receipts.clone(),
                },
            ));
        }
        out.push(event(
            brief.updated,
            EventBody::BriefAccepted {
                target: brief.target,
                text: brief.text.clone(),
                pinned: brief.pinned,
            },
        ));
    }
    out.extend(demo.events.iter().cloned());
    out
}

/// The earliest time anywhere in the demo, so the listed things exist before anything happens to
/// them.
fn earliest(demo: &DemoWorkspace) -> TimestampMs {
    demo.sessions
        .iter()
        .map(|s| s.started)
        .chain(demo.dispatches.iter().map(|d| d.started))
        .chain(demo.asks.iter().map(|a| a.created))
        .chain(demo.briefs.iter().map(|b| b.updated))
        .chain(demo.events.iter().map(|e| e.at))
        .min()
        .unwrap_or(0)
}

impl WorkService {
    /// Imports `demo` into an empty work model, as one append (see [`demo_events`]). The events
    /// are authored by the demo's first person.
    ///
    /// # Errors
    ///
    /// `invalid` if the demo is for another workspace or has no person; `conflict` if the work
    /// model already has members, machines or projects; store errors (nothing is appended then).
    pub fn seed(&self, demo: &DemoWorkspace) -> Result<RevRange> {
        if demo.workspace.id != self.workspace() {
            return Err(WorkError::invalid(format!(
                "The demo is for workspace {}, not {}.",
                demo.workspace.id,
                self.workspace()
            )));
        }
        let person = demo
            .members
            .iter()
            .find(|m| m.kind == MemberKind::Human)
            .ok_or_else(|| WorkError::invalid("The demo has no person to import it as."))?;
        let _guard = self.lock();
        if self.read(query::has_data)? {
            return Err(WorkError::conflict(
                "The workspace already has data; seed only an empty one.",
            ));
        }
        self.append(&demo_events(demo, person.id))
    }
}
