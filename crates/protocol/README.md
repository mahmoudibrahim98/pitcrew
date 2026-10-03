# pitcrew-protocol

Shared wire types, serde JSON and generated TypeScript. Files API types live in `src/files.rs`:
`FileList`, `FileContent` and `WriteFile`. A write must include `revision`; null requests exclusive
creation. File limits are shared constants. `ErrorCode` adds `too_large` (413) and `unsupported`
(501); ordinary errors retain their existing shape.

Run `cargo test -p pitcrew-protocol --features ts` to regenerate `packages/protocol-ts` explicitly.
