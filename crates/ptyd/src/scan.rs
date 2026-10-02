//! Watches a terminal's output for what a terminal must answer or remember, since no terminal
//! emulator is attached to ptyd's PTYs:
//!
//! - **Queries.** Programs ask the terminal where the cursor is (`CSI 6 n`, `CSI ? 6 n`), for its
//!   status (`CSI 5 n`) and what it is (`CSI c`, `CSI > c`), and some wait for the answer: a
//!   TUI asking for the cursor position at start, and ConPTY itself, which asks once when it
//!   starts and waits. ptyd answers these, with fixed replies (a VT100 with advanced video, for
//!   `CSI c`) or the screen model's cursor. Nothing a program writes is ever echoed back.
//! - **Application cursor keys** (`DECCKM`, `CSI ? 1 h` / `l`, reset by `ESC c` and `CSI ! p`),
//!   so arrow keys are sent the way the program asked for.
//!
//! The scan is a small state machine over the bytes (sequences may be split anywhere between
//! reads) that keeps at most [`MAX_PARAMS`] bytes of a sequence.

/// Parameter and intermediate bytes kept per sequence; a longer one is not acted on.
const MAX_PARAMS: usize = 32;

const ESC: u8 = 0x1b;
const CAN: u8 = 0x18;
const SUB: u8 = 0x1a;

/// What the output asks of the terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Event {
    /// Application cursor keys on or off.
    AppCursor(bool),
    /// A fixed answer to send back.
    Answer(&'static [u8]),
    /// The cursor's position to send back: `CSI row ; col R`, with `?` after the CSI if
    /// `private`.
    Cursor {
        /// The query was `CSI ? 6 n`.
        private: bool,
    },
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum State {
    #[default]
    Ground,
    Escape,
    Csi,
}

/// The scanner of one terminal's output.
#[derive(Debug, Default)]
pub(crate) struct Scanner {
    state: State,
    params: Vec<u8>,
    /// The current sequence is longer than [`MAX_PARAMS`].
    overlong: bool,
}

impl Scanner {
    /// Scans `data`, appending what it asks for to `events`.
    pub(crate) fn feed(&mut self, data: &[u8], events: &mut Vec<Event>) {
        let mut i = 0;
        while i < data.len() {
            if self.state == State::Ground {
                match data[i..].iter().position(|&b| b == ESC) {
                    None => return,
                    Some(n) => {
                        i += n + 1;
                        self.state = State::Escape;
                        continue;
                    }
                }
            }
            let byte = data[i];
            i += 1;
            match self.state {
                State::Ground => {}
                State::Escape => {
                    self.state = match byte {
                        b'[' => {
                            self.params.clear();
                            self.overlong = false;
                            State::Csi
                        }
                        b'c' => {
                            // RIS: a full reset.
                            events.push(Event::AppCursor(false));
                            State::Ground
                        }
                        ESC => State::Escape,
                        _ => State::Ground,
                    };
                }
                State::Csi => match byte {
                    ESC => self.state = State::Escape,
                    CAN | SUB => self.state = State::Ground,
                    0x20..=0x3f => {
                        if self.params.len() < MAX_PARAMS {
                            self.params.push(byte);
                        } else {
                            self.overlong = true;
                        }
                    }
                    0x40..=0x7e => {
                        if !self.overlong {
                            self.finish(byte, events);
                        }
                        self.state = State::Ground;
                    }
                    // Other controls are carried out in place; the sequence goes on.
                    _ => {}
                },
            }
        }
    }

    fn finish(&self, last: u8, events: &mut Vec<Event>) {
        let params = self.params.as_slice();
        match (last, params) {
            (b'n', b"6") => events.push(Event::Cursor { private: false }),
            (b'n', b"?6") => events.push(Event::Cursor { private: true }),
            (b'n', b"5") => events.push(Event::Answer(b"\x1b[0n")),
            (b'c', b"" | b"0") => events.push(Event::Answer(b"\x1b[?1;2c")),
            (b'c', b">" | b">0") => events.push(Event::Answer(b"\x1b[>0;10;1c")),
            (b'p', b"!") => events.push(Event::AppCursor(false)),
            (b'h' | b'l', [b'?', modes @ ..])
                if modes.split(|&b| b == b';').any(|mode| mode == b"1") =>
            {
                events.push(Event::AppCursor(last == b'h'));
            }
            _ => {}
        }
    }
}

/// The answer to a cursor position query, 1-based.
pub(crate) fn cursor_report(private: bool, row: u16, col: u16) -> Vec<u8> {
    let mark = if private { "?" } else { "" };
    format!("\x1b[{mark}{};{}R", u32::from(row) + 1, u32::from(col) + 1).into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan(data: &[u8]) -> Vec<Event> {
        let mut events = Vec::new();
        Scanner::default().feed(data, &mut events);
        events
    }

    #[test]
    fn queries_are_answered_and_modes_followed() {
        assert_eq!(scan(b"abc\x1b[6n"), vec![Event::Cursor { private: false }]);
        assert_eq!(scan(b"\x1b[?6n"), vec![Event::Cursor { private: true }]);
        assert_eq!(scan(b"\x1b[5n"), vec![Event::Answer(b"\x1b[0n")]);
        assert_eq!(
            scan(b"\x1b[c\x1b[0c\x1b[>c"),
            vec![
                Event::Answer(b"\x1b[?1;2c"),
                Event::Answer(b"\x1b[?1;2c"),
                Event::Answer(b"\x1b[>0;10;1c")
            ]
        );
        assert_eq!(
            scan(b"\x1b[?1h\x1b[?25;1;1049l\x1b[?1h\x1bc\x1b[?1h\x1b[!p"),
            vec![
                Event::AppCursor(true),
                Event::AppCursor(false),
                Event::AppCursor(true),
                Event::AppCursor(false),
                Event::AppCursor(true),
                Event::AppCursor(false),
            ]
        );
        // Not queries: other modes, other finals, an aborted or overlong sequence.
        for quiet in [
            &b"\x1b[?12h\x1b[?10l\x1b[1h\x1b[6m\x1b[2J\x1b[16n\x1b[?1;2c"[..],
            b"\x1b[6\x18n",
            b"\x1b[6\x1bn",
        ] {
            assert!(scan(quiet).is_empty(), "{quiet:?}");
        }
        let mut long = b"\x1b[".to_vec();
        long.extend(std::iter::repeat_n(b'6', 100));
        long.push(b'n');
        assert!(scan(&long).is_empty());
        assert!(scan(b"\x1b[?1h".repeat(3).as_slice()).len() == 3);
    }

    #[test]
    fn any_split_gives_the_same_events() {
        let input = b"x\x1b[6ny\x1b[?1;25h\x1b[>c\x1b\x1b[5n\x1b[?6n\x1bc";
        let whole = scan(input);
        assert_eq!(whole.len(), 6);
        for split in 0..=input.len() {
            let mut scanner = Scanner::default();
            let mut events = Vec::new();
            scanner.feed(&input[..split], &mut events);
            scanner.feed(&input[split..], &mut events);
            assert_eq!(events, whole, "split at {split}");
        }
    }

    #[test]
    fn cursor_reports_are_one_based() {
        assert_eq!(cursor_report(false, 0, 0), b"\x1b[1;1R");
        assert_eq!(cursor_report(true, 4, 17), b"\x1b[?5;18R");
    }
}
