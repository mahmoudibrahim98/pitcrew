//! Models of how shells split a command line, for checking `pitcrew_remote::quote`. They are
//! written here from the shells' rules, independently of the crate's own test-only models (which
//! the fuzz crate cannot see), so the two can catch each other's mistakes.

/// How a POSIX shell splits a simple command into words: blanks separate words, single quotes are
/// literal, double quotes allow `\` before `$`, `` ` ``, `"`, `\` and newline, and a bare `\`
/// escapes the next character. Anything that would make the shell do more than split (expansions,
/// operators, globs, comments, a leading tilde, history) is an error: a round trip through this
/// model proves the quoting leaves nothing for the shell to interpret.
///
/// # Errors
/// The line has something a shell would interpret, or an unterminated quote.
pub fn sh_split(line: &str) -> Result<Vec<String>, String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut in_word = false;
    let mut chars = line.chars();
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
                        Some('$' | '`') => return Err("expansion in double quotes".to_owned()),
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
            '$' | '`' | ';' | '&' | '|' | '<' | '>' | '(' | ')' | '*' | '?' | '[' | ']' | '{'
            | '}' | '!' | '^' => return Err(format!("unquoted {c:?}")),
            '#' | '~' | '=' if !in_word => return Err(format!("unquoted {c:?} at a word start")),
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

/// Login-shell families that read single quotes differently.
#[derive(Clone, Copy, Debug)]
pub enum Dialect {
    /// sh, bash, dash, ksh, zsh: single quotes are literal.
    Posix,
    /// fish: `\\` and `\'` are escapes inside single quotes.
    Fish,
    /// csh, tcsh: `!` is history expansion even inside single quotes, and a newline there is an
    /// error.
    Csh,
}

/// How a login shell of `dialect` splits a line of plain words and single-quoted words. Anything
/// else outside quotes is an error: the wrapper needs nothing more.
///
/// # Errors
/// The line needs more than plain and single-quoted words, or the dialect reads it differently.
pub fn login_split(line: &str, dialect: Dialect) -> Result<Vec<String>, String> {
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

/// What `/bin/sh -c 'eval "$(printf "\ooo…")"'` hands to `eval`: the wrapper must be exactly
/// that, with only three-digit octal escapes inside, and the bytes must be UTF-8. `None` for any
/// other shape.
#[must_use]
pub fn unwrap_remote(wrapped: &str) -> Option<String> {
    const HEAD: &str = "/bin/sh -c 'eval \"$(printf \"";
    const TAIL: &str = "\")\"'";
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
