//! Untrusted text. Every string taken from an event goes through here before it is stored in a
//! block or shown in a summary, so lengths are bounded and nothing can reorder or hide text.

use pitcrew_protocol::model::Receipt;

/// Longest file path kept in a block, in characters.
pub(crate) const PATH_CHARS: usize = 200;
/// Longest handle, task key, workstream name or file name used in prose, in characters.
pub(crate) const NAME_CHARS: usize = 60;
/// Longest title, status line or quoted text kept, in characters.
pub(crate) const TITLE_CHARS: usize = 80;
/// Longest tool target read when classifying a command, in characters.
pub(crate) const TARGET_CHARS: usize = 200;
/// Longest job id kept, in characters.
pub(crate) const JOB_CHARS: usize = 32;
/// Longest text field a receipt may carry, in bytes. Longer receipts are not kept.
const RECEIPT_TEXT_BYTES: usize = 1024;

/// Characters that change text direction or are invisible. They are dropped so a crafted name
/// cannot make a summary read differently from what it says.
fn is_hidden(c: char) -> bool {
    matches!(
        c,
        '\u{061C}'
            | '\u{200B}'..='\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{2069}'
            | '\u{FEFF}'
    )
}

/// Cleans text for display: hidden characters are dropped, control characters and whitespace
/// runs become one space, the ends are trimmed, and at most `max` characters are kept (the last
/// one is `…` when the text was cut). Work is bounded by `max`, not by the input length.
pub(crate) fn clean(s: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    let scan_limit = max.saturating_mul(4).saturating_add(64);
    let mut out = String::new();
    let mut count = 0usize;
    let mut space = false;
    let mut cut = false;
    for (scanned, c) in s.chars().enumerate() {
        if scanned >= scan_limit {
            cut = true;
            break;
        }
        if is_hidden(c) {
            continue;
        }
        if c.is_whitespace() || c.is_control() {
            space = count > 0;
            continue;
        }
        let need = if space { 2 } else { 1 };
        if count + need > max {
            cut = true;
            break;
        }
        if space {
            out.push(' ');
            count += 1;
            space = false;
        }
        out.push(c);
        count += 1;
    }
    if cut {
        if count >= max {
            out.pop();
        }
        while out.ends_with(' ') {
            out.pop();
        }
        out.push('…');
    }
    out
}

/// Like [`clean`], but keeps the end of the text, which is the informative part of a path:
/// `…/paper/method.tex`.
pub(crate) fn clean_tail(s: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    // Only the last few bytes can matter; start at a character boundary.
    let mut start = s
        .len()
        .saturating_sub(max.saturating_mul(4).saturating_add(64));
    while start < s.len() && !s.is_char_boundary(start) {
        start += 1;
    }
    let window = s.get(start..).unwrap_or_default();
    let mut chars: Vec<char> = window
        .chars()
        .filter(|c| !is_hidden(*c))
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let cut = start > 0 || chars.len() > max;
    if cut {
        let keep = max.saturating_sub(1);
        let skip = chars.len().saturating_sub(keep);
        chars.drain(..skip);
        let mut out = String::from("…");
        out.extend(chars);
        out
    } else {
        chars.into_iter().collect()
    }
}

/// The first `max` characters of `s`, without copying.
pub(crate) fn prefix(s: &str, max: usize) -> &str {
    match s.char_indices().nth(max) {
        Some((i, _)) => s.get(..i).unwrap_or(s),
        None => s,
    }
}

/// Whether a path can be kept as it is: short, with nothing [`clean_tail`] would change.
pub(crate) fn is_plain_path(s: &str) -> bool {
    s.len() <= PATH_CHARS && !s.chars().any(|c| c.is_control() || is_hidden(c))
}

/// The last component of a path, for prose.
pub(crate) fn basename(path: &str) -> &str {
    let trimmed = path.trim_end_matches(['/', '\\']);
    match trimmed.rfind(['/', '\\']) {
        Some(i) => trimmed.get(i + 1..).unwrap_or(trimmed),
        None if trimmed.is_empty() => path,
        None => trimmed,
    }
}

/// Whether a receipt is small enough to keep. Receipts come from untrusted events and are passed
/// through as they are, so oversized ones are dropped rather than copied into every block.
pub(crate) fn receipt_ok(r: &Receipt) -> bool {
    let short = |s: &str| s.len() <= RECEIPT_TEXT_BYTES;
    match r {
        Receipt::Transcript { .. } | Receipt::Event { .. } => true,
        Receipt::Commit { repo, sha } => short(repo) && short(sha),
        Receipt::PullRequest { url } => short(url),
        Receipt::Job { id, .. } => short(id),
        Receipt::File { location } => {
            short(&location.path) && location.branch.as_deref().is_none_or(short)
        }
    }
}

