# Stream G · Integrations

**Goal:** two-way sync with GitHub and Jira, where **every outward write is approved by a
person** and each field has one owner.

**Owns:** `crates/sync-github/**`, `crates/sync-jira/**`, `crates/store/migrations/04*`.
**Depends on:** stream 0, E's commands.  **Model:** Sonnet-class.
**Read first:** ADR-0006, ADR-0007 (the `sync` mover); `ExternalRef` in the protocol.

## Work packages

1. **GitHub** (REST with ETags and conditional requests): issues ↔ tasks, milestones ↔
   workstreams, pull requests as receipts. Rate-limit aware; incremental by `since`.
2. **Jira Cloud** (REST v3; site, e-mail and API token) first; Data Center (PAT) behind the same
   trait. Issues ↔ tasks, epics ↔ workstreams.
3. **Field ownership:** a table per system saying which side owns each field (e.g. title from
   upstream, status mirrored only through the `sync` mover rules). Conflicts become asks.
4. **Write-approval queue:** every outward write (create issue, comment, close, label) becomes an
   ask of kind `approval`; it runs only after a person approves, and the result is an event.
5. **Credentials:** received from the desktop or `gh auth token` on the primary machine; stored
   0600; never logged; fine-grained, repo-scoped where possible.
6. **Routes** for integration setup and status (propose the contract).

## Acceptance

- Tests use recorded HTTP fixtures (no network in CI).
- An upstream close moves a task to done only when `can_move(Sync)` allows it; in-progress work
  is never touched.
- No outward write happens without an answered approval ask (tested).

## Do not

Store credentials in the event log, or write to trackers without approval.
