# Stream H · API and auth

**Goal:** the daemon's API: axum over a unix socket or named pipe, tokens and scopes, the
delta stream, terminal streaming, and composition of every crate's routes. Plus TypeScript
types for the UI.

**Owns:** `crates/api/**`, `crates/auth/**`.  **Depends on:** stream 0.  **Model:** Opus-class.
**Read first:** ADR-0006, ADR-0009; `docs/build/contracts/api-v1.md`;
`crates/protocol/src/api.rs`.

## Work packages

1. **Listener:** axum served on a unix socket in a 0700 directory (Windows: a named pipe with a
   per-user ACL); peer credential check on every connection; no TCP unless explicitly enabled for
   development on loopback.
2. **Tokens** (`crates/auth`): device and agent tokens, 256-bit random, stored hashed; minting,
   rotation, revocation; agent tokens bound to an agent member and its owner. Redaction in logs.
3. **Auth middleware:** `Authorization: Bearer` for HTTP; `pitcrew.bearer.<token>` subprotocol for
   WebSockets. Inserts `Extension<Caller>`. Route-level scope checks for agent-allowed routes.
   Audit log for person-only actions.
4. **Composition:** merge `routes()` from each crate; `GET /v1/host/info` (unauthenticated).
   Until E, F and D land, serve their routes from the fixtures so the desktop can connect.
5. **Delta stream:** `GET /v1/stream?since=` with hello, replay of missed revisions, 50–100 ms
   batching, pings; bounded per-client buffers (a slow client is dropped, then resumes by `since`).
6. **Terminal WebSocket:** bridge to the runtime's offset-addressed output and input.
7. **TypeScript types:** propose and implement the export (e.g. `ts-rs` behind a `ts` feature in
   the protocol crate, via a contract PR) into `packages/protocol-ts`, with a CI check that the
   generated files are current.
8. **Conformance tests:** a black-box suite for API v1 that can run against both the mock hub
   and the real daemon.

## Acceptance

- Tokens never accepted from query strings; a test proves it.
- Agent token on a device-only route → 403; unknown token → 401; body `author` ignored.
- Stream: reconnect with `since` returns exactly the missed events (property test).
- A second user on the same machine cannot connect (peer-credential test on Linux).
- Verb round trip over the socket ≤ 50 ms p99 locally.

## Do not

Implement domain logic here (E, F, G, D own it); expose TCP by default.
