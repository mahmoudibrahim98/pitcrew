# Protocol TypeScript

Private, type-only package generated from `pitcrew-protocol`. It has no runtime entry point.
The UI still uses its own declarations; see [DRIFT.md](DRIFT.md) before adopting these types.

From the repository root:

```sh
corepack pnpm install --frozen-lockfile
cargo test -p pitcrew-protocol --features ts --locked export_bindings
corepack pnpm --filter @pitcrew/protocol-ts typecheck
git diff --exit-code -- packages/protocol-ts
test -z "$(git status --porcelain --untracked-files=all -- packages/protocol-ts)"
```

Commit regenerated files with the Rust change. The exporter rejects stale bindings, exports all
serialized public protocol types, and writes a sorted `index.ts`. It also serializes representative
Rust values into `tests/fixtures.ts`, after asserting their Rust JSON round trips. TypeScript checks
those values with strict optional properties. The CI workflow repeats this process and detects
changed, removed and newly generated files.

Bindings are generated in a fresh temporary directory, then written over the package's
files without deleting checked-in directories. Removed types require an explicit repository
change; stale files fail generation. Read cursor requests/responses and `cursor_moved` are
generated with the other public API types.

The optional `ts` feature is off by default. `ts-rs` 12.0.1 (MIT, Rust 1.78 minimum) understands the
protocol's serde tags and renames. Explicit overrides describe string-encoded ids and keys and
omitted `Option` fields. Task patch fields retain their inner `null` for clearing a value. Serde
`default` alone does not make a serialized field optional: the serializer still emits it.
The library reports unsupported serde `transparent` / conversion attributes during generation;
string aliases and round-trip fixtures verify their wire representation without suppressing warnings.

JSON integers are generated as `number`, matching the wire, including timestamps and offsets.
TypeScript cannot express integer ranges or enforce JavaScript's safe-integer bounds. String types
also do not validate ULIDs, key syntax or calendar dates. Validate untrusted JSON at the boundary.
The protocol's non-exhaustive enums may acquire variants; consumers must handle unknown tags.
