//! Building the remote command line, and checking host names.
//!
//! ssh hands the remote side one string, which the user's **login shell** parses. That shell
//! may be sh, bash, zsh, ksh, fish, csh or tcsh, and they disagree about quoting: fish reads
//! `\\` and `\'` inside single quotes as escapes, and csh and tcsh expand `!` inside single
//! quotes and refuse a newline there. So the command travels in one fixed, shell-neutral
//! wrapper:
//!
//! ```text
//! /bin/sh -c 'unset -f printf 2>/dev/null; eval "$(printf "\ooo\ooo…")"'
//! ```
//!
//! Each `\ooo` is one byte of the POSIX command line, as three octal digits. The string the
//! login shell sees is therefore the wrapper's fixed characters plus backslashes that are each
//! followed by a digit. It has no `\\`, no `\'`, no `!`, no newline, no quote inside the single
//! quotes, and its only `$` is the wrapper's. Every shell named above hands the single-quoted
//! part to `/bin/sh` unchanged. There `printf` turns the escapes back into the command line, and
//! `eval` runs it with POSIX semantics. A `printf` function imported from the environment (bash
//! imports exported ones) is dropped first, so it cannot stand in for the decoding; `unset` and
//! `eval` are special built-ins, which a POSIX shell does not let a function replace.
//!
//! The command line quotes each argument with POSIX single quotes (a literal `'` is written
//! `'\''`). The command name is always quoted, so it is never a reserved word, an assignment or
//! an option to `eval`.
//!
//! Needs `/bin/sh` with `$(…)` on the remote side: every Linux, macOS and BSD has it.
//!
//! **Unsupported and unsafe:** login shells whose single quotes are not literal for the
//! characters above. xonsh may decode `\ooo` itself; `printf` would then read a `\047` from the
//! command as a real quote, and argv could break out. [`crate::Ssh::probe`] refuses such hosts.

use crate::SshError;

/// The wrapper, before and after the escapes.
const HEAD: &str = "/bin/sh -c 'unset -f printf 2>/dev/null; eval \"$(printf \"";
const TAIL: &str = "\")\"'";

/// The longest command [`remote_command`] builds. Each byte of the POSIX command line takes four.
/// - Unix: Linux limits a single argument to 128 KiB, and the login shell receives the command
///   as one; the command line itself can be about 32 KiB.
/// - Windows: `CreateProcess` limits the whole ssh command line to 32,767 characters, so the
///   command gets 30,000 and ssh's path and options the rest; the command line itself about
///   7,500.
pub const MAX_REMOTE_COMMAND: usize = if cfg!(windows) {
    30_000
} else {
    128 * 1024 - 1
};

// On Windows, at least 2 KiB of the 32,767 characters stay for the program path and options.
const _: () = assert!(!cfg!(windows) || MAX_REMOTE_COMMAND + 2 * 1024 <= 32_767);

/// Characters that never need quoting. `=` is left out on purpose: an unquoted `A=b` in first
/// position is an assignment, not a command. `~`, `#`, `*`, `?`, `[`, `{` are left out too.
fn is_plain(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '/' | ',' | ':' | '@' | '+' | '%')
}

/// Quotes one word for a POSIX shell.
#[must_use]
pub fn sh_quote(word: &str) -> String {
    if !word.is_empty() && word.chars().all(is_plain) {
        return word.to_owned();
    }
    always_quote(word)
}

fn always_quote(word: &str) -> String {
    let mut out = String::with_capacity(word.len() + 2);
    out.push('\'');
    for c in word.chars() {
        if c == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

/// Joins `argv` into one POSIX command line: the command name always quoted, every other
/// argument quoted when it needs it. This is what the remote `/bin/sh` runs; send it with
/// [`remote_command`], never as it is.
///
/// # Errors
/// `argv` is empty, or an argument contains a NUL byte (which no process can receive).
pub fn posix_command<S: AsRef<str>>(argv: &[S]) -> Result<String, SshError> {
    if argv.is_empty() {
        return Err(SshError::InvalidArgument("the command is empty".to_owned()));
    }
    let mut words = Vec::with_capacity(argv.len());
    for (i, arg) in argv.iter().enumerate() {
        let arg = arg.as_ref();
        if arg.contains('\0') {
            return Err(SshError::InvalidArgument(
                "an argument contains a NUL byte".to_owned(),
            ));
        }
        words.push(if i == 0 {
            always_quote(arg)
        } else {
            sh_quote(arg)
        });
    }
    Ok(words.join(" "))
}

/// The string to hand ssh for running `argv` under any login shell: [`posix_command`] inside the
/// shell-neutral wrapper described in the module docs.
///
/// # Errors
/// As [`posix_command`], or the result is longer than [`MAX_REMOTE_COMMAND`].
pub fn remote_command<S: AsRef<str>>(argv: &[S]) -> Result<String, SshError> {
    let line = posix_command(argv)?;
    // `$(…)` drops trailing newlines; the line ends with a quote or a plain character.
    debug_assert!(!line.ends_with('\n'));
    let len = HEAD.len() + 4 * line.len() + TAIL.len();
    if len > MAX_REMOTE_COMMAND {
        return Err(SshError::InvalidArgument(format!(
            "the command is too long ({len} bytes once wrapped; the limit is {MAX_REMOTE_COMMAND})"
        )));
    }
    let mut out = String::with_capacity(len);
    out.push_str(HEAD);
    for byte in line.bytes() {
        out.push('\\');
        for shift in [6, 3, 0] {
            out.push(char::from(b'0' + ((byte >> shift) & 7)));
        }
    }
    out.push_str(TAIL);
    Ok(out)
}

/// Whether `c` may appear in a host name given to ssh.
fn is_host_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '%' | '[' | ']' | '@' | '-')
}

