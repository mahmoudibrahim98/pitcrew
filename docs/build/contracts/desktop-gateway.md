# Desktop gateway

How the UI, running in the desktop app's webview, reaches each workspace's daemon.

- **The webview never holds a token** (ADR-0003). It never opens a socket or makes an HTTP request
  to a daemon itself. It calls the gateway, the Rust side of the desktop app (stream K). The
  gateway adds the workspace's device token and forwards the call over the daemon's socket, pipe or
  SSH tunnel.
- The calls below are Tauri commands. Stream L's data layer implements the UI side
  (`apps/ui/src/data`); stream K implements the gateway (`apps/desktop`).
- Requests and messages are exactly API v1 ([api-v1.md](api-v1.md)). The gateway does not
  interpret them, apart from the checks below.
- **Changes** go through a contract change, like the API's. A new optional field or command is not
  breaking.

## Conventions

- **Arguments:** Tauri passes arguments by their names. `gateway_request` takes one argument,
  `req`: the UI calls `invoke('gateway_request', { req: {...} })`. The socket commands take flat
  arguments, as written below.
- **Field names** are camelCase on the wire, as written here (`contentType`). On the Rust side,
  use `#[serde(rename_all = "camelCase")]`.
- **Versions:** the `tauri` crate and `@tauri-apps/api` share major.minor (2.12 today); the Tauri
  CLI refuses a mismatch.
- **`connecting`:** a workspace that is `connecting` may answer `unreachable` until it is
  `ready`. The UI retries, and reconnects at once on `ready`.

## Knowing where the UI runs

The UI is in the desktop app when `window.__TAURI_INTERNALS__` exists. There it uses the gateway
for everything, and never reads `VITE_PITCREW_API` or a token. In a browser it uses HTTP and
WebSockets as today (development only).

## Workspaces

`gateway_workspaces() → GatewayWorkspace[]`

```ts
interface GatewayWorkspace {
  id: string;        // the daemon's workspace id (a ULID), as in `/w/$ws/…`
  name: string;
  kind: 'local' | 'remote';
  host?: string;     // SSH host or `wsl:<distro>` from desktop-owned records, never the hub
  state: 'connecting' | 'ready' | 'unreachable' | 'needs_pairing';
  detail?: string;   // why it is unreachable or needs pairing, for people to read
}
```

The UI must show `host` next to remote workspace names in the workspace switcher (including
accessible menu names), top bar and remove dialog.
Older desktops may omit it; local workspaces leave it unset. A hub rename never changes this host.

The gateway emits the Tauri event `gateway://workspaces` with the same list whenever it changes.

