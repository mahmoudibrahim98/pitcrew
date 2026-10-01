//! The recap routes in the real daemon, over the hub's recap index: `GET /v1/recaps/blocks` and
//! `GET /v1/recaps/days` with `--demo`.
//!
//! The seeded demo is not the mock's fixture (hub-work's README, "The seeded demo is not the
//! fixture"), so these check what the contract promises of any log rather than the fixture's values:
//! order, paging, filters, well-formed spans, and that days and blocks agree.

#![allow(clippy::unwrap_used)]

mod common;

use common::{Daemon, id};
use pitcrew_protocol::recap::{BlocksPage, DayRecap, DaysPage, RecapBlock, Summary};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::time::Duration;

const WAIT: Duration = Duration::from_secs(30);

fn state_dir() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let state = tmp.path().join("state");
    (tmp, state)
}

/// `GET /v1/recaps/blocks<query>`, which must answer 200 with a well-formed page.
fn blocks(daemon: &Daemon, token: &str, query: &str) -> BlocksPage {
    let reply = daemon.get(&format!("/v1/recaps/blocks{query}"), Some(token));
    assert_eq!(reply.status, 200, "{query}: {}", reply.body);
    serde_json::from_str(&reply.body).unwrap_or_else(|e| panic!("{query}: {e}: {}", reply.body))
}

/// `GET /v1/recaps/days<query>`, which must answer 200 with a well-formed page.
fn days(daemon: &Daemon, token: &str, query: &str) -> DaysPage {
    let reply = daemon.get(&format!("/v1/recaps/days{query}"), Some(token));
    assert_eq!(reply.status, 200, "{query}: {}", reply.body);
    serde_json::from_str(&reply.body).unwrap_or_else(|e| panic!("{query}: {e}: {}", reply.body))
}

/// Every block `GET /v1/recaps/blocks?limit=<limit><query>` pages back through until
/// `at_start`. A page before the start is never empty.
fn all_blocks(daemon: &Daemon, token: &str, query: &str, limit: usize) -> Vec<RecapBlock> {
    let mut out: Vec<RecapBlock> = Vec::new();
    for _ in 0..1000 {
        let before = out
            .last()
            .map(|b| format!("&before={}", b.block.id.0))
            .unwrap_or_default();
        let page = blocks(daemon, token, &format!("?limit={limit}{query}{before}"));
        assert!(page.blocks.len() <= limit, "{query}");
        assert!(
            page.at_start || !page.blocks.is_empty(),
            "{query}: an empty page before the start"
        );
        out.extend(page.blocks);
        if page.at_start {
            return out;
        }
    }
    panic!("{query}: paging never reached the start");
}

/// Every entry `GET /v1/recaps/days?limit=<limit><query>` pages back through until `at_start`.
/// A page before the start is never empty, and holds at most `limit` dates.
fn all_days(daemon: &Daemon, token: &str, query: &str, limit: usize) -> Vec<DayRecap> {
    let mut out: Vec<DayRecap> = Vec::new();
    for _ in 0..1000 {
        let before = out
            .last()
            .map(|d| format!("&before={}", d.date.0))
            .unwrap_or_default();
        let page = days(daemon, token, &format!("?limit={limit}{query}{before}"));
        let dates: BTreeSet<&str> = page.days.iter().map(|d| d.date.0.as_str()).collect();
        assert!(dates.len() <= limit, "{query}: {dates:?}");
        assert!(
            page.at_start || !page.days.is_empty(),
            "{query}: an empty page before the start"
        );
        out.extend(page.days);
        if page.at_start {
            return out;
        }
    }
    panic!("{query}: paging never reached the start");
}

/// Every span is a non-empty clause of the text, on UTF-8 character boundaries, with at least
/// one receipt; spans come in order and never overlap.
fn well_formed(summary: &Summary) {
    assert!(!summary.spans.is_empty(), "{summary:?}");
    let mut end = 0;
    for span in &summary.spans {
        assert!(
            span.range.start >= end,
            "out of order or overlapping: {summary:?}"
        );
        assert!(span.range.start < span.range.end, "{summary:?}");
        let clause = summary
            .text
            .get(span.range.clone())
            .unwrap_or_else(|| panic!("not a UTF-8 slice of the text: {span:?} in {summary:?}"));
        assert!(!clause.trim().is_empty(), "{summary:?}");
        assert!(!span.receipts.is_empty(), "no receipt: {summary:?}");
        end = span.range.end;
    }
}