/// Checks a host name (an alias from `~/.ssh/config`, a DNS name, an address, or `user@host`)
/// before it is put on ssh's command line. Only `A-Z a-z 0-9 . _ : % [ ] @ -` are allowed:
/// older OpenSSH (e.g. 9.5, bundled with Windows) passes other characters on to
/// `ProxyCommand %h` and `Match exec`, where a shell would read them. It must not look like an
/// option, before or after `@`.
///
/// # Errors
/// [`SshError::InvalidHost`] naming the problem.
pub fn validate_host(host: &str) -> Result<(), SshError> {
    let problem = if host.is_empty() {
        Some("it is empty".to_owned())
    } else if host.starts_with('-') {
        Some("it starts with '-'".to_owned())
    } else if host.contains("@-") {
        Some("the part after '@' starts with '-'".to_owned())
    } else {
        host.chars()
            .find(|&c| !is_host_char(c))
            .map(|c| format!("it contains {c:?}"))
    };
    match problem {
        Some(why) => Err(SshError::InvalidHost(format!("{host:?}: {why}"))),
        None => Ok(()),
    }
}

/// A model of how a POSIX shell splits a simple command into words: blanks separate words,
/// single quotes are literal, double quotes allow `\` before `$`, `` ` ``, `"`, `\` and newline,
/// and a bare `\` escapes the next character. Anything that would make the shell do more than
/// split (expansions, operators, globs, comments, tilde) is an error, so a round trip through
/// this model proves the quoting leaves nothing for the shell to interpret.
#[cfg(test)]
pub(crate) fn sh_split(line: &str) -> Result<Vec<String>, String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut in_word = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            ' ' | '\t' | '\n' => {
                if in_word {
                    words.push(std::mem::take(&mut word));
                    in_word = false;
                }
            }
            '\'' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(c) => word.push(c),
                        None => return Err("unterminated single quote".to_owned()),
                    }
                }
            }
            '"' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            Some(c @ ('$' | '`' | '"' | '\\')) => word.push(c),
                            Some('\n') => {}
                            Some(c) => {
                                word.push('\\');
                                word.push(c);
                            }
                            None => return Err("unterminated double quote".to_owned()),
                        },
                        Some('$' | '`') => return Err("expansion inside double quotes".to_owned()),
                        Some(c) => word.push(c),
                        None => return Err("unterminated double quote".to_owned()),
                    }
                }
            }
            '\\' => match chars.next() {
                Some('\n') => {}
                Some(c) => {
                    in_word = true;
                    word.push(c);
                }
                None => return Err("trailing backslash".to_owned()),
            },
            '$' | '`' | ';' | '&' | '|' | '<' | '>' | '(' | ')' | '*' | '?' | '[' | '{' | '!'
            | '^' => {
                return Err(format!("unquoted {c:?}"));
            }
            '#' | '~' if !in_word => return Err(format!("unquoted {c:?} at word start")),
            c if c.is_control() => return Err(format!("unquoted control character {c:?}")),
            c => {
                in_word = true;
                word.push(c);
            }
        }
    }
    if in_word {
        words.push(word);
    }
    Ok(words)
}

/// Login-shell families, for [`login_split`].
#[cfg(test)]
#[derive(Clone, Copy, Debug)]
pub(crate) enum Dialect {
    /// sh, bash, dash, ksh, zsh: single quotes are literal.
    Posix,
    /// fish: `\\` and `\'` are escapes inside single quotes.
    Fish,
    /// csh, tcsh: `!` is expanded inside single quotes, and a newline there is an error.
    Csh,
}

