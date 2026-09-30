# Security Policy

PitCrew runs AI agents on your machines, holds credentials, and connects to remote hosts. We take
security reports seriously.

## Reporting a vulnerability

Report vulnerabilities **privately**, using GitHub's
[private vulnerability reporting](https://docs.github.com/en/code-security/security-advisories/guidance-on-reporting-and-writing-information-about-vulnerabilities/privately-reporting-a-security-vulnerability)
on this repository. Do not open a public issue.

Include:
- what is affected;
- how to reproduce it;
- the impact you expect.

We aim to acknowledge reports within 3 working days and to agree a disclosure date with you.

## Supported versions

PitCrew is pre-alpha. No version is supported yet. This section will list supported releases from
v0.1 on.

## Design commitments

These are recorded in [`docs/adr`](docs/adr) and are reviewed on every security-sensitive change:

- **No listening TCP ports on shared machines.** The daemon listens on a user-only unix socket (a
  named pipe on Windows).
- **Tokens are sent only in headers**, never in URLs or logs. Device tokens are kept in the OS
  keychain.
- **Agents hold scoped tokens.** Actions only a person may take (sending to GitHub or Jira,
  decisions, settings) need that person's device token.
- **Senders are stamped by the daemon.** The author of a message comes from the caller's token,
  never from the message body.
- **The remote helper needs no root and no internet.** Its checksum is verified before it runs.
