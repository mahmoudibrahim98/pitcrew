//! The recap index (`src/recap.rs`): blocks and day recaps against the demo fixture and against
//! the recap engine run over the whole log, paging and filters, growing blocks, the day cache, and
//! receipts.

mod recap_common;

use pitcrew_fixtures::{DemoWorkspace, demo_recaps, demo_workspace};
use pitcrew_hub_work::{
    BlockFilter, DaysScope, RecapIndex, Recaps, WorkService, demo_events, projections,
};
use pitcrew_protocol::events::{Event, EventBody};
use pitcrew_protocol::ids::{AskId, EventId, MemberId, TaskId, TaskKey};
use pitcrew_protocol::model::{Ask, AskKind, AskState, Date, LinkBasis, Receipt};
use pitcrew_protocol::recap::{BlockKey, DayRecap, RecapBlock};
use pitcrew_recap::{Config, Directory};
use pitcrew_store::{Store, StoreOptions};
use recap_common::{
    Ids, Oracle, SplitMix, T0, World, all_blocks, all_days, allowed_receipts, check_receipts, dump,
    gen_events, log_events, open_store, oracle_dump, refused, scopes,
};
use std::collections::HashSet;
use std::sync::Arc;
use ulid::Ulid;

fn demo() -> DemoWorkspace {
    demo_workspace().expect("the demo workspace parses")
}

/// What the hub's projections know before the demo's slice of events, as the fixture has it.
fn demo_directory(ws: &DemoWorkspace) -> Directory {
    let mut dir = Directory::new();
    ws.members.iter().for_each(|m| dir.add_member(m));
    ws.workstreams.iter().for_each(|w| dir.add_workstream(w));
    ws.tasks.iter().for_each(|t| dir.add_task(t));
    ws.sessions.iter().for_each(|s| dir.add_session(s));
    ws.dispatches.iter().for_each(|d| dir.add_dispatch(d));
    ws.asks.iter().for_each(|a| dir.add_ask(a));
    dir
}

/// The fixture's blocks newest first, as `GET /v1/recaps/blocks` serves them.
fn fixture_blocks() -> Vec<RecapBlock> {
    let mut blocks = demo_recaps().expect("demo-recaps.json parses").blocks;
    blocks.sort_by_key(|b| std::cmp::Reverse(b.block.id));
    blocks
}

/// The fixture's days for a scope, newest date first.
fn fixture_days(scope: DaysScope, ws: &DemoWorkspace) -> Vec<DayRecap> {
    let recaps = demo_recaps().expect("demo-recaps.json parses");
    let mut days: Vec<DayRecap> = match scope {
        DaysScope::Project(p) => recaps
            .projects
            .iter()
            .filter(|d| d.project == p)
            .flat_map(|d| d.days.clone())
            .collect(),
        DaysScope::Workstream(w) => {
            let project = ws.workstreams.iter().find(|x| x.id == w).map(|x| x.project);
            recaps
                .projects
                .iter()
                .filter(|d| Some(d.project) == project)
                .flat_map(|d| d.days.clone())
                .filter(|d| d.workstream == Some(w))
                .collect()
        }
    };
    days.sort_by(|a, b| b.date.cmp(&a.date));
    days
}

fn demo_scopes(ws: &DemoWorkspace) -> Vec<DaysScope> {
    ws.workstreams
        .iter()
        .map(|w| DaysScope::Workstream(w.id))
        .chain(ws.projects.iter().map(|p| DaysScope::Project(p.id)))
        .collect()
}

/// Fed the fixture's events, with what the fixture knew before them, the index serves exactly
/// `crates/fixtures/data/demo-recaps.json`: every block with its line, and every day of every
/// project and workstream.
#[test]
fn the_fixtures_events_give_the_fixture() {
    let ws = demo();
    let mut recaps = Recaps::new(Some(demo_directory(&ws)));
    recaps.push(&ws.events);
    let page = recaps
        .blocks(&BlockFilter::default(), None, Some(200))
        .expect("blocks");
    assert!(page.at_start);
    assert_eq!(page.blocks, fixture_blocks());
    for scope in demo_scopes(&ws) {
        let page = recaps.days(scope, 0, None, Some(30)).expect("days");
        assert!(page.at_start);
        assert_eq!(page.days, fixture_days(scope, &ws), "{scope:?}");
    }
    // Again, from the cache.
    let written = recaps.days_written();
    for scope in demo_scopes(&ws) {
        let page = recaps.days(scope, 0, None, Some(30)).expect("days");
        assert_eq!(page.days, fixture_days(scope, &ws), "{scope:?}");
    }
    assert_eq!(recaps.days_written(), written);
}

/// The listing events of [`demo_events`] (members, projects, workstreams, tasks, sessions,
/// dispatches, asks, briefs) moved two days before the slice, then the slice itself: a log in
/// which the hub knows what the fixture knows before the slice, and nothing of the listing is
/// within a gap of it.
fn listing_then_slice(ws: &DemoWorkspace) -> Vec<Event> {
    let author = ws.members[0].id;
    let slice: HashSet<EventId> = ws.events.iter().map(|e| e.id).collect();
    let first = ws.events.iter().map(|e| e.at).min().unwrap_or(T0);
    let all = demo_events(ws, author);
    let listing_end = all
        .iter()
        .position(|e| slice.contains(&e.id))
        .unwrap_or(all.len());
    let mut log: Vec<Event> = all[..listing_end]
        .iter()
        .cloned()
        .map(|mut e| {
            e.at = first - 2 * 86_400_000;
            e
        })
        .collect();
    log.extend(ws.events.iter().cloned());
    log
}

