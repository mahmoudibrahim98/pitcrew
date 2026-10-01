//! Sessions linked to workstreams by folder and branch (work package 4), with the real Claude
//! adapter. The rules themselves are table-tested in `src/link.rs`.

#![allow(clippy::unwrap_used)]

mod common;

use common::{CollectSink, FIXTURE_ID, claude_file, config, eventually, fixture_lines, labels};
use pitcrew_ingest::claude::ClaudeAdapter;
use pitcrew_protocol::events::{Event, EventBody};
use pitcrew_protocol::ids::{MachineId, WorkstreamId};
use pitcrew_protocol::model::{LinkBasis, Location};
use pitcrew_runner::{MemoryLocations, WorkstreamLocation};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

const WAIT: Duration = Duration::from_secs(5);
/// The fixture's working folder and branch.
const CWD: &str = "/home/sam/work/diffusion-paper/paper";

fn at(machine: MachineId, w: WorkstreamId, path: &str, branch: Option<&str>) -> WorkstreamLocation {
    WorkstreamLocation {
        workstream: w,
        location: Location {
            machine,
            path: path.into(),
            branch: branch.map(Into::into),
        },
    }
}

fn links(events: &[Event]) -> Vec<(WorkstreamId, LinkBasis)> {
    events
        .iter()
        .filter_map(|e| match &e.body {
            EventBody::SessionLinked {
                workstream: Some(w),
                basis,
                ..
            } => Some((*w, *basis)),
            _ => None,
        })
        .collect()
}

fn start(
    home: &Path,
    state: &Path,
    machine: MachineId,
    locations: &Arc<MemoryLocations>,
) -> (pitcrew_runner::RunnerHandle, Arc<CollectSink>) {
    let sink = Arc::new(CollectSink::default());
    let mut cfg = config(home, state).with_locations(locations.clone());
    cfg.machine = machine;
    let runner =
        pitcrew_runner::start(cfg, vec![Arc::new(ClaudeAdapter::new())], sink.clone()).unwrap();
    (runner, sink)
}

#[test]
fn sessions_are_linked_by_folder_then_branch_and_manual_links_win() {
    let home = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let machine = MachineId::new();
    let (paper, feature, elsewhere) = (
        WorkstreamId::new(),
        WorkstreamId::new(),
        WorkstreamId::new(),
    );
    let locations = Arc::new(MemoryLocations::new(vec![
        at(machine, paper, "/home/sam/work/diffusion-paper", None),
        // Another machine's folder never matches.
        at(MachineId::new(), elsewhere, CWD, None),
    ]));
    std::fs::write(
        claude_file(home.path(), FIXTURE_ID),
        fixture_lines()[..5].concat(),
    )
    .unwrap();

    let (runner, sink) = start(home.path(), state.path(), machine, &locations);
    sink.wait_for(4, WAIT).expect("discovery");
    assert_eq!(
        labels(&sink.events()),
        [
            "discovered:Working",
            "linked:Folder",
            "tool:TodoWrite",
            "tool:Read"
        ]
    );
    assert_eq!(links(&sink.events()), [(paper, LinkBasis::Folder)]);

    // A workstream on the session's branch, in the same folder: the branch is the better match.
    locations.set_locations(vec![
        at(machine, paper, "/home/sam/work/diffusion-paper", None),
        at(
            machine,
            feature,
            "/home/sam/work/diffusion-paper",
            Some("main"),
        ),
    ]);
    runner.locations_changed();
    sink.wait_for(5, WAIT).expect("relinked");
    assert_eq!(
        links(&sink.events()),
        [(paper, LinkBasis::Folder), (feature, LinkBasis::Branch)]
    );

    // A person links it by hand: a deeper location no longer moves it.
    let session = common::discovered(&sink.events()).id;
    locations.set_link(session, Some(LinkBasis::Manual));
    locations.set_locations(vec![at(machine, elsewhere, CWD, None)]);
    runner.locations_changed();
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(sink.len(), 5, "{:?}", labels(&sink.events()));
    runner.stop();

    // A restart does not link it again.
    locations.set_link(session, None);
    locations.set_locations(vec![at(
        machine,
        feature,
        "/home/sam/work/diffusion-paper",
        Some("main"),
    )]);
    let (runner, sink) = start(home.path(), state.path(), machine, &locations);
    std::thread::sleep(Duration::from_millis(400));
    runner.locations_changed();
    std::thread::sleep(Duration::from_millis(300));
    runner.stop();
    assert!(sink.events().is_empty(), "{:?}", labels(&sink.events()));
}

#[test]
fn without_locations_nothing_is_linked() {
    let home = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    std::fs::write(
        claude_file(home.path(), FIXTURE_ID),
        fixture_lines()[..5].concat(),
    )
    .unwrap();
    let sink = Arc::new(CollectSink::default());
    let runner = pitcrew_runner::start(
        config(home.path(), state.path()),
        vec![Arc::new(ClaudeAdapter::new())],
        sink.clone(),
    )
    .unwrap();
    sink.wait_for(3, WAIT).expect("discovery");
    runner.locations_changed();
    assert!(!eventually(Duration::from_millis(300), || sink.len() > 3));
    runner.stop();
    assert!(links(&sink.events()).is_empty());
}