/// A model of how each login-shell family splits a line of plain words and single-quoted words.
/// Anything else outside quotes is an error: the wrapper needs nothing more.
#[cfg(test)]
pub(crate) fn login_split(line: &str, dialect: Dialect) -> Result<Vec<String>, String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut in_word = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            ' ' => {
                if in_word {
                    words.push(std::mem::take(&mut word));
                    in_word = false;
                }
            }
            '\'' => {
                in_word = true;
                loop {
                    match (chars.next(), dialect) {
                        (Some('\''), _) => break,
                        (Some('\\'), Dialect::Fish)
                            if matches!(chars.peek(), Some('\\' | '\'')) =>
                        {
                            word.extend(chars.next());
                        }
                        (Some('!'), Dialect::Csh) => return Err("history expansion".to_owned()),
                        (Some('\n'), Dialect::Csh) => return Err("newline in quotes".to_owned()),
                        (Some(c), _) => word.push(c),
                        (None, _) => return Err("unterminated single quote".to_owned()),
                    }
                }
            }
            c if c.is_ascii_alphanumeric() || matches!(c, '/' | '-' | '.' | '_') => {
                in_word = true;
                word.push(c);
            }
            c => return Err(format!("unquoted {c:?}")),
        }
    }
    if in_word {
        words.push(word);
    }
    Ok(words)
}