/// The same through the hub: a store whose log holds the fixture's events after what the fixture
/// knew before them. The blocks of the slice, and its days, are the fixture's.
#[test]
fn the_hub_gives_the_fixture_for_its_events() {
    let ws = demo();
    let dir = tempfile::tempdir().expect("tempdir");
    let work = WorkService::new(open_store(dir.path()), ws.workspace.clone());
    work.store()
        .append(&listing_then_slice(&ws))
        .expect("append");
    let slice: HashSet<EventId> = ws.events.iter().map(|e| e.id).collect();
    let blocks: Vec<RecapBlock> = all_blocks(&work, &BlockFilter::default(), 50)
        .into_iter()
        .filter(|b| slice.contains(&b.block.id))
        .collect();
    assert_eq!(blocks, fixture_blocks());
    let dates: HashSet<Date> = fixture_blocks()
        .iter()
        .map(|b| pitcrew_recap::date_of(b.block.start, 0))
        .collect();
    for scope in demo_scopes(&ws) {
        let days: Vec<DayRecap> = all_days(&work, scope, 0, 7)
            .into_iter()
            .filter(|d| dates.contains(&d.date))
            .collect();
        assert_eq!(days, fixture_days(scope, &ws), "{scope:?}");
    }
}

/// The demo as `WorkService::seed` imports it differs from the fixture. The index is right about
/// the log it has (it serves what the engine makes of that log); the log is not the fixture's:
///
/// - **The seed's own events are activity.** It writes one event per listed thing
///   (`workstream_created`, `task_created`, `session_discovered`, `dispatch_started`,
///   `ask_raised`, `brief_accepted`, ...), where the fixture's directory knew all that without
///   events. The engine makes blocks of them: "created PAP-1, created PAP-2, ...", "started a
///   session", "dispatched @runner to PAP-4", "asked @sam for a review", "accepted the brief".
/// - **The seeded log is not in time order.** The seed groups its events by kind, each at its own
///   time (workstreams and tasks at the demo's earliest time, sessions when they started, asks
///   when they were raised, briefs when they were accepted), and the slice follows them all
///   although many of its events are older; a brief the slice accepts again is accepted once more
///   after it. The engine closes a block when it sees an event more than the gap (20 minutes)
///   later, so out-of-order times split one session's work into several blocks (the "Seed runs"
///   run's session gets three blocks that start in the same millisecond) and put slice events
///   into blocks the seed's events began.
/// - **The seed's event ids are new** (`EventId::new()`: today's time, random within a
///   millisecond), so the blocks they begin sort ahead of the slice's in the newest-first order,
///   and in no stable order among themselves.
///
/// So: the fixture's blocks that no seed event reaches come back unchanged; the events of every
/// other one are in blocks of the same key that also hold seed events; and every block that is
/// not the fixture's holds seed events.
#[test]
fn the_seeded_demo_differs_by_the_seeds_own_events() {
    let ws = demo();
    let dir = tempfile::tempdir().expect("tempdir");
    let work = WorkService::new(open_store(dir.path()), ws.workspace.clone());
    work.seed(&ws).expect("seed");
    let slice: HashSet<EventId> = ws.events.iter().map(|e| e.id).collect();
    let seeded = all_blocks(&work, &BlockFilter::default(), 200);

    // The index serves what the engine makes of the seeded log: the difference is in the log.
    let log = log_events(&work);
    let oracle = Oracle::new(&log);
    assert_eq!(seeded, oracle.blocks(&BlockFilter::default()));
    for scope in demo_scopes(&ws) {
        assert_eq!(
            all_days(&work, scope, 0, 7),
            oracle.days(scope, 0),
            "{scope:?}"
        );
    }

    // Which events a block holds, as far as its id, last event and receipts show.
    let events_of = |b: &RecapBlock| -> HashSet<EventId> {
        let mut ids: HashSet<EventId> = [b.block.id, b.block.last].into();
        ids.extend(b.block.receipts().filter_map(|r| match r {
            Receipt::Event { id } => Some(*id),
            _ => None,
        }));
        ids
    };
    let holds_seed_events = |b: &RecapBlock| events_of(b).iter().any(|id| !slice.contains(id));
    let fixture = fixture_blocks();
    let mut changed = 0;
    for f in &fixture {
        if seeded.contains(f) {
            continue;
        }
        // Its events went into blocks of the same key that hold seed events too.
        changed += 1;
        let mine = events_of(f);
        let into: Vec<&RecapBlock> = seeded
            .iter()
            .filter(|b| b.block.key == f.block.key && !events_of(b).is_disjoint(&mine))
            .collect();
        assert!(
            !into.is_empty(),
            "the fixture's {:?} is nowhere",
            f.block.id
        );
        assert!(into.iter().all(|b| holds_seed_events(b)));
    }
    let added = seeded.iter().filter(|b| !fixture.contains(b)).count();
    for b in seeded.iter().filter(|b| !fixture.contains(b)) {
        assert!(
            holds_seed_events(b),
            "{:?} holds only the slice's events but is not the fixture's",
            b.block.id
        );
    }
    eprintln!(
        "seeded demo: {} blocks; of the fixture's {}, {} unchanged and {changed} changed; {added} \
         blocks hold the seed's own events",
        seeded.len(),
        fixture.len(),
        fixture.len() - changed,
    );
    assert!(changed < fixture.len());
    check_receipts(
        &recap_common::Dump {
            blocks: seeded,
            filtered: vec![],
            days: vec![],
        },
        &allowed_receipts(&log),
    );
}