/// The blocks' ids as the API writes them (bare ULIDs; an id's `Display` has its prefix).
fn block_ids(blocks: &[RecapBlock]) -> Vec<String> {
    blocks.iter().map(|b| b.block.id.0.to_string()).collect()
}

/// An entry's workstream as the API writes it, `None` for the entry without one.
fn workstream_of(day: &DayRecap) -> Option<String> {
    day.workstream.map(|w| w.0.to_string())
}

/// The ids of a project's workstreams.
fn workstreams(daemon: &Daemon, token: &str, project: &str) -> Vec<String> {
    let reply = daemon.get(&format!("/v1/workstreams?project={project}"), Some(token));
    assert_eq!(reply.status, 200, "{}", reply.body);
    reply
        .json()
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["id"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn with_demo_blocks_are_newest_first_and_page_and_filter() {
    let (_tmp, state) = state_dir();
    let daemon = Daemon::start(&state, &["--demo"]);
    let device = daemon.device_token();

    // Built once at start, off the request path.
    daemon.wait_for_log("built the recap index", WAIT);
    let logs = daemon.stderr();
    let built = logs
        .lines()
        .find(|l| l.contains("built the recap index"))
        .unwrap();
    let rev: u64 = built
        .split_whitespace()
        .find_map(|w| w.strip_prefix("rev="))
        .unwrap()
        .parse()
        .unwrap();
    assert!(rev > 0, "{built}");
    assert!(built.contains(" ms="), "{built}");

    // Every block, newest first, each with a well-formed line.
    let all = blocks(&daemon, &device, "?limit=200");
    assert!(all.at_start, "the demo has fewer than 200 blocks");
    assert!(all.blocks.len() > 1);
    let ids = block_ids(&all.blocks);
    for pair in ids.windows(2) {
        assert!(pair[0] > pair[1], "not newest first: {ids:?}");
    }
    for recap in &all.blocks {
        well_formed(&recap.line);
        // (Not `id <= last`: the seed's events have new ids, before the demo's own older ones.)
        assert!(recap.block.start <= recap.block.end);
    }
    // The default page is the newest 50, or all of them.
    let default = blocks(&daemon, &device, "");
    assert_eq!(
        block_ids(&default.blocks),
        ids[..ids.len().min(50)].to_vec()
    );

    // Paging to the start gives the whole list, at any page size.
    for limit in [1, 3, 4] {
        assert_eq!(block_ids(&all_blocks(&daemon, &device, "", limit)), ids);
    }
    // Before the oldest block, nothing.
    let oldest = all.blocks.last().unwrap().block.id.0;
    assert_eq!(
        blocks(&daemon, &device, &format!("?before={oldest}")),
        BlocksPage {
            blocks: Vec::new(),
            at_start: true
        }
    );

    // Each filter gives exactly the blocks carrying that link, and filters combine.
    let session = "01JB000000000000000SES0002";
    let seed_runs = "01JB000000000000000WST0002";
    let carries = |filter: &str, value: &str, b: &RecapBlock| -> bool {
        let block = &b.block;
        match filter {
            "session" => block.session.is_some_and(|s| s.0.to_string() == value),
            "task" => block.tasks.iter().any(|t| t.0.to_string() == value),
            "workstream" => block.workstream.is_some_and(|w| w.0.to_string() == value),
            "project" => block.project.is_some_and(|p| p.0.to_string() == value),
            _ => unreachable!(),
        }
    };
    for (filter, value) in [
        ("session", session),
        ("session", id::SES1),
        ("task", id::PAP1),
        ("workstream", seed_runs),
        ("workstream", id::SUBMISSION),
        ("project", id::TOOLING),
        ("project", id::PAPER),
    ] {
        let expected: Vec<String> = all
            .blocks
            .iter()
            .filter(|b| carries(filter, value, b))
            .map(|b| b.block.id.0.to_string())
            .collect();
        assert!(!expected.is_empty(), "{filter}={value}");
        let query = format!("&{filter}={value}");
        assert_eq!(
            block_ids(&all_blocks(&daemon, &device, &query, 200)),
            expected,
            "{query}"
        );
        assert_eq!(
            block_ids(&all_blocks(&daemon, &device, &query, 2)),
            expected,
            "{query} by 2"
        );
    }
    let both: Vec<String> = all
        .blocks
        .iter()
        .filter(|b| carries("project", id::PAPER, b) && carries("session", session, b))
        .map(|b| b.block.id.0.to_string())
        .collect();
    assert!(!both.is_empty());
    assert_eq!(
        block_ids(&all_blocks(
            &daemon,
            &device,
            &format!("&project={}&session={session}", id::PAPER),
            200
        )),
        both
    );
    let nothing = BlocksPage {
        blocks: Vec::new(),
        at_start: true,
    };
    assert_eq!(
        blocks(
            &daemon,
            &device,
            &format!("?project={}&session={session}", id::TOOLING)
        ),
        nothing
    );
    // An unknown id is an empty page; a prefixed, lower-case id is the same id.
    assert_eq!(
        blocks(&daemon, &device, "?task=01JB000000000000000TSK0099"),
        nothing
    );
    assert_eq!(
        blocks(
            &daemon,
            &device,
            &format!("?session=ses_{}", session.to_lowercase())
        ),
        blocks(&daemon, &device, &format!("?session={session}"))
    );

    // What the contract calls invalid.
    for query in [
        "?session=nope",
        "?task=PAP-1",
        "?before=1790761920000",
        "?limit=0",
        "?limit=ten",
    ] {
        let reply = daemon.get(&format!("/v1/recaps/blocks{query}"), Some(&device));
        assert_eq!(reply.status, 400, "{query}: {}", reply.body);
        assert_eq!(reply.code(), "invalid", "{query}");
    }
}

#[test]
fn with_demo_days_agree_with_blocks_at_any_tz() {
    let (_tmp, state) = state_dir();
    let daemon = Daemon::start(&state, &["--demo"]);
    let device = daemon.device_token();

    for project in [id::PAPER, id::TOOLING] {
        let query = format!("&project={project}");
        let entries = all_days(&daemon, &device, &query, 30);
        assert!(!entries.is_empty(), "{project}");
        // Newest date first; within a date, the entry without a workstream first, then by id.
        for pair in entries.windows(2) {
            let (a, b) = (&pair[0], &pair[1]);
            assert!(
                a.date.0 > b.date.0 || (a.date == b.date && workstream_of(a) < workstream_of(b)),
                "out of order: {:?} then {:?}",
                (&a.date, a.workstream),
                (&b.date, b.workstream)
            );
        }
        // Each entry covers blocks of its own workstream, and every block of the project is in
        // exactly one entry.
        let project_blocks = all_blocks(&daemon, &device, &query, 200);
        let mut covered = Vec::new();
        for day in &entries {
            well_formed(&day.summary);
            assert!(!day.blocks.is_empty(), "{day:?}");
            for block in &day.blocks {
                let found = project_blocks
                    .iter()
                    .find(|b| b.block.id == *block)
                    .unwrap_or_else(|| panic!("{block} is not a block of {project}"));
                assert_eq!(found.block.workstream, day.workstream, "{block}");
                covered.push(block.0.to_string());
            }
        }
        covered.sort();
        let mut expected = block_ids(&project_blocks);
        expected.sort();
        assert_eq!(covered, expected, "{project}");

        // A workstream's days are its entries among its project's.
        for workstream in workstreams(&daemon, &device, project) {
            let own = all_days(&daemon, &device, &format!("&workstream={workstream}"), 30);
            let among: Vec<DayRecap> = entries
                .iter()
                .filter(|d| workstream_of(d).as_deref() == Some(workstream.as_str()))
                .cloned()
                .collect();
            assert_eq!(own, among, "{workstream}");
        }

        // One date a page, concatenating to the whole.
        let mut paged = Vec::new();
        let mut before = String::new();
        loop {
            let page = days(&daemon, &device, &format!("?limit=1{query}{before}"));
            let dates: BTreeSet<&str> = page.days.iter().map(|d| d.date.0.as_str()).collect();
            assert!(dates.len() == 1 || (page.days.is_empty() && page.at_start));
            paged.extend(page.days.iter().cloned());
            if page.at_start {
                break;
            }
            before = format!("&before={}", page.days.last().unwrap().date.0);
        }
        assert_eq!(paged, entries, "{project} by 1");

        // `tz=0` is the default; any offset puts the same blocks into days, maybe others.
        assert_eq!(
            days(&daemon, &device, &format!("?tz=0&limit=30{query}")),
            days(&daemon, &device, &format!("?limit=30{query}"))
        );
        for tz in [60, -300, 840, -840] {
            let shifted = all_days(&daemon, &device, &format!("&tz={tz}{query}"), 30);
            let mut blocks: Vec<String> = shifted
                .iter()
                .flat_map(|d| d.blocks.iter().map(|b| b.0.to_string()))
                .collect();
            blocks.sort();
            assert_eq!(blocks, expected, "{project} at tz={tz}");
            for day in &shifted {
                well_formed(&day.summary);
            }
        }
    }

    // Nothing for an unknown project; what the contract calls invalid.
    assert_eq!(
        days(&daemon, &device, "?project=prj_01JB000000000000000PRJ0099"),
        DaysPage {
            days: Vec::new(),
            at_start: true
        }
    );
    for query in [
        String::new(),
        "?tz=0".to_owned(),
        format!("?workstream={}&project={}", id::SUBMISSION, id::PAPER),
        format!("?project={}&before=2026-13-01", id::PAPER),
        format!("?project={}&before=2026-9-30", id::PAPER),
        format!("?project={}&limit=0", id::PAPER),
        format!("?project={}&tz=841", id::PAPER),
        format!("?project={}&tz=1.5", id::PAPER),
        format!("?project={}&tz=UTC", id::PAPER),
    ] {
        let reply = daemon.get(&format!("/v1/recaps/days{query}"), Some(&device));
        assert_eq!(reply.status, 400, "{query}: {}", reply.body);
        assert_eq!(reply.code(), "invalid", "{query}");
    }
}

#[test]
fn recaps_need_a_device_token() {
    let (_tmp, state) = state_dir();
    let daemon = Daemon::start(&state, &["--demo"]);
    let agent = daemon.agent_token();
    for path in [
        "/v1/recaps/blocks".to_owned(),
        format!("/v1/recaps/days?project={}", id::PAPER),
    ] {
        let as_agent = daemon.get(&path, Some(&agent));
        assert_eq!(as_agent.status, 403, "{path}: {}", as_agent.body);
        assert_eq!(as_agent.code(), "forbidden", "{path}");
        let anonymous = daemon.get(&path, None);
        assert_eq!(anonymous.status, 401, "{path}: {}", anonymous.body);
        assert_eq!(anonymous.code(), "unauthorized", "{path}");
    }
}

/// The index catches up on every query: the next query after a write shows it, without waiting.
#[test]
fn a_comment_is_in_the_next_blocks_query() {
    let (_tmp, state) = state_dir();
    let daemon = Daemon::start(&state, &["--demo"]);
    let device = daemon.device_token();
    let before = blocks(&daemon, &device, "?limit=200");

    let reply = daemon.post(
        "/v1/tasks/PAP-2/comments",
        Some(&device),
        &json!({ "text": "Starting on the figure.", "mentions": [] }),
    );
    assert_eq!(reply.status, 201, "{}", reply.body);
    let comment: Value = reply.json();
    let comment = comment["id"].as_str().unwrap().to_owned();
    assert!(!block_ids(&before.blocks).contains(&comment));

    // It begins a block of its own: the demo's activity is days older than the engine's gap.
    let after = blocks(&daemon, &device, &format!("?task={}", id::PAP2));
    let begun = after
        .blocks
        .iter()
        .find(|b| b.block.id.0.to_string() == comment)
        .unwrap_or_else(|| panic!("no block begins with {comment}: {after:?}"));
    assert!(begun.block.counts.comments >= 1, "{begun:?}");
    assert!(
        begun
            .block
            .tasks
            .iter()
            .any(|t| t.0.to_string() == id::PAP2),
        "{begun:?}"
    );
    well_formed(&begun.line);
    assert!(block_ids(&blocks(&daemon, &device, "?limit=200").blocks).contains(&comment));

    // And today's entry of the task's workstream covers it.
    let today = days(&daemon, &device, &format!("?workstream={}", id::SUBMISSION));
    let newest = today.days.first().unwrap();
    assert!(
        newest.blocks.iter().any(|b| b.0.to_string() == comment),
        "{newest:?}"
    );
}
