//! A terminal's visible screen, computed from its output with bounded work. The tmux runtime and
//! pitcrew-ptyd share it.
//!
//! - **Lazily.** The model is fed from the terminal's replay buffer when the screen is read, so
//!   output nobody looks at costs no emulation. Each model has its own lock: one terminal's
//!   screen never holds up another terminal, or the thread that records output.
//! - **Sizes in order.** A size change is recorded with the offset it happened at, and applied
//!   there when the model catches up.
//! - **Bounded.** If more output arrived since the last look than the buffer keeps, the model
//!   starts again from the oldest byte kept (2 MiB redraws any screen many times; modes set before
//!   it, such as a scroll region, are lost), and so it does if the backlog times the screen's area
//!   is past a work budget, from the last 256 KiB. Counts and strings in the output are bounded
//!   first ([`clamp`]); the model is at least 2 columns wide (vt100 panics drawing a wide
//!   character in 1); and should vt100 panic anyway, the model starts again past that output
//!   instead of failing on it at every read.

mod clamp;

use std::collections::VecDeque;
use std::sync::Mutex;

use pitcrew_interfaces::runtime::{OutputChunk, Screen};

use self::clamp::CsiClamp;
use crate::lock;
use crate::replay::ReplayBuffer;

/// Terminal sizes accepted, in columns and in rows, as the API does: 1 to 1000.
pub const MAX_SIZE: u16 = 1000;

/// The screen model's work per read, in bytes of output times cells of screen. 2 MiB of output
/// on a 200 by 80 screen fits; a big backlog on a huge screen does not.
const WORK_BUDGET: u64 = 1 << 35;
/// What a model that is over its budget starts again from.
const BUDGET_TAIL: usize = 256 << 10;
/// Size changes remembered until the screen model has applied them.
pub(crate) const RESIZES_KEPT: usize = 64;

/// Rows of a screen model.
pub(crate) fn clamp(size: u16) -> u16 {
    size.clamp(1, MAX_SIZE)
}

/// Columns of a screen model: vt100 0.16.2 panics drawing a wide character on 1 column.
pub(crate) fn columns(size: u16) -> u16 {
    size.clamp(2, MAX_SIZE)
}

/// The last `keep` bytes of `pieces`, with their offsets.
fn tail(pieces: Vec<(u64, Vec<u8>)>, keep: usize) -> Vec<(u64, Vec<u8>)> {
    let mut kept = Vec::new();
    let mut left = keep;
    for (offset, data) in pieces.into_iter().rev() {
        if left == 0 {
            break;
        }
        if data.len() <= left {
            left -= data.len();
            kept.push((offset, data));
        } else {
            let cut = data.len() - left;
            kept.push((offset + cut as u64, data[cut..].to_vec()));
            left = 0;
        }
    }
    kept.reverse();
    kept
}

/// Output a screen model has not processed, with the size changes among it.
pub(crate) struct Unseen {
    /// In order, each with the offset of its first byte.
    pieces: Vec<(u64, Vec<u8>)>,
    /// What the model had not seen was dropped: it starts again.
    restart: bool,
    resizes: Vec<(u64, u16, u16)>,
    end: u64,
    /// The latest size: (cols, rows).
    size: (u16, u16),
}

impl Unseen {
    /// `pieces` (in order, each with its offset) for a screen of `size`, ending at `end`.
    /// `restart` says the model missed output and must start again. Applies the work budget:
    /// a backlog whose size times the screen's area is past it is cut to its last 256 KiB.
    pub(crate) fn new(
        pieces: Vec<(u64, Vec<u8>)>,
        restart: bool,
        resizes: Vec<(u64, u16, u16)>,
        end: u64,
        size: (u16, u16),
    ) -> Self {
        let total: usize = pieces.iter().map(|(_, data)| data.len()).sum();
        let area = u64::from(size.0) * u64::from(size.1);
        let (pieces, restart) =
            if total > BUDGET_TAIL && (total as u64).saturating_mul(area) > WORK_BUDGET {
                (tail(pieces, BUDGET_TAIL), true)
            } else {
                (pieces, restart)
            };
        Self {
            pieces,
            restart,
            resizes,
            end,
            size,
        }
    }
}

/// A terminal's screen, from its output.
pub(crate) struct ScreenModel {
    parser: vt100::Parser,
    clamp: CsiClamp,
    /// Output before this offset has been processed.
    pub(crate) screened: u64,
    #[cfg(test)]
    pub(crate) panic_once: bool,
}

