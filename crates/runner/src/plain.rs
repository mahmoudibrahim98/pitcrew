//! Checks for values from places the runner does not control (transcripts, hooks, hub commands)
//! before they reach a program's arguments or the runner's memory.
//!
//! A plain value starts with an ASCII letter or digit, so it can never be read as an option, and
//! is short and free of spaces and control characters.

/// The longest plain value, in bytes.
pub(crate) const MAX_LEN: usize = 128;

/// A CLI's session id (Claude's and Codex's UUIDs, OpenCode's `ses_…`): a letter or digit, then
/// up to 127 letters, digits, `.`, `_` or `-`.
pub(crate) fn is_id(s: &str) -> bool {
    plain(s, |c| matches!(c, b'.' | b'_' | b'-'))
}

/// A model name: as an id, plus `/` (OpenCode's `provider/model`), `:` (a tag), `@` (a version)
/// and `[`, `]` (Claude's `sonnet[1m]`).
pub(crate) fn is_model(s: &str) -> bool {
    plain(s, |c| {
        matches!(c, b'.' | b'_' | b'-' | b'/' | b':' | b'@' | b'[' | b']')
    })
}

fn plain(s: &str, extra: impl Fn(u8) -> bool) -> bool {
    let b = s.as_bytes();
    b.first().is_some_and(u8::is_ascii_alphanumeric)
        && b.len() <= MAX_LEN
        && b.iter().all(|&c| c.is_ascii_alphanumeric() || extra(c))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_and_models() {
        for id in [
            "2b6f1a8e-4c1d-4f5e-9a37-0c8d1e2f3a4b",
            "ses_01JB000000000000",
            "t1",
            "a.b",
            &"a".repeat(MAX_LEN),
        ] {
            assert!(is_id(id), "{id}");
        }
        for not in [
            "",
            "-",
            "--dangerously-skip-permissions",
            "-p",
            ".hidden",
            "_x",
            "a b",
            "a\nb",
            "a/b",
            "a=b",
            "ünï",
            &"a".repeat(MAX_LEN + 1),
        ] {
            assert!(!is_id(not), "{not:?}");
        }
        for model in [
            "opus",
            "claude-sonnet-4-5",
            "sonnet[1m]",
            "anthropic/claude-sonnet-4",
            "llama3:8b",
            "claude-x@20250101",
        ] {
            assert!(is_model(model), "{model}");
        }
        for not in ["", "-m", "--model", "a b", "a=b", "a;b", "a\tb"] {
            assert!(!is_model(not), "{not:?}");
        }
    }
}
