//! Bounds the screen model's work per byte of pane output.
//!
//! vt100 0.16.2 repeats `CSI n L` (insert lines), `CSI n T` (scroll down) and `CSI n @` (insert
//! characters) `n` times with no limit, and `n` can be 65535: 128 bytes of `ESC[65535L` keep it
//! busy for seconds. (An upstream bug worth reporting.) [`CsiClamp`] rewrites the count of those,
//! and of `M`, `S`, `P` and `X`, to at most the screen's rows or columns before vt100 sees it,
//! which changes nothing a screen can show. It follows vte's states closely enough to find every
//! CSI that vte dispatches: a CSI survives C0 controls, DEL and bytes from 0x80 (vte executes or
//! ignores them in place), and only CAN, SUB or ESC end it early.

/// The longest CSI kept back while waiting for its final byte. A longer one is dropped.
const MAX_HELD: usize = 4096;

const ESC: u8 = 0x1b;
const CAN: u8 = 0x18;
const SUB: u8 = 0x1a;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum State {
    #[default]
    Ground,
    /// After ESC (already passed on).
    Escape,
    /// After ESC and an intermediate byte: not a CSI.
    EscapeIntermediate,
    /// Inside `ESC [`; the bytes from `[` are held back.
    Csi,
    /// Inside a CSI too long to keep: dropped up to its final byte.
    Skip,
}

/// A streaming filter over pane output, one per screen model.
#[derive(Debug, Default)]
pub(crate) struct CsiClamp {
    state: State,
    held: Vec<u8>,
}

impl CsiClamp {
    /// Appends `input`, filtered, to `out`, for a screen of `rows` by `cols`. Sequences may be
    /// split anywhere between calls.
    pub(crate) fn filter(&mut self, input: &[u8], rows: u16, cols: u16, out: &mut Vec<u8>) {
        let mut i = 0;
        while i < input.len() {
            if self.state == State::Ground {
                match input[i..].iter().position(|&b| b == ESC) {
                    None => {
                        out.extend_from_slice(&input[i..]);
                        return;
                    }
                    Some(n) => {
                        out.extend_from_slice(&input[i..=i + n]);
                        i += n + 1;
                        self.state = State::Escape;
                        continue;
                    }
                }
            }
            let byte = input[i];
            i += 1;
            self.step(byte, rows, cols, out);
        }
    }

    fn step(&mut self, byte: u8, rows: u16, cols: u16, out: &mut Vec<u8>) {
        let executed = matches!(byte, 0x00..=0x17 | 0x19 | 0x1c..=0x1f);
        match self.state {
            State::Ground => out.push(byte),
            State::Escape => {
                if byte == b'[' {
                    self.held.clear();
                    self.held.push(byte);
                    self.state = State::Csi;
                    return;
                }
                out.push(byte);
                self.state = match byte {
                    ESC => State::Escape,
                    CAN | SUB => State::Ground,
                    _ if executed || byte == 0x7f || byte >= 0x80 => State::Escape,
                    0x20..=0x2f => State::EscapeIntermediate,
                    _ => State::Ground,
                };
            }
            State::EscapeIntermediate => {
                out.push(byte);
                self.state = match byte {
                    ESC => State::Escape,
                    CAN | SUB | 0x30..=0x7e => State::Ground,
                    _ => State::EscapeIntermediate,
                };
            }
            State::Csi => match byte {
                _ if executed => out.push(byte),
                CAN | SUB => {
                    out.append(&mut self.held);
                    out.push(byte);
                    self.state = State::Ground;
                }
                ESC => {
                    out.append(&mut self.held);
                    out.push(byte);
                    self.state = State::Escape;
                }
                0x40..=0x7e => {
                    self.held.push(byte);
                    clamped(&self.held, rows, cols, out);
                    self.held.clear();
                    self.state = State::Ground;
                }
                _ => {
                    self.held.push(byte);
                    if self.held.len() > MAX_HELD {
                        self.held.clear();
                        // vte has seen only the ESC: cancel it.
                        out.push(CAN);
                        self.state = State::Skip;
                    }
                }
            },
            State::Skip => match byte {
                _ if executed => out.push(byte),
                CAN | SUB => {
                    out.push(byte);
                    self.state = State::Ground;
                }
                ESC => {
                    out.push(byte);
                    self.state = State::Escape;
                }
                0x40..=0x7e => self.state = State::Ground,
                _ => {}
            },
        }
    }
}

