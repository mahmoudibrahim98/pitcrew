# pitcrew-runner

Runner service: watchers, session linking, derived events to the hub, file API.

## Workstream files

`files::Files::new(state)` provides blocking list/read/write operations for a root resolved by the
hub. Validate relative paths before I/O, retain checked ancestor handles, refuse every link or
Windows reparse point, and compare opened identities. Unix opens and replacement use directory
handles; Windows holds ancestors without delete sharing. Lists keep the first 5,000 UTF-8 names
in byte order and report truncation. Files are capped at 8 MiB, with SHA-256 revisions and UTF-8
or canonical base64 content.

Writes are serialized, reject `.git` and multi-link targets, recheck revisions, and replace an
exclusive temporary file in the same folder, preserving permissions (Unix group and mode,
Windows DACL included). A Unix group change must succeed before the original mode is restored;
otherwise replacement is refused.
New files are private. Before replacement, `file-backups` in state retains the newest three
backups per root/path and 64 MiB total, evicting oldest first by a strictly increasing stored identifier;
each is at most 8 MiB. Hash keys
normalize Windows casing. Unix storage is current-user-owned 0700/0600. Windows creates and
checks protected owner-only DACLs through native Windows security calls in pitcrew-trust;
ACL failures refuse the write. Existing public, linked, malformed or hard-linked backup storage fails closed.

Same-account concurrent mutation is not a separate security principal: Unix cannot prevent a
privileged owner moving an already-open directory after the final check, and Windows must close
the target handle before the atomic rename. Handles prevent link redirection; identity and
revision checks detect swaps observed before replacement, but do not provide a filesystem-wide
transaction against another writer. No path, content or OS error is logged or returned in errors.

**Owned by stream D** — see [docs/build/streams/D.md](../../docs/build/streams/D.md).

The crate docs (`src/lib.rs`) describe the watcher and the in-process hub link: the store sink,
hooks, terminals, commands and linking. This page records the rules a host must know when it
wires the runner into the daemon.

## Who may change a session through a hook

`RunnerHooks` (the API's `HookSink`) turns agent hooks into session state. A hook applies only if
its sender may change the session, judged by the session's agent as the host's `SessionAgents`
reports it:

- **Agent token** (`scope` agent): only a session whose agent is the token's member. Its
  `on_behalf_of` (the owner) widens nothing: a sibling agent of the same owner is refused.
- **Device token** (a person): only a session with no agent, or whose agent that person owns.
  An agent the hub records without an owner is changed by its own hooks only, never a person's.
- **Anything else is refused**: dropped and logged at debug with the reason; never applied and
  never held. A session whose agent is `Unknown` is refused for everyone. Without
  `RunnerConfig::with_agents` every agent is unknown, so every hook is refused.
- **Held hooks**: a hook for a session the runner has not indexed yet (Claude writes its
  transcript only at the first prompt) is held with its sender, and decided when the session is
  discovered. One refused then is dropped; the allowed ones are folded into the session's first
  state, oldest first.
- **Sub-agents run as their parent.** When `SessionAgents` answers `NoAgent` for a session whose
  transcript names a parent (a Claude sub-agent in `<session>/subagents/`), the runner asks for
  the parent's agent and judges by that. The hub has not stored a sub-agent yet when its held
  hooks are decided (its `session_discovered` is on its way to the store), nor until the sink's
  write lands, so only its parent's agent is known then. It fails closed:
  - the sub-agent's own answer, when it is an agent or `Unknown`, stands: the parent never
    overrides it;
  - the parent's answer is taken as it is: its `Unknown` (or a lookup that panics) refuses
    everyone, a person included;
  - the parent's transcript `<session>.jsonl` counts only if it is a regular file (a link is not
    followed) inside the sub-agent's own home; otherwise the sub-agent has no parent, and is
    judged by its own answer alone. So a sub-agent can never be its own parent, nor two each
    other's;
  - if the parent cannot be looked up (an index or I/O error), the sub-agent is `Unknown` and
    every hook is refused; the lookup is tried again at the next hook.

  The parent found at discovery, the one its `session_discovered` names, is kept with the
  sub-agent's row, so a restart judges by the same one. (A sub-agent indexed before this was kept
  gets it at its first hook.)
