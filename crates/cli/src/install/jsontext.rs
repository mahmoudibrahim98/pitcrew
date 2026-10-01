//! A tiny, span-aware JSON reader and editor used to edit `settings.json` **surgically**: every
//! byte of the original file that we are not deliberately changing is copied through untouched,
//! so unrelated keys, indentation, and formatting choices survive a round trip exactly.
//!
//! This is not a general JSON library. The caller first checks the whole document parses with
//! `serde_json` (so every value here is known to be well-formed); these functions only need to
//! find the byte spans of top-level members of a *known-good* object or array, and to add or
//! remove one member/element while leaving every other byte alone.

/// One entry in an object or array: its value's span `[value_start, value_end)`, and `end`, the
/// index right after it and right after a following `,` if the source has one (JSON never puts
/// a comma after the last entry, so at most one entry in a container lacks it).
#[derive(Clone, Copy)]
pub(crate) struct Entry {
    pub(crate) value_start: usize,
    pub(crate) value_end: usize,
    pub(crate) end: usize,
    /// For an object member, where its key starts (the byte to delete from, removing the whole
    /// member). Equal to `value_start` for an array element.
    pub(crate) start: usize,
}

/// One member of a JSON object: its unescaped key plus its entry.
pub(crate) struct Member {
    pub(crate) key: String,
    pub(crate) entry: Entry,
}

/// An object's members and the index of its closing `}`.
pub(crate) struct Obj {
    pub(crate) members: Vec<Member>,
    pub(crate) close: usize,
}

/// An array's elements and the index of its closing `]`.
pub(crate) struct Arr {
    pub(crate) elements: Vec<Entry>,
    pub(crate) close: usize,
}

/// Finds the first non-whitespace byte at or after `i`.
pub(crate) fn skip_ws(b: &[u8], mut i: usize) -> usize {
    while i < b.len() && matches!(b[i], b' ' | b'\t' | b'\n' | b'\r') {
        i += 1;
    }
    i
}

/// The indentation (spaces/tabs) between the start of its line and `pos`, for reusing an
/// existing entry's style on a new one. Empty if `pos` is not alone at its line's start.
pub(crate) fn indent_before(b: &[u8], pos: usize) -> &str {
    let mut start = pos;
    while start > 0 && matches!(b[start - 1], b' ' | b'\t') {
        start -= 1;
    }
    if start == 0 || b[start - 1] == b'\n' {
        std::str::from_utf8(&b[start..pos]).unwrap_or("")
    } else {
        ""
    }
}

/// Parses a JSON string starting at `b[i]` (`"`), returning the unescaped text and the index
/// right after the closing `"`. The input is assumed well-formed (checked by `serde_json`
/// first).
fn parse_string(b: &[u8], i: usize) -> (String, usize) {
    let mut out = String::new();
    let mut i = i + 1; // past the opening quote
    while i < b.len() {
        match b[i] {
            b'"' => return (out, i + 1),
            b'\\' if i + 1 < b.len() => {
                i += 1;
                match b[i] {
                    b'"' => out.push('"'),
                    b'\\' => out.push('\\'),
                    b'/' => out.push('/'),
                    b'b' => out.push('\u{8}'),
                    b'f' => out.push('\u{c}'),
                    b'n' => out.push('\n'),
                    b'r' => out.push('\r'),
                    b't' => out.push('\t'),
                    b'u' if i + 4 < b.len() => {
                        let hex = std::str::from_utf8(&b[i + 1..i + 5]).unwrap_or("0");
                        let code = u32::from_str_radix(hex, 16).unwrap_or(0);
                        if let Some(c) = char::from_u32(code) {
                            out.push(c);
                        }
                        i += 4;
                    }
                    other => out.push(other as char),
                }
                i += 1;
            }
            // A multi-byte UTF-8 sequence: copy the whole character, not just this byte.
            byte => {
                let len = utf8_len(byte);
                let end = (i + len).min(b.len());
                if let Ok(s) = std::str::from_utf8(&b[i..end]) {
                    out.push_str(s);
                }
                i = end;
            }
        }
    }
    (out, i)
}

