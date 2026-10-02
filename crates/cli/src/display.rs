//! Making the daemon's text safe to print on a terminal.
//!
//! Titles, comments and messages come from other people and agents. In text output, control
//! characters (escape sequences could rewrite the terminal), bidirectional-override characters
//! (they reorder what is shown) and other invisible Unicode format characters (they can hide or
//! spoof text without being seen) are removed. `--json` output is left exact.
//!
//! The invisible and bidirectional characters are `pitcrew_protocol::text::is_hidden`, the one set
//! the recap engine and the GitHub and Jira syncs drop too. This module adds control characters.
//! The line and paragraph separators in the set (U+2028, U+2029) break a line, so [`line`] and
//! [`text`] show them as a space rather than dropping them, as the recap engine's `clean` does.

use pitcrew_protocol::text::is_line_separator;

fn is_hidden(c: char) -> bool {
    c.is_control() || pitcrew_protocol::text::is_hidden(c)
}

/// One line: every control character is dropped, except that tabs, line breaks and the Unicode
/// line/paragraph separators become spaces.
#[must_use]
pub fn line(text: &str) -> String {
    text.chars()
        .filter_map(|c| match c {
            '\t' | '\n' | '\r' => Some(' '),
            c if is_line_separator(c) => Some(' '),
            c if is_hidden(c) => None,
            c => Some(c),
        })
        .collect()
}

/// Several lines: like [`line`], but line breaks and tabs are kept.
#[must_use]
pub fn text(text: &str) -> String {
    text.chars()
        .filter_map(|c| match c {
            '\n' | '\t' => Some(c),
            c if is_line_separator(c) => Some(' '),
            c if is_hidden(c) => None,
            c => Some(c),
        })
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

    #[test]
    fn the_union_characters_are_dropped_too() {
        // Everything this stream's fix adds beyond what was already here: the combining grapheme
        // joiner, the Mongolian vowel separator, the Hangul filler characters, variation
        // selectors (both blocks) and the start of the tag-character block (`U+E0000`, not just
        // `U+E0001` onward).
        assert_eq!(line("a\u{034F}b"), "ab", "combining grapheme joiner");
        assert_eq!(line("a\u{180E}b"), "ab", "Mongolian vowel separator");
        assert_eq!(
            line("a\u{115F}\u{1160}\u{3164}\u{FFA0}b"),
            "ab",
            "Hangul filler characters"
        );
        assert_eq!(
            line("a\u{FE0F}\u{E0100}b"),
            "ab",
            "variation selectors, both blocks"
        );
        assert_eq!(line("a\u{E0000}b"), "ab", "the start of the tag block");
    }

    #[test]
    fn line_and_paragraph_separators_become_a_space_not_nothing() {
        assert_eq!(line("Done.\u{2028}Next"), "Done. Next");
        assert_eq!(text("Done.\u{2029}Next"), "Done. Next");
        assert_eq!(line("a\u{2028}\u{2029}b"), "a  b");
    }

    /// The hidden set, written out in full: control characters plus
    /// `pitcrew_protocol::text::is_hidden`, which the recap engine and the GitHub and Jira syncs
    /// drop too. Pinned in each of them (without the controls): change them together.
    const HIDDEN: &[(u32, u32)] = &[
        (0x0000, 0x001F), // C0 controls
        (0x007F, 0x009F), // DEL and the C1 controls
        (0x00AD, 0x00AD),
        (0x034F, 0x034F),
        (0x061C, 0x061C),
        (0x115F, 0x1160),
        (0x180E, 0x180E),
        (0x200B, 0x200F),
        (0x2028, 0x2029),
        (0x202A, 0x202E),
        (0x2060, 0x2064),
        (0x2066, 0x2069),
        (0x3164, 0x3164),
        (0xFE00, 0xFE0F),
        (0xFEFF, 0xFEFF),
        (0xFFA0, 0xFFA0),
        (0xFFF9, 0xFFFB),
        (0xE0000, 0xE007F),
        (0xE0100, 0xE01EF),
    ];

    #[test]
    fn the_hidden_set_is_pinned() {
        for c in (0..=0x10_FFFFu32).filter_map(char::from_u32) {
            let listed = HIDDEN
                .iter()
                .any(|&(lo, hi)| (lo..=hi).contains(&u32::from(c)));
            assert_eq!(is_hidden(c), listed, "U+{:04X}", u32::from(c));
            if listed {
                // Dropped, or a line break shown as a space; never printed as itself.
                let shown = line(&format!("a{c}b"));
                assert!(shown == "ab" || shown == "a b", "U+{:04X}", u32::from(c));
            }
        }
    }
}