/// Every page, filter and day of a generated log through the hub equals the engine run over the
/// whole log at once, at several page sizes; and every receipt points into the log.
#[test]
fn pages_and_filters_match_the_engine() {
    let world = World::new(2, 5, 12, 9);
    let mut ids = Ids::default();
    let mut log = world.setup(&mut ids, T0 - 86_400_000);
    // Bursts a few minutes apart, with pauses of an hour now and then, over several days.
    log.extend(gen_events(
        &SplitMix(11).specs(2_500, 600_000),
        &world,
        &mut ids,
        T0,
    ));
    let dir = tempfile::tempdir().expect("tempdir");
    let work = WorkService::new(open_store(dir.path()), world.workspace.clone());
    for chunk in log.chunks(400) {
        work.store().append(chunk).expect("append");
    }
    let events = log_events(&work);
    assert!(events.len() < log.len(), "some task creations were refused");
    let oracle = Oracle::new(&events);
    let expected = oracle_dump(&oracle, &world);
    assert!(expected.blocks.len() > 200, "more than one page at the cap");
    assert!(expected.days.iter().any(|d| d.len() > 10));
    for limit in [1, 3, 50, 200, 1_000] {
        let got = dump(&work, &world, limit);
        assert_eq!(got, expected, "limit {limit}");
    }
    check_receipts(&expected, &allowed_receipts(&events));
    // Spot checks without any filter code: a workstream's or project's blocks are the unfiltered
    // list's blocks with that workstream or project, in the same order.
    let every = all_blocks(&work, &BlockFilter::default(), 200);
    for w in &world.workstreams {
        let filter = BlockFilter {
            workstream: Some(w.id),
            ..BlockFilter::default()
        };
        let mine: Vec<RecapBlock> = every
            .iter()
            .filter(|b| b.block.workstream == Some(w.id))
            .cloned()
            .collect();
        assert!(!mine.is_empty());
        assert_eq!(all_blocks(&work, &filter, 7), mine);
    }
    for p in &world.projects {
        let filter = BlockFilter {
            project: Some(p.id),
            ..BlockFilter::default()
        };
        let mine: Vec<RecapBlock> = every
            .iter()
            .filter(|b| b.block.project == Some(p.id))
            .cloned()
            .collect();
        assert!(!mine.is_empty());
        assert_eq!(all_blocks(&work, &filter, 7), mine);
    }
}

/// Filters combine, `before` is exclusive and may be any id, and the defaults and caps are the
/// protocol's.
#[test]
fn filters_combine_and_limits_default_and_cap() {
    let world = World::new(2, 4, 10, 8);
    let mut ids = Ids::default();
    let mut log = world.setup(&mut ids, T0 - 86_400_000);
    log.extend(gen_events(
        &SplitMix(5).specs(3_000, 900_000),
        &world,
        &mut ids,
        T0,
    ));
    let dir = tempfile::tempdir().expect("tempdir");
    let work = WorkService::new(open_store(dir.path()), world.workspace.clone());
    work.store().append(&log).expect("append");
    let oracle = Oracle::new(&log_events(&work));

    let all = oracle.blocks(&BlockFilter::default());
    assert!(all.len() > 200);
    let page = work
        .recap_blocks(&BlockFilter::default(), None, None)
        .expect("page");
    assert_eq!(page.blocks.len(), 50);
    assert!(!page.at_start);
    let page = work
        .recap_blocks(&BlockFilter::default(), None, Some(10_000))
        .expect("page");
    assert_eq!(page.blocks.len(), 200);
    assert_eq!(page.blocks[..], all[..200]);

    // Combined: a session's blocks on one task, in one workstream and project.
    let session = world.sessions[0].id;
    let task = world.sessions[0].task.expect("linked");
    let mut combined = 0;
    for filter in [
        BlockFilter {
            session: Some(session),
            task: Some(task),
            ..BlockFilter::default()
        },
        BlockFilter {
            session: Some(session),
            task: Some(task),
            workstream: world.tasks[0].workstream,
            project: Some(world.tasks[0].project),
        },
    ] {
        let got = all_blocks(&work, &filter, 4);
        assert_eq!(got, oracle.blocks(&filter));
        assert!(got.iter().all(|b| b.block.session == Some(session)));
        assert!(got.iter().all(|b| b.block.tasks.contains(&task)));
        combined += got.len();
    }
    assert!(combined > 0);

    // `before` is exclusive, and any id works: one between two blocks' ids, or beyond them all.
    let third = all[2].block.id;
    let page = work
        .recap_blocks(&BlockFilter::default(), Some(third), Some(2))
        .expect("page");
    assert_eq!(page.blocks[..], all[3..5]);
    let between = EventId(ulid::Ulid::from(u128::from(all[3].block.id.0) + 1));
    let page = work
        .recap_blocks(&BlockFilter::default(), Some(between), Some(1))
        .expect("page");
    assert_eq!(page.blocks[..], all[3..4]);
    let page = work
        .recap_blocks(
            &BlockFilter::default(),
            Some(EventId(ulid::Ulid::from(u128::MAX))),
            Some(1),
        )
        .expect("page");
    assert_eq!(page.blocks[..], all[..1]);
    let page = work
        .recap_blocks(
            &BlockFilter::default(),
            Some(EventId(ulid::Ulid::nil())),
            Some(5),
        )
        .expect("page");
    assert!(page.blocks.is_empty() && page.at_start);

    // Days: 7 dates by default, 30 at most.
    let scope = DaysScope::Project(world.projects[0].id);
    let every = oracle.days(scope, 0);
    let dates = |days: &[DayRecap]| {
        let mut d: Vec<Date> = days.iter().map(|d| d.date.clone()).collect();
        d.dedup();
        d.len()
    };
    assert!(dates(&every) > 7);
    let page = work.recap_days(scope, 0, None, None).expect("days");
    assert_eq!(dates(&page.days), 7);
    let page = work.recap_days(scope, 0, None, Some(500)).expect("days");
    assert_eq!(dates(&page.days), dates(&every).min(30));
    assert_eq!(page.days[..], every[..page.days.len()]);
    // `before` a date, exclusive; one with no activity before it is the start.
    let newest = every[0].date.clone();
    let page = work
        .recap_days(scope, 0, Some(&newest), Some(1))
        .expect("days");
    assert!(page.days.iter().all(|d| d.date < newest));
    let page = work
        .recap_days(scope, 0, Some(&Date("2000-01-01".into())), Some(3))
        .expect("days");
    assert!(page.days.is_empty() && page.at_start);
    let page = work
        .recap_days(scope, 0, Some(&Date("9999-12-31".into())), None)
        .expect("days");
    assert_eq!(page.days.first(), every.first());
}