fn utf8_len(first_byte: u8) -> usize {
    match first_byte {
        0x00..=0x7F => 1,
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        _ => 4,
    }
}

/// Skips one JSON value starting at the first non-whitespace byte of it, returning the index
/// right after the value.
pub(crate) fn skip_value(b: &[u8], i: usize) -> usize {
    match b.get(i) {
        Some(b'"') => parse_string(b, i).1,
        Some(b'{' | b'[') => {
            let mut depth: u32 = 1;
            let mut j = i + 1;
            while j < b.len() && depth > 0 {
                match b[j] {
                    b'"' => j = parse_string(b, j).1,
                    b'{' | b'[' => {
                        depth += 1;
                        j += 1;
                    }
                    b'}' | b']' => {
                        depth -= 1;
                        j += 1;
                    }
                    _ => j += 1,
                }
            }
            j
        }
        _ => {
            let mut j = i;
            while j < b.len() && !matches!(b[j], b',' | b'}' | b']' | b' ' | b'\t' | b'\n' | b'\r')
            {
                j += 1;
            }
            j
        }
    }
}

/// One value (object member or array element) starting at `start` (a key's opening `"` for an
/// object, or the value itself for an array), given where its value begins.
fn entry(b: &[u8], start: usize, value_start: usize) -> Entry {
    let value_end = skip_value(b, value_start);
    let after = skip_ws(b, value_end);
    let end = if b.get(after) == Some(&b',') {
        after + 1
    } else {
        value_end
    };
    Entry {
        start,
        value_start,
        value_end,
        end,
    }
}

/// The members of an object whose `{` is at `open`.
pub(crate) fn object(b: &[u8], open: usize) -> Obj {
    let mut i = skip_ws(b, open + 1);
    let mut members = Vec::new();
    while i < b.len() && b[i] != b'}' {
        let key_start = i;
        let (key, after_key) = parse_string(b, i);
        let colon = skip_ws(b, after_key);
        let value_start = skip_ws(b, colon + 1);
        let e = entry(b, key_start, value_start);
        let next = skip_ws(b, e.end);
        members.push(Member { key, entry: e });
        i = next;
    }
    Obj { members, close: i }
}

/// The elements of an array whose `[` is at `open`.
pub(crate) fn array(b: &[u8], open: usize) -> Arr {
    let mut i = skip_ws(b, open + 1);
    let mut elements = Vec::new();
    while i < b.len() && b[i] != b']' {
        let e = entry(b, i, i);
        let next = skip_ws(b, e.end);
        elements.push(e);
        i = next;
    }
    Arr { elements, close: i }
}

/// Removes one entry (object member or array element) from `doc`, keeping the container valid:
/// a non-last entry is deleted together with its own trailing comma; the last entry is deleted
/// together with the comma that used to follow the previous one (there is none to put back). The
/// only remaining entry is deleted together with one surrounding `\n` + indent run on each side
/// (at most one, matching exactly what `append`'s empty-container branch would have added),
/// leaving the container exactly as it was before that entry's own insertion — not just its own
/// span, which would otherwise leave an orphaned blank line behind.
#[must_use]
pub(crate) fn remove(doc: &str, entries: &[Entry], index: usize) -> String {
    let e = entries[index];
    let (del_start, del_end) = if index + 1 < entries.len() {
        (e.start, e.end)
    } else if index > 0 {
        (entries[index - 1].value_end, e.end)
    } else {
        let bytes = doc.as_bytes();
        let mut start = e.start;
        while start > 0 && matches!(bytes[start - 1], b' ' | b'\t') {
            start -= 1;
        }
        if start > 0 && bytes[start - 1] == b'\n' {
            start -= 1;
        }
        let mut end = e.end;
        while end < bytes.len() && matches!(bytes[end], b' ' | b'\t') {
            end += 1;
        }
        if end < bytes.len() && bytes[end] == b'\n' {
            end += 1;
        }
        (start, end)
    };
    format!("{}{}", &doc[..del_start], &doc[del_end..])
}

