# pitcrew-api

HTTP and WebSocket API over a unix socket or named pipe; composes every crate's routes.

**Owned by stream H** — see [docs/build/streams/H.md](../../docs/build/streams/H.md).

## Transport

| `Listen` | Where | Protection |
|---|---|---|
| `Unix { dir }` | `dir/pitcrewd.sock` | `dir` is 0700, the socket 0600; every connection's peer uid must equal the daemon's. A stale socket is removed; a live one, or anything that is not a socket, is left alone. |
| `Pipe { name }` | e.g. `\\.\pipe\pitcrewd-<user>` | A DACL granting only the current user; remote clients rejected; the first instance must create the name, so it cannot be squatted. |
| `DevTcp { addr }` | loopback only | **Development only**, never a default. Tokens are the only protection; a `Host` guard blocks DNS rebinding. |

`Listen::private_default(run_dir)` picks the socket or the pipe for the platform.

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
pitcrew_api::serve(&Listen::private_default(run_dir), info, tokens, parts, shutdown).await?;
```

Or `Bound::bind(&listen)` first (to report where it listens with `describe()`), then
`bound.serve(pitcrew_api::router(info, tokens, parts), shutdown)`.
