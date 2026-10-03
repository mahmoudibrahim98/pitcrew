# Contracts

Everything a stream may rely on from another stream is written down here. If it is not here, it
is an internal detail and may change without notice.

## The contracts

| Contract | Where | Used by |
|---|---|---|
| Domain model, ids, keys, move rules | `crates/protocol/src/{ids,model}.rs` | everyone |
| Event log (`Event`, `EventBody`) | `crates/protocol/src/events.rs` | C, D, E, F, G, H, UI |
| Runner ↔ hub protocol (JSON lines) | `crates/protocol/src/runner.rs` | D, H, J |
| API frames, `HostInfo`, `Caller`, errors, token scopes | `crates/protocol/src/api.rs` | H, I, K, every crate with `routes()` |
| Transcript items and pages | `crates/protocol/src/transcript.rs` | A, D, H, M |
| HTTP and WebSocket API | [contracts/api-v1.md](contracts/api-v1.md) | H, I, K, L, M, N, O, mock hub |
| `Runtime` trait (terminals) | `crates/interfaces/src/runtime.rs` | B implements, D uses |
| `SourceAdapter` trait (transcripts) | `crates/interfaces/src/source.rs` | A implements, D and O use |
| Fakes of both traits | `crates/interfaces/src/fake.rs` | D and anyone testing |
| Store schema v0 (event log) | `crates/store/migrations/0001_init.sql` | C, E, F, G, O |
| Demo workspace and sample transcripts | `crates/fixtures/data/` | everyone |
| Design tokens | `packages/tokens/` | L, M, N, O |

`crates/protocol` has contract tests (`tests/contract.rs`) for every wire shape. If one fails,
you changed the protocol.

## Patterns every stream follows

- **Routes.** A crate that serves HTTP exposes `pub fn routes<S: Clone + Send + Sync + 'static>()
  -> axum::Router<S>`. It reads the caller from `axum::Extension<pitcrew_protocol::api::Caller>`
  (inserted by the API layer after authentication) and its own service from an `Extension`
  added by the daemon. Errors are `ApiError` bodies with `ErrorCode::http_status()`. Crates never
  parse tokens.
- **Events.** Every state change appends an `Event` with `author = caller.member` and
  `on_behalf_of = caller.on_behalf_of`. Never trust a body's claim about who is acting.
- **Migrations.** Files are `crates/store/migrations/NNNN_<name>.sql` in your stream's range;
  tables are `STRICT`; a merged migration is never edited.
- **Time and ids.** UTC milliseconds (`TimestampMs`); ULIDs created where the thing is created.
- **No private data** anywhere: fixtures are synthetic; real samples go in ignored `private/`
  folders.

## Changing a contract

1. Open a pull request from `s/0/contract-<topic>` (or ask the integrator in your report). Say
   who is affected.
2. **Adding** an optional field, an enum variant on a `#[non_exhaustive]` enum, or a route is
   compatible. **Removing or renaming** anything, or changing a meaning, is breaking: bump
   `PROTOCOL_VERSION` in `crates/protocol/src/version.rs` and update `PROTOCOL_MIN` if old peers
   can no longer talk to new ones.
3. Update the contract tests, the mock hub, the fixtures and this folder in the same pull request.
4. The integrator merges it and tells the affected streams, which rebase.

## Planned contract additions

These are known gaps. The stream named proposes the contract; until then, work against a local
stand-in inside your own paths.

| Gap | Proposed by | Notes |
|---|---|---|
| TypeScript types generated from `crates/protocol` into `packages/protocol-ts` | H | e.g. `ts-rs` behind a `ts` feature in protocol; UI streams hand-type what they use until then |
| Machine scan API: **in API v1** ("Machine scan"), types in `crates/protocol/src/scan.rs` | A + O | `POST /v1/machines/{id}/scan` for a hub's own machine: progress and the report as newline-delimited JSON in its own answer, not over the stream. Scanning a remote machine is still to come |
| Machines and helper setup API (add machine, check, install helper, launchers) | J + K | The onboarding wizard's backend |
| Files API (tree, read, write within project locations) | D | Allowed roots, size caps, canonicalisation |
| Agent accounts and usage (`claude`/`codex` logins per machine, usage status) | D + M | Status bar and account chips |
| Integrations API (GitHub, Jira setup; approval queue) | G | Approvals are asks of kind `approval` |
| Rooms and messages (team channels, `@mentions`) | E | Comments exist; rooms come later |