impl ScreenModel {
    pub(crate) fn new(cols: u16, rows: u16, offset: u64) -> Self {
        Self {
            parser: vt100::Parser::new(clamp(rows), columns(cols), 0),
            clamp: CsiClamp::default(),
            screened: offset,
            #[cfg(test)]
            panic_once: false,
        }
    }

    fn catch_up(&mut self, unseen: Unseen) {
        let mut resizes = unseen.resizes.into_iter().peekable();
        if unseen.restart {
            let start = unseen.pieces.first().map_or(unseen.end, |p| p.0);
            let (mut rows, mut cols) = self.parser.screen().size();
            while let Some(&(at, c, r)) = resizes.peek()
                && at <= start
            {
                (cols, rows) = (c, r);
                resizes.next();
            }
            self.parser = vt100::Parser::new(rows, cols, 0);
            self.clamp = CsiClamp::default();
        }
        for (offset, data) in unseen.pieces {
            let mut at = offset;
            let mut rest = data.as_slice();
            loop {
                while let Some(&(when, cols, rows)) = resizes.peek()
                    && when <= at
                {
                    self.parser.screen_mut().set_size(rows, cols);
                    resizes.next();
                }
                let until = resizes.peek().map_or(u64::MAX, |r| r.0);
                let take = usize::try_from(until - at)
                    .unwrap_or(usize::MAX)
                    .min(rest.len());
                self.feed(&rest[..take]);
                rest = &rest[take..];
                at += take as u64;
                if rest.is_empty() {
                    break;
                }
            }
        }
        for (_, cols, rows) in resizes {
            self.parser.screen_mut().set_size(rows, cols);
        }
        self.screened = unseen.end;
    }

    fn feed(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        #[cfg(test)]
        if std::mem::take(&mut self.panic_once) {
            panic!("injected screen model panic");
        }
        let (rows, cols) = self.parser.screen().size();
        let mut clamped = Vec::with_capacity(bytes.len());
        self.clamp.filter(bytes, rows, cols, &mut clamped);
        self.parser.process(&clamped);
    }

    fn snapshot(&self) -> Screen {
        let screen = self.parser.screen();
        let (rows, cols) = screen.size();
        let (cursor_row, cursor_col) = screen.cursor_position();
        let mut lines: Vec<String> = screen
            .rows(0, cols)
            .map(|row| row.trim_end_matches(' ').to_owned())
            .collect();
        lines.truncate(usize::from(rows));
        Screen {
            rows: lines,
            cols,
            cursor_row,
            cursor_col,
        }
    }

    /// Feeds `unseen` and returns the screen. If vt100 panics on that output, the model starts
    /// again past it (an empty screen now) rather than failing on it at every read.
    pub(crate) fn show(&mut self, unseen: Unseen, terminal: &dyn std::fmt::Display) -> Screen {
        let (end, (cols, rows)) = (unseen.end, unseen.size);
        let shown = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.catch_up(unseen);
            self.snapshot()
        }));
        shown.unwrap_or_else(|_| {
            tracing::warn!(%terminal, "the screen model failed on this output; starting it again");
            *self = Self::new(cols, rows, end);
            self.snapshot()
        })
    }
}

/// One terminal's output history (a [`ReplayBuffer`]) and its size changes.
struct History {
    buffer: ReplayBuffer,
    /// The latest size: (cols, rows), as the model uses it.
    size: (u16, u16),
    /// Size changes the model may not have applied yet: (offset, cols, rows).
    resizes: VecDeque<(u64, u16, u16)>,
}

impl History {
    fn unseen(&mut self, from: u64) -> Unseen {
        while self.resizes.front().is_some_and(|r| r.0 < from) {
            self.resizes.pop_front();
        }
        // The buffer has dropped bytes the model never saw: it starts again from what is kept.
        let restart = self.buffer.start() > from;
        let chunk = self.buffer.read(from, usize::MAX);
        Unseen::new(
            vec![(chunk.offset, chunk.data)],
            restart,
            self.resizes.iter().copied().collect(),
            self.buffer.end(),
            self.size,
        )
    }
}

/// A terminal's output and screen, for a process that owns the terminal itself (pitcrew-ptyd):
/// output is appended as it arrives, read back by offset, and emulated only when the screen is
/// read. The history and the model each have their own lock; appending takes only the
/// history's, briefly, so a screen being computed never holds up output. Lock order: the model,
/// then the history.
pub struct Screened {
    history: Mutex<History>,
    model: Mutex<ScreenModel>,
}

impl std::fmt::Debug for Screened {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let history = lock(&self.history);
        f.debug_struct("Screened")
            .field("end", &history.buffer.end())
            .field("size", &history.size)
            .finish_non_exhaustive()
    }
}

