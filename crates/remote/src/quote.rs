//! Building the remote command line, and checking host names.
//!
//! ssh hands the remote side a single string, which the user's login shell parses. Every
//! argument is therefore quoted with POSIX single quotes: inside them nothing is special, and a
//! literal `'` is written as `'\''`. This is correct for sh, bash, dash, ksh and zsh. csh and
//! tcsh also accept it, except that they reject a newline inside quotes; fish reads `\\` inside
//! single quotes as one backslash. Commands meant for any login shell should avoid newlines and
//! backslashes, or wrap themselves in `sh -c` with a single-line script (as the probe does).

use crate::SshError;

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

/// Joins `argv` into one command line for the remote shell.
///
/// # Errors
/// `argv` is empty, or an argument contains a NUL byte (which no process can receive).
pub fn remote_command<S: AsRef<str>>(argv: &[S]) -> Result<String, SshError> {
    if argv.is_empty() {
        return Err(SshError::InvalidArgument("the command is empty".to_owned()));
    }
    let mut words = Vec::with_capacity(argv.len());
    for arg in argv {
        let arg = arg.as_ref();
        if arg.contains('\0') {
            return Err(SshError::InvalidArgument(
                "an argument contains a NUL byte".to_owned(),
            ));
        }
        words.push(sh_quote(arg));
    }
    Ok(words.join(" "))
}

/// Checks a host name (an alias from `~/.ssh/config`, a DNS name, or `user@host`) before it is
/// put on ssh's command line. It must not look like an option, and must not contain whitespace
/// or control characters.
///
/// # Errors
/// [`SshError::InvalidHost`] naming the problem.
pub fn validate_host(host: &str) -> Result<(), SshError> {
    let problem = if host.is_empty() {
        Some("it is empty")
    } else if host.starts_with('-') {
        Some("it starts with '-'")
    } else if host.chars().any(char::is_whitespace) {
        Some("it contains whitespace")
    } else if host.chars().any(char::is_control) {
        Some("it contains a control character")
    } else {
        None
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
    fn a_command_round_trips() {
        let argv = ["rm", "-rf", "it's; $(x) `y`\n", "--", "日本"];
        let line = remote_command(&argv).unwrap();
        assert_eq!(sh_split(&line).unwrap(), argv);
    }

    #[test]
    fn nul_and_empty_commands_are_refused() {
        assert!(remote_command::<&str>(&[]).is_err());
        assert!(remote_command(&["a\0b"]).is_err());
    }

    #[test]
    fn host_names_are_checked() {
        for good in ["cluster", "user@login.example.org", "10.0.0.1", "[::1]"] {
            validate_host(good).unwrap();
        }
        for bad in ["", "-oProxyCommand=x", "a b", "a\tb", "a\nb", "a\u{7f}b"] {
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

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(512))]

        #[test]
        fn quoting_round_trips(argv in prop::collection::vec("[^\\x00]*", 1..6)) {
            let line = remote_command(&argv).unwrap();
            prop_assert_eq!(sh_split(&line).unwrap(), argv);
        }

        #[test]
        fn quoting_round_trips_shell_heavy_input(
            argv in prop::collection::vec("[ -~\\n\\t'\"\\\\$`;é]{0,12}", 1..6)
        ) {
            let line = remote_command(&argv).unwrap();
            prop_assert_eq!(sh_split(&line).unwrap(), argv);
        }
    }
}
