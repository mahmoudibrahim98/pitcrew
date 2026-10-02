//! `pitcrew_runtime::ControlParser::feed` on arbitrary tmux control-mode bytes.
//!
//! Input: one control byte `k`, then `k % 32` chunk sizes, then the stream.
//!
//! Checks, besides "no panic":
//! 1. **Chunking.** Feeding the stream in arbitrary chunks gives the same notifications, or the
//!    same latched `DesyncError`, and the same end state as feeding it at once. On arbitrary bytes
//!    a desync error (a line or reply over the limits) is an allowed outcome, not a crash.
//! 2. **Replies are opaque.** The stream's lines, framed as the body of one command reply,
//!    come back as exactly that reply's lines, with no notification escaping. Reply bodies carry
//!    pane content (titles, names, captured text), so a line such as `%exit` or `%output %1 …`
//!    inside one must never become a real notification. Lines that close this very reply
//!    (`%end`/`%error` with its time and number) are left out: tmux cannot escape those.
//! 3. **Output decoding.** Bytes escaped the way tmux escapes `%output` decode back exactly.
//!
//! Checks 2 and 3 build well-formed streams. One small enough that it cannot reach the default
//! limits must parse: a `DesyncError` there is a finding (threat model O15).
#![no_main]

use libfuzzer_sys::fuzz_target;
use pitcrew_runtime::control::{CommandReply, DesyncError, ParserLimits};
use pitcrew_runtime::{ControlParser, Notification, PaneId};

fuzz_target!(|input: &[u8]| {
    let Some((&k, rest)) = input.split_first() else {
        return;
    };
    let (cuts, stream) = rest.split_at(usize::from(k % 32).min(rest.len()));

    // 1. Chunking.
    let mut whole_parser = ControlParser::new();
    let whole = whole_parser.feed(stream);
    let mut parser = ControlParser::new();
    let mut chunked = Ok(Vec::new());
    let mut at = 0;
    let mut sizes = cuts.iter().map(|&c| usize::from(c % 64)).cycle();
    while at < stream.len() {
        // Without chunk sizes, one byte at a time. A size of 0 feeds an empty slice, then one
        // byte, so the loop always moves on.
        let mut n = sizes.next().unwrap_or(1);
        if n == 0 {
            feed_into(&mut parser, &[], &mut chunked);
            n = 1;
        }
        let end = (at + n).min(stream.len());
        feed_into(&mut parser, &stream[at..end], &mut chunked);
        at = end;
    }
    assert_eq!(chunked, whole, "chunking changed the notifications");
    assert_eq!(
        parser.finish(),
        whole_parser.finish(),
        "chunking changed the end state"
    );

    // 2. Replies are opaque.
    let time = 1_790_755_200 + u64::from(k);
    let number = u64::from(k) * 7 + 1;
    let mut framed = format!("%begin {time} {number} 1\n").into_bytes();
    let mut expected: Vec<&[u8]> = Vec::new();
    for line in stream.split(|&b| b == b'\n') {
        if closes(line, time, number) {
            continue;
        }
        framed.extend_from_slice(line);
        framed.push(b'\n');
        expected.push(line);
    }
    framed.extend_from_slice(format!("%end {time} {number} 1\n").as_bytes());
    if let Some(got) = parse_well_formed(&framed) {
        let [Notification::CommandReply(CommandReply { lines, failed, .. })] = &got[..] else {
            panic!("a reply body escaped its reply: {got:?}");
        };
        assert!(!failed);
        assert_eq!(
            lines.len(),
            expected.len(),
            "reply lines were lost or added"
        );
        for (got, want) in lines.iter().zip(&expected) {
            assert_eq!(strip_cr(got), strip_cr(want), "a reply line changed");
        }
    }

    // 3. Output decoding.
    let mut line = b"%output %7 ".to_vec();
    for &b in stream {
        if b < b' ' || b == b'\\' {
            line.extend_from_slice(format!("\\{b:03o}").as_bytes());
        } else {
            line.push(b);
        }
    }
    line.push(b'\n');
    if let Some(got) = parse_well_formed(&line) {
        assert_eq!(
            got,
            [Notification::Output {
                pane: PaneId(7),
                data: stream.to_vec(),
            }],
            "escaped output did not decode back"
        );
    }
});

/// Feed one chunk, collecting notifications until the first error. After it, every feed must
/// return that same error: it is latched until the parser is dropped.
fn feed_into(
    parser: &mut ControlParser,
    bytes: &[u8],
    into: &mut Result<Vec<Notification>, DesyncError>,
) {
    let result = parser.feed(bytes);
    match into {
        Ok(all) => match result {
            Ok(more) => all.extend(more),
            Err(error) => *into = Err(error),
        },
        Err(first) => assert_eq!(result, Err(*first), "a desync error was not latched"),
    }
}

/// Parse a stream this target built. Past the default limits a `DesyncError` is the right answer
/// (`None`); below them it is a finding.
fn parse_well_formed(wire: &[u8]) -> Option<Vec<Notification>> {
    let result = ControlParser::new().feed(wire);
    if within_default_limits(wire) {
        return Some(result.expect("a small, well-formed stream must parse (threat model O15)"));
    }
    result.ok()
}

/// Whether no line and no reply in `wire` can reach the default limits, whatever its shape. The
/// worst reply is all empty lines: each costs its LF plus 32 bytes of overhead.
fn within_default_limits(wire: &[u8]) -> bool {
    let limits = ParserLimits::default();
    wire.len() <= limits.max_line_bytes && wire.len().saturating_mul(33) <= limits.max_reply_bytes
}

/// Whether `line` could close the reply `time number`: `%end` or `%error` with those guard
/// values. Deliberately broader than the parser (a trailing CR is ignored, the flags field is not
/// checked), so the check never expects a line the parser may rightly treat as the end.
fn closes(line: &[u8], time: u64, number: u64) -> bool {
    let line = strip_cr(line);
    let mut fields = line.split(|&b| b == b' ');
    let name = fields.next().unwrap_or_default();
    if name != b"%end" && name != b"%error" {
        return false;
    }
    let (Some(t), Some(n)) = (fields.next(), fields.next()) else {
        return false;
    };
    number_of(t) == Some(time) && number_of(n) == Some(number)
}

fn number_of(bytes: &[u8]) -> Option<u64> {
    if bytes.is_empty() || !bytes.iter().all(u8::is_ascii_digit) {
        return None;
    }
    std::str::from_utf8(bytes).ok()?.parse().ok()
}

fn strip_cr(line: &[u8]) -> &[u8] {
    line.strip_suffix(b"\r").unwrap_or(line)
}
