# 0001. Record architecture decisions

- **Status:** Accepted
- **Date:** 2026-09-30

## Context

PitCrew is built by several contributors at once, many of them AI agents working in parallel
streams (ADR-0011). An agent starting a work package has no memory of earlier discussions. It
needs the reasons behind the design in the repository, next to the code, or it will "fix" a
deliberate choice.

## Decision

We keep architecture decision records in `docs/adr/`, numbered, one decision each, in the format
Status / Context / Decision / Consequences. Accepted records are not edited except for typos; a
new record supersedes an old one and both say so.

## Consequences

- Stream cards (`docs/build/streams/`) cite the ADRs they depend on.
- A pull request that contradicts an ADR must either follow it or come with a new ADR.
