# 0006. Unix sockets and scoped bearer tokens

- **Status:** Accepted
- **Date:** 2026-09-30

## Context

The daemon runs on shared machines (HPC login nodes host hundreds of users). Agents run with
the user's filesystem rights, and they read untrusted text: transcripts, issues, web pages,
files. If agents hold the same token as the person, a prompt injection can act as the person.

## Decision

- The daemon listens only on a **unix socket** in a 0700 directory (Windows: a **named pipe**
  with a per-user ACL), never on TCP by default. Every connection's peer uid is checked
  (`SO_PEERCRED` / `getpeereid`).
- Authentication is `Authorization: Bearer <token>`, **never a query string**. WebSockets carry
  the token as a subprotocol (`pitcrew.bearer.<token>`), because browsers cannot set headers
  there. Tokens are redacted from logs.
- Two scopes (`TokenScope`):
  - **device**: a person's desktop. Stored in the OS keychain; rotatable.
  - **agent**: agents and hooks, limited to agent verbs on their own sessions and tasks.
- **The host stamps `author` and `on_behalf_of`** on every event from the token. Nothing in a
  request body can claim to be someone else.
- Person-only actions (decisions, approvals of outward writes, ending sessions, settings) need a
  device token and are written to an audit log.

## Consequences

- An agent can move its own task to review, never to done (ADR-0007), and cannot approve its own
  outward actions.
- Remote daemons are reached through an SSH tunnel to the socket, so no port is ever open.
- Later, people joining a workspace (milestone M6) are new members with their own device tokens;
  the model does not change.
