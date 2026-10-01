# Brief G · The fuzzers' findings in GitHub and Jira sync

- **Stream:** G · Integrations. **Branch:** `s/G/fuzz-findings`. **Paths:** `crates/sync-github/**`,
  `crates/sync-jira/**` (+ `Cargo.lock`).
- **First read:** [README.md](README.md), `docs/security/threat-model.md` (findings R10, R28–R32;
  open items O30 and O39), and the reproductions in `fuzz/regressions/github_sync/`,
  `github_links/`, `jira_sync/` and `jira_adf/` (the `open-*` files, one per finding).

## Goal

Fix what stream Q's round-3 fuzzers found in the two sync crates. Each fix gets a unit test that
uses the reproduction's input, so the fuzz input passes afterwards.

## What to build

1. **R29 (medium): ADF to text is quadratic.** `adf_to_text` recounts the whole output at every
   node: 692 ms for a 763 KiB description in a release build, and six of those fit in one page.
   Keep a running count. Test: the reproduction finishes in under 50 ms in release, or under a
   generous debug bound in the test.
2. **R28 (low):** a hostile `Retry-After` overflows `now_unix + retry_after` in both crates, and
   large values aren't capped. Use saturating arithmetic, and cap the wait at the crate's maximum
   backoff.
3. **R30 (low):** a Data Center `startAt` near `u64::MAX` overflows in `parse_search_page`. Use
   checked arithmetic, and treat a bad value as a malformed page.
4. **R31 (low):** `path_is_under` drops empty segments, so `/api//v3/…` and `//api/v3/…` are
   followed with the token. Refuse a `next` URL with an empty segment in its path: compare the
   segments strictly, empty ones included.
5. **R32 (low):** a closing reference `owner/.#1` is accepted, and its link resolves to another
   page. Refuse `.` as well as `..` for both owner and repo.
6. **R10 residuals (O30):** a kept `html_url` has no length cap, and its port and userinfo aren't
   checked.
   - Cap it at 2 KiB.
   - Require the default port, or the configured web port for GHE.
   - Refuse userinfo.
7. **Seam for the fuzzer:** a `#[doc(hidden)] pub fn trusted_next_url(...)`, so `fuzz/` can call
   it directly. Document that it is not API.
8. **Doc:** `bounds.rs`'s comment that recap "could not mirror" the hidden set is out of date. The
   sets now match. Say so.

## Acceptance

- One test per finding with the reproduction's input. All existing tests pass unchanged.
- fmt, clippy (Linux and Windows target), `cargo test --workspace`, `cargo deny check`, and the
  guards all pass.

## Out of scope

Renaming the fuzz inputs (stream Q does that when it reruns), and the real HTTPS transport.