/// What the contract calls `400 invalid` is an `invalid` error; unknown ids are empty pages at the
/// start; nothing panics.
#[test]
fn odd_input_is_refused_or_empty_never_a_panic() {
    let ws = demo();
    let dir = tempfile::tempdir().expect("tempdir");
    let work = WorkService::new(open_store(dir.path()), ws.workspace.clone());
    work.seed(&ws).expect("seed");
    let invalid = |r: pitcrew_hub_work::Result<_>| match r {
        Err(e) => assert_eq!(e.code(), pitcrew_protocol::api::ErrorCode::Invalid, "{e}"),
        Ok(()) => panic!("accepted"),
    };
    let project = DaysScope::Project(ws.projects[0].id);
    invalid(
        work.recap_blocks(&BlockFilter::default(), None, Some(0))
            .map(drop),
    );
    invalid(work.recap_days(project, 0, None, Some(0)).map(drop));
    for tz in [841, -841, i32::MAX, i32::MIN] {
        invalid(work.recap_days(project, tz, None, None).map(drop));
    }
    for date in [
        "",
        "2026-9-30",
        "2026-13-01",
        "30-09-2026",
        "2026-09-30T00:00",
    ] {
        invalid(
            work.recap_days(project, 0, Some(&Date(date.into())), None)
                .map(drop),
        );
    }
    // The widest offsets and the far ends of the calendar are fine.
    for tz in [840, -840] {
        for before in [
            None,
            Some("0000-01-01"),
            Some("9999-12-31"),
            Some("2026-02-31"),
        ] {
            let before = before.map(|d| Date(d.into()));
            work.recap_days(project, tz, before.as_ref(), Some(30))
                .expect("days");
        }
    }
    // Ids nothing is linked to.
    let nobody = BlockFilter {
        session: Some(pitcrew_protocol::ids::SessionId(ulid::Ulid::from(7u128))),
        ..BlockFilter::default()
    };
    let page = work.recap_blocks(&nobody, None, None).expect("blocks");
    assert!(page.blocks.is_empty() && page.at_start);
    let unknown =
        DaysScope::Workstream(pitcrew_protocol::ids::WorkstreamId(ulid::Ulid::from(7u128)));
    let page = work.recap_days(unknown, 0, None, None).expect("days");
    assert!(page.days.is_empty() && page.at_start);
    // A filter that matches nothing although each of its ids is known.
    let other = ws
        .projects
        .iter()
        .find(|p| p.id != ws.workstreams[0].project)
        .expect("two projects");
    let mismatched = BlockFilter {
        workstream: Some(ws.workstreams[0].id),
        project: Some(other.id),
        ..BlockFilter::default()
    };
    let page = work.recap_blocks(&mismatched, None, None).expect("blocks");
    assert!(page.blocks.is_empty() && page.at_start);
}

/// Hostile timestamps and text still give pages, at any offset, and dates in range.
#[test]
fn hostile_times_and_text_are_served() {
    let world = World::new(1, 2, 3, 3);
    let mut ids = Ids::default();
    let mut log = world.setup(&mut ids, 0);
    let specs: Vec<recap_common::Spec> = (0..400u32)
        .map(|i| {
            let dt = match i % 5 {
                0 => i64::MAX / 3,
                1 => i64::MIN / 3,
                2 => -1,
                _ => 1_000,
            };
            ((i % 24) as u8, (i / 3) as u8, (i / 7) as u8, i % 2 == 0, dt)
        })
        .collect();
    let mut events = gen_events(&specs, &world, &mut ids, 0);
    for (i, e) in events.iter_mut().enumerate() {
        if let EventBody::FileEdited { path, .. } = &mut e.body {
            *path = format!("\u{202e}{}\u{0}{}", "a/".repeat(i % 300), "x".repeat(i));
        }
    }
    log.extend(events);
    let dir = tempfile::tempdir().expect("tempdir");
    let work = WorkService::new(open_store(dir.path()), world.workspace.clone());
    work.store().append(&log).expect("append");
    for tz in [-840, 0, 840] {
        for scope in scopes(&world) {
            for d in all_days(&work, scope, tz, 30) {
                assert!(d.date.is_well_formed(), "{}", d.date.0);
            }
        }
    }
    let blocks = all_blocks(&work, &BlockFilter::default(), 200);
    assert!(!blocks.is_empty());
    let oracle = Oracle::new(&log_events(&work));
    assert_eq!(blocks, oracle.blocks(&BlockFilter::default()));
}

