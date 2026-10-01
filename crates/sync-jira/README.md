# pitcrew-sync-jira

Read Jira issues and epics, for Jira Cloud and Jira Data Center, safely and incrementally; field
ownership; the write-approval queue and outward writes are later briefs. This crate is read-only
and never writes to Jira.

See the [crate's own docs](src/lib.rs) ("Shape" and "Reuse, not a fork") for the architecture and
for exactly what is reused from [`pitcrew-sync-github`](../sync-github/README.md) versus added
locally. In short: the transport seam, recorded-fixture format, bounds/backoff helpers and the
`Intent`/field-ownership shape are a dependency on sync-github, not a fork of it; the JQL builder,
the Atlassian Document Format converter, the Cloud/Data Center `Deployment` trait and the Jira
wire types are specific to this crate.

**Owned by stream G** — see [docs/build/streams/G.md](../../docs/build/streams/G.md).