- **Codex and OpenCode sub-agents have no parent yet**: their transcripts do not name one the
  runner reads. They are judged by their own answer alone, which today is `NoAgent`: any person's
  hook can change them, and no agent's.
- **Which session a hook names.** A hook names the CLI's own id; a sub-agent's is whatever its
  transcript says (`agentId`). Sessions and sub-agents are looked up apart, sessions first: a
  sub-agent named like a session never takes that session's hooks, whichever is found first.
  Of two sub-agents with one id, the one indexed first keeps it (by its session id, so after a
  restart too); the other is refused it, with a warning. Among sessions, the last transcript
  found takes an id, as before. A resumed session is never matched to a sub-agent either.
- Codex's `notify` follows the same rule.

The sender travels from `HookSink::deliver` to where the session is resolved as an explicit
`Origin::Hook(sender)`. The runner's own reports (a command that ended a session) are
`Origin::Runner`, so skipping the check is a visible decision in the code.

### Floods

- A hook whose session id is not a plain id (at most 128 bytes of letters, digits, `.`, `_` and
  `-`, starting with a letter or digit) is dropped; a status line is cut to its first line and
  120 characters. Each held or queued hook is therefore small.
- Each sender (member) holds at most 32 hooks; at its quota it loses its own oldest.
- Past 1024 held hooks in all, the sender holding the most loses its oldest (ties: the highest
  member id).
- Held hooks expire after 10 minutes.
- A new held hook looks for its transcript at once, at most once every 5 seconds per sender, and
  only in local homes: network homes keep their own, rarer schedule.
- Reported states waiting for the watcher (at most 1024) are dropped the same way: the sender
  with the most waiting loses its oldest.

So a flood from one token costs that token, not another sender's held `SessionEnd`.

### `SessionAgents`

The host implements it over the hub's sessions and members. `MemoryAgents` is for tests, or for
a host that fills it as the hub's sessions change; a session it was not told about has no agent.
Such a host must fill it synchronously with the hub's writes: a dispatch's session must be in it
before the dispatch's CLI starts.

- `Agent { agent, owner }` for a session stored with an agent (`owner` is `None` if the hub has
  none for it); `NoAgent` for one stored without, or not stored at all; `Unknown` whenever in
  doubt (a failed lookup, an agent whose member cannot be read).
- A sub-agent session (one with a `parent`) runs as its parent: answer the parent's agent. The
  runner states sub-agent sessions without an agent of their own; as it adopts the ids the hub
  names (below), a dispatched agent's sub-agents must resolve to that agent, or its hooks from
  them are refused and any person's apply. For a sub-agent the hub has not stored, `NoAgent` is
  the right answer: the runner then asks about the parent itself (above).
- It must see the hub's **latest** session writes. A stale cache answering `NoAgent` for a
  session that has since gained an agent would let any person's hook change it.
- It must **not call back into the runner** (its handle, hooks, terminals or commands): the
  runner asks from its watcher thread. It should answer quickly; a panic counts as `Unknown`.

## Session ids, and sessions the hub named

- The runner mints its own `SessionId` for every transcript it discovers, and its
  `session_discovered` names **no agent**.