/// A block whose last event is within the gap of new activity grows: the same id comes back with a
/// later `last`, more counts, a new line, and its day's paragraph changes with it. Past the gap,
/// the next event starts a new block.
#[test]
fn an_open_block_grows() {
    let world = World::new(1, 1, 2, 2);
    let mut ids = Ids::default();
    let dir = tempfile::tempdir().expect("tempdir");
    let work = WorkService::new(open_store(dir.path()), world.workspace.clone());
    work.store()
        .append(&world.setup(&mut ids, T0 - 86_400_000))
        .expect("append");
    let session = world.sessions[0].id;
    let filter = BlockFilter {
        session: Some(session),
        ..BlockFilter::default()
    };
    let scope = DaysScope::Workstream(world.workstreams[0].id);
    let today = pitcrew_recap::date_of(T0, 0);
    // File edits a minute apart (kind 4: an edit in session `a` of file `b`). The setup's
    // session discovery, a day before, is a block of its own.
    let edit =
        |ids: &mut Ids, at: i64, file: u8| gen_events(&[(4, 0, file, false, 0)], &world, ids, at);
    work.store().append(&edit(&mut ids, T0, 0)).expect("append");
    let first = work.recap_blocks(&filter, None, None).expect("blocks");
    let day = work.recap_days(scope, 0, None, None).expect("days");
    assert_eq!(first.blocks.len(), 2);
    let block = &first.blocks[0].block;
    assert_eq!(block.counts.file_edits, 1);
    assert_eq!(day.days[0].date, today);
    assert_eq!(day.days[0].blocks, vec![block.id]);

    work.store()
        .append(&edit(&mut ids, T0 + 60_000, 1))
        .expect("append");
    let grown = work.recap_blocks(&filter, None, None).expect("blocks");
    let grown_day = work.recap_days(scope, 0, None, None).expect("days");
    assert_eq!(grown.blocks.len(), 2);
    let again = &grown.blocks[0];
    assert_eq!(again.block.id, block.id);
    assert_ne!(again.block.last, block.last);
    assert_eq!(again.block.counts.file_edits, 2);
    assert_ne!(again.line, first.blocks[0].line);
    assert_eq!(
        grown.blocks[1], first.blocks[1],
        "older blocks do not change"
    );
    assert_eq!(grown_day.days.len(), day.days.len());
    assert_eq!(grown_day.days[0].blocks, vec![block.id]);
    assert_ne!(grown_day.days[0].summary, day.days[0].summary);
    assert_eq!(grown_day.days[1..], day.days[1..]);

    // Past the gap: a new block, the old one as it was.
    work.store()
        .append(&edit(&mut ids, T0 + 60_000 + 21 * 60_000, 2))
        .expect("append");
    let later = work.recap_blocks(&filter, None, None).expect("blocks");
    assert_eq!(later.blocks.len(), 3);
    assert_eq!(later.blocks[1], *again);
    assert!(later.blocks[0].block.id > block.id);
    let later_day = work.recap_days(scope, 0, None, None).expect("days");
    assert_eq!(later_day.days[0].date, today);
    assert_eq!(later_day.days[0].blocks.len(), 2);
}

/// Pushes `events` and keeps them, for a rebuild to compare with.
fn feed(recaps: &mut Recaps, log: &mut Vec<Event>, events: Vec<Event>) {
    recaps.push(&events);
    log.extend(events);
}

/// A repeated query writes no paragraph again; a block that grows rewrites only the days it is
/// in; a rename rewrites them all; a new member, task or ask that nothing named yet rewrites none;
/// and learning the name of something a block already named rewrites them all, as a rebuild would
/// write them.
#[test]
fn the_day_cache_rewrites_only_what_changed() {
    let world = World::new(1, 2, 4, 4);
    let mut ids = Ids::default();
    let mut recaps = Recaps::new(None);
    let mut log = Vec::new();
    let setup = world.setup(&mut ids, T0 - 3 * 86_400_000);
    feed(&mut recaps, &mut log, setup);
    // A day of work in each workstream, three days running (kind 0: a tool run in session `a`;
    // sessions 0 and 1 work on tasks of workstreams 0 and 1).
    for day in 0..3 {
        for a in 0..2u8 {
            let at = T0 - (2 - day) * 86_400_000 + i64::from(a) * 3_600_000;
            let run = gen_events(&[(0, a, 0, false, 0)], &world, &mut ids, at);
            feed(&mut recaps, &mut log, run);
        }
    }
    let scope = DaysScope::Project(world.projects[0].id);
    let first = recaps.days(scope, 0, None, None).expect("days");
    let written = recaps.days_written();
    assert!(written >= 3, "{written}");
    let again = recaps.days(scope, 0, None, None).expect("days");
    assert_eq!(again, first);
    assert_eq!(
        recaps.days_written(),
        written,
        "a repeated query writes nothing"
    );

    // Session 1's last block grows: only its day and workstream is written again.
    let last_at = T0 + 3_600_000;
    let edit = gen_events(&[(4, 1, 0, false, 0)], &world, &mut ids, last_at + 60_000);
    feed(&mut recaps, &mut log, edit);
    let grown = recaps.days(scope, 0, None, None).expect("days");
    assert_eq!(recaps.days_written(), written + 1);
    let changed: Vec<_> = grown
        .days
        .iter()
        .zip(&first.days)
        .filter(|(a, b)| a != b)
        .collect();
    assert_eq!(changed.len(), 1);

    // An agent's new handle may be in any paragraph: they are all written again.
    let before = recaps.days_written();
    let rename = gen_events(&[(16, 0, 0, false, 0)], &world, &mut ids, T0);
    feed(&mut recaps, &mut log, rename);
    let renamed = recaps.days(scope, 0, None, None).expect("days");
    assert_eq!(
        recaps.days_written(),
        before + renamed.days.len() as u64,
        "every entry is written again"
    );
    assert!(
        renamed
            .days
            .iter()
            .any(|d| d.summary.text.contains("@agent0x0"))
    );

    // A new member, and a new task with an ask about it next week: nothing named them before,
    // so the only paragraph written is next week's.
    let next_week = T0 + 7 * 86_400_000;
    let project = &world.projects[0];
    let ws0 = world.workstreams[0].id;
    let new_task = recap_common::task(
        TaskId(Ulid::from(900u128)),
        TaskKey::new(project.key.clone(), 900).expect("key"),
        project.id,
        Some(ws0),
    );
    let newcomer = recap_common::agent(MemberId(Ulid::from(901u128)), world.person, "@newcomer");
    let ask = Ask {
        id: AskId(Ulid::from(902u128)),
        kind: AskKind::Question,
        from: world.agents[0],
        to: world.person,
        task: Some(new_task.id),
        session: None,
        title: "Which seed?".into(),
        body: String::new(),
        options: vec![],
        receipts: vec![],
        state: AskState::Open,
        answer: None,
        created: next_week,
    };
    let news = vec![
        world.event(
            &mut ids,
            next_week,
            world.person,
            EventBody::MemberAdded { member: newcomer },
        ),
        world.event(
            &mut ids,
            next_week,
            world.person,
            EventBody::TaskCreated {
                task: new_task.clone(),
            },
        ),
        world.event(
            &mut ids,
            next_week + 60_000,
            world.agents[0],
            EventBody::AskRaised { ask },
        ),
    ];
    let before = recaps.days_written();
    feed(&mut recaps, &mut log, news);
    let with_news = recaps.days(scope, 0, None, None).expect("days");
    assert_eq!(recaps.days_written(), before + 1, "only next week's");
    assert_eq!(with_news.days[1..], renamed.days[..]);

    // A session linked to a task nobody has created yet: its paragraph says "a task". When the
    // task is created, two days on, the paragraph that named it is written with its key.
    let early = recap_common::task(
        TaskId(Ulid::from(903u128)),
        TaskKey::new(project.key.clone(), 903).expect("key"),
        project.id,
        Some(ws0),
    );
    let link = world.event(
        &mut ids,
        next_week + 120_000,
        world.person,
        EventBody::SessionLinked {
            session: world.sessions[0].id,
            workstream: Some(ws0),
            task: Some(early.id),
            basis: LinkBasis::Manual,
        },
    );
    feed(&mut recaps, &mut log, vec![link]);
    let linked = recaps.days(scope, 0, None, None).expect("days");
    let text = |page: &pitcrew_protocol::recap::DaysPage, date: &Date| -> String {
        page.days
            .iter()
            .filter(|d| d.date == *date)
            .map(|d| d.summary.text.clone())
            .collect()
    };
    let week_date = pitcrew_recap::date_of(next_week, 0);
    assert!(text(&linked, &week_date).contains("linked the session to a task"));
    let created = world.event(
        &mut ids,
        next_week + 2 * 86_400_000,
        world.person,
        EventBody::TaskCreated { task: early },
    );
    feed(&mut recaps, &mut log, vec![created]);
    let named = recaps.days(scope, 0, None, None).expect("days");
    assert!(text(&named, &week_date).contains("linked the session to PAP-903"));
    let mut rebuilt = Recaps::new(None);
    rebuilt.push(&log);
    assert_eq!(rebuilt.days(scope, 0, None, None).expect("days"), named);
}