/// Undoes [`remote_command`], strictly: the wrapper exactly, then only `\ooo` escapes. Returns
/// the POSIX command line, or `None` if `wrapped` has any other shape.
#[cfg(test)]
pub(crate) fn unwrap_remote(wrapped: &str) -> Option<String> {
    let escapes = wrapped.strip_prefix(HEAD)?.strip_suffix(TAIL)?.as_bytes();
    if escapes.is_empty() || escapes.len() % 4 != 0 {
        return None;
    }
    let mut bytes = Vec::with_capacity(escapes.len() / 4);
    for chunk in escapes.chunks(4) {
        let [b'\\', a @ b'0'..=b'3', b @ b'0'..=b'7', c @ b'0'..=b'7'] = *chunk else {
            return None;
        };
        bytes.push(((a - b'0') << 6) | ((b - b'0') << 3) | (c - b'0'));
    }
    String::from_utf8(bytes).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn plain_words_stay_plain() {
        assert_eq!(sh_quote("ls"), "ls");
        assert_eq!(sh_quote("-la"), "-la");
        assert_eq!(sh_quote("/usr/bin/env"), "/usr/bin/env");
    }

    #[test]
    fn hostile_words_are_quoted() {
        let cases = [
            ("", "''"),
            ("it's", "'it'\\''s'"),
            ("a;b", "'a;b'"),
            ("$(rm -rf ~)", "'$(rm -rf ~)'"),
            ("`id`", "'`id`'"),
            ("a\nb", "'a\nb'"),
            ("A=b", "'A=b'"),
            ("~", "'~'"),
            ("*", "'*'"),
            ("#x", "'#x'"),
            ("héllo", "'héllo'"),
        ];
        for (input, want) in cases {
            assert_eq!(sh_quote(input), want, "{input:?}");
        }
    }

    #[test]
    fn the_command_name_is_always_quoted() {
        assert_eq!(posix_command(&["ls", "-la"]).unwrap(), "'ls' -la");
        assert_eq!(posix_command(&["-rf"]).unwrap(), "'-rf'");
        assert_eq!(posix_command(&["if", "x"]).unwrap(), "'if' x");
    }

    #[test]
    fn a_command_round_trips() {
        let argv = ["rm", "-rf", "it's; $(x) `y`\n", "--", "日本"];
        let line = posix_command(&argv).unwrap();
        assert_eq!(sh_split(&line).unwrap(), argv);
        assert_eq!(
            unwrap_remote(&remote_command(&argv).unwrap()).unwrap(),
            line
        );
    }

    #[test]
    fn the_wrapper_looks_like_this() {
        assert_eq!(
            remote_command(&["echo", "a'b"]).unwrap(),
            r#"/bin/sh -c 'unset -f printf 2>/dev/null; eval "$(printf "\047\145\143\150\157\047\040\047\141\047\134\047\047\142\047")"'"#
        );
    }

    #[test]
    fn nul_empty_and_huge_commands_are_refused() {
        assert!(remote_command::<&str>(&[]).is_err());
        assert!(remote_command(&["a\0b"]).is_err());
        let big = "x".repeat(MAX_REMOTE_COMMAND / 4);
        assert!(matches!(
            remote_command(&["echo", &big]),
            Err(SshError::InvalidArgument(_))
        ));
        let fits = "x".repeat(MAX_REMOTE_COMMAND / 4 - 40);
        assert!(remote_command(&["echo", &fits]).is_ok());
    }

    #[test]
    fn host_names_are_checked() {
        for good in [
            "cluster",
            "user@login.example.org",
            "first.last@node-01",
            "10.0.0.1",
            "[::1]",
            "fe80::1%eth0",
            "gpu_node:2",
        ] {
            validate_host(good).unwrap();
        }
        for bad in [
            "",
            "-oProxyCommand=x",
            "user@-oProxyCommand=x",
            "a@b@-c",
            "a b",
            "a\tb",
            "a\nb",
            "a\u{7f}b",
            "a;b",
            "a$b",
            "a`b`",
            "a'b",
            "a\"b",
            "a|b",
            "a&b",
            "a(b)",
            "a/b",
            "a\\b",
            "a*b",
            "a!b",
            "a~b",
            "a{b}",
            "ä",
        ] {
            assert!(validate_host(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn the_model_refuses_what_a_shell_would_expand() {
        for line in ["a $b", "a;b", "`x`", "\"$x\"", "~/x", "a*", "'open"] {
            assert!(sh_split(line).is_err(), "{line:?}");
        }
        assert_eq!(sh_split(r#"a "b c" d\ e"#).unwrap(), ["a", "b c", "d e"]);
    }

    /// The dialect models read the review's attacks the way the real shells do.
    #[test]
    fn the_login_models_see_the_old_attacks() {
        // The old output for argv [`\'; touch pwned; #`]: fish ends the quote early.
        let old = r"'\'\''; touch pwned; #'";
        assert!(login_split(old, Dialect::Fish).is_err());
        assert!(login_split("'a!b'", Dialect::Csh).is_err());
        assert!(login_split("'a\nb'", Dialect::Csh).is_err());
        assert_eq!(
            login_split(r"x 'a\\b'", Dialect::Fish).unwrap(),
            ["x", r"a\b"]
        );
        assert_eq!(
            login_split(r"x 'a\\b'", Dialect::Posix).unwrap(),
            ["x", r"a\\b"]
        );
    }

    fn check_wrapped(argv: &[String]) -> Result<(), TestCaseError> {
        let wrapped = remote_command(argv).unwrap();
        // Nothing any login shell reads specially, apart from the wrapper's own characters.
        prop_assert!(wrapped.is_ascii());
        prop_assert!(!wrapped.contains("\\\\"), "{wrapped}");
        prop_assert!(!wrapped.contains("\\'"), "{wrapped}");
        prop_assert!(!wrapped.contains('!'));
        prop_assert!(!wrapped.contains('\n'));
        prop_assert!(!wrapped.chars().any(|c| c.is_ascii_control()));
        prop_assert_eq!(wrapped.matches('$').count(), 1);
        prop_assert_eq!(wrapped.matches('\'').count(), 2);
        // Every family of login shell passes the same script to /bin/sh.
        let inner = login_split(&wrapped, Dialect::Posix).unwrap();
        prop_assert_eq!(inner.len(), 3);
        prop_assert_eq!(&inner[..2], &["/bin/sh", "-c"]);
        for dialect in [Dialect::Fish, Dialect::Csh] {
            prop_assert_eq!(&login_split(&wrapped, dialect).unwrap(), &inner);
        }
        // ...which decodes to a command line that splits back into argv exactly.
        let line = unwrap_remote(&wrapped).unwrap();
        prop_assert!(!line.ends_with('\n'));
        prop_assert_eq!(&sh_split(&line).unwrap(), argv);
        Ok(())
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(512))]

        #[test]
        fn quoting_round_trips(argv in prop::collection::vec("[^\\x00]*", 1..6)) {
            let line = posix_command(&argv).unwrap();
            prop_assert_eq!(sh_split(&line).unwrap(), argv);
        }

        #[test]
        fn quoting_round_trips_shell_heavy_input(
            argv in prop::collection::vec("[ -~\\n\\t'\"\\\\$`;é]{0,12}", 1..6)
        ) {
            let line = posix_command(&argv).unwrap();
            prop_assert_eq!(sh_split(&line).unwrap(), argv);
        }

        /// Arbitrary argv: the wrapped command never contains a sequence that fish, csh or tcsh
        /// reads differently from sh.
        #[test]
        fn wrapped_commands_are_shell_neutral(argv in prop::collection::vec("[^\\x00]*", 1..6)) {
            check_wrapped(&argv)?;
        }

        /// The same, biased towards the characters that broke fish and csh.
        #[test]
        fn wrapped_commands_are_shell_neutral_for_hostile_input(
            argv in prop::collection::vec("[\\\\'!\\n\"$`;# a-z]{0,16}", 1..6)
        ) {
            check_wrapped(&argv)?;
        }
    }
}
