# pitcrew-api

HTTP and WebSocket API over a unix socket or named pipe; composes every crate's routes.

**Owned by stream H** — see [docs/build/streams/H.md](../../docs/build/streams/H.md).

## Transport

| `Listen` | Where | Protection |
|---|---|---|
| `Unix { dir }` | `dir/pitcrewd.sock` | `dir` is created 0700; an existing one must already be ours and private (it is never re-permissioned). The socket is 0600; every connection's peer uid must equal the daemon's. A stale socket of ours is removed; a live one, another user's, or anything that is not a socket is left alone. |
| `Pipe { name }` | `\\.\pipe\pitcrewd-<user SID>` | A DACL granting only the current user; remote clients rejected. The daemon must create the name (first instance), so it never joins someone else's pipe. That does **not** stop another user creating the name while the daemon is down, so clients must check the server (below). |
| `DevTcp { addr }` | loopback only | **Development only**, never a default. Tokens are the only protection; a `Host` guard blocks DNS rebinding. |

`Listen::private_default(run_dir)` picks the socket or the pipe for the platform.

## Before a client sends a token

`pitcrew_api::client` has the checks the desktop and the CLI must make, so a socket or pipe
planted by another user never receives a token:

- Unix: `check_unix_socket(dir)` before connecting (directory ours and 0700, socket ours), and
  `check_unix_peer(&stream)` after (the server runs as us).
- Windows: `check_pipe_server(&client)` after connecting (the process serving the pipe runs as
  the current user).

## Auth

- HTTP: `Authorization: Bearer <token>`. Query strings are never read.
- WebSocket upgrades may instead offer `Sec-WebSocket-Protocol: pitcrew.v1, pitcrew.bearer.<token>`.
  WebSocket handlers answer with `pitcrew_auth::WS_PROTOCOL` (`ws.protocols([WS_PROTOCOL])`).
- On success the token is removed from the headers and `Caller` is inserted as an extension. See
  [`pitcrew-auth`](../auth/README.md) for how routes read it.
- Failures are `ApiError` bodies: `401 unauthorized` (none, unknown, revoked, or not `Bearer`),
  `403 forbidden` (agent on a device route), `404 not_found` (unknown route or method).

## For the composition root

```rust
let tokens: Arc<dyn TokenStore> = Arc::new(FileTokenStore::open(&state_dir)?);
let info = pitcrew_api::local_host_info(env!("CARGO_PKG_VERSION"), vec![HostRole::Hub, HostRole::Runner], caps);
let parts = RouterParts::new()
    .agent(hub_work::agent_routes())    // routes marked **agent** in api-v1.md
    .device(hub_work::device_routes()); // everything else
pitcrew_api::serve(&Listen::private_default(run_dir)?, info, tokens, parts, shutdown).await?;
```

Do not merge more routes into the router this builds: they would be unauthenticated. Put every
route in `RouterParts`.

Or `Bound::bind(&listen)` first (to report where it listens with `describe()`), then
`bound.serve(pitcrew_api::router(info, tokens, parts), shutdown)`.
