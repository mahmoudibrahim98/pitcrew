# pitcrew-auth

Device and agent tokens, scopes, and author stamping (ADR-0006).

**Owned by stream H** — see [docs/build/streams/H.md](../../docs/build/streams/H.md).

## Tokens

- 256 random bits, unpadded base64url, prefixed by scope: `pcd_…` (device, a person's desktop)
  and `pca_…` (agent). The prefix makes a leaked token easy to spot.
- Only the SHA-256 of a token is stored, with its `Caller` and creation time. Lookups compare
  hashes in constant time.
- `TokenStore` mints, verifies, revokes and rotates. `FileTokenStore` keeps the registry in
  `<state dir>/tokens.json`, written atomically through a uniquely named temporary file.
- **Single writer:** only the daemon opens `FileTokenStore`; the CLI and the desktop mint and
  revoke through the API. The store holds an exclusive lock on `tokens.lock` for its lifetime,
  so a second opener fails with `TokenError::Locked`.
- **Private on disk (Unix):** the state directory is created 0700, and an existing one must
  already be ours with no group or other access; it is never re-permissioned. The registry is
  opened without following symlinks and must be ours and not writable by others. Anything else
  fails closed.
- **Windows:** files take their directory's ACL, so the state directory must be under the
  user's profile (e.g. `%LOCALAPPDATA%`).
- An agent token must name its owner (`on_behalf_of`); a device token must not.
- `SecretToken`'s `Debug` prints only the prefix. Logs name tokens by `TokenId` (`tok_…`).

## Using the caller in your routes

`pitcrew-api` authenticates every request before it reaches your `routes()` and inserts the
`Caller` into the request's extensions. You never parse tokens.

```rust
use axum::{Json, Router, routing::{get, post}};
use pitcrew_auth::{Authenticated, Person, device_only};

// Any scope: read the caller and stamp `author` / `on_behalf_of` from it.
async fn list(Authenticated(caller): Authenticated) -> Json<Vec<Task>> { … }

// One person-only handler: an agent token gets `403 forbidden`.
async fn approve(Person(caller): Person) -> … { … }

// A whole router of person-only routes.
let settings = device_only(Router::new().route("/v1/settings", post(save)));
```

- `Authenticated` accepts both scopes; `Person` and `device_only` reject agents with `403`.
- If no caller is present (a route mounted outside the API layer), both extractors answer `401`,
  so a wiring mistake fails closed.
- `ErrorResponse` renders an `ApiError` body with the status of its code; return it from handlers.
- Routes an agent may call are the ones marked **agent** in `docs/build/contracts/api-v1.md`.
  When handing routes to the composition root, give those to `RouterParts::agent` and everything
  else to `RouterParts::device` (see `pitcrew-api`); device is the safe default.
- Agent writes are limited to the agent's own tasks and sessions. That rule needs domain data, so
  it belongs in the domain crate's handler: compare `caller.member` with the task's assignee.
