//! `pitcrew_sync_jira::adf::adf_to_text` on Atlassian Document Format trees: an issue's
//! description on Jira Cloud, which anyone who can edit the issue writes (B10, U2).
//!
//! Input: a mode byte, then
//! - mode 0: a JSON document (as the search page carries it), or
//! - mode 1: a small program that builds a tree, so that wide (past 20,000 nodes), deep (past
//!   32 levels) and long (past 65,536 characters) documents are reached from short inputs. Each
//!   byte is an operation `op = b % 8` with an argument `b / 8`: open a container, close it, add
//!   text of a repeated character, add text with a hidden character, add a mention, emoji or link
//!   card, add a break or rule, repeat the last node, or wrap it in nested quotes. Text in a node
//!   deeper than 32 levels is `Ω`, and in a node past the 20,000th visited it is `Ж`; neither
//!   appears anywhere else.
//!
//! Checks, besides "no panic":
//! - **bounded output**: at most 65,536 characters;
//! - **hidden characters gone**: no bidi control, zero-width character, tag character and the
//!   like (`pitcrew_fuzz::is_hidden_char`) survives;
//! - **bounded depth and nodes**: no text from a node past the depth or node caps (`Ω`, `Ж`);
//! - **bounded work**: one document takes at most a quarter of a second (R29, fixed: the walk
//!   recounted the output on every node, so 20,000 nodes after 65,000 characters took far longer
//!   than a linear walk; it now keeps a running count).
#![no_main]

use libfuzzer_sys::fuzz_target;
use pitcrew_fuzz::is_hidden_char;
use pitcrew_sync_jira::adf::adf_to_text;
use pitcrew_sync_jira::bounds::{MAX_ADF_DEPTH, MAX_ADF_NODES, MAX_BODY_CHARS};
use serde_json::{Value, json};
use std::time::{Duration, Instant};

/// The most nodes, characters and levels a program builds, so one input stays fast (and deep
/// values do not overflow the stack when they are dropped).
const MAX_BUILT: usize = 3 * MAX_ADF_NODES;
const MAX_CHARS: usize = 300_000;
const MAX_HEIGHT: usize = 150;
/// Work allowed for one document: a linear walk of a document at every cap takes a few
/// milliseconds here, even instrumented.
const BUDGET: Duration = Duration::from_millis(250);

const CONTAINERS: [&str; 8] = [
    "paragraph",
    "blockquote",
    "listItem",
    "bulletList",
    "heading",
    "codeBlock",
    "tableRow",
    "panel",
];
const CHARS: [char; 8] = ['a', ' ', '\n', 'é', '日', '😀', '<', '"'];
const HIDDEN: [char; 8] = [
    '\u{202E}',
    '\u{200B}',
    '\u{2066}',
    '\u{FEFF}',
    '\u{00AD}',
    '\u{E0041}',
    '\u{2028}',
    '\u{180E}',
];
const DEEP: char = 'Ω';
const LATE: char = 'Ж';