/// `held` is `[`, the parameters and the final byte. Passes it on, with the first parameter
/// lowered to the screen's size for the counted sequences that have plain parameters.
fn clamped(held: &[u8], rows: u16, cols: u16, out: &mut Vec<u8>) {
    let (Some(&last), Some(body)) = (held.last(), held.get(1..held.len() - 1)) else {
        out.extend_from_slice(held);
        return;
    };
    let limit = match last {
        b'L' | b'M' | b'S' | b'T' => rows,
        b'@' | b'P' | b'X' => cols,
        _ => {
            out.extend_from_slice(held);
            return;
        }
    };
    // vte ignores DEL and bytes from 0x80 inside a CSI. A private marker or an intermediate
    // makes it a different sequence, which vt100 does not repeat.
    let params = body.iter().filter(|&&b| b < 0x7f);
    if !params
        .clone()
        .all(|&b| b.is_ascii_digit() || b == b';' || b == b':')
    {
        out.extend_from_slice(held);
        return;
    }
    let first = params
        .take_while(|b| b.is_ascii_digit())
        .fold(0u32, |n, &d| {
            n.saturating_mul(10).saturating_add(u32::from(d - b'0'))
        });
    if first > u32::from(limit) {
        out.push(b'[');
        out.extend_from_slice(limit.max(1).to_string().as_bytes());
        out.push(last);
    } else {
        out.extend_from_slice(held);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(input: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        CsiClamp::default().filter(input, 24, 80, &mut out);
        out
    }

    #[test]
    fn counts_are_lowered_to_the_screen_size() {
        assert_eq!(run(b"a\x1b[65535Lb"), b"a\x1b[24Lb");
        assert_eq!(run(b"\x1b[70000T"), b"\x1b[24T");
        assert_eq!(run(b"\x1b[999@"), b"\x1b[80@");
        assert_eq!(
            run(b"\x1b[100M\x1b[100S\x1b[100P\x1b[100X"),
            b"\x1b[24M\x1b[24S\x1b[80P\x1b[80X"
        );
        // Only the first parameter counts; sub-parameters too.
        assert_eq!(run(b"\x1b[500;7L"), b"\x1b[24L");
        assert_eq!(run(b"\x1b[500:3L"), b"\x1b[24L");
        // DEL and high bytes are ignored by vte inside a CSI, so they do not hide a count.
        assert_eq!(run(b"\x1b[65\x7f535L"), b"\x1b[24L");
        assert_eq!(run(b"\x1b[65\xff535L"), b"\x1b[24L");
    }

    #[test]
    fn other_sequences_pass_unchanged() {
        for input in [
            &b"plain text \xe2\x9c\x93"[..],
            b"\x1b[24L\x1b[1L\x1b[L\x1b[0L\x1b[;5L",
            b"\x1b[38;2;255;0;0m\x1b[2J\x1b[999;999H\x1b[65535A",
            b"\x1b[?65535L\x1b[65535 L\x1b[>65535T",
            b"\x1b]0;title with \x1b[65535L inside\x07",
            b"\x1b(B\x1b)0[65535L",
            b"\x1bM\x1b7\x1b8",
        ] {
            let expected: Vec<u8> = if input.starts_with(b"\x1b]0") {
                // The OSC ends at the ESC; what follows is a real CSI.
                b"\x1b]0;title with \x1b[24L inside\x07".to_vec()
            } else {
                input.to_vec()
            };
            assert_eq!(run(input), expected, "{:?}", String::from_utf8_lossy(input));
        }
    }

    #[test]
    fn controls_inside_a_csi_do_not_hide_it() {
        // vte executes C0 controls in place and keeps the CSI; so does the filter.
        // (The ESC is passed on at once; vte also executes a control met in its escape state.)
        assert_eq!(run(b"\x1b[65\n535L"), b"\x1b\n[24L");
        assert_eq!(run(b"\x1b\n[65535L"), b"\x1b\n[24L");
        // CAN and SUB abort it; ESC starts another.
        assert_eq!(run(b"\x1b[655\x18L"), b"\x1b[655\x18L");
        assert_eq!(run(b"\x1b[655\x1b[65535L"), b"\x1b[655\x1b[24L");
    }

    #[test]
    fn an_overlong_csi_is_dropped() {
        let mut input = b"\x1b[".to_vec();
        input.extend(std::iter::repeat_n(b'9', MAX_HELD + 10));
        input.extend_from_slice(b"Lok");
        assert_eq!(run(&input), b"\x1b\x18ok");
    }

    #[test]
    fn any_split_gives_the_same_output() {
        let input =
            b"x\x1b[65535L\x1b[2;3H\x1b\n[999@\x1b[?25h\x1b]2;t\x07\x1b[38;5;1mend\x1b[100T";
        let whole = run(input);
        for split in 0..=input.len() {
            let mut clamp = CsiClamp::default();
            let mut out = Vec::new();
            clamp.filter(&input[..split], 24, 80, &mut out);
            clamp.filter(&input[split..], 24, 80, &mut out);
            assert_eq!(out, whole, "split at {split}");
        }
    }
}
