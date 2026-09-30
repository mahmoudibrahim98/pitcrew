# Brief H · Listener, tokens and auth

- **Stream:** H · API and auth. **Branch:** `s/H/listener-and-tokens`. **Paths:**
  `crates/api/**`, `crates/auth/**` only.
- **First read:** [README.md](README.md), then `docs/build/streams/H.md`, ADR-0006, ADR-0009,
  `docs/build/contracts/api-v1.md` (transport, auth, errors), `crates/protocol/src/api.rs`
  (`Caller`, `TokenScope`, `ErrorCode::http_status`, `HostInfo`).

## Goal

The daemon's front door: a **private local listener**, **device and agent tokens**, and **auth
middleware** that turns a bearer token into a `Caller`. It also serves the first route,
`GET /v1/host/info`. This is work packages 1–4 of your card, minus composing other crates.

## What to build

1. **Tokens** (`crates/auth`):
   - 256-bit random tokens, base64url, prefixed by scope so leaks are easy to spot (`pcd_` for
     device, `pca_` for agent).
   - Store only the **SHA-256** of each token, with its `Caller` (member, scope, owner) and a
     creation time. Stream H owns no database migrations, so keep a small registry file (JSON,
     written atomically, 0600 on Unix) in a state directory, behind a `TokenStore` trait so it
     can move into the hub store later.
   - Mint, verify (constant-time compare), revoke and rotate.
   - `Debug` and logging never print a token.
2. **Auth middleware** (`crates/api`, axum 0.8):
   - HTTP: `Authorization: Bearer <token>`. **Tokens in a query string are ignored, never
     accepted.**
   - WebSocket: `Sec-WebSocket-Protocol: pitcrew.v1, pitcrew.bearer.<token>`, answered with
     `pitcrew.v1`.
   - On success, insert `axum::Extension<Caller>`. Failures return an `ApiError` body with the
     right status (401, 403).
   - Provide a way to mark routes **device-only** (a layer or helper), and put it in
     `pitcrew-auth`, so domain crates can use it without depending on `pitcrew-api`. Document
     the pattern in the crate README.
3. **Listener:**
   - **Unix:** a socket in a 0700 directory. Remove a stale socket safely. Check the peer's uid
     on every accepted connection (tokio `peer_cred`) and drop other users. Do the check in a
     wrapper that implements axum's `serve::Listener`.
   - **Windows:** a named pipe that rejects remote clients and is restricted to the current
     user. If that needs a security descriptor, isolate the `unsafe` in one module
     (`#[allow(unsafe_code)]` on that module only, with a soundness comment). If it grows large,
     stop at a clean point and list it under "What I did not do".
   - A **loopback TCP** mode for development only, off unless explicitly enabled.
4. **Routes:**
   - `GET /v1/host/info` (no auth), returning `HostInfo` with `roles`, `protocol` and
     `protocol_min` from the protocol crate, plus machine facts.
   - An unknown route returns `404 not_found` as an `ApiError`.
5. **Public API for the composition root** (`crates/daemon`, stream 0): something like
   `pitcrew_api::serve(config, token_store, router_parts) -> Future`. Do not edit
   `crates/daemon`; describe the wiring in your report.

## Acceptance

- `tower::ServiceExt::oneshot` tests:
  - no token → 401;
  - unknown token → 401;
  - a token in the query only → 401;
  - an agent token on a device-only route → 403;
  - a device token passes;
  - the `Caller` a handler sees matches the token.
- WebSocket subprotocol auth test (tokio-tungstenite or hyper in tests is fine).
- Unix: an end-to-end request over a real socket in a temp directory, and a check that the
  directory is 0700.
- The token registry round-trips; revoked tokens fail; stored files never contain a raw token.
- `host/info` matches the contract's JSON shape.

## Out of scope

The delta stream, the terminal WebSocket, composing other crates' `routes()`, TypeScript export
(later briefs), and editing `crates/protocol` (propose changes instead).
