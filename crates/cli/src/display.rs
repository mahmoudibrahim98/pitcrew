//! Making the daemon's text safe to print on a terminal.
//!
//! Titles, comments and messages come from other people and agents. In text output, control
//! characters (escape sequences could rewrite the terminal), bidirectional-override characters
//! (they reorder what is shown) and other invisible Unicode format characters (they can hide or
//! spoof text without being seen) are removed. `--json` output is left exact.

/// Unicode bidirectional formatting characters: marks, embeddings, overrides and isolates.
/// (General Category `Cf`, like every character in [`is_invisible_format`].)
fn is_bidi_control(c: char) -> bool {
    matches!(
        c,
        '\u{061C}' | '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}'
    )
}

/// Other Unicode "format" characters (General Category `Cf`) that render as nothing: zero-width
/// space/joiners, the word joiner and invisible math operators, the BOM, interlinear-annotation
/// marks, and the language-tag block (used to smuggle hidden text after a visible emoji). A task
/// title made of these plus ordinary letters can look identical to another title while comparing
/// unequal, or hide extra instructions a reader would never see.
fn is_invisible_format(c: char) -> bool {
    matches!(
        c,
        '\u{00AD}'
            | '\u{200B}'..='\u{200D}'
            | '\u{2060}'..='\u{2064}'
            | '\u{FEFF}'
            | '\u{FFF9}'..='\u{FFFB}'
            | '\u{E0001}'
            | '\u{E0020}'..='\u{E007F}'
    )
}

fn is_hidden(c: char) -> bool {
    c.is_control() || is_bidi_control(c) || is_invisible_format(c)
}

/// One line: every control character is dropped, except that tabs and line breaks become spaces.
#[must_use]
pub fn line(text: &str) -> String {
    text.chars()
        .filter_map(|c| match c {
            '\t' | '\n' | '\r' => Some(' '),
            c if is_hidden(c) => None,
            c => Some(c),
        })
        .collect()
}

/// Several lines: like [`line`], but line breaks and tabs are kept.
#[must_use]
pub fn text(text: &str) -> String {
    text.chars()
        .filter(|&c| c == '\n' || c == '\t' || !is_hidden(c))
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

    #[test]
    fn invisible_format_characters_cannot_spoof_a_title() {
        // Zero-width space/joiners, the word joiner, a BOM, and a hidden emoji-tag payload, all
        // interleaved with plain text that must survive untouched.
        let hostile = "Looks\u{200B}Safe\u{200C}\u{200D}\u{2060}\u{FEFF}: run rm\u{E0020}\u{E007F}";
        assert_eq!(line(hostile), "LooksSafe: run rm");
        assert_eq!(text(hostile), "LooksSafe: run rm");
        // A soft hyphen and an invisible math operator are stripped too.
        assert_eq!(line("sub\u{00AD}title\u{2062}x"), "subtitlex");
    }
}