/// An entry evicted for the directory's bound (not just a rename) moves `names_version`, so the
/// next query rewrites every day paragraph that may have named it: one that named the evicted
/// agent now says "someone" instead, as a rebuild with the same bound would write it.
#[test]
fn an_eviction_rewrites_the_days_that_named_it() {
    let world = World::new(1, 1, 2, 2);
    let mut ids = Ids::default();
    // Keeps at most 5 members: exactly the setup's person and four agents, so the next distinct
    // one evicts the least recently used.
    let mut recaps = Recaps::with_config(Config::default(), Some(Directory::with_limit(5)));
    let mut log = Vec::new();
    feed(
        &mut recaps,
        &mut log,
        world.setup(&mut ids, T0 - 86_400_000),
    );

    // Agent 1 (handle `@agent1`) runs a tool in session 0: the day's paragraph names them.
    let run = gen_events(&[(0, 0, 0, false, 0)], &world, &mut ids, T0);
    feed(&mut recaps, &mut log, run);
    let scope = DaysScope::Project(world.projects[0].id);
    let agent1 = world.agents[0];
    let handle = recaps.names().handle(agent1).expect("known").to_owned();
    let before = recaps.days(scope, 0, None, None).expect("days");
    assert!(
        before.days[0].summary.text.contains(&handle),
        "{}",
        before.days[0].summary.text
    );
    let written = recaps.days_written();

    // Four more distinct members, all added by the person (so agent 1 is never touched again),
    // evict the setup's members one by one, agent 1 last.
    for n in 0..4u128 {
        let newcomer = recap_common::agent(
            MemberId(Ulid::from(950 + n)),
            world.person,
            &format!("@new{n}"),
        );
        let add = vec![world.event(
            &mut ids,
            T0,
            world.person,
            EventBody::MemberAdded { member: newcomer },
        )];
        feed(&mut recaps, &mut log, add);
    }
    assert!(recaps.names().handle(agent1).is_none(), "evicted");

    // The next query rewrites every cached entry (the eviction's generation bump is not scoped
    // to one day), including the one that no longer shows the evicted handle.
    let after = recaps.days(scope, 0, None, None).expect("days");
    assert_eq!(
        recaps.days_written(),
        written + before.days.len() as u64,
        "every entry is written again"
    );
    assert!(
        !after.days[0].summary.text.contains(&handle),
        "{}",
        after.days[0].summary.text
    );
    assert!(after.days[0].summary.text.contains("Someone"));

    // A fresh index with the same bound, fed the whole log, agrees: a live index kept current
    // through evictions equals a rebuild.
    let mut rebuilt = Recaps::with_config(Config::default(), Some(Directory::with_limit(5)));
    rebuilt.push(&log);
    assert_eq!(rebuilt.days(scope, 0, None, None).expect("days"), after);
    assert_eq!(
        rebuilt
            .blocks(&BlockFilter::default(), None, Some(50))
            .expect("blocks"),
        recaps
            .blocks(&BlockFilter::default(), None, Some(50))
            .expect("blocks")
    );
}

