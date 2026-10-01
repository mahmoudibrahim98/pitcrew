//! Atlassian Document Format (ADF) → plain text.
//!
//! Jira Cloud (REST v3) renders a description as an ADF tree: `{"type":"doc","version":1,
//! "content":[...]}`. This flattens it to the plain text a person reads, bounded in depth and
//! size regardless of what the document contains — it is read from an issue's description, which
//! is attacker- or bug-controlled input, not from a trusted source. **Nothing here interprets a
//! link, a mention or any other markup as an instruction**: every node is either literal text to
//! copy out, or a container to flatten; this is text for a person to read, never something this
//! crate (or the agent reading the result later) acts on.
//!
//! Data Center (REST v2) descriptions are already plain text and never reach this module; see
//! [`crate::change::description_text`].

use crate::bounds::{MAX_ADF_DEPTH, MAX_ADF_NODES, MAX_BODY_CHARS, cap_chars};
use serde_json::Value;

/// Walks `node` (and its `content` children) to plain text, stopping once [`MAX_ADF_DEPTH`] is
/// reached (no deeper recursion is attempted past that — the Rust call stack itself is bounded by
/// this constant, not just the output), once [`MAX_ADF_NODES`] nodes have been visited in total
/// (a wide-but-shallow document), or once the output already holds [`MAX_BODY_CHARS`] (the same
/// cap every other body/description goes through).
#[must_use]
pub fn adf_to_text(node: &Value) -> String {
    let mut out = String::new();
    let mut nodes_visited = 0usize;
    // A running character count, kept alongside `out` rather than recomputed from it (R29: the
    // previous version re-scanned the whole output with `out.chars().count()` at every node and
    // every child, making one flatten quadratic in the output size — 692 ms for a 763 KiB
    // description in a release build, against 0.9 ms for either half of it alone).
    let mut chars_written = 0usize;
    walk(node, 0, &mut nodes_visited, &mut chars_written, &mut out);
    cap_chars(&out, MAX_BODY_CHARS)
}

/// Appends `text` to `out` and keeps `chars_written` in step with it, so callers never need to
/// re-derive the character count from `out` itself (see [`adf_to_text`]'s doc on why that was the
/// R29 quadratic-time bug).
fn push(out: &mut String, chars_written: &mut usize, text: &str) {
    out.push_str(text);
    *chars_written += text.chars().count();
}

