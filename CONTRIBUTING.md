# Contributing to PitCrew

PitCrew is built in **streams**. A stream is one area of the code, owned exclusively by one
contributor or agent at a time. This lets many people work in parallel without touching each
other's files.

## Before you start

Read the [architecture](docs/architecture.md) and [getting-started guide](docs/getting-started.md)
for the process map, prerequisites, checks and isolated demo.

1. Read [`docs/build/README.md`](docs/build/README.md) and the card for your stream in
   [`docs/build/streams`](docs/build/streams).
2. Check [`docs/build/ownership.json`](docs/build/ownership.json): it lists the paths your stream
   may change. CI rejects changes outside them.
3. Build against the **contracts** (`crates/protocol`, `crates/interfaces`, `apps/mock-hub`), not
   against another stream's internals.

## Branches and pull requests

- Name branches `s/<stream>/<short-topic>`, for example `s/A/claude-parser`. Cross-stream briefs use
  `integrator/<topic>`. Use exactly the branch named by your brief.
- Keep pull requests small and focused on one work package.
- **Contributors never merge their own pull requests.** The integrator merges.
- CI must be green:
  - formatting (`cargo fmt`) and linting (`cargo clippy -D warnings`);
  - tests;
  - the **path guard**;
  - the **ownership audit**;
  - the **scrub gate**, which rejects private identifiers.
- End every pull request description with **"What I did not do"**: anything skipped, left as a
  stub, or unverified.

## Changing a contract

Contracts are shared by every stream. To change one, open a pull request on branch `s/0/contract-…`
that explains who is affected. The integrator reviews it, bumps the version, and tells the
affected streams. Never edit `crates/protocol` from another stream's branch.

## Local checks

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
npm test                       # mock hub and CI-script tests (no install needed)
node scripts/ci/ownership-audit.mjs
node scripts/ci/scrub-gate.mjs
```

On Windows, run the Rust commands inside WSL or with a native Rust toolchain.

## Security

Don't open public issues for vulnerabilities; see [SECURITY.md](SECURITY.md). Never commit real
agent transcripts, tokens or private paths. Test data belongs in `crates/fixtures`, and it must be
synthetic.

## Conduct

This project follows the [Code of Conduct](CODE_OF_CONDUCT.md).
