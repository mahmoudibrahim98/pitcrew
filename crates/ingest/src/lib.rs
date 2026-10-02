//! # pitcrew-ingest
//!
//! Transcript parsers for Claude Code, Codex and OpenCode (incremental, by offset), and the machine scan.
//!
//! **Owned by stream A.** The work packages are in `docs/build/streams/A.md`. Build against
//! `pitcrew-protocol` and `pitcrew-interfaces` only, never another stream's internals.
//!
//! - [`claude::ClaudeAdapter`]: Claude Code transcripts (`~/.claude/projects/*/*.jsonl`).
//! - [`codex::CodexAdapter`]: Codex CLI rollouts (`~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl`).
//! - [`opencode::OpenCodeAdapter`]: OpenCode sessions in its SQLite store
//!   (`~/.local/share/opencode/opencode.db`).
//! - [`scan::scan`]: a read-only, parallel scan of a machine's agent history for onboarding and
//!   "scan again": counts, plus suggested projects and workstreams.
//!
//! Transcripts are attacker-controllable text: every parser bounds its allocations, skips lines it
//! cannot use, and exposes a `parse_line` function so it can be fuzzed on its own. A transcript is
//! read only if it is still a regular file when it is opened, not a link to another file (see
//! [`NotRegularFile`]).

#![forbid(unsafe_code)]

mod bound;
pub mod claude;
pub mod codex;
mod jsonl;
mod lines;
mod open;
pub mod opencode;
mod patch;
pub mod scan;
mod text;
mod time;

pub use jsonl::{MAX_REPORTED_SKIPS, ReadReport};
pub use lines::{MAX_LINE_BYTES, SkipReason, SkippedLine};
pub use open::{FileKind, NotRegularFile, refusal};

/// The protocol version this crate was built against.
pub const PROTOCOL_VERSION: u32 = pitcrew_protocol::PROTOCOL_VERSION;

/// The user's home folder, where the agents keep theirs (see [`home_from`]).
fn user_home() -> Option<std::path::PathBuf> {
    home_from(|name| std::env::var_os(name))
}

/// The home folder `var` names. On Windows `USERPROFILE`, else `HOME`: the agents find their homes
/// there (Claude Code's and OpenCode's `os.homedir()` read `USERPROFILE`, Codex asks Windows for
/// the profile), and a `HOME` set for Git or another Unix tool can name another folder. Elsewhere
/// `HOME`, else `USERPROFILE`. An empty variable counts as unset.
fn home_from(var: impl Fn(&str) -> Option<std::ffi::OsString>) -> Option<std::path::PathBuf> {
    let (first, second) = if cfg!(windows) {
        ("USERPROFILE", "HOME")
    } else {
        ("HOME", "USERPROFILE")
    };
    let set = |name| var(name).filter(|d| !d.is_empty());
    set(first)
        .or_else(|| set(second))
        .map(std::path::PathBuf::from)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::ffi::OsString;
    use std::path::PathBuf;

    fn home(pairs: &[(&str, &str)]) -> Option<PathBuf> {
        let env: HashMap<&str, OsString> = pairs.iter().map(|(k, v)| (*k, v.into())).collect();
        super::home_from(|name| env.get(name).cloned())
    }

    #[test]
    fn the_home_is_the_platforms_own_variable_first() {
        let both = [("HOME", "/h/unix"), ("USERPROFILE", "/h/profile")];
        let first = if cfg!(windows) {
            "/h/profile"
        } else {
            "/h/unix"
        };
        assert_eq!(home(&both), Some(PathBuf::from(first)));
        assert_eq!(home(&[("HOME", "/h/unix")]), Some(PathBuf::from("/h/unix")));
        assert_eq!(
            home(&[("USERPROFILE", "/h/profile")]),
            Some(PathBuf::from("/h/profile"))
        );
        // An empty variable is unset.
        let (empty, other, fallback) = if cfg!(windows) {
            ("USERPROFILE", "HOME", "/h/unix")
        } else {
            ("HOME", "USERPROFILE", "/h/profile")
        };
        assert_eq!(
            home(&[(empty, ""), (other, fallback)]),
            Some(PathBuf::from(fallback))
        );
        assert_eq!(home(&[(empty, "")]), None);
        assert_eq!(home(&[]), None);
    }
}
