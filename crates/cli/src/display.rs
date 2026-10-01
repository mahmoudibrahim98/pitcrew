//! Making the daemon's text safe to print on a terminal.
//!
//! Titles, comments and messages come from other people and agents. In text output, control
//! characters (escape sequences could rewrite the terminal) and bidirectional-override
//! characters (they reorder what is shown) are removed. `--json` output is left exact.

/// Unicode bidirectional formatting characters: marks, embeddings, overrides and isolates.
fn is_bidi_control(c: char) -> bool {
    matches!(
        c,
        '\u{061C}' | '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}'
    )
}

/// One line: every control character is dropped, except that tabs and line breaks become spaces.
#[must_use]
pub fn line(text: &str) -> String {
    text.chars()
        .filter_map(|c| match c {
            '\t' | '\n' | '\r' => Some(' '),
            c if c.is_control() || is_bidi_control(c) => None,
            c => Some(c),
        })
        .collect()
}

/// Several lines: like [`line`], but line breaks and tabs are kept.
#[must_use]
pub fn text(text: &str) -> String {
    text.chars()
        .filter(|&c| c == '\n' || c == '\t' || !(c.is_control() || is_bidi_control(c)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_and_bidi_controls_are_removed() {
        let hostile = "Fix\u{1b}[2J\u{1b}]0;owned\u{7}\u{9b}31m it\u{202E}gnp.exe\u{2066}\u{7f}";
        assert_eq!(line(hostile), "Fix[2J]0;owned31m itgnp.exe");
        assert_eq!(line("a\tb\r\nc"), "a b  c");
        assert_eq!(text("a\tb\nc\u{1b}[1m\r"), "a\tb\nc[1m");
        assert_eq!(
            line("Plain, ünïcödé and emoji 🙂"),
            "Plain, ünïcödé and emoji 🙂"
        );
    }
}