impl Screened {
    /// An empty terminal of `cols` by `rows` (each kept within 1 to [`MAX_SIZE`]) that keeps
    /// the last `history` bytes of output.
    pub fn new(history: usize, cols: u16, rows: u16) -> Self {
        Self {
            history: Mutex::new(History {
                buffer: ReplayBuffer::new(history),
                size: (columns(cols), clamp(rows)),
                resizes: VecDeque::new(),
            }),
            model: Mutex::new(ScreenModel::new(cols, rows, 0)),
        }
    }

    /// Records output; returns where the stream ends now.
    pub fn append(&self, data: &[u8]) -> u64 {
        let mut history = lock(&self.history);
        history.buffer.append(data);
        history.buffer.end()
    }

    /// Records a size change, at the current end of the output.
    pub fn resized(&self, cols: u16, rows: u16) {
        let mut history = lock(&self.history);
        let size = (columns(cols), clamp(rows));
        if size == history.size {
            return;
        }
        history.size = size;
        if history.resizes.len() >= RESIZES_KEPT {
            history.resizes.pop_front();
        }
        let end = history.buffer.end();
        history.resizes.push_back((end, size.0, size.1));
    }

    /// Output from `from`, at most `max` bytes (see [`ReplayBuffer::read`]).
    pub fn read(&self, from: u64, max: usize) -> OutputChunk {
        lock(&self.history).buffer.read(from, max)
    }

    /// Where the output ends.
    pub fn end(&self) -> u64 {
        lock(&self.history).buffer.end()
    }

    /// The screen now: emulates the output since the last read, up to the work budget. Bounded,
    /// but call it from a blocking thread rather than an async executor's.
    pub fn screen(&self, terminal: &dyn std::fmt::Display) -> Screen {
        let mut model = lock(&self.model);
        let unseen = lock(&self.history).unseen(model.screened);
        model.show(unseen, terminal)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_screened_terminal_follows_output_and_resizes_in_order() {
        let t = Screened::new(1024, 20, 5);
        t.append(b"\x1b[2J\x1b[3;5Hprompt> \x1b[1;1Htop\rT\x1b[3;13H");
        let s = t.screen(&"t");
        assert_eq!(s.rows.len(), 5);
        assert_eq!(s.rows[0], "Top");
        assert_eq!(s.rows[2], "    prompt>");
        assert_eq!((s.cursor_row, s.cursor_col), (2, 12));
        // Drawn at 20 columns, then the screen grows to 40.
        t.append(b"\x1b[5;1H0123456789abcdefghij");
        t.resized(40, 3);
        t.append(b"\x1b[3;1H0123456789abcdefghijKLMNOP");
        let s = t.screen(&"t");
        assert_eq!((s.cols, s.rows.len()), (40, 3));
        assert_eq!(s.rows[2], "0123456789abcdefghijKLMNOP");
        let chunk = t.read(0, 4);
        assert_eq!((chunk.offset, chunk.data.as_slice()), (0, &b"\x1b[2J"[..]));
        assert_eq!(chunk.end, t.end());
    }

    #[test]
    fn a_screened_terminal_restarts_after_an_overflow_and_bounds_floods() {
        let t = Screened::new(64, 20, 5);
        t.append(b"\x1b[3;3Hkept");
        assert_eq!(t.screen(&"t").rows[2], "  kept");
        t.append(&[b'.'; 200]);
        t.append(b"\x1b[1;1Hfresh");
        let s = t.screen(&"t");
        assert!(s.rows[0].starts_with("fresh"), "{s:?}");
        assert!(!s.rows.iter().any(|r| r.contains("kept")), "{s:?}");

        let big = Screened::new(1 << 20, 80, 24);
        let flood: Vec<u8> = b"\x1b[65535L\x1b[65535T\x1b[65535@"
            .iter()
            .copied()
            .cycle()
            .take(64 << 10)
            .collect();
        big.append(&flood);
        big.append(b"\x1b[1;1Hstill here");
        let started = std::time::Instant::now();
        let s = big.screen(&"t");
        assert!(
            started.elapsed() < std::time::Duration::from_secs(2),
            "{:?}",
            started.elapsed()
        );
        assert_eq!(s.rows[0], "still here");
    }

    #[test]
    fn a_screened_model_that_panics_starts_again_past_that_output() {
        let t = Screened::new(1024, 20, 5);
        t.append(b"breaks it");
        lock(&t.model).panic_once = true;
        assert!(t.screen(&"t").rows.iter().all(String::is_empty));
        t.append(b"next");
        assert_eq!(t.screen(&"t").rows[0], "next");
    }
}