fn walk(
    node: &Value,
    depth: usize,
    nodes_visited: &mut usize,
    chars_written: &mut usize,
    out: &mut String,
) {
    if depth > MAX_ADF_DEPTH || *nodes_visited >= MAX_ADF_NODES || *chars_written >= MAX_BODY_CHARS
    {
        return;
    }
    *nodes_visited += 1;
    let Some(obj) = node.as_object() else {
        return;
    };
    let node_type = obj.get("type").and_then(Value::as_str).unwrap_or("");

    match node_type {
        "text" => {
            if let Some(text) = obj.get("text").and_then(Value::as_str) {
                push(out, chars_written, text);
            }
        }
        "mention" => {
            // A mention's visible label, not anything to resolve or act on.
            let label = obj
                .get("attrs")
                .and_then(|a| a.get("text"))
                .and_then(Value::as_str)
                .unwrap_or("@someone");
            push(out, chars_written, label);
        }
        "emoji" => {
            let shortname = obj
                .get("attrs")
                .and_then(|a| a.get("shortName"))
                .and_then(Value::as_str)
                .unwrap_or("");
            push(out, chars_written, shortname);
        }
        "hardBreak" => push(out, chars_written, "\n"),
        "rule" => push(out, chars_written, "\n---\n"),
        "inlineCard" | "blockCard" | "embedCard" => {
            // A link card with no visible text of its own: show the bare URL as literal text, not
            // as something to open or follow.
            if let Some(url) = obj
                .get("attrs")
                .and_then(|a| a.get("url"))
                .and_then(Value::as_str)
            {
                push(out, chars_written, url);
            }
        }
        _ => {}
    }

    if let Some(children) = obj.get("content").and_then(Value::as_array) {
        for child in children {
            if *nodes_visited >= MAX_ADF_NODES || *chars_written >= MAX_BODY_CHARS {
                break;
            }
            walk(child, depth + 1, nodes_visited, chars_written, out);
        }
        // Block-level nodes get a trailing newline once their children are flattened, so
        // paragraphs/headings/list items don't run together; inline nodes (text, mention, …)
        // handled above never reach here (they have no `content`).
        if matches!(
            node_type,
            "paragraph" | "heading" | "listItem" | "blockquote" | "codeBlock" | "tableRow"
        ) {
            push(out, chars_written, "\n");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn flattens_paragraphs_of_text() {
        let doc = json!({
            "type": "doc", "version": 1,
            "content": [
                {"type": "paragraph", "content": [{"type": "text", "text": "Hello, "}, {"type": "text", "text": "world."}]},
                {"type": "paragraph", "content": [{"type": "text", "text": "Second paragraph."}]},
            ],
        });
        let text = adf_to_text(&doc);
        assert!(text.contains("Hello, world."));
        assert!(text.contains("Second paragraph."));
    }

    #[test]
    fn a_mention_renders_its_visible_label_not_an_id() {
        let doc = json!({
            "type": "doc",
            "content": [{"type": "paragraph", "content": [
                {"type": "text", "text": "cc "},
                {"type": "mention", "attrs": {"id": "abc123", "text": "@Alice"}},
            ]}],
        });
        let text = adf_to_text(&doc);
        assert!(text.contains("@Alice"));
        assert!(!text.contains("abc123"));
    }

    #[test]
    fn a_link_card_shows_the_url_as_literal_text() {
        let doc = json!({
            "type": "doc",
            "content": [{"type": "paragraph", "content": [
                {"type": "inlineCard", "attrs": {"url": "https://jira.example.com/browse/DEMO-1"}},
            ]}],
        });
        let text = adf_to_text(&doc);
        assert!(text.contains("https://jira.example.com/browse/DEMO-1"));
    }

    #[test]
    fn deeply_nested_adf_is_capped_not_overflowed() {
        // 500 levels of nested "blockquote" wrapping a single text node — well past
        // MAX_ADF_DEPTH (32) — wrapping a single text node: without the depth cap, flattening
        // would recurse one `walk` stack frame per level instead of stopping at 32.
        let mut node = json!({"type": "text", "text": "center"});
        for _ in 0..500 {
            node = json!({"type": "blockquote", "content": [node]});
        }
        let doc = json!({"type": "doc", "content": [node]});
        // Must simply return (bounded output), not blow the stack or hang.
        let text = adf_to_text(&doc);
        assert!(text.chars().count() <= MAX_BODY_CHARS);
    }

    #[test]
    fn a_huge_flat_document_is_capped_at_max_body_chars() {
        let content: Vec<Value> = (0..5_000)
            .map(|_| json!({"type": "paragraph", "content": [{"type": "text", "text": "x".repeat(50)}]}))
            .collect();
        let doc = json!({"type": "doc", "content": content});
        let text = adf_to_text(&doc);
        assert_eq!(text.chars().count(), MAX_BODY_CHARS);
    }

    #[test]
    fn a_wide_shallow_document_is_bounded_by_node_count() {
        // Many thousands of siblings at depth 1, each contributing almost no text: the node-count
        // cap must still stop the walk in bounded time, independent of the character cap.
        let content: Vec<Value> = (0..(MAX_ADF_NODES * 2))
            .map(|_| json!({"type": "text", "text": ""}))
            .collect();
        let doc = json!({"type": "doc", "content": content});
        let text = adf_to_text(&doc);
        assert!(text.chars().count() < MAX_BODY_CHARS);
    }

    #[test]
    fn r29_flattening_stays_fast_on_a_large_document_with_many_trailing_nodes() {
        // Stream Q's open-r29-quadratic-walk regression: one large text node (four-byte
        // characters, so the output is hundreds of KB even while staying under the character
        // cap), followed by thousands of further nodes. The pre-fix `walk` recounted the whole
        // output (`out.chars().count()`) at every one of those trailing nodes, making one
        // flatten quadratic in the output size: 692 ms for a 763 KiB description in a release
        // build, against 0.9 ms for either half alone. A running count keeps this linear, so
        // this must finish well within a generous bound even in an unoptimised test build.
        let big_text: String = std::iter::repeat_n('\u{1F600}', 65_280).collect(); // 😀, 4 bytes
        let mut content = vec![json!({
            "type": "paragraph",
            "content": [{"type": "text", "text": big_text}],
        })];
        content.extend((0..20_000).map(|_| json!({"type": "text", "text": ""})));
        let doc = json!({"type": "doc", "content": content});

        let started = std::time::Instant::now();
        let text = adf_to_text(&doc);
        let took = started.elapsed();

        assert!(!text.is_empty());
        assert!(
            took < std::time::Duration::from_secs(2),
            "flattening took {took:?}, which is quadratic-walk slow, not linear-walk fast"
        );
    }

    #[test]
    fn non_object_and_unknown_nodes_never_panic() {
        assert_eq!(adf_to_text(&json!(null)), "");
        assert_eq!(adf_to_text(&json!("not a node")), "");
        assert_eq!(adf_to_text(&json!({"type": "somethingUnknown"})), "");
    }

    proptest::proptest! {
        /// Arbitrary JSON values never panic the walker, at any shape.
        #[test]
        fn never_panics_on_arbitrary_json(text in ".{0,200}") {
            let doc = json!({"type": "doc", "content": [{"type": "paragraph", "content": [{"type": "text", "text": text}]}]});
            let _ = adf_to_text(&doc);
        }
    }
}
