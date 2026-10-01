# pitcrew-runner

Runner service: watchers, session linking, derived events to the hub, file API.

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
  runner states sub-agent sessions without an agent of their own; once it adopts dispatch ids
  (below), a dispatched agent's sub-agents must resolve to that agent, or its hooks from them are
  refused and any person's apply.
- It must see the hub's **latest** session writes. A stale cache answering `NoAgent` for a
  session that has since gained an agent would let any person's hook change it.
- It must **not call back into the runner** (its handle, hooks, terminals or commands): the
  runner asks from its watcher thread. It should answer quickly; a panic counts as `Unknown`.

## Session ids today, and dispatch

- The runner mints its own `SessionId` for every transcript it discovers, and its
  `session_discovered` names **no agent**.
- The hub's sessions projection keeps the agent a session already has when it is stated again
  without one (`agent = COALESCE(excluded.agent, agent)` in `hub-work`'s `work.sessions`).
- A session the hub has not stored therefore has no agent: an agent comes only from a dispatch,
  which stores its session, agent named, before its CLI starts.
- **Consequence today:** a dispatched session has two ids, the dispatch's (with the agent) and
  the runner's (without). Hooks resolve to the runner's, so the dispatched agent's own hooks are
  refused, and any person's apply as for a session without an agent.
- **When the runner implements the hub's `Dispatcher`**, it must report the session under the
  dispatch's `DispatchRequest::session` and not discover it again under another id. The design
  allows it: `RunnerCommands` already starts Claude with a native id it chose (`--session-id`)
  and records the terminal under it, so that record can carry the dispatch's `SessionId`, and
  discovery can take the id from it instead of minting one. Its re-statements must keep naming no
  agent, which the hub reads as "keep the one you have". The rule then holds as intended: the hub
  knows the agent before the first hook.
- **Sub-agents** keep runner-minted ids even then (the dispatch names only the main session),
  with `parent` set to it. `SessionAgents` must resolve them to their parent's agent (see above).
