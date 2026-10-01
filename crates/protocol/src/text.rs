//! Untrusted text: the characters dropped before people or agents read it.
//!
//! One set for every crate that cleans text from agents, transcripts or trackers: the recap
//! engine (`pitcrew-recap`), the GitHub and Jira syncs (`pitcrew-sync-github`) and the CLI's
//! terminal output (`pitcrew-cli`). Each may add rules of its own (the CLI also drops control
//! characters; the recap turns line and paragraph separators into a space), but none drops fewer.

/// Whether `c` is invisible, renders as a blank placeholder, or changes the direction of text.
///
/// A crafted name made of these and ordinary letters can read differently from what it says
/// (bidi overrides), look the same as another name while comparing unequal (zero-width
/// characters, fillers, variation selectors), or carry text a person does not see but a model
/// reading it does (the Unicode tag characters). The line and paragraph separators are in the set
/// too: they are not shown as themselves, though a cleaner may show them as a space
/// ([`is_line_separator`]).
#[must_use]
pub fn is_hidden(c: char) -> bool {
    matches!(
        c,
        '\u{00AD}' // soft hyphen
            | '\u{034F}' // combining grapheme joiner
            | '\u{061C}' // Arabic letter mark
            | '\u{115F}' | '\u{1160}' // Hangul choseong and jungseong fillers
            | '\u{180E}' // Mongolian vowel separator
            | '\u{200B}'..='\u{200F}' // zero-width space and joiners, direction marks
            | '\u{2028}'..='\u{2029}' // line and paragraph separators
            | '\u{202A}'..='\u{202E}' // direction embeddings and overrides
            | '\u{2060}'..='\u{2064}' // word joiner, invisible operators
            | '\u{2066}'..='\u{2069}' // direction isolates
            | '\u{3164}' // Hangul filler
            | '\u{FE00}'..='\u{FE0F}' // variation selectors
            | '\u{FEFF}' // byte order mark, zero-width no-break space
            | '\u{FFA0}' // halfwidth Hangul filler
            | '\u{FFF9}'..='\u{FFFB}' // interlinear annotation marks
            | '\u{E0000}'..='\u{E007F}' // tag characters
            | '\u{E0100}'..='\u{E01EF}' // variation selectors supplement
    )
}

/// U+2028 and U+2029: [hidden](is_hidden), but they break a line, so text that is shown as one
/// line shows them as a space rather than running the words on either side together.
#[must_use]
pub fn is_line_separator(c: char) -> bool {
    matches!(c, '\u{2028}' | '\u{2029}')
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The set, written out. `pitcrew-recap`, `pitcrew-sync-github` and `pitcrew-cli` pin the same
    /// table in their own tests: change them together.
    const HIDDEN: &[(u32, u32)] = &[
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
        }
    }

    #[test]
    fn line_separators_are_hidden_too() {
        for c in (0..=0x10_FFFFu32).filter_map(char::from_u32) {
            if is_line_separator(c) {
                assert!(is_hidden(c), "U+{:04X}", u32::from(c));
            }
        }
        assert!(is_line_separator('\u{2028}') && is_line_separator('\u{2029}'));
        assert!(!is_line_separator('\n'));
    }

    #[test]
    fn controls_and_ordinary_text_are_not_in_the_set() {
        // Control characters are each crate's own rule: sync-github keeps newlines in bodies.
        for c in [
            '\n', '\t', '\u{1b}', '\u{7f}', '\u{9b}', 'a', 'é', '🙂', ' ', '\u{A0}',
        ] {
            assert!(!is_hidden(c), "U+{:04X}", u32::from(c));
        }
    }
}
