# pitcrew-protocol

Shared wire types, serde JSON and generated TypeScript. Files API types live in `src/files.rs`:
`FileList`, `FileContent` and `WriteFile`. A write must include `revision`; null requests exclusive
creation. File limits are shared constants. `ErrorCode` adds `too_large` (413) and `unsupported`
(501); ordinary errors retain their existing shape.

Run `cargo test -p pitcrew-protocol --features ts` to regenerate `packages/protocol-ts` explicitly.

Session import types live in `import`: inclusion rules, the durable choice, and dry-run/commit counts. Regenerate the TypeScript exports with `cargo test -p pitcrew-protocol --features ts`.

Board drafts live in `board` (api-v1.md, "Board drafts"): the preview and its cost and estimate,
the start, the drafting agent's proposal and its bounds (`MAX_PROPOSAL_BYTES` and the rest), the
review, and a draft as the hub keeps it, with `DraftId` (`drf_…`) in `ids`. Three events carry
them: `board_draft_started` (sizes and the prompt's version, never the summary), `board_proposed`
and `board_draft_reviewed`. Tasks a person accepts carry the label `DRAFTED_LABEL` (`drafted`).
