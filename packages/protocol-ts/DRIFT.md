# UI protocol drift

Compared against `apps/ui/src/data/types.ts` and the request/response use in `data/api.ts` at
`8d3e012`. A temporary TypeScript equality probe compared all 84 same-named types; seven differed,
traced to the two direct field differences below. The probe was removed after the check; it is not
a requirement that the UI adopt every protocol type in this PR.

| Type / field | Rust JSON / generated type | UI declaration | Contract agrees with |
| --- | --- | --- | --- |
| `Machine.info` | `info?: MachineInfo`, with hostname, OS, arch, tmux, optional scheduler and network-home fact | Field omitted from `Machine` | Rust: API v1's model comes from `protocol`; machine facts are also explicit in host info. UI has intentionally kept fewer fields. |
| `TranscriptItem` / `tool_use.input` | Optional JSON value (objects, arrays, primitives and nested null), no top-level null because `None` is skipped | `input?: unknown`, accepting functions, symbols and other non-JSON values | Rust: the transcript endpoint carries JSON; UI's type is wider. |
| `EventBody` / `machine_added.data.machine` | Full `Machine` including optional info | Reduced UI `Machine` | Rust, inherited difference above |
| `Event.body` | Full event variants | Contains the reduced `Machine` in `machine_added` | Rust, inherited difference above |
| `EventsPage.events` | `Event[]` with full machine facts | UI `Event[]` | Rust, inherited difference above |
| `StreamFrame` / `events.events` | Full `Event[]` | UI `Event[]` | Rust, inherited difference above |
| `TranscriptPage.items` | Strict JSON-valued tool input | UI transcript with `unknown` input | Rust, inherited difference above |

The other 77 same-named types match structurally, including all event discriminants, task patch
omission versus clearing with null, receipt tags, optional brief proposal, session parent,
recap fact discriminants and UTF-8 span ranges. No UI declaration or wire format changed.

**Board drafts** (2026-10, brief 0-draft-board): `EventBody` gained `board_draft_started`,
`board_proposed` and `board_draft_reviewed`, and `BoardDraft`, `DraftPreview`, `DraftReviewed` and
their parts are new. The UI's `data/types.ts` does not list them (stream L's): the data layer
treats an event type it does not know as newer than itself and refetches everything, and the
projects views declare the board types they use in `apps/ui/src/projects/board-drafts.ts`, the same
shapes as the generated ones.

## Different names and missing declarations

| Rust type | UI counterpart | Contract position |
| --- | --- | --- |
| `Date` | `CalendarDate` | Both string; same wire representation. Renaming is required on adoption to avoid the global JS `Date`. |
| `SetupDone` | `SetupResult` | Same fields / shapes; API v1 calls the response `SetupDone`. |
| `ApiError` | `ApiErrorBody` | Same `code` and `message`; API v1's common error body. |
| `ProjectKey` | Inline `string` in `Project.key` / `NewProject.key` | Same wire representation, Rust validates the documented key syntax. |
| `MachineInfo` | No standalone UI type | Described by host info and the Rust machine model. |
| `HostInfo`, `HostRole`, `Capability` | No data-layer declaration | API v1's unauthenticated `/v1/host/info` is not currently exposed by `createApi`. |
| `Caller`, `TokenScope` | No UI type | Authentication context belongs to the server; not an API response today. |
| `CommandId`, `RunnerToHub`, `HubToRunner`, `RunnerCommand`, `CommandOutcome` | No UI type | Runner protocol, not a direct desktop API copy; exported for completeness. |
| `TimestampMs` | `TimestampMs`; timestamps elsewhere use `number` | Same JSON number; no bigint on the wire. |

UI-only `WorkspaceInfo`, `NewComment`, `DispatchRequest`, `AskAnswer`, `BriefEdit`, filter/query
types and `RecapDayScope` describe API shapes which have no public serialized definition in
`protocol` today. Their fields agree with API v1; this exporter cannot replace them yet.
`TranscriptKind`, `TranscriptItemOf`, `EventType`, `ActivityPage`, `Ulid` and the kind/check
constants are UI helpers or aliases. Gateway/remote/transport types describe the desktop bridge,
not this protocol crate, so they remain separate. No contract change is required.
