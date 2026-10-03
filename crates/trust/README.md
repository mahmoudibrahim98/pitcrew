# pitcrew-trust

The check every program PitCrew launches passes, and on Windows the one copy of the code that
secures and checks its named pipes.

**Owned by stream 0.** Small on purpose: no async, nothing beyond the platform's own calls
(`rustix` on Unix, `windows-sys` and tokio's pipe constructor on Windows).

## `check_trusted`: not a planted binary

`check_trusted(path)` holds when the program (or a file PitCrew reads) is one only the person or
the system could have put there:

- **Unix:** the file it resolves to, that file's directory, and the directory of the path as given
  each belong to root or to the effective user, and none can be written by group or others
  (sticky `1777` folders included). Whoever can write a directory on the way can swap the program,
  or the link to it.
- **Windows:** a file with a `Zone.Identifier` stream (downloaded from the web and not unblocked)
  is refused. Its owner is not checked.

It holds when it is asked, so callers check just before they start the program:

- the desktop checks `pitcrewd` (`apps/desktop/src-tauri/src/daemon/locate.rs`) and its remote
  helpers;
- the daemon checks `pitcrew-ptyd` when it chooses its terminals' runtime (`Plan::choose`: a ptyd
  that fails is warned about and not used, as if it were missing), and `pitcrew_runtime`'s PTY
  runtime checks it again just before each launch (`pty::launch::launch`), since ptyd is started
  at the first terminal, possibly hours later.

Signing, and checks beyond these, are not done here.

## `windows`: pipe security (Windows only)

The API's pipe (`pitcrew-api`), the askpass pipe (`pitcrew-remote`) and pitcrew-ptyd's pipe
(`pitcrew-runtime`) used to carry three copies of this code. Each keeps its own policy (the
descriptor it asks for, what it checks) and calls these:

- **who:** `current_user_sid`, `current_identity` (user and integrity level), `is_elevated`,
  `default_owner_sid` (the owner a process's objects get when they name none), `token_identity`
  and `token_user_sid` for a token the caller opened;
- **an object, through any handle with `READ_CONTROL`:** `owner_sid`, `dacl` (a `Dacl` of `Ace`s,
  each SID in full and each access mask as stored, so `GA` granted on a pipe reads back as
  `FILE_ALL_ACCESS`), `label_integrity`;
- **text:** `canonical_sid` (a SID from `S-1-…` or an SDDL alias such as `BA`), and in `sddl`
  (built and tested on every platform) `parse_label`, `integrity_rid`, `label`;
- **making pipes:** `SecurityDescriptor::from_sddl(text)` and `create_pipe(options, name)`.

DACLs are compared by SID, never as SDDL text, which names some SIDs by alias (the built-in
Administrator, as CI's Windows runner is, as `LA`).

`src/windows.rs` is the crate's only `unsafe` code, with a `SAFETY` comment on every block;
`lib.rs` denies it elsewhere, and `tests/unsafe_guard.rs` fails if another file allows it.

## Tests

`cargo test -p pitcrew-trust`. On Unix: owner and mode, a group- or world-writable program or
folder, a link into a folder others can write and a link in one, a missing file; the labels'
text. On Windows: a downloaded program refused; the current user, integrity level and default
owner; SIDs from text and aliases; a pipe made from a descriptor read back through the server's
handle and a client's (owner, every DACL entry with its mask, a denial, the label); a pipe with
default security; bad descriptors refused. The Windows tests run on CI's (elevated) runner and
natively.