`gateway_local_host() → { name: string }`: this computer's host name, for onboarding's default
machine name (`POST /v1/setup`'s `machine_name`). Only its first label (no domain, no `.local`),
cleaned like a workspace name (no control, bidi or invisible characters; whitespace collapsed), at
most 60 characters; `"This computer"` when nothing is left. Main window only.
The UI's `/w/$ws` routes use these ids. Adding, pairing and removing workspaces are later
commands (onboarding, stream O).

## Requests

`gateway_request(req: GatewayRequest) → GatewayResponse`

```ts
interface GatewayRequest {
  workspace: string;
  method: 'GET' | 'POST' | 'PATCH' | 'PUT' | 'DELETE';
  path: string;      // '/v1/…', with an optional '?query'
  body?: string;     // JSON text
}
interface GatewayResponse {
  status: number;
  contentType?: string;
  body: string;      // the response body as text (API v1 bodies are JSON); "" when there is none
}
```

- **Every daemon answer is a response,** whatever its status: a 404 or 409 comes back as
  `{status, body}` with the `ApiError` body, as over HTTP.
- **The gateway checks:**
  - `path` starts with `/v1/`, and is not an integration's credential route (see "Integration
    credentials");
  - it has no `..` segment, no `//`, no `\`, no `#` and no control characters;
  - its query, if any, is passed on as it is;
  - `body` is at most 1 MiB, and is sent with `Content-Type: application/json`.
- **The webview sets no headers.** The gateway sends `Authorization`, `Content-Type` and `Host`
  itself.
- **The response** carries only `status`, `contentType` and the body. No other daemon header
  reaches the webview. The gateway refuses a response body over 32 MiB.
- **Gateway failures** reject the command's promise with a `GatewayError`. A request the daemon
  never answered is one of these, never a made-up HTTP status:

```ts
interface GatewayError {
  code: 'unknown_workspace' | 'needs_pairing' | 'unreachable' | 'invalid' | 'too_large' | 'internal';
  message: string;
}
```

## Integration credentials

`gateway_integration_credential({ workspace, integration, secret }) → GatewayResponse`

- Hands a GitHub or Jira integration's secret to the workspace's daemon once, as
  `PUT /v1/integrations/{integration}/credential` with body `{ "secret": … }` (api-v1.md,
  "Integrations"), and resolves with the daemon's answer whatever its status (`204` when stored).
  `integration` is an id (letters, digits, `_` and `-`, at most 64); anything else is `invalid`
  before any connection is made.
- **`gateway_request` refuses that route** (`invalid`), however its path is written (letter case,
  percent-escapes): the webview's general channel never carries a secret, and no answer ever
  holds one (the daemon never returns it).
- The gateway holds the secret only while the request is sent, never logs it (the request is
  logged like any other: workspace, method, route, status, time), and never stores it: the daemon
  keeps it, privately, in its own state directory.
- In a browser (development) the UI sends the same `PUT` itself; there is no gateway there.

## Sockets

The live stream (`/v1/stream`) and terminals (`/v1/sessions/{id}/terminal`) go through gateway
sockets, which carry the same frames as API v1's WebSockets.

`gateway_socket_open({ workspace, path, events: Channel }) → { socket: number }`

- **Paths:** only `/v1/stream` and `/v1/sessions/{id}/terminal`, with their queries. Any other
  path is `invalid`.
- **Opening:** the gateway opens the WebSocket to the daemon with
  `Sec-WebSocket-Protocol: pitcrew.v1, pitcrew.bearer.<token>`. The command resolves once the
  upgrade has succeeded.
- **When the upgrade fails,** it rejects with a `GatewayError`. The daemon's HTTP status, if
  there was one, is in `message`, and `unavailable` maps to `unreachable`.
- **Messages on the `events` channel,** in order:
  - `{ "type": "text", "data": string }` is a text frame;
  - an `ArrayBuffer` is a binary frame (a raw IPC body, not JSON);
  - `{ "type": "close", "code": number, "reason": string }` is always the last message, also
    after the webview's own `gateway_socket_close`. When the connection to the daemon breaks
    without a close frame, the code is 1006.
- **The gateway answers the daemon's Pings itself.** The webview never sees Ping or Pong.

`gateway_socket_send({ socket, text?: string, binary?: number[] | Uint8Array })`

- Exactly one of `text` and `binary`. The size limits are API v1's: 1 MiB on terminals, 4 KiB
  on the stream. Over a limit, the gateway closes the socket with 1009, as the daemon would.
- Sending on a closed socket rejects with `invalid`.
- **Order.** Tauri may run commands concurrently, so the UI keeps at most one
  `gateway_socket_send` in flight per socket. The command resolves once the gateway has queued the
  frame to the daemon, in that order.

`gateway_socket_close({ socket, code?: number, reason?: string })` closes the socket (1000 by
default). It is idempotent.

**Back-pressure.** The gateway keeps at most 8 MiB of incoming frames that the webview has not
taken. Past that, it closes the socket with 1013, and the UI reconnects with `since` or `from` as
it already does.

**Cleanup.** When the webview reloads or the window closes, the gateway closes every socket that
window opened.

## Remote workspaces

A workspace whose hub runs on another machine is reached over SSH: a login node, a server, or a
SLURM compute node through its login node. The gateway uses stream J's `pitcrew-remote` (probe,
deploy, launchers, the tunnel's `Connector`). Adding one takes two steps, so that **nothing changes
on the remote until the person has seen what will happen**. For SLURM, that includes the exact job
script.

`gateway_ssh_hosts() → { hosts: string[] }`: the concrete `Host` names in the person's ssh config,
for a picker. The person may also type a host.

The WSL extension uses the same preview, add, registry and reconnect flow as SSH.

`gateway_wsl_distros() → { available: boolean, distros: WslDistro[] }` lists local distros:

```ts
interface WslDistro { name: string; default: boolean; running: boolean; version: number }
interface WslTarget { kind: 'wsl'; distro: string }
```

Missing WSL is a normal `{ available: false, distros: [] }` answer, and so is WSL without any
distro (wsl.exe then fails the listing). A stopped WSL2 distro can be selected: probe and plan
start it first, allowing up to 120 s for WSL's cold start, before the usual 30 s probe. WSL1 is
refused with an explanation. The wizard offers “A WSL distro on this computer” only when there
is a distro to choose.

Probe accepts `{ host }` unchanged, or `{ host: '', target: WslTarget }`. Plans use the same
target form; a nonempty SSH host together with a target is refused. Add takes the opaque plan
unchanged, which binds the chosen transport and distro. WSL permits only direct and tmux.
No SSH configuration, askpass, host keys, SLURM or systemd-user are involved. Commands use
`%SystemRoot%\System32\wsl.exe -d <distro> --cd ~ --exec /bin/sh -c …`, with the distro as one
argument, POSIX-quoted commands and `WSL_UTF8=1` (so wsl.exe's own errors read as text); an ssh
host may not start with `wsl:`, the form a WSL workspace's `host` takes. The API uses `pitcrewd connect` over stdio, never port forwarding. Deployment uses
the Linux musl helper, compiled checksum manifest, atomic switch and existing private-directory
checks. The saved registry records the target; restart and retry use the usual connection ladder
and workspace states. Pairing reads the token over that transport and keeps it in the OS keychain.

`gateway_remote_probe({ host }) → RemoteProbe`:

```ts
interface RemoteProbe {
  host: string;
  os: string; arch: string;                 // e.g. "linux", "x86_64"
  helper?: { version: string; running: boolean };
  slurm?: { version: string; defaultPartition?: string; srunOverlap: boolean };
  tmux?: { version: string };              // offer the tmux launcher only from 3.2 on
  check?: MachineCheck;                    // api-v1.md, "Machine setup"
}
```

`check` is the machine check, made **in the probe's own call** before PitCrew is installed there
(`pitcrew_remote`'s `Ssh::probe_and_check`: one login, so a host that asks for a password or a
one-time code asks once for both): the rows of API v1's `MachineCheck` for each agent CLI and its
version, tmux, git, gh, the free space in the home folder and SLURM where `sbatch` is (never on
WSL), then a `helper` row from what the probe found (`ok` running, `warn` installed but stopped,
`missing`; the last two with the fix `install_helper`, which is the add itself). It runs only
`command -v`, each tool's `--version` (under `timeout 10` where there is one) and `df`: nothing is
installed or written. Since it is the probe's call, the gateway always answers it; `check` stays
optional in the type. The UI reads it
as it reads the hub's (a missing tool's fix is its install page, from the UI's own table).

`gateway_remote_plan(req) → RemotePlan` says what adding would do, without doing it:

```ts
interface RemotePlanRequest {
  host: string;
  target?: WslTarget;
  launcher: 'direct' | 'tmux' | 'slurm';
  site?: string;                            // a site recipe's name, for slurm
  job?: { partition?: string; account?: string; qos?: string; time?: string;
          cpus?: number; memory?: string; gpus?: string };
}
interface RemotePlan {
  plan: string;                             // an opaque id, valid for 10 minutes
  steps: string[];                          // e.g. "Copy pitcrewd 0.4.0 to ~/.pitcrew", "Submit the job below"
  jobScript?: string;                       // slurm: exactly the text that will be submitted
}
```

`gateway_remote_add({ plan, events: Channel }) → GatewayWorkspace` carries out a plan:
- deploy the helper;
- launch it, submitting exactly the shown script for SLURM;
- wait for its endpoint, connect, and pair;
- register the workspace.

Progress arrives on `events` as `{ step: string, state: 'running' | 'done' | 'failed', detail?:
string }`, ending with one `done` or `failed` for the whole add. A plan is used once. Errors are
`GatewayError`s; a refused plan or launch is `invalid`, and a lost connection is `unreachable`.

The `running` messages' `detail` is the add's live log, one line each: for the helper's step,
`checking for a copy already there`, then `already there, and verified (sha256 and version)`, or
`uploading`, `N% sent` (every 10 %), `verifying the sha256 and the version on the machine` and
`installed and verified`; for the launch, `starting it in the background` or `starting it in
tmux` and then `it runs and listens` (or `it was already running`), or for SLURM `submitting the
job script shown` and the job's state while it waits (`job 4242 pending (Priority)`). Onboarding
shows them as lines, as they come.

- **Pairing:** the gateway reads the remote hub's device token over the same SSH connection, from
  the file `pitcrewd token show-path` names, and keeps it in the OS keychain (`TokenStore`). The
  token never reaches the webview, a log, or a file of ours.
- **A fresh remote hub** is set up like a local one: the UI calls `POST /v1/setup` through
  `gateway_request` for that workspace.
- `gateway_remote_cancel({ plan })` stops an add that is still running, and undoes what it
  started (stops the helper, cancels a submitted job). The add then ends `failed`. Cancelling an
  add that has finished does nothing.
- `gateway_workspace_retry({ workspace })` tries a remote workspace's connection again at once, for
  example after a sign-in was cancelled while reconnecting (which leaves it `unreachable` until
  then). It returns when the attempt has started; the state follows on `gateway://workspaces`.
  Retries while an attempt runs make one more attempt after it, not one each, and the workspace is
  `connecting` during the attempt.
- `gateway_workspace_remove({ workspace, stopHelper: boolean })` forgets a workspace and deletes
  its keychain token. With `stopHelper`, it first stops the remote helper (cancelling its job for
  SLURM).
- **States:** a remote workspace's state follows its connection: `ready` when connected,
  `connecting` while connecting or unverifiable (with the reason in `detail`), and `unreachable`
  with the reason.

### Prompts: passwords, one-time codes and host keys

SSH may ask for a password, a key's passphrase, a one-time code, or a confirmation of a new host
key. Any command that talks to a remote can cause one, and so can the connection reconnecting. The
gateway emits `gateway://prompt`:

```ts
interface GatewayPrompt {
  id: string;
  host: string;
  kind: 'password' | 'passphrase' | 'otp' | 'host_key' | 'confirm' | 'notice';
  text: string;                    // ssh's question, cleaned of control characters; untrusted
  fingerprint?: string;            // host_key: the key's fingerprint, to compare
}
```

and the UI answers with `gateway_prompt_reply({ id, answer?: string, accept?: boolean })`: `answer`
for the first three kinds, and `accept` for a host key or a `confirm` (ssh's other yes/no
 questions, such as accepting updated host keys). A `notice` (such as `touch your security key`)
 needs no answer: it is closed when ssh moves on, and a reply with neither stops ssh. A reply with
 neither cancels. A prompt that
is no longer wanted is withdrawn with the event `gateway://prompt-closed` `{ id }`.

- **`kind` says who asks, and is never guessed from `text`.** `passphrase` and `host_key` are
  only for ssh's own local questions (unlocking a key on this computer, trusting a new host key).
  Anything the server sends (keyboard-interactive text, which OpenSSH marks with a leading
  `(user@host)`) is `password` or `otp`, whatever its words say. The UI tells the person where the
  answer goes: to the host for `password` and `otp`, and nowhere off this computer for
  `passphrase`.
- **ssh 8.4 or newer.** Older ssh does not mark the server's text, so the gateway shows and answers
  no prompt for it: the call fails saying ssh 8.4 or newer is needed. Keys that need no prompt still
  work.
- **Prompts are held for the page.** A prompt raised before the page listens (a reconnect at
  launch) is held until the page first calls `gateway_workspaces`, and every open prompt is emitted
  again, with the same `id`, after a reload. The UI de-duplicates by `id`.
- An answer is passed to ssh once, and is never stored or logged. It is in memory only as long as
  the reply takes.
- A host key the person accepts goes into their own `known_hosts`, by ssh itself.
- The UI shows `text` as text, and says which host is asking.

## Navigation from outside the window

Deep links (`pitcrew://w/<ws>/inbox`, `pitcrew://w/<ws>/task/<id>`,
`pitcrew://w/<ws>/session/<id>`, `pitcrew://w/<ws>/project/<id>` and
`pitcrew://w/<ws>/workstream/<id>`), and clicks on the app's own notifications, open a place in the
UI. The gateway emits the Tauri event `gateway://navigate`, then shows and focuses the main window.
Its payload is a typed target, not a path:

```ts
interface NavigateTarget {
  workspace: string;                                              // a workspace id (ULID)
  kind: 'inbox' | 'task' | 'session' | 'project' | 'workstream';
  id?: string;                                                    // required unless kind is 'inbox'
}
```

- **The gateway accepts only those link shapes.** Every id is a bare ULID or a known display
  form. Anything else from a deep link is dropped and logged (shortened), never forwarded.
- **The UI checks the target again,** maps it to its own route (`paths.task(ws, id)`, and so on),
  and navigates. An unknown workspace goes to `/` with a notice.
- **A link that launched the app** is held by the gateway until the page first calls
  `gateway_workspaces`. The UI must start listening to `gateway://navigate` before that first
  call. A held target expires after 60 seconds.
- **Deep links arrive from any web page,** so they never act: they only navigate. No deep link
  answers an ask, moves a task, or sends input.

## Desktop updates

Main window only; no updater plugin permissions are granted to the webview.
`gateway_update_status()` returns `{ enabled, prereleases, portable: boolean, version?: string,
notesUrl?: string, downloadUrl?: string }`.
`gateway_update_check()` checks now and returns the same status.
`gateway_update_channel({ prereleases: boolean })` saves the choice (default false), discards
the pending update, and checks again. `gateway://update` carries the status after each check.
Checks also run at startup and every 24 hours. An empty compiled public key disables checks.
`gateway_update_install({ version: string })` installs only the pending version shown to the
person, after explicit consent; the updater verifies its minisign signature before installation.
Signatures must bind the artifact to the offered version in their authenticated trusted comment.
It rejects stale versions, concurrent operations, channel changes, and verification failures.
`gateway_update_notes({ version: string })` opens only that pending version's fixed GitHub
release page in the system browser. Feed text never becomes a URL or HTML in the webview.
Successful installation restarts the app (Windows' installer exits the process itself).
Stable checks use the GitHub latest release's `latest.json`; opting in selects the highest
eligible semantic version with a feed from GitHub's latest 100 published releases, including
pre-releases. Downgrades and equal versions are never offered. Linux self-update is for
AppImage installs; deb/rpm users update through their package manager.

**A portable copy** (`portable.txt` next to the app's executable: the portable Windows zip) has
`portable: true`. It checks whether or not a public key is compiled in (`enabled` is always true),
but never downloads or installs: `gateway_update_install` for the pending version opens the fixed
page of the portable workflow's successful runs on `main` (where the newest
`pitcrew-windows-x64-portable.zip` is) in the system browser, and fails with a sentence saying
so, for the UI to show. While an update is pending, `downloadUrl` is that page (portable copies
only). An installed copy has `portable: false` and no `downloadUrl`.

## Security notes

- Only the app's own windows can call these commands (Tauri capabilities). No remote URL is ever
  loaded into a window.
- The gateway logs workspace ids, paths without their query, statuses and timings. It never logs
  tokens, bodies or frames.
- A test in the gateway proves that no token reaches the webview: no command's result, event or
  error ever contains it.
