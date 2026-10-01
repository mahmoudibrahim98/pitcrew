//! `pitcrew_remote::quote` on arbitrary argv: the quoting that carries every command PitCrew runs
//! on a remote machine through the user's login shell, whatever that shell is.
//!
//! Input: the argv, its words separated by NUL bytes (no process can receive a NUL).
//!
//! Checks, against the shell models in `fuzz/src/shell.rs`:
//! - `sh_quote(word)` splits back to exactly `word` in a POSIX shell, with nothing expanded;
//! - `posix_command(argv)` splits back to exactly `argv`, and its command name is always quoted;
//! - `remote_command(argv)`, when it fits, uses only the wrapper's own characters and octal
//!   escapes (no `\\`, no `\'`, no `!`, no newline or other control character, one `$`, two
//!   quotes), every login-shell family (POSIX, fish, csh) hands `/bin/sh` the same script, and
//!   that script decodes to exactly the POSIX command line; when it does not fit, it is refused;
//! - a host name `validate_host` accepts has only the allowed characters and cannot be read as an
//!   option.
#![no_main]

use libfuzzer_sys::fuzz_target;
use pitcrew_fuzz::shell::{Dialect, login_split, sh_split, unwrap_remote};
use pitcrew_remote::quote::{
    MAX_REMOTE_COMMAND, posix_command, remote_command, sh_quote, validate_host,
};

fuzz_target!(|input: &[u8]| {
    let text = String::from_utf8_lossy(input);
    let argv: Vec<&str> = text.split('\0').collect();

    for word in &argv {
        let quoted = sh_quote(word);
        assert_eq!(
            sh_split(&quoted).as_deref(),
            Ok(&[(*word).to_owned()][..]),
            "sh_quote({word:?}) = {quoted:?} does not split back"
        );
        check_host(word);
    }

    let line = posix_command(&argv).expect("argv without NUL is a command");
    assert!(line.starts_with('\''), "the command name is not quoted");
    let words = sh_split(&line).unwrap_or_else(|e| panic!("{line:?} does not split: {e}"));
    assert_eq!(words, argv, "the command line splits to other words");

    match remote_command(&argv) {
        Ok(wrapped) => {
            assert!(wrapped.len() <= MAX_REMOTE_COMMAND, "over the length limit");
            assert!(wrapped.is_ascii(), "non-ASCII in {wrapped:?}");
            assert!(!wrapped.contains("\\\\") && !wrapped.contains("\\'"));
            assert!(!wrapped.contains('!'), "csh would expand '!'");
            assert!(!wrapped.chars().any(|c| c.is_ascii_control()));
            assert_eq!(wrapped.matches('$').count(), 1);
            assert_eq!(wrapped.matches('\'').count(), 2);
            let inner = login_split(&wrapped, Dialect::Posix)
                .unwrap_or_else(|e| panic!("a POSIX login shell rejects it: {e}"));
            assert_eq!(inner.len(), 3);
            assert_eq!(&inner[..2], ["/bin/sh", "-c"]);
            for dialect in [Dialect::Fish, Dialect::Csh] {
                assert_eq!(
                    login_split(&wrapped, dialect).as_ref(),
                    Ok(&inner),
                    "{dialect:?} reads the wrapper differently"
                );
            }
            assert_eq!(
                unwrap_remote(&wrapped).as_deref(),
                Some(line.as_str()),
                "the wrapper does not decode to the command line"
            );
        }
        Err(_) => {
            let wrapped_len = "/bin/sh -c 'eval \"$(printf \"".len() + 4 * line.len() + 4;
            assert!(
                wrapped_len > MAX_REMOTE_COMMAND,
                "a command that fits ({wrapped_len} bytes) was refused"
            );
        }
    }
});

fn check_host(host: &str) {
    if validate_host(host).is_err() {
        return;
    }
    assert!(!host.is_empty());
    assert!(!host.starts_with('-'), "{host:?} reads as an option");
    assert!(
        !host.contains("@-"),
        "{host:?} reads as an option after '@'"
    );
    assert!(
        host.chars().all(|c| c.is_ascii_alphanumeric()
            || matches!(c, '.' | '_' | ':' | '%' | '[' | ']' | '@' | '-')),
        "{host:?} has a character a shell or ProxyCommand could read"
    );
}