/// Inserts `new_text` (one bare value, no surrounding comma) as a new last entry of a container
/// whose entries are `entries` and which closes at `close`. `child_indent` is the indentation to
/// put before it; `parent_indent` is the one line below, in front of the closing bracket, used
/// only when the container is currently empty.
#[must_use]
pub(crate) fn append(
    doc: &str,
    entries: &[Entry],
    close: usize,
    new_text: &str,
    child_indent: &str,
    parent_indent: &str,
) -> String {
    match entries.last() {
        Some(last) => format!(
            "{},\n{child_indent}{new_text}{}",
            &doc[..last.value_end],
            &doc[last.value_end..]
        ),
        None => format!(
            "{}\n{child_indent}{new_text}\n{parent_indent}{}",
            &doc[..close],
            &doc[close..]
        ),
    }
}

/// A string as a JSON string literal (with the surrounding quotes).
#[must_use]
pub(crate) fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_object_members_and_their_spans() {
        let src = br#"{"a": 1, "b":   {"x":2}, "c": [1,2]}"#;
        let obj = object(src, 0);
        assert_eq!(obj.members.len(), 3);
        assert_eq!(obj.members[0].key, "a");
        let v = obj.members[0].entry;
        assert_eq!(&src[v.value_start..v.value_end], b"1");
        assert_eq!(obj.members[1].key, "b");
        let v = obj.members[1].entry;
        assert_eq!(&src[v.value_start..v.value_end], br#"{"x":2}"#);
        assert_eq!(obj.close, src.len() - 1);
    }

    #[test]
    fn reads_array_elements() {
        let src = br#"[ "a", {"k": [1, 2]}, 3 ]"#;
        let arr = array(src, 0);
        assert_eq!(arr.elements.len(), 3);
        let v = arr.elements[1];
        assert_eq!(&src[v.value_start..v.value_end], br#"{"k": [1, 2]}"#);
    }

    #[test]
    fn handles_escapes_and_empty_containers() {
        let src = br#"{"a\"b": "x\ny", "e": {}, "arr": []}"#;
        let obj = object(src, 0);
        assert_eq!(obj.members[0].key, "a\"b");
        let (text, _) = parse_string(src, obj.members[0].entry.value_start);
        assert_eq!(text, "x\ny");
        let empty_obj = object(src, obj.members[1].entry.value_start);
        assert!(empty_obj.members.is_empty());
        let empty_arr = array(src, obj.members[2].entry.value_start);
        assert!(empty_arr.elements.is_empty());
    }

    #[test]
    fn indent_is_the_whitespace_since_the_line_began() {
        let src = b"{\n  \"a\": 1,\n  \"b\": 2\n}";
        let obj = object(src, 0);
        assert_eq!(indent_before(src, obj.members[1].entry.start), "  ");
        assert_eq!(indent_before(src, 0), "");
    }

    #[test]
    fn append_then_remove_restores_the_original_bytes() {
        for original in [
            "{\n  \"a\": 1\n}",
            "{\n  \"a\": 1,\n  \"b\": 2\n}",
            "{}",
            "{\n}",
        ] {
            let bytes = original.as_bytes();
            let obj = object(bytes, 0);
            let member_indent = if let Some(last) = obj.members.last() {
                indent_before(bytes, last.entry.start).to_owned()
            } else {
                "  ".to_owned()
            };
            let after = append(
                original,
                &obj.members.iter().map(|m| m.entry).collect::<Vec<_>>(),
                obj.close,
                r#""z": 9"#,
                &member_indent,
                "",
            );
            // Re-scan the grown text and remove the entry we just added; it must be the last.
            let grown = after.as_bytes();
            let grown_obj = object(grown, 0);
            let entries: Vec<Entry> = grown_obj.members.iter().map(|m| m.entry).collect();
            let restored = remove(&after, &entries, entries.len() - 1);
            assert_eq!(restored, original, "original: {original:?}");
        }
    }

    #[test]
    fn escapes_round_trip_through_serde() {
        let s = "a\"b\\c\n\td\u{7}";
        let escaped = escape(s);
        let back: String = serde_json::from_str(&escaped).unwrap();
        assert_eq!(back, s);
    }
}
