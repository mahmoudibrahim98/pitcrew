# Getting started as a contributor

Start with the [architecture](architecture.md). PitCrew is pre-alpha: the daemon, UI, desktop
gateway and synthetic demo run, while several integration paths are still incomplete. Use a
fresh demo state, not your real agent homes. Read [CONTRIBUTING](../CONTRIBUTING.md) and the
[briefs README](build/briefs/README.md) before changing files.

## Prerequisites

Use current stable Rust with rustfmt and clippy. The root workspace requires Rust **1.88+**;
the separate desktop workspace requires **1.90+**. Use Node **22.18+** (native TypeScript support)
and Corepack; the root `package.json` pins pnpm **10.34.6**. CI uses Node 22 and stable Rust.
If your Node distribution omits Corepack, install it using its
[official instructions](https://github.com/nodejs/corepack#installation).

| OS | Native build prerequisites |
|---|---|
| Linux | C compiler/linker and pkg-config; for the desktop, `libwebkit2gtk-4.1-dev`, `libayatana-appindicator3-dev`, `librsvg2-dev`, `libxdo-dev` and `libssl-dev` (the package names in Ubuntu CI). Other distributions use equivalent WebKitGTK 4.1/GTK 3 development packages. A graphical session is needed to run the desktop. |
| macOS | Xcode Command Line Tools and the platform SDK; the desktop uses system WebKit. |
| Windows | Rust's MSVC toolchain, Visual Studio C++ Build Tools with a Windows SDK, and WebView2 for the desktop. Native terminal support uses ConPTY, not tmux. |

On Unix, install tmux **3.2+** to exercise tmux integration tests. Without it those tests may
skip; the PTY sidecar is the runtime fallback. Remote desktop connections use system OpenSSH;
password/host-key prompts need **8.4+**, while older versions are restricted to keys without
prompts. See [CI](../.github/workflows/ci.yml) and
[Tauri's platform prerequisites](https://v2.tauri.app/start/prerequisites/).

**Verification context:** commands below were exercised in a Linux cloud container with Rust
1.99, Node 24 and the pinned pnpm. macOS and native Windows setup/run steps were **not verified**
in that environment because those operating systems were unavailable. The Bash demo applies to
Linux/macOS, or a Linux checkout inside WSL; WSL itself was **not verified** there. Native Windows
users can use the [daemon README's PowerShell example](../crates/daemon/README.md#development-the-ui-against-the-daemon),
with a newly generated state directory each time.

## Checkout and dependencies

```bash
git clone https://github.com/mahmoudibrahim98/pitcrew.git
cd pitcrew
corepack pnpm install --frozen-lockfile
cargo fetch --locked
cargo fetch --manifest-path apps/desktop/src-tauri/Cargo.toml --locked
```

In a cloud environment whose network is cut after setup, both dependency downloads must be in
the setup script. The desktop is a separate Cargo workspace with a separate lockfile.

To reduce development build disk use, set these in the shell running Cargo (Bash):

```bash
export CARGO_PROFILE_DEV_DEBUG=0
export CARGO_PROFILE_TEST_DEBUG=0
export CARGO_INCREMENTAL=0
```

On native Windows PowerShell, the equivalents are below; **not verified on Windows** here:

```powershell
$env:CARGO_PROFILE_DEV_DEBUG = '0'
$env:CARGO_PROFILE_TEST_DEBUG = '0'
$env:CARGO_INCREMENTAL = '0'
```

Keep the default target directories. No release or fuzz build is needed for this walkthrough.

## Build and test

Run from the repository root:

```bash
cargo build --workspace --locked
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked --no-fail-fast
npm test
corepack pnpm --filter @pitcrew/ui typecheck
corepack pnpm --filter @pitcrew/ui lint
corepack pnpm --filter @pitcrew/ui test
corepack pnpm --filter @pitcrew/ui build
```

For iteration, select the crate you changed instead of the whole workspace. Full Rust tests can
take 10–20 minutes. Runtime tests require valid private socket paths and OS ownership; a container
whose parent directories belong to an unrelated UID can refuse those paths. That is an environment
limitation, not a reason to loosen the checks. The full workspace command was run here but did not pass: private runtime path refusals, an
existing ingest scan snapshot mismatch and remote process-cleanup failures were observed.
Desktop remote integration tests also refused the unrelated-UID ancestors. The PR report records
the results; platform CI runs in its own environment. No check was weakened for this guide.

The root `npm test` runs mock-hub and CI-script tests without an npm install; UI dependencies
come from the frozen pnpm lockfile. A UI production build fails if `VITE_PITCREW_TOKEN` is set.
Keep the development token scoped to the dev-server command as shown below.

The desktop is checked separately:

```bash
cargo build --manifest-path apps/desktop/src-tauri/Cargo.toml --locked
cargo fmt --manifest-path apps/desktop/src-tauri/Cargo.toml --check
cargo clippy --manifest-path apps/desktop/src-tauri/Cargo.toml --all-targets --locked -- -D warnings
cargo test --manifest-path apps/desktop/src-tauri/Cargo.toml --locked --no-fail-fast
```

Tests use temporary homes, fixture transcripts and stand-in agent CLIs. Never run tests against
real `~/.claude`, `~/.codex`, OpenCode data or SSH profiles.

Before committing, and again after committing so the diff checks see your changes:

```bash
node scripts/ci/ownership-audit.mjs
node scripts/ci/path-guard.mjs --base origin/main
node scripts/ci/scrub-gate.mjs --base origin/main
```

Use exactly the brief's branch. The path guard checks ownership by branch, the audit requires one
owner for every tracked file, and CI's scrub gate also has the maintainers' private patterns.

## Browser UI against the mock hub

In one terminal at the repository root:

```bash
npm run mock-hub
```

In another:

```bash
corepack pnpm --filter @pitcrew/ui dev
```

Open `http://127.0.0.1:5173`. The development defaults use the mock at
`http://127.0.0.1:47317` and its synthetic device token. Stop both with Ctrl-C. The mock resets
on restart, serves canned terminal output and static recap fixtures, and supports recap days
only at `tz=0`. It can demonstrate routes the daemon does not implement yet.

Vite uses `strictPort`: if 5173 is already occupied, it must fail, rather than silently moving.

## Browser UI against a real demo daemon

The following is one Bash session at the repository root. It creates fresh synthetic state and
starts the already-built daemon on loopback. Do not reuse a production state directory.

```bash
PITCREW_DEMO_STATE="$(mktemp -d "${TMPDIR:-/tmp}/pitcrew-demo.XXXXXX")"
chmod 700 "$PITCREW_DEMO_STATE"
./target/debug/pitcrewd --state-dir "$PITCREW_DEMO_STATE" serve --demo \
  --listen tcp:127.0.0.1:47460 > "$PITCREW_DEMO_STATE/daemon.log" 2>&1 &
PITCREW_DEMO_PID=$!
```

Wait for `pitcrewd listening on` in that log. If the process exits, read the log and stop; do
not proceed with a token from another state directory. `--demo` alone watches no agent homes;
a later restart without it would watch the daemon's homes by default, so keep demos isolated.

Then launch Vite in the same terminal, stopping the mock UI dev server first:

```bash
VITE_PITCREW_API=http://127.0.0.1:47460 \
VITE_PITCREW_TOKEN="$(cat "$PITCREW_DEMO_STATE/device.token")" \
  corepack pnpm --filter @pitcrew/ui dev
```

The token is read from a private file into the child environment, never printed or placed in a
URL or process argument. This browser token path is for development only; the desktop uses its
gateway. Stop Vite with Ctrl-C, then try the agent CLI against the still-running demo:

```bash
PITCREW_URL=http://127.0.0.1:47460 \
PITCREW_TOKEN_FILE="$PITCREW_DEMO_STATE/demo-agent.token" \
  ./target/debug/pitcrew --json whoami
```

Ordinary CLI verbs require the demo's registered agent token; `device.token` is for the UI.
Task dispatch currently returns `503`; manual session linking, tracker wiring and import backends
are also not built yet. See the [architecture's integration gaps](architecture.md#integration-still-to-do).

When finished, stop only this demo and remove its temporary state:

```bash
kill "$PITCREW_DEMO_PID"
wait "$PITCREW_DEMO_PID"
rm -rf -- "$PITCREW_DEMO_STATE"
unset PITCREW_DEMO_STATE PITCREW_DEMO_PID
```

## Desktop development

**Interactive desktop launch was not verified in the cloud environment:** it had no display or
Xvfb, and private runtime paths under unrelated-UID ancestors were refused. Build and test results
are listed in the PR report. On a local graphical machine, first run the Vite command from the
mock section; a debug desktop trusts whichever page answers on `127.0.0.1:5173`.

In another Bash terminal at the root, use a fresh desktop state:

```bash
PITCREW_DESKTOP_STATE="$(mktemp -d "${TMPDIR:-/tmp}/pitcrew-desktop.XXXXXX")"
chmod 700 "$PITCREW_DESKTOP_STATE"
mkdir -m 700 "$PITCREW_DESKTOP_STATE/home"
env -u CLAUDE_CONFIG_DIR -u CODEX_HOME -u OPENCODE_CONFIG_DIR -u XDG_CONFIG_HOME \
HOME="$PITCREW_DESKTOP_STATE/home" USERPROFILE="$PITCREW_DESKTOP_STATE/home" \
APPDATA="$PITCREW_DESKTOP_STATE/home/AppData/Roaming" \
LOCALAPPDATA="$PITCREW_DESKTOP_STATE/home/AppData/Local" \
XDG_DATA_HOME="$PITCREW_DESKTOP_STATE/home/.local/share" \
PITCREW_STATE_DIR="$PITCREW_DESKTOP_STATE" \
PITCREW_PITCREWD="$PWD/target/debug/pitcrewd" \
  ./apps/desktop/src-tauri/target/debug/pitcrew-desktop
```

The desktop reaches its own local daemon through a private socket/pipe, even though Vite was
started using its browser mock defaults. Complete first-run setup in the app. The explicit home variables keep the daemon's
default watchers in an empty synthetic home; the [fixture helpers](../crates/fixtures/README.md)
use the same isolation for tests. The app's local daemon supervisor requires a
trusted binary path, so a group-writable checkout may be refused rather than executed.

After the app exits, remove only the temporary state you created:

```bash
rm -rf -- "$PITCREW_DESKTOP_STATE"
unset PITCREW_DESKTOP_STATE
```

Native Windows runs the debug `.exe` with the same state, daemon-path and synthetic-home
variables set in PowerShell; that launch was **not verified on Windows** here. The
[desktop README](../apps/desktop/src-tauri/README.md) covers settings, helper checksums, tray
behavior and production builds. Remote SSH deployment is not part of this walkthrough.

## Where to read next

- [Briefs and current task status](build/briefs/README.md), then your brief and its stream card.
- [Ownership rules](build/ownership.md) and [contract change process](build/contracts.md).
- [API v1](build/contracts/api-v1.md) and [desktop gateway](build/contracts/desktop-gateway.md).
- [Threat model](security/threat-model.md), including open security items.
- [Benchmarks](../benches/README.md) and [packaging](../packaging/README.md).
