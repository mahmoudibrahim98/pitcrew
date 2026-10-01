# Brief E · The recap index follows the engine's directory

- **Stream:** E · Work model. **Branch:** `s/E/recap-names`. **Paths:** `crates/hub-work/**`
  (+ `Cargo.lock`).
- **First read:** [README.md](README.md), [E-recap-index.md](E-recap-index.md) (merged),
  [F-recap-hardening.md](F-recap-hardening.md) (merged), the `crates/recap` README (the bounded
  `Directory`, `names_version()`, `Directory::ask`, `observe` learning `member_added`), and
  `crates/hub-work/src/recap.rs`.

## Goal

Stream F bounded the recap engine's `Directory` with an LRU, and gave it `names_version()` and a
public `ask()`. The hub's recap index must follow, so that a live index always equals a rebuild,
including past the directory's bound, and so it uses half the memory.

## What to build

1. **`names_version()` drives the day cache.**
   - Today the cache is cleared when the hub's own per-kind before/after comparison says a name
     changed. That misses evictions: past 100,000 entries, an evicted name leaves cached
     paragraphs saying the old name, while a rebuild says "someone" or "a task".
   - Read `v = names.names_version()`, apply the event, and treat `names.names_version() != v`
     as "names changed".
   - Keep the check for names a block mentioned while they were unknown.
2. **Use `Directory::ask`** instead of the hub's own `asks` map (and its `seeded` bookkeeping).
3. **One directory, not two.** `observe` now learns `member_added`, so use the builder's
   directory for names, and drop the separate `names` copy. That halves the worst-case memory,
   which is about 125–170 MB per directory at the bound. The test oracle must observe every event
   the same way.
4. **Docs:** remove the README's fixed "Known differences from the activity index" and the
   stale "which the engine's directory does not follow".
5. **Tests:**
   - with a tiny directory limit (`Directory::with_limit`), a live index kept current through
     evictions equals a rebuild, both through the store and in memory (extend
     `tests/recap_props.rs`);
   - an eviction followed by a query rewrites the affected days.

## Acceptance

- The tests above pass, and the existing recap tests pass unchanged.
- fmt, clippy (Linux and Windows target), `cargo test --workspace`, `cargo deny check`, and the
  guards all pass.

## Out of scope

Persisting blocks (a later brief), and changes to the engine (stream F).
