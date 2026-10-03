# API mismatch report

Run against `main` at `8d3e012`, using fresh demo state on both targets. No daemon or contract code
was changed. Expected failures link to these row ids through `daemon-deviations.json`.

| Row | Case | Mock before / after | Daemon | API-v1 position / action |
| --- | --- | --- | --- | --- |
| M1 | Malformed `project` on workstreams; `project/workstream/assignee` on tasks; `machine/workstream/task` on sessions; `to` on asks; `project/workstream/task/session` on events (12 cases) | `200`, empty / **`400 invalid`** | `400 invalid` | “Malformed body or query” is 400. Fixed mock with one shared id-filter parser, preserving absent filters and accepting prefixed/case-insensitive ULIDs as Rust does. |
| D3 | `POST /sessions/{id}/link`, by a person and by an agent | `200 Session`, manual link; an agent's `403 forbidden` | `404 not_found` for both | Contract includes route and `session_linked`, a person-only write; daemon README identifies it as missing. Follow-up: implement manual link; both cases pinned expected failures. |
| D4 | Known session's terminal route called as ordinary HTTP | `400 invalid` | `404 not_found` | Errors section explicitly requires 400 for WebSocket routes called without an upgrade. Upgrade handler works on the daemon; add its ordinary-HTTP rejection route. Pinned expected failure. |
| A1 | Malformed id in a resource path | `404 not_found` | `404 not_found` | Contract explicitly defines malformed body/query (400), unknown resources in paths (404), and malformed recap filters (400), but gives no rule for malformed ordinary path ids. Both agree at 404. Suite permits 400 or 404 with matching ApiError, reports the observed status; propose an explicit rule in a contract follow-up. |
| C1 | Demo session transcript contents | Canned fixture transcript | Empty page for a local demo session with no indexed transcript | Both behaviors are described by the mock README and API-v1's demo/indexing note. Suite validates shape and paging without inventing content equality. |
| C2 | Recap updates and nonzero timezone | Static fixture recaps; only tz=0 supported | Derived recaps, arbitrary supported fixed offsets | API-v1 explicitly documents the mock exception. Shared suite checks tz=0, malformed/out-of-range tz, shape, filters and paging; freshness after writes / nonzero valid tz remain server-specific tests. |

A setup conflict may be returned before invalid fields are read on an already-seeded server; this
suite sends a valid setup body and tests 409, avoiding an unspecified validation precedence.
Likewise it tests invalid session command bodies on a live demo session, without attempting a
real command. Empty send text is a String allowed by API-v1 and was not labeled a deviation.