- The hub's sessions projection keeps the agent a session already has when it is stated again
  without one (`agent = COALESCE(excluded.agent, agent)` in `hub-work`'s `work.sessions`), and a
  firm link (`dispatch`, `manual`, `claimed`).
- **A session the hub named.** A dispatch (and `POST /v1/sessions` with `agent` or `task`) stores
  its session first, agent named, state `starting`, then sends `StartSession` with `session` set
  to its id. The runner then:
  - records the terminal under that session at once: text, keys, interrupts and ends reach it
    before its transcript exists, and `RunnerTerminals::session_of(terminal)` answers it;
  - gives the CLI the environment the host's `SessionEnv` returns for the session
    (`RunnerConfig::with_session_env`). The daemon's gives a session run as an agent
    `PITCREW_TOKEN_FILE`, the path of a private file holding an **agent** token bound to that
    agent and its owner, and where the hub listens, with `PITCREW_TOKEN` and the endpoint
    variables it does not listen on set empty so that nothing the CLI inherits wins over them.
    Values are paths and addresses, never secrets (a runtime keeps a program's variables). An
    error refuses the start;
  - has the CLI's transcript **adopt** the id instead of minting one, before anything names the
    session:
    - **Claude**: by exact CLI id. The runner chose the `--session-id`, so the transcript's name
      (`<id>.jsonl`) is known when it is found, and its row is made under the named session
      (`add()`); also when a sub-agent of it is found first and its parent's row is made then
      (`parent_of()`);
    - **Codex and OpenCode**: by folder and start time, at the first read, when the claim of a
      waiting terminal (`Store::claim_terminal`) finds one started for a named session: the row,
      saved under the id it was found with, moves to the named one in the same transaction, and
      the watcher's maps follow (`Watcher::adopt`). Nothing has named the first id yet;
    - **a start still under way**: a CLI may write its transcript before the runner has recorded
      its terminal (the runtime's `start` has not returned). The start is known to the watcher
      meanwhile (`Shared::starting`, a `Pending` start), so the transcript still takes the
      name, by the same rules; it is then reported without its terminal, which is linked to the
      session when it is recorded;
  - refuses a second start in a folder where a CLI matched by folder (Codex, OpenCode) still
    waits for its transcript (inside the 15-minute claim window, its program running), when
    either start is for a named session: the two could not be told apart. Two starts for no
    named session are left to the folder match, as before;
  - refuses to start a session it knows already (a transcript or a terminal under that id).
- **A terminal whose program ended** may already have written its transcript. Before a start in
  its folder retires it, reconciliation calls it gone, or an end command reports it ended, the
  watcher scans and matches transcripts already present (`Shared::scan_exit`, without index or
  runtime locks held). After that scan, an unmatched terminal accepts no later folder match;
  migration `0003_folder_claims.sql` keeps that closed claim across runner restarts.
  a Codex or OpenCode start in its folder retires it (forgets its row). A failed or timed-out
  scan leaves it eligible and refuses retirement until discovery succeeds for its engine's
  homes and any unread transcript that could belong to it. Unrelated engines and transcripts
  with another exact CLI id or a nonmatching folder/start time do not hold its scan open. A Claude terminal
  is matched by its exact
  id, so it keeps its row until the host retires it (`RunnerCommands::retire`, once the hub gave
  up on the session).
- Re-statements keep naming no agent, which the hub reads as "keep the one you have": the hub
  knows the agent before the first hook, and the agent's own hooks change its session.
- **Sub-agents** keep runner-minted ids, with `parent` set to the named session.
  `SessionAgents` must resolve them to their parent's agent (see above).
- `RunnerCommands::started(session)` tells the host where a named start stands: `Reported` (its
  transcript is indexed under it and its terminal runs), `Exited` (its transcript is indexed but
  its terminal exited or disappeared), `Running` (its start is under way, from the moment its
  command
  runs until it returns, `Shared::under_way`; or its terminal's program runs and the transcript
  is not found yet), `TooLate` (matched by folder, its program runs, and the 15-minute claim
  window is over: no transcript can be taken for it any more), `Gone` (no terminal and no
  transcript here, or its program ended first) or `Unknown`. The daemon reconciles the sessions
  it stored ahead of the runner with it, and calls `RunnerCommands::retire` for each one it
  abandons: its terminal is forgotten once its program has ended. Imported transcripts without
  a runner-started terminal remain `Reported`; a missing local index is never evidence that an
  imported CLI exited.
  Startup refresh keeps the terminal row of a runner-started CLI with a session even when the
  runtime no longer lists its id. That row preserves provenance: an indexed dispatched session
  still answers `Exited` if its CLI disappeared while the daemon was down. Hand-linked terminals
  and unclaimed starts retain the usual refresh behavior; no missing terminal is followed by its
  old target. Imported transcripts never acquire this exit evidence.

## File discovery and cursors

The daemon opts its Claude and Codex homes into `RunnerConfig::cache_file_discovery`. For these
file-layout adapters, periodic discovery first checks a bounded snapshot of directory identities
and modification times, avoiding a walk and sort of unchanged transcript names. Explicit rescans,
new-file notifications, lost events, failed watches and polling homes always run the adapter.
On Unix, OpenCode can also skip periodic discovery of unchanged quiet databases: the snapshot
checks each database's identity, size, mtime and ctime. Any SQLite side file disables that cache,
so an active WAL or rollback journal always uses the adapter. Other platforms keep OpenCode's
original discovery schedule. Custom adapters keep the default (`false`). The slow transcript
sweep keeps its schedule and still detects missed writes, replacements and deletions even when directory names do not change.

The daemon also opts these adapters into `RunnerConfig::byte_file_cursors`: when a read's byte
cursor reaches the file size just checked, the runner can save it as caught up immediately. It
avoids a second read and identical cursor write just to establish EOF. Partial lines keep their
cursor before EOF and still resume normally. OpenCode and custom adapters keep the old loop;
the flag defaults to `false` and is for the concrete Claude/Codex JSONL adapters only. Layout
snapshots hold at most 4,096 directories; a link, unreadable folder or larger tree disables the
cache. Unix directory ctime is checked too, so restoring mtime cannot conceal a name change.
File-system clocks are coarse (a clock tick on NTFS, up to two seconds on FAT and some network
volumes), so a folder or database modified in the last two seconds is never cached: a second
change in the same tick would keep its stamp.

With a nonzero `RunnerConfig::notification_window`, the first notification after a quiet period
is due at the existing 100 ms debounce, without rounding. A notification arriving while another
read or delivery is due or running can be read immediately. Earlier pending notifications keep
their own deadlines. The busy window ends when that work completes, including overlapping reads
and deliveries; there is no timer or deadline grid. The duration's magnitude is retained for
caller compatibility but no longer sets a grid (the daemon still passes 175 ms). Zero keeps
ordinary per-file debounce. Polling and hook reports retain their schedules.
Adjacent completed reads of different transcripts can share one sink acceptance and one cursor
transaction when already queued, without waiting for another batch, preserving order and limiting
a group to four batches and `max_batch_events` events (an indivisible source
item retains its existing exception). Cursors move only after the whole group is accepted; failed
acceptance retries the same events, and a failed transaction pins every affected row for replay.

## Memory

The watcher tracks every transcript in its index, so what it keeps of one is paid 10,000 times
over in a heavy history. It keeps only what tells a change and what routes hooks to the session
(`Tracked` in `src/watch.rs`): the session id, the engine, the path (one `Arc<Path>`, shared by
its maps, its folder watches and the transcript pages), the inner id, size, mtime and file
identity as last read, `caught_up` and `discovered`, the CLI's id and whether it is a sub-agent,
and the parent its hooks are judged by. The tracked transcripts are a vector sorted by id (ids
only grow), not a `BTreeMap`, whose nodes a run of growing ids leaves half full.

The rest of a cold row (the cursor with the adapter's state, the session's metadata and facts, the
accepted items of a replay) is read from the index (`Store::load`) when the transcript changes, a
hook reports, the sessions are linked again or the transcript is deleted, and let go once the
sink thread has saved every batch that carries it (counted per row). Up to 64 saved hot rows stay
in a cache, avoiding an index lookup on every live write. An in-use row leaves that cache before
it changes; its unsaved batches do not count toward the cache limit and cannot be evicted. Cursor
snapshots are immutable and shared with the sink, while their saved JSON is unchanged.
A row stays in memory while it holds a change no batch carries yet (a report that moved when the state was last reported but
not the state, a re-index before a read that failed), while a replay after a crash is under way,
or after a save that failed (it is then ahead of the index, as before). At start, each row is read
whole and only that much of it is kept.

Measured with `heaptrack` on a restart over P-measure's 10,000 transcripts (`benches/README.md`,
"At scale"): about **0.87 KiB a transcript** (the maps' hash tables 409 bytes, the tracked entry
243, the path 128, the CLI's id 32, folder watches 32, the identity 18), against about 4 KiB when
every row was held whole.

## Transcript pages

`RunnerHandle::transcripts()` hands out a `RunnerTranscripts` (cheap to clone, `Send + Sync`);
`RunnerHandle::transcript_page` is the same call on the handle. `transcript_page(session, before,
limit)` serves api-v1's "Transcript paging":

- **Which transcript.** Only transcripts the watcher tracks: the session is found by the runner's
  own `SessionId` among them, never a file it stopped watching (deleted). The page is read with
  the adapter's own `read_page`, so the adapter's safe open applies.
- **The page.** Tail-first: `before: None` is the newest page; a page's `from` as `before` is the
  one before it. Pages hold whole records (a page may exceed `limit`), a partial last line is left
  out, and `at_start` says nothing older exists. `limit: None` is 200 (`DEFAULT_PAGE_LIMIT`), more
  than 1000 counts as 1000 (`MAX_PAGE_LIMIT`), and `0` as `1`.
- **Errors.** `PageError::UnknownSession`: the runner's index has never had the session.
  `PageError::Unavailable { reason }`: it has, but the transcript is deleted, no longer watched,
  or cannot be read (an I/O error, an adapter that fails or panics), or the read did not end in
  time, or too many are under way. `reason` names no path.
- **Bounded.** Reads run on the runner's own small pool (`PageOptions`: 2 threads, 4 waiting,
  10 seconds by default) and the call waits at most the timeout; a read that does not return is
  left on its pool thread. With every thread busy and the queue full, a call is refused at once
  (`Unavailable`, "busy"). So a file system that stops answering, and a UI that keeps polling,
  tie up these threads only, never the caller's (nor tokio's blocking pool, which the hook
  intake uses too). `transcripts()` clones share the pool the runner starts;
  `transcripts_with(options)` gets one of its own.
- **Logging.** A failed page is logged as a warning the first time for a session, then at debug.
- **Blocking**, for at most the timeout: call it from `spawn_blocking`, not on an async thread.
- Like the hooks, it keeps reading after the runner stops, and keeps its index open.

### For stream 0: replacing the daemon's stand-in

`crates/daemon/src/transcripts.rs` finds a transcript by matching the native id against the file
names each CLI uses, among what its `Recorded` adapters saw at discovery. The runner now does it by
session id, from its index. To switch:

1. In `daemon/src/runner.rs`, hand `pitcrew_runner::start` the plain adapters (no `Recorded`, no
   `Found`), and keep `handle.transcripts()` in `Runner` next to `hooks` and `terminals`, with an
   accessor.
2. Keep the route's own checks: the query (`400` for a `before` or `limit` that is not a whole
   number, and for `limit=0`), the hub's lookup (`404` for a session the hub does not have), and
   the machine (`503` for another machine, or without a runner). Call the runner from
   `spawn_blocking`. The route's own 10-second timeout may stay, but the call is bounded now
   (by `PageOptions::timeout`), so a hung file system no longer leaves a tokio blocking thread
   behind per request: it ties up at most the runner's page threads, and further requests are
   refused at once.