/// The cache keeps at most its capacity, the least recently used out first, and answers the same
/// whatever it holds.
#[test]
fn the_day_cache_is_bounded() {
    let world = World::new(2, 4, 10, 8);
    let mut ids = Ids::default();
    let mut log = world.setup(&mut ids, T0 - 86_400_000);
    log.extend(gen_events(
        &SplitMix(3).specs(1_500, 900_000),
        &world,
        &mut ids,
        T0,
    ));
    let oracle = Oracle::new(&log);
    let mut small = Recaps::new(None).with_day_cache(5);
    small.push(&log);
    let mut none = Recaps::new(None).with_day_cache(0);
    none.push(&log);
    for tz in (-840..=840).step_by(60) {
        for scope in scopes(&world) {
            let page = small.days(scope, tz, None, Some(30)).expect("days");
            assert!(small.cached_days() <= 5);
            let expected = oracle.days(scope, tz);
            assert_eq!(page.days[..], expected[..page.days.len()], "{scope:?} {tz}");
            assert_eq!(
                none.days(scope, tz, None, Some(30)).expect("days"),
                page,
                "{scope:?} {tz}"
            );
            assert_eq!(none.cached_days(), 0);
        }
    }
    assert_eq!(small.cached_days(), 5);
    // The default holds a whole view and more.
    let mut default = Recaps::new(None);
    default.push(&log);
    for scope in scopes(&world) {
        default.days(scope, 0, None, Some(30)).expect("days");
    }
    let written = default.days_written();
    assert!(default.cached_days() as u64 == written && written > 5);
    assert!(default.cached_days() <= pitcrew_hub_work::DAY_CACHE_ENTRIES);
}

/// A `task_created` the tasks projection refused (another task holds its key) is not activity: no
/// block counts it, and the task it would have created is named nowhere.
#[test]
fn refused_task_creations_are_left_out() {
    let world = World::new(1, 1, 3, 2);
    let mut ids = Ids::default();
    let dir = tempfile::tempdir().expect("tempdir");
    let work = WorkService::new(open_store(dir.path()), world.workspace.clone());
    work.store()
        .append(&world.setup(&mut ids, T0 - 86_400_000))
        .expect("append");
    // Kind 17 with the flag: a new task with task 0's key.
    let clash = gen_events(&[(17, 0, 0, true, 0)], &world, &mut ids, T0);
    let EventBody::TaskCreated { task } = &clash[0].body else {
        panic!("a task creation");
    };
    let refused_id = task.id;
    work.store().append(&clash).expect("append");
    assert_eq!(refused(&work).len(), 1);
    let blocks = all_blocks(&work, &BlockFilter::default(), 50);
    assert!(blocks.iter().all(|b| !b.block.tasks.contains(&refused_id)));
    assert!(blocks.iter().all(|b| b.block.last != clash[0].id));
    let filter = BlockFilter {
        task: Some(refused_id),
        ..BlockFilter::default()
    };
    let page = work.recap_blocks(&filter, None, None).expect("blocks");
    assert!(page.blocks.is_empty() && page.at_start);
}

/// Whether a `task_created` was refused is known only once the tasks projection has applied it. A
/// process without the work model's projections appends work and a refused task creation: the
/// index waits at the projection's revision rather than take the creation in, and once this
/// store's next append catches the projections up, it leaves the creation out, as a rebuild does.
#[test]
fn a_lagging_tasks_projection_holds_the_index_back() {
    let world = World::new(1, 2, 4, 2);
    let mut ids = Ids::default();
    let dir = tempfile::tempdir().expect("tempdir");
    let work = WorkService::new(open_store(dir.path()), world.workspace.clone());
    let setup = world.setup(&mut ids, T0 - 86_400_000);
    work.store().append(&setup).expect("append");
    // A tool run, a new task with task 0's key (kind 17 with the flag), another tool run.
    let theirs = gen_events(
        &[
            (0, 0, 0, false, 0),
            (17, 0, 0, true, 60_000),
            (0, 0, 1, false, 60_000),
        ],
        &world,
        &mut ids,
        T0,
    );
    let EventBody::TaskCreated { task } = &theirs[1].body else {
        panic!("a task creation");
    };
    let refused_id = task.id;
    {
        let bare = Store::open(dir.path().join("hub.db"), StoreOptions::default())
            .expect("open without projections");
        bare.append(&theirs).expect("append");
    }
    let setup_rev = setup.len() as u64;
    assert_eq!(work.store().latest_rev().expect("rev"), setup_rev + 3);
    assert!(refused(&work).is_empty(), "the projection has not seen it");
    assert_eq!(work.sync_recaps().expect("sync"), setup_rev);
    let waiting = all_blocks(&work, &BlockFilter::default(), 50);
    let theirs_ids: HashSet<EventId> = theirs.iter().map(|e| e.id).collect();
    assert!(waiting.iter().all(|b| !theirs_ids.contains(&b.block.last)));

    // This store's next append catches the projections up: the creation is refused, and left out.
    let ours = gen_events(&[(4, 1, 0, false, 0)], &world, &mut ids, T0 + 180_000);
    work.store().append(&ours).expect("append");
    assert_eq!(refused(&work).len(), 1);
    let latest = work.store().latest_rev().expect("rev");
    assert_eq!(work.sync_recaps().expect("sync"), latest);
    let got = all_blocks(&work, &BlockFilter::default(), 50);
    assert!(got.iter().all(|b| !b.block.tasks.contains(&refused_id)));
    assert!(got.iter().any(|b| theirs_ids.contains(&b.block.last)));
    assert_eq!(
        got,
        Oracle::new(&log_events(&work)).blocks(&BlockFilter::default())
    );
    let rebuilt = WorkService::new(Arc::clone(work.store()), world.workspace.clone());
    assert_eq!(all_blocks(&rebuilt, &BlockFilter::default(), 50), got);
}

