# Brief C · Network filesystems, the single-host lease, and maintenance

- **Stream:** C · Store. **Branch:** `s/C/nfs-and-maintenance`. **Paths:** `crates/store/*`,
  `crates/store/src/**`, `crates/store/tests/**`, `crates/store/benches/**`,
  `crates/store/migrations/01*`.
- **First read:** [README.md](README.md), then `docs/build/streams/C.md` (work packages 4 and
  5), ADR-0004, ADR-0009, the merged briefs [C-open-and-log](C-open-and-log.md),
  [C-store-hardening](C-store-hardening.md) and [C-projections](C-projections.md), and
  `crates/store` on `main`.

## Why

On HPC clusters the home directory is usually NFS, Lustre or GPFS. SQLite's WAL mode needs
shared memory, which network filesystems don't provide safely, and two hosts writing one file
corrupt it. The hub must detect this and protect the file.

## What to build

1. **Detect the filesystem** of the store's folder: `fn detect(dir: &Path) -> FsKind`, with
   `FsKind` being `Local`, `Network { name }` or `Unknown { name }`.
   - **Linux:** `statfs` `f_type`. **macOS:** `f_fstypename`. Use `rustix` at the workspace's
     exact version, as `pitcrew-auth` does, with no unsafe.
   - Use an **allowlist of known-local types**: ext2/3/4, xfs, btrfs, zfs, tmpfs, f2fs,
     bcachefs, overlayfs, apfs, hfs and similar. `Unknown`, any `fuse*` type, `nfs*`, `cifs`,
     `smb*`, `lustre`, `gpfs`, `beegfs`, `9p`, `virtiofs`, `gfs2`, `ocfs2` and `vboxsf` are all
     treated as network.
   - **Windows:** UNC paths (`\\server\share`, `\\?\UNC\…`) are network. A drive letter is
     local unless the caller overrides it. No unsafe; document the limitation.
   - `StoreOptions` gains `fs: FsMode`, being `Auto`, `Local` or `Network`, so callers and tests
     can force the result.
2. **Network mode:**
   - Settings: `journal_mode=DELETE`, `locking_mode=EXCLUSIVE`, and the same `synchronous`
     setting as now.
   - The reader connection from `C-projections` must still work in this mode, or be disabled
     with reads going through the writer. Choose one, and document and test it.
   - **The single-host lease:** a lease file next to the database, e.g. `<db>.lease`.
     - Contents: host name, pid, a random owner id, and an expiry.
     - Written atomically (temp file + rename in the same folder), with private permissions.
     - `open` takes the lease, or fails with a clear `Error::Leased { host, pid, until }`.
     - Take over only after expiry. On the **same host**, also take over once the pid is gone
       (check liveness without unsafe; if that isn't possible portably, rely on expiry).
     - The owner renews the lease. Provide a `Lease` handle with `renew()` plus a documented
       interval (for example a 60 s lease, renewed every 20 s), or a small renew thread that
       stops on drop. Choose one and say why.
     - Before renewing, check the file still holds our owner id. If someone took over (our
       clock stalled or we were suspended), stop writing: every later append fails with
       `Error::LeaseLost`.
     - Release on drop. Time comes from an injectable clock, so tests can expire leases.
   - Local mode keeps WAL and takes no lease.
3. **Maintenance:**
   - `snapshot(path)` uses `VACUUM INTO` to a new file, and works while the store is in use. If
     you want rusqlite's `backup` feature, that is a root `Cargo.toml` change: stop and ask in
     your report.
   - `integrity_check()` returns a clear result: `quick_check` by default, a `full` option for
     `integrity_check`.
   - `export(writer)` writes the event log as JSON lines. `import(reader)` loads it into an
     **empty** store and rebuilds the projections. An import is a new log, so it gets a new
     `log_id`. Both are meant for tests and support, not sync.

## Acceptance

- **Detection:** unit tests for the type mapping (magic numbers and names, including `UNKNOWN`
  and `fuseblk`), and one real test on this machine's temp dir (local). The forced modes work.
- **Network mode:** the journal settings are asserted after open, and appends, reads and
  projections work.
- **The lease:**
  - a second open of the same file in network mode fails with `Leased`;
  - an expired lease is taken over;
  - renewal works;
  - a taken-over owner gets `LeaseLost` on its next append;
  - drop releases the lease;
  - a torn or garbage lease file is handled safely (treated as expired only after its mtime is
    older than the lease length).
- **Maintenance:**
  - a snapshot opens with identical events and projections;
  - a corrupted copy (bytes flipped mid-file) fails `integrity_check`;
  - export then import gives the same events (ids, bodies, order) and the same projection
    tables.
- No unwrap or expect in library code, and no unsafe.

## Out of scope

Choosing the node-local state folder (the daemon does that), hub replication or sync, and a
reader pool.