3. Replace `Found::find` and `adapter.read_page` with
   `transcripts.transcript_page(session, before, Some(limit))` (or pass the query's `Option` and
   let the runner apply the default and the cap), and map:
   - `Ok(page)` → `200`;
   - `PageError::UnknownSession` → `404`;
   - `PageError::Unavailable` → `503 unavailable` (busy and timed-out reads too; their `reason`
     says which, and a `Retry-After` would suit them).
4. Delete `Found`, `Recorded`, `names` and their tests, and the route's own warning on a failed
   read: the runner logs a failed page as a warning once per session, then at debug.

Three answers change. A deleted transcript was an empty page and is now `503`; a read error was
`500` and is now `503`; and a session the hub has on this machine that the runner never indexed
(a demo session, or a dispatched session under the dispatch's id until the runner adopts it) was
an empty page and is now `404`. If the UI should keep showing such sessions with an empty transcript,
as the mock hub does, the daemon can answer an empty page for `UnknownSession` instead; that is
its call.

## Proposal: who caused a hook's state change

Today every event the runner emits is authored by its configured owner with no `on_behalf_of`, and
`StoreSink` stamps such events with that owner. A `session_state_changed` (and `session_ended`)
caused by an agent's hook, say the writer agent's `Stop`, is therefore authored as the session's
person, not as the agent whose hook caused it. The runner knows the sender where it applies the
hook (`Origin::Hook(sender)`), so either option below is small on the runner's side. No code yet.

**Option A: a `StoreSink` rule.** The runner hands the sink, with each batch, the sender of the hook
that caused it; `StoreSink` authors those events as that caller (`author` the member,
`on_behalf_of` its owner for an agent token), and everything else as the owner, as today.

- For: no protocol change; stream D alone; the hub's rule "author comes from the token" still
  holds in the solo case, since the hook's token was checked by the hub's own hook intake.
- Against: `EventSink::accept` takes only events, so the sender needs a new parameter or a side
  channel. It works in process only: a remote runner sends its batches with its own token, and the
  hub stamps that, so `author` would mean the hook's sender in the solo case and the runner on a
  cluster. It also makes authorship depend on a race: a hook that beats the transcript authors the
  change as its agent, while the same change read from the transcript a moment earlier is the
  person's. And held hooks folded into one `session_discovered` can have several senders.

**Option B: a protocol field.** `author` keeps meaning who reported the event (the hub stamps it, as
now), and an optional field says what caused a state change, e.g. on `session_state_changed` and
`session_ended`: `"cause": {"kind": "hook", "receipt": "<HookReceiptId>"}`, or `"transcript"`, or
`"runner"` (a command it ran). The runner fills it from `Origin`; a missing field reads as unknown.

- **Informational only.** `cause` is for people reading the history (recaps, the audit trail).
  It is never taken as `author` or `on_behalf_of`, never used to authorize anything, never moves
  `task_moved`'s attribution (its mover), and no office rule reads it. Only the token-stamped
  `author` carries authority.
- **A receipt, not a member.** It names the hook by a receipt id the hub issued when its hook
  intake accepted the delivery, not by a raw member id. The hub knows which token each receipt
  came with, so a runner cannot claim a member it never received a hook from: a receipt the hub
  did not issue, or issued for another session, is shown as unverified. The runner gets the
  receipt with the hook (`HookEvent` would carry it) and keeps it with a held hook.
- **Folded held hooks.** Held hooks from several senders fold into one `session_discovered`.
  It names every receipt it folded in, oldest first, and the last one that changed the state is
  the one that set the session's first state; refused held hooks are not named.
- For: one meaning everywhere, in process and remote; authority stays with the hub's stamping,
  and the cause is information the recap and audit views can show ("the writer's Stop hook");
  older events and clients are unaffected (an optional field). It also records the transcript and
  command cases, which A cannot tell apart from each other.
- Against: a contract change (stream 0: protocol, `api-v1.md`, the hook intake's receipts, the
  mock hub and UI types). A remote runner's `cause` is still its claim about which receipt caused
  a change; the receipt bounds the claim to hooks the hub really received.

**Recommendation: B.** Authorship should keep one meaning, stamped by the hub from the token that
delivered the events, and the cause is better recorded as what it is. A also cannot cover the
remote runner, which is where hooks from many agents on a cluster will come from. Until B lands,
hook-caused state stays authored as the session's person.

## Active state leases

Working, Waiting and Starting expire to Idle after five minutes without a transcript write or
accepted hook, unless the runtime lists the session's terminal as alive. Five minutes tolerates
long gaps in streamed output; known terminals keep long silent commands active. Existing safety
sweeps apply expiry (30 seconds locally, 120 seconds on network homes), and first discovery
normalizes old transcripts before publishing them. Ended/Unreachable are preserved. Expiry
clears a stale status line but does not advance reported_at, so a later transcript turn works
normally. No transcript is rewritten. Inactive rows stay unloaded during subsequent checks;
terminal liveness is a bounded list call cached for at most 30 seconds. The daemon supplies
`RunnerConfig::with_runtime` before the watcher starts, so an old transcript with a live terminal
is not expired during the startup gap before command routes attach.

`RunnerCommands::session_options` detects executable CLIs on PATH without executing them and
reports launch-supported modes, including bypass only where enabled. `set_title` stores a
person's title in the separate session_titles table (migration 0004); adoption and restart apply
it over the transcript's title.