/// Each query reads the log from where the index left off: appends by this store, by another
/// connection to the same file (another process), and between two queries all show up, once.
#[test]
fn every_append_is_seen_once() {
    let world = World::new(1, 2, 4, 4);
    let mut ids = Ids::default();
    let dir = tempfile::tempdir().expect("tempdir");
    let work = WorkService::new(open_store(dir.path()), world.workspace.clone());
    assert_eq!(work.sync_recaps().expect("sync"), 0);
    let setup = world.setup(&mut ids, T0 - 86_400_000);
    work.store().append(&setup).expect("append");
    let specs = SplitMix(9).specs(600, 300_000);
    let events = gen_events(&specs, &world, &mut ids, T0);
    let (ours, theirs) = events.split_at(300);
    work.store().append(&ours[..100]).expect("append");
    assert_eq!(work.sync_recaps().expect("sync"), 100 + setup.len() as u64);
    work.store().append(&ours[100..]).expect("append");
    // Another connection to the same file, as another process would append.
    let other = Store::open_with(
        dir.path().join("hub.db"),
        StoreOptions::default(),
        projections(),
    )
    .expect("open again");
    for chunk in theirs.chunks(7) {
        other.append(chunk).expect("append");
    }
    drop(other);
    let rev = work.store().latest_rev().expect("rev");
    let got = all_blocks(&work, &BlockFilter::default(), 37);
    assert_eq!(work.sync_recaps().expect("sync"), rev);
    let oracle = Oracle::new(&log_events(&work));
    assert_eq!(got, oracle.blocks(&BlockFilter::default()));
    let placed: u64 = got.iter().map(|b| u64::from(b.block.counts.events)).sum();
    let placed_once: HashSet<EventId> = got.iter().map(|b| b.block.id).collect();
    assert_eq!(placed_once.len(), got.len());
    assert!(placed <= rev);
}

/// Every receipt of the seeded demo's blocks, lines and days points at an event in the log or at
/// something one carries; and a block's id and last are events in the log.
#[test]
fn every_receipt_points_into_the_log() {
    let ws = demo();
    let dir = tempfile::tempdir().expect("tempdir");
    let work = Arc::new(WorkService::new(
        open_store(dir.path()),
        ws.workspace.clone(),
    ));
    work.seed(&ws).expect("seed");
    let events = log_events(&work);
    let allowed = allowed_receipts(&events);
    let blocks = all_blocks(&*work, &BlockFilter::default(), 5);
    let days: Vec<DayRecap> = demo_scopes(&ws)
        .into_iter()
        .flat_map(|s| all_days(&*work, s, 0, 2))
        .collect();
    check_receipts(
        &recap_common::Dump {
            blocks,
            filtered: vec![],
            days: vec![days],
        },
        &allowed,
    );
    // The demo's own receipts (transcripts, jobs, commits) are among them.
    assert!(
        allowed
            .iter()
            .any(|r| matches!(r, Receipt::Transcript { .. }))
    );
    assert!(allowed.iter().any(|r| matches!(r, Receipt::Job { .. })));
}

/// The seam as the API calls it: through `Arc<dyn RecapIndex>`, from several threads at once.
#[test]
fn the_index_is_shared() {
    let world = World::new(1, 2, 4, 4);
    let mut ids = Ids::default();
    let dir = tempfile::tempdir().expect("tempdir");
    let work = Arc::new(WorkService::new(
        open_store(dir.path()),
        world.workspace.clone(),
    ));
    work.store()
        .append(&world.setup(&mut ids, T0 - 86_400_000))
        .expect("append");
    let events = gen_events(&SplitMix(1).specs(400, 300_000), &world, &mut ids, T0);
    let index: Arc<dyn RecapIndex> = work.clone();
    std::thread::scope(|s| {
        s.spawn(|| {
            for chunk in events.chunks(20) {
                work.store().append(chunk).expect("append");
            }
        });
        for _ in 0..3 {
            let index = Arc::clone(&index);
            let world = &world;
            s.spawn(move || {
                for i in 0..20 {
                    let filter = BlockFilter {
                        session: Some(world.sessions[i % 4].id),
                        ..BlockFilter::default()
                    };
                    index.recap_blocks(&filter, None, Some(3)).expect("blocks");
                    let scope = DaysScope::Project(world.projects[0].id);
                    index.recap_days(scope, 0, None, Some(2)).expect("days");
                }
            });
        }
    });
    let oracle = Oracle::new(&log_events(&work));
    assert_eq!(
        all_blocks(&*index, &BlockFilter::default(), 100),
        oracle.blocks(&BlockFilter::default())
    );
}

/// The generated logs have blocks of every key, so the tests above cover them all.
#[test]
fn generated_logs_have_every_kind_of_block() {
    let world = World::new(2, 5, 12, 9);
    let mut ids = Ids::default();
    let mut log = world.setup(&mut ids, T0 - 86_400_000);
    log.extend(gen_events(
        &SplitMix(11).specs(2_500, 600_000),
        &world,
        &mut ids,
        T0,
    ));
    let oracle = Oracle::new(&log);
    for kind in [
        |k: &BlockKey| matches!(k, BlockKey::Session(_)),
        |k: &BlockKey| matches!(k, BlockKey::Workstream(_)),
        |k: &BlockKey| matches!(k, BlockKey::Project(_)),
    ] {
        assert!(oracle.blocks.iter().any(|b| kind(&b.key)));
    }
    assert!(oracle.blocks.iter().any(|b| b.workstream.is_none()));
}