/// Pushes onto a vector that is usually tiny, growing it from one element instead of four. Most
/// blocks hold one or two of each thing, and a block's size is what bounds how fast many of them
/// can be built.
pub(crate) fn push_small<T>(list: &mut Vec<T>, item: T) {
    if list.len() == list.capacity() {
        list.reserve_exact(list.len().max(1));
    }
    list.push(item);
}

/// Appends receipts that are new to `list`, keeping the first `cap`.
pub(crate) fn push_first<'a>(
    list: &mut Vec<Receipt>,
    new: impl IntoIterator<Item = &'a Receipt>,
    cap: usize,
) {
    for r in new {
        if list.len() >= cap {
            break;
        }
        if receipt_ok(r) && !list.contains(r) {
            push_small(list, r.clone());
        }
    }
}

/// Appends receipts that are new to `list`. When the list is full, the latest receipts replace
/// the previous latest ones, so the list always holds the earliest and the newest evidence.
pub(crate) fn push_latest<'a>(
    list: &mut Vec<Receipt>,
    new: impl IntoIterator<Item = &'a Receipt>,
    cap: usize,
) {
    let cap = cap.max(2);
    let mut fresh: Vec<Receipt> = Vec::new();
    for r in new {
        if fresh.len() >= cap - 1 {
            break;
        }
        if receipt_ok(r) && !list.contains(r) && !fresh.contains(r) {
            fresh.push(r.clone());
        }
    }
    if fresh.is_empty() {
        return;
    }
    let keep = (cap - fresh.len()).min(list.len());
    list.truncate(keep);
    for r in fresh {
        push_small(list, r);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_bounds_and_scrubs() {
        assert_eq!(clean("  hello \n\t world  ", 60), "hello world");
        assert_eq!(clean("abcdef", 4), "abc…");
        assert_eq!(clean("abcd", 4), "abcd");
        assert_eq!(clean("ab\u{202E}cd", 10), "abcd");
        assert_eq!(clean("a\u{0}b", 10), "a b");
        assert_eq!(clean("", 10), "");
        assert_eq!(clean("abc", 0), "");
        assert_eq!(clean("ab cdef", 3), "ab…");
        let long = "x".repeat(1_000_000);
        assert_eq!(clean(&long, 5).chars().count(), 5);
        let spaces = format!("a{}b", " ".repeat(1_000_000));
        assert_eq!(clean(&spaces, 5), "a…");
    }

    #[test]
    fn clean_tail_keeps_the_end() {
        assert_eq!(clean_tail("paper/method.tex", 60), "paper/method.tex");
        assert_eq!(clean_tail("/a/b/c/method.tex", 11), "…method.tex");
        assert_eq!(clean_tail("é".repeat(500).as_str(), 3), "…éé");
        assert_eq!(clean_tail("a\u{202E}b\nc", 10), "ab c");
        assert_eq!(clean_tail("abc", 0), "");
    }

    #[test]
    fn prefix_and_plain_paths() {
        assert_eq!(prefix("héllo", 2), "hé");
        assert_eq!(prefix("hi", 5), "hi");
        assert_eq!(prefix("hi", 0), "");
        assert!(is_plain_path("paper/method.tex"));
        assert!(!is_plain_path("a\u{202E}b"));
        assert!(!is_plain_path(&"x".repeat(PATH_CHARS + 1)));
        // A plain path is kept exactly as clean_tail would keep it.
        assert_eq!(
            clean_tail("paper/method.tex", PATH_CHARS),
            "paper/method.tex"
        );
    }

    #[test]
    fn basename_takes_the_last_component() {
        assert_eq!(basename("paper/method.tex"), "method.tex");
        assert_eq!(basename("C:\\x\\y.rs"), "y.rs");
        assert_eq!(basename("dir/"), "dir");
        assert_eq!(basename("/"), "/");
        assert_eq!(basename("plain"), "plain");
    }

    #[test]
    fn push_latest_keeps_first_and_newest() {
        use pitcrew_protocol::ids::{EventId, SessionId};
        let s = SessionId(ulid::Ulid::from(1u128));
        let r = |offset| Receipt::Transcript { session: s, offset };
        let mut list = Vec::new();
        for i in 0..10 {
            push_latest(&mut list, [&r(i)], 3);
        }
        assert_eq!(list, vec![r(0), r(1), r(9)]);
        let e = Receipt::Event {
            id: EventId(ulid::Ulid::from(2u128)),
        };
        push_first(&mut list, [&e, &e], 4);
        assert_eq!(list.len(), 4);
        let big = Receipt::PullRequest {
            url: "u".repeat(5000),
        };
        let mut other = Vec::new();
        push_first(&mut other, [&big], 4);
        assert!(other.is_empty());
    }
}