enum Node {
    Text(String),
    Leaf(Value),
    Container(&'static str, Vec<Node>),
}

/// Builds the root's children from `program`.
fn build(program: &[u8]) -> Vec<Node> {
    let mut stack: Vec<(&'static str, Vec<Node>)> = vec![("doc", Vec::new())];
    let mut built = 0usize;
    let mut chars = 0usize;
    let mut bytes = program.iter().copied();
    while let Some(b) = bytes.next() {
        if built >= MAX_BUILT {
            break;
        }
        let (op, arg) = (b % 8, usize::from(b / 8));
        let open = stack.len();
        let top = &mut stack.last_mut().expect("the root").1;
        match op {
            0 if open < MAX_HEIGHT => stack.push((CONTAINERS[arg % CONTAINERS.len()], Vec::new())),
            0 => {}
            1 => {
                if stack.len() > 1 {
                    let (kind, children) = stack.pop().expect("a container");
                    stack
                        .last_mut()
                        .expect("the root")
                        .1
                        .push(Node::Container(kind, children));
                    built += 1;
                }
            }
            2 => {
                let len = (usize::from(bytes.next().unwrap_or(1)) << (arg % 4 * 4))
                    .min(MAX_CHARS - chars);
                top.push(Node::Text(
                    std::iter::repeat_n(CHARS[arg % CHARS.len()], len).collect(),
                ));
                chars += len;
                built += 1;
            }
            3 => {
                top.push(Node::Text(format!("x{}y", HIDDEN[arg % HIDDEN.len()])));
                built += 1;
            }
            4 => {
                let label = format!("@a{}b", HIDDEN[arg % HIDDEN.len()]);
                top.push(Node::Leaf(match arg % 3 {
                    0 => json!({"type": "mention", "attrs": {"id": "x", "text": label}}),
                    1 => json!({"type": "emoji", "attrs": {"shortName": label}}),
                    _ => json!({"type": "inlineCard", "attrs": {"url": format!("https://jira.example.com/{label}")}}),
                }));
                built += 1;
            }
            5 => {
                top.push(Node::Leaf(
                    json!({"type": if arg % 2 == 0 { "hardBreak" } else { "rule" }}),
                ));
                built += 1;
            }
            6 => {
                // Repeat the last node up to 2^(arg % 16) times, within the budgets.
                if let Some(last) = top.last() {
                    let (nodes, text) = size(last);
                    let times = (1usize << (arg % 16))
                        .min((MAX_BUILT - built) / nodes)
                        .min((MAX_CHARS - chars).checked_div(text).unwrap_or(usize::MAX));
                    let copies: Vec<Node> = (0..times).map(|_| copy(last)).collect();
                    built += times * nodes;
                    chars += times * text;
                    top.extend(copies);
                }
            }
            _ => {
                // Wrap the last node in up to 4 * (arg + 1) quotes, within the height budget.
                if let Some(mut node) = top.pop() {
                    let room = MAX_HEIGHT.saturating_sub(open + height(&node));
                    let levels = (4 * (arg + 1)).min(room);
                    for _ in 0..levels {
                        node = Node::Container("blockquote", vec![node]);
                    }
                    built += levels;
                    top.push(node);
                }
            }
        }
    }
    while stack.len() > 1 {
        let (kind, children) = stack.pop().expect("a container");
        stack
            .last_mut()
            .expect("the root")
            .1
            .push(Node::Container(kind, children));
    }
    stack.pop().expect("the root").1
}

fn copy(node: &Node) -> Node {
    match node {
        Node::Text(t) => Node::Text(t.clone()),
        Node::Leaf(v) => Node::Leaf(v.clone()),
        Node::Container(k, c) => Node::Container(k, c.iter().map(copy).collect()),
    }
}

/// Nodes and characters in `node`'s tree.
fn size(node: &Node) -> (usize, usize) {
    match node {
        Node::Container(_, c) => c
            .iter()
            .map(size)
            .fold((1, 0), |(n, t), (cn, ct)| (n + cn, t + ct)),
        Node::Text(t) => (1, t.chars().count()),
        Node::Leaf(_) => (1, 8),
    }
}

/// Levels in `node`'s tree.
fn height(node: &Node) -> usize {
    match node {
        Node::Container(_, c) => 1 + c.iter().map(height).max().unwrap_or(0),
        _ => 1,
    }
}

/// The JSON tree, with marker text in the nodes the walk must not reach: those deeper than
/// `MAX_ADF_DEPTH`, and those after the first `MAX_ADF_NODES` the walk visits (in pre-order,
/// counting only nodes it can reach by depth).
fn to_json(node: &Node, depth: usize, visited: &mut usize) -> Value {
    let reachable = depth <= MAX_ADF_DEPTH;
    let late = reachable && *visited >= MAX_ADF_NODES;
    if reachable {
        *visited += 1;
    }
    let marker = (!reachable).then_some(DEEP).or(late.then_some(LATE));
    // Built by moving the children in: `json!` would copy each subtree again at every level.
    let object = |kind: &str, field: &str, value: Value| {
        let mut map = serde_json::Map::new();
        map.insert("type".to_owned(), Value::String(kind.to_owned()));
        map.insert(field.to_owned(), value);
        Value::Object(map)
    };
    match (node, marker) {
        (Node::Text(_) | Node::Leaf(_), Some(m)) => object("text", "text", Value::String(m.into())),
        (Node::Text(t), None) => object("text", "text", Value::String(t.clone())),
        (Node::Leaf(v), None) => v.clone(),
        (Node::Container(kind, children), _) => {
            let content: Vec<Value> = children
                .iter()
                .map(|c| to_json(c, depth + 1, visited))
                .collect();
            object(kind, "content", Value::Array(content))
        }
    }
}

fuzz_target!(|input: &[u8]| {
    let Some((&mode, rest)) = input.split_first() else {
        return;
    };
    let (doc, generated) = if mode % 2 == 0 {
        let Ok(doc) = serde_json::from_slice::<Value>(rest) else {
            return;
        };
        (doc, false)
    } else {
        let children = build(rest);
        let mut visited = 1;
        let content: Vec<Value> = children
            .iter()
            .map(|c| to_json(c, 1, &mut visited))
            .collect();
        let mut doc = serde_json::Map::new();
        doc.insert("type".to_owned(), json!("doc"));
        doc.insert("version".to_owned(), json!(1));
        doc.insert("content".to_owned(), Value::Array(content));
        (Value::Object(doc), true)
    };

    let started = Instant::now();
    let text = adf_to_text(&doc);
    let took = started.elapsed();

    assert!(
        text.chars().count() <= MAX_BODY_CHARS,
        "{} characters",
        text.chars().count()
    );
    if let Some(c) = text.chars().find(|&c| is_hidden_char(c)) {
        panic!("a hidden character survives: U+{:04X}", u32::from(c));
    }
    if generated {
        assert!(!text.contains(DEEP), "text from past the depth cap");
        assert!(!text.contains(LATE), "text from past the node cap");
    }
    assert!(took <= BUDGET, "one document took {took:?}");
});
