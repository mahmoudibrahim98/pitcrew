//! What the runtime knows about its terminals: each one's pane, liveness, output history and
//! screen model. Updated from `%output`, `%window-close` and `%layout-change` on the reader
//! thread, and read by the runtime's calls.
//!
//! The screen model is fed lazily, from the replay buffer, when the screen is read or resized:
//! output nobody looks at costs no terminal emulation. If more output arrived since than the
//! buffer keeps, the model starts again from the oldest byte kept (2 MiB of output redraws any
//! screen many times; modes set before it, such as a scroll region, are lost).

use std::collections::{HashMap, VecDeque};

use pitcrew_interfaces::runtime::{OutputChunk, Screen, TerminalInfo};
use pitcrew_protocol::ids::TerminalId;

use super::SESSION;
use crate::control::{PaneId, WindowId};
use crate::replay::ReplayBuffer;

/// How far ahead of the output the stored resume offset is kept. A restart after a crash
/// resumes numbering at most this far past the last byte a reader saw.
pub(crate) const RESERVE: u64 = 1 << 20;

/// Dead terminals kept readable, newest deaths first.
const DEAD_KEPT: usize = 16;
/// Output kept per pane that is not (yet) a known terminal: a new window's first bytes can
/// arrive before its creator records it.
const UNCLAIMED_BYTES: usize = 256 << 10;
/// Such panes tracked at once.
const UNCLAIMED_PANES: usize = 32;

/// Terminal sizes the runtime accepts, as the API does.
pub(crate) const MAX_SIZE: u16 = 1000;

pub(crate) struct Term {
    pub(crate) id: TerminalId,
    pub(crate) name: String,
    pub(crate) window: WindowId,
    pub(crate) pane: PaneId,
    pub(crate) pid: Option<u32>,
    pub(crate) alive: bool,
    buffer: ReplayBuffer,
    screen: vt100::Parser,
    /// Output up to here has been fed to `screen`.
    screened: u64,
    /// The resume offset last stored in tmux (or being stored).
    reserved: u64,
}

impl Term {
    pub(crate) fn info(&self) -> TerminalInfo {
        TerminalInfo {
            id: self.id,
            name: self.name.clone(),
            pid: self.pid,
            alive: self.alive,
            native_target: Some(format!("{SESSION}:{}", self.window)),
        }
    }

    pub(crate) fn end(&self) -> u64 {
        self.buffer.end()
    }

    pub(crate) fn read(&self, from: u64, max: usize) -> OutputChunk {
        self.buffer.read(from, max)
    }

    /// Feeds the screen model the output it has not seen.
    fn catch_up(&mut self) {
        let unseen = self.buffer.read(self.screened, usize::MAX);
        if unseen.truncated {
            let (rows, cols) = self.screen.screen().size();
            self.screen = vt100::Parser::new(rows, cols, 0);
        }
        self.screen.process(&unseen.data);
        self.screened = unseen.end;
    }

    pub(crate) fn screen(&mut self) -> Screen {
        self.catch_up();
        let screen = self.screen.screen();
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

    /// Output so far was drawn at the old size.
    pub(crate) fn set_size(&mut self, cols: u16, rows: u16) {
        let (cols, rows) = (clamp(cols), clamp(rows));
        if self.screen.screen().size() != (rows, cols) {
            self.catch_up();
            self.screen.screen_mut().set_size(rows, cols);
        }
    }
}

fn clamp(size: u16) -> u16 {
    size.clamp(1, MAX_SIZE)
}

/// A terminal to record: started now, or found in tmux after a restart.
pub(crate) struct Claim {
    pub(crate) id: TerminalId,
    pub(crate) name: String,
    pub(crate) window: WindowId,
    pub(crate) pane: PaneId,
    pub(crate) pid: Option<u32>,
    pub(crate) cols: u16,
    pub(crate) rows: u16,
    /// Where output numbering starts.
    pub(crate) offset: u64,
    pub(crate) alive: bool,
}

/// One pane as `list-panes` reported it (see [`LIST_FORMAT`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Listed {
    pub(crate) window: WindowId,
    pub(crate) pane: PaneId,
    pub(crate) pid: Option<u32>,
    pub(crate) dead: bool,
    pub(crate) cols: u16,
    pub(crate) rows: u16,
    pub(crate) offset: Option<u64>,
    pub(crate) terminal: Option<TerminalId>,
    pub(crate) name: String,
}

/// The `list-panes -F` format [`parse_listed`] reads. The window name is last: it may hold
/// spaces and `|`.
pub(crate) const LIST_FORMAT: &str = "#{window_id} #{pane_id} #{pane_pid} #{pane_dead} #{pane_width} #{pane_height} #{@pitcrew-offset}|#{@pitcrew-terminal}|#{window_name}";

/// Reads one line of [`LIST_FORMAT`]; `None` if it is malformed. Pane text never reaches these
/// fields, but they are checked anyway.
pub(crate) fn parse_listed(line: &[u8]) -> Option<Listed> {
    let line = std::str::from_utf8(line).ok()?;
    let mut fields = line.splitn(7, ' ');
    let window = WindowId::parse(fields.next()?.as_bytes())?;
    let pane = PaneId::parse(fields.next()?.as_bytes())?;
    let pid = fields.next()?.parse().ok();
    let dead = match fields.next()? {
        "0" => false,
        "1" => true,
        _ => return None,
    };
    let cols = fields.next()?.parse().ok()?;
    let rows = fields.next()?.parse().ok()?;
    let mut rest = fields.next()?.splitn(3, '|');
    let offset = rest.next()?;
    let offset = if offset.is_empty() {
        None
    } else {
        Some(offset.parse().ok()?)
    };
    let terminal = match rest.next()? {
        "" => None,
        id => Some(id.parse().ok()?),
    };
    Some(Listed {
        window,
        pane,
        pid,
        dead,
        cols,
        rows,
        offset,
        terminal,
        name: rest.next()?.to_owned(),
    })
}

/// The panes of a tmux layout (`%layout-change`) with their sizes: `(pane number, cols, rows)`.
/// `csum,WxH,X,Y,ID` for one pane; `csum,WxH,X,Y{…}` or `[…]` around comma-separated cells.
pub(crate) fn layout_panes(layout: &[u8]) -> Vec<(u64, u16, u16)> {
    let mut panes = Vec::new();
    let Some(comma) = layout.iter().position(|&b| b == b',') else {
        return panes;
    };
    let mut rest = &layout[comma + 1..];
    if cell(&mut rest, &mut panes, 0).is_none() {
        panes.clear();
    }
    panes
}

fn cell(input: &mut &[u8], panes: &mut Vec<(u64, u16, u16)>, depth: usize) -> Option<()> {
    if depth > 64 {
        return None;
    }
    let cols = number(input)?;
    expect(input, b'x')?;
    let rows = number(input)?;
    expect(input, b',')?;
    number(input)?;
    expect(input, b',')?;
    number(input)?;
    match input.first() {
        Some(b',') => {
            *input = &input[1..];
            let pane = number(input)?;
            panes.push((pane, u16::try_from(cols).ok()?, u16::try_from(rows).ok()?));
            Some(())
        }
        Some(&open @ (b'{' | b'[')) => {
            *input = &input[1..];
            let close = if open == b'{' { b'}' } else { b']' };
            loop {
                cell(input, panes, depth + 1)?;
                match input.first() {
                    Some(b',') => *input = &input[1..],
                    Some(&b) if b == close => {
                        *input = &input[1..];
                        return Some(());
                    }
                    _ => return None,
                }
            }
        }
        _ => None,
    }
}

fn number(input: &mut &[u8]) -> Option<u64> {
    let digits = input.iter().take_while(|b| b.is_ascii_digit()).count();
    if digits == 0 || digits > 19 {
        return None;
    }
    let value = std::str::from_utf8(&input[..digits]).ok()?.parse().ok()?;
    *input = &input[digits..];
    Some(value)
}

fn expect(input: &mut &[u8], byte: u8) -> Option<()> {
    if input.first() == Some(&byte) {
        *input = &input[1..];
        Some(())
    } else {
        None
    }
}

/// The runtime's terminals.
pub(crate) struct Terminals {
    terms: HashMap<TerminalId, Term>,
    panes: HashMap<PaneId, TerminalId>,
    unclaimed: HashMap<PaneId, Vec<u8>>,
    unclaimed_order: VecDeque<PaneId>,
    dead: VecDeque<TerminalId>,
    history: usize,
    /// Set while the runtime shuts down: output is no longer recorded, so the offsets it stores
    /// in tmux stay exact.
    pub(crate) closing: bool,
}

impl Terminals {
    pub(crate) fn new(history: usize) -> Self {
        Self {
            terms: HashMap::new(),
            panes: HashMap::new(),
            unclaimed: HashMap::new(),
            unclaimed_order: VecDeque::new(),
            dead: VecDeque::new(),
            history,
            closing: false,
        }
    }

    pub(crate) fn get(&self, id: TerminalId) -> Option<&Term> {
        self.terms.get(&id)
    }

    pub(crate) fn get_mut(&mut self, id: TerminalId) -> Option<&mut Term> {
        self.terms.get_mut(&id)
    }

    pub(crate) fn all(&self) -> impl Iterator<Item = &Term> {
        self.terms.values()
    }

    pub(crate) fn any_alive(&self) -> bool {
        self.terms.values().any(|t| t.alive)
    }

    /// Records output. `reserve(pane, offset)` stores a new resume offset in tmux before the
    /// stream passes the stored one; it returns whether the command was sent.
    pub(crate) fn output(
        &mut self,
        pane: PaneId,
        data: &[u8],
        reserve: impl FnOnce(PaneId, u64) -> bool,
    ) {
        if self.closing || data.is_empty() {
            return;
        }
        let Some(term) = self.panes.get(&pane).and_then(|id| self.terms.get_mut(id)) else {
            self.unclaimed(pane, data);
            return;
        };
        let end = term.buffer.end().saturating_add(data.len() as u64);
        if end.saturating_add(RESERVE / 2) > term.reserved {
            let next = end.saturating_add(RESERVE);
            if reserve(pane, next) {
                term.reserved = next;
            }
        }
        term.buffer.append(data);
    }

    fn unclaimed(&mut self, pane: PaneId, data: &[u8]) {
        if !self.unclaimed.contains_key(&pane) {
            if self.unclaimed_order.len() >= UNCLAIMED_PANES
                && let Some(oldest) = self.unclaimed_order.pop_front()
            {
                self.unclaimed.remove(&oldest);
            }
            self.unclaimed_order.push_back(pane);
        }
        let kept = self.unclaimed.entry(pane).or_default();
        kept.extend_from_slice(data);
        if kept.len() > UNCLAIMED_BYTES {
            let excess = kept.len() - UNCLAIMED_BYTES;
            kept.drain(..excess);
        }
    }

    /// Records a terminal, with any output its pane produced before. Numbering starts at
    /// `claim.offset`; nobody has read the earlier bytes, so they are renumbered from there.
    pub(crate) fn claim(&mut self, claim: Claim) -> TerminalInfo {
        let early = self.unclaimed.remove(&claim.pane).unwrap_or_default();
        self.unclaimed_order.retain(|p| *p != claim.pane);
        if let Some(old) = self.terms.get(&claim.id) {
            self.panes.remove(&old.pane);
        }
        if !claim.alive {
            // An ended program's pane id may already belong to a newer pane.
            self.panes.remove(&claim.pane);
        }
        let mut term = Term {
            id: claim.id,
            name: claim.name,
            window: claim.window,
            pane: claim.pane,
            pid: claim.pid,
            alive: claim.alive,
            buffer: ReplayBuffer::with_capacity_at(self.history, claim.offset),
            screen: vt100::Parser::new(clamp(claim.rows), clamp(claim.cols), 0),
            screened: claim.offset,
            reserved: claim.offset,
        };
        term.buffer.append(&early);
        let info = term.info();
        if claim.alive {
            self.panes.insert(claim.pane, claim.id);
        }
        self.terms.insert(claim.id, term);
        if !claim.alive {
            self.dead.retain(|d| *d != claim.id);
            self.bury(claim.id);
        }
        info
    }

    /// A resume offset is stored in tmux.
    pub(crate) fn reserved(&mut self, id: TerminalId, offset: u64) {
        if let Some(term) = self.terms.get_mut(&id) {
            term.reserved = term.reserved.max(offset);
        }
    }

    /// A window has closed. True if a live terminal ended with it.
    pub(crate) fn window_closed(&mut self, window: WindowId) -> bool {
        let ids: Vec<TerminalId> = self
            .terms
            .values()
            .filter(|t| t.window == window && t.alive)
            .map(|t| t.id)
            .collect();
        for &id in &ids {
            self.died(id);
        }
        !ids.is_empty()
    }

    pub(crate) fn pane_size(&mut self, pane: PaneId, cols: u16, rows: u16) {
        if let Some(term) = self.panes.get(&pane).and_then(|id| self.terms.get_mut(id)) {
            term.set_size(cols, rows);
        }
    }

    pub(crate) fn set_size(&mut self, id: TerminalId, cols: u16, rows: u16) {
        if let Some(term) = self.terms.get_mut(&id) {
            term.set_size(cols, rows);
        }
    }

    /// A terminal's program has ended. Its pane no longer maps to it: pane ids are unique only
    /// within one server's life, and a later server reuses them.
    pub(crate) fn died(&mut self, id: TerminalId) {
        let Some(term) = self.terms.get_mut(&id) else {
            return;
        };
        if !term.alive {
            return;
        }
        term.alive = false;
        let pane = term.pane;
        if self.panes.get(&pane) == Some(&id) {
            self.panes.remove(&pane);
        }
        self.bury(id);
    }

    /// Forgets output of panes no terminal has claimed. Called before attaching: such output
    /// only matters within one connection, and pane ids repeat across servers.
    pub(crate) fn forget_unclaimed(&mut self) {
        self.unclaimed.clear();
        self.unclaimed_order.clear();
    }

    /// The server is gone: so is every terminal in it.
    pub(crate) fn all_died(&mut self) {
        let ids: Vec<TerminalId> = self
            .terms
            .values()
            .filter(|t| t.alive)
            .map(|t| t.id)
            .collect();
        for id in ids {
            self.died(id);
        }
    }

    fn bury(&mut self, id: TerminalId) {
        self.dead.push_back(id);
        while self.dead.len() > DEAD_KEPT {
            if let Some(gone) = self.dead.pop_front()
                && let Some(term) = self.terms.remove(&gone)
                && self.panes.get(&term.pane) == Some(&gone)
            {
                self.panes.remove(&term.pane);
            }
        }
    }

    /// Brings the records up to date with `list-panes`: tagged panes not known yet are adopted
    /// (numbering resumes at their stored offset), known ones are updated, and live terminals
    /// missing from the list have ended. Returns the adopted terminals, whose resume offsets the
    /// caller stores again before anyone reads them.
    pub(crate) fn reconcile(&mut self, listed: &[Listed]) -> Vec<(TerminalId, PaneId, u64)> {
        let mut adopted = Vec::new();
        let mut seen = Vec::new();
        for row in listed {
            let Some(id) = row.terminal else { continue };
            seen.push(id);
            match self.terms.get_mut(&id) {
                Some(term) => {
                    if term.pane != row.pane {
                        self.panes.remove(&term.pane);
                        self.panes.insert(row.pane, id);
                        term.pane = row.pane;
                    }
                    term.window = row.window;
                    term.pid = row.pid.or(term.pid);
                    term.set_size(row.cols, row.rows);
                    if row.dead {
                        self.died(id);
                    }
                }
                None => {
                    let offset = row.offset.unwrap_or(0);
                    self.claim(Claim {
                        id,
                        name: row.name.clone(),
                        window: row.window,
                        pane: row.pane,
                        pid: row.pid,
                        cols: row.cols,
                        rows: row.rows,
                        offset,
                        alive: !row.dead,
                    });
                    if !row.dead {
                        adopted.push((id, row.pane, offset.saturating_add(RESERVE)));
                    }
                }
            }
        }
        let gone: Vec<TerminalId> = self
            .terms
            .values()
            .filter(|t| t.alive && !seen.contains(&t.id))
            .map(|t| t.id)
            .collect();
        for id in gone {
            self.died(id);
        }
        adopted
    }

    /// Every live terminal's pane and exact end, to store when the runtime shuts down.
    pub(crate) fn ends(&self) -> Vec<(PaneId, u64)> {
        self.terms
            .values()
            .filter(|t| t.alive)
            .map(|t| (t.pane, t.buffer.end()))
            .collect()
    }
}

/// A window name tmux shows as typed: control characters become spaces (tmux could print
/// them raw), and it is cut to 100 characters.
pub(crate) fn window_name(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(100)
        .collect::<String>()
        .trim()
        .to_owned()
}

/// A POSIX environment variable name.
pub(crate) fn is_env_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    bytes
        .next()
        .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
        && bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claim(id: TerminalId, pane: u64, offset: u64) -> Claim {
        Claim {
            id,
            name: "t".into(),
            window: WindowId(pane),
            pane: PaneId(pane),
            pid: Some(42),
            cols: 20,
            rows: 5,
            offset,
            alive: true,
        }
    }

    #[test]
    fn layouts_give_every_pane_its_size() {
        assert_eq!(layout_panes(b"a87e,100x30,0,0,1"), vec![(1, 100, 30)]);
        assert_eq!(
            layout_panes(b"419a,80x24,0,0[80x12,0,0,1,80x11,0,13,2]"),
            vec![(1, 80, 12), (2, 80, 11)]
        );
        assert_eq!(
            layout_panes(b"c0de,120x40,0,0{60x40,0,0,3,59x40,61,0[59x20,61,0,4,59x19,61,21,5]}"),
            vec![(3, 60, 40), (4, 59, 20), (5, 59, 19)]
        );
        for bad in [
            &b""[..],
            b"nocomma",
            b"a,80x24,0,0",
            b"a,80x24,0,0,",
            b"a,80x24,0,0[80x12,0,0,1",
            b"a,99999999x24,0,0,1",
            b"a,80y24,0,0,1",
        ] {
            assert!(layout_panes(bad).is_empty(), "{bad:?}");
        }
        let deep = format!("a,{}", "1x1,0,0[".repeat(100));
        assert!(layout_panes(deep.as_bytes()).is_empty());
    }

    #[test]
    fn listed_panes_parse_strictly() {
        let id = TerminalId::new();
        let line = format!("@3 %7 1234 0 80 24 2097152|{id}|agent | one");
        assert_eq!(
            parse_listed(line.as_bytes()),
            Some(Listed {
                window: WindowId(3),
                pane: PaneId(7),
                pid: Some(1234),
                dead: false,
                cols: 80,
                rows: 24,
                offset: Some(2_097_152),
                terminal: Some(id),
                name: "agent | one".into(),
            })
        );
        let untagged = parse_listed(b"@0 %0  1 80 24 ||pitcrew-start").expect("untagged");
        assert_eq!(
            (
                untagged.terminal,
                untagged.offset,
                untagged.dead,
                untagged.pid
            ),
            (None, None, true, None)
        );
        for bad in [
            &b"@0 %0 99 0 80 24"[..],
            b"@0 %0 99 2 80 24 ||x",
            b"0 %0 99 0 80 24 ||x",
            b"@0 %0 99 0 80 24 x||",
            b"@0 %0 99 0 80 24 |term_nonsense|",
            b"@0 %0 99 0 80 24 \xff||",
        ] {
            assert_eq!(parse_listed(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn early_output_is_kept_and_renumbered_when_claimed() {
        let mut t = Terminals::new(1024);
        t.output(PaneId(5), b"early ", |_, _| true);
        let id = TerminalId::new();
        t.claim(claim(id, 5, 100));
        t.output(PaneId(5), b"late", |_, _| true);
        let chunk = t.get(id).expect("term").read(0, 100);
        assert_eq!((chunk.offset, chunk.end, chunk.truncated), (100, 110, true));
        assert_eq!(chunk.data, b"early late");
        assert_eq!(t.get_mut(id).expect("term").screen().rows[0], "early late");
    }

    #[test]
    fn unclaimed_output_is_bounded() {
        let mut t = Terminals::new(1024);
        for pane in 0..(UNCLAIMED_PANES as u64 + 5) {
            t.output(PaneId(pane), &[b'x'; 10], |_, _| true);
        }
        assert_eq!(t.unclaimed.len(), UNCLAIMED_PANES);
        assert!(!t.unclaimed.contains_key(&PaneId(0)));
        t.output(PaneId(100), &vec![b'y'; UNCLAIMED_BYTES + 10], |_, _| true);
        assert_eq!(t.unclaimed[&PaneId(100)].len(), UNCLAIMED_BYTES);
    }

    #[test]
    fn the_stored_offset_stays_ahead_of_the_output() {
        let mut t = Terminals::new(4096);
        let id = TerminalId::new();
        t.claim(claim(id, 1, 0));
        let mut stored = Vec::new();
        let chunk = vec![b'z'; 300 << 10];
        for _ in 0..10 {
            t.output(PaneId(1), &chunk, |_, offset| {
                stored.push(offset);
                true
            });
            let end = t.get(id).expect("term").end();
            assert!(
                stored.last().is_some_and(|&s| s >= end + RESERVE / 2),
                "{end}"
            );
        }
        // About one reservation per RESERVE/2 bytes, not one per write.
        assert!(stored.len() <= 7, "{stored:?}");
        // A reservation that could not be sent is retried with the next output.
        let before = stored.len();
        let big = vec![b'w'; RESERVE as usize];
        t.output(PaneId(1), &big, |_, _| false);
        t.output(PaneId(1), b"x", |_, offset| {
            stored.push(offset);
            true
        });
        assert_eq!(stored.len(), before + 1);
        // Nothing is recorded while closing.
        t.closing = true;
        let end = t.get(id).expect("term").end();
        t.output(PaneId(1), b"ignored", |_, _| true);
        assert_eq!(t.get(id).expect("term").end(), end);
    }

    #[test]
    fn reconcile_adopts_updates_and_buries() {
        let mut t = Terminals::new(1024);
        let (known, vanished, found, dead) = (
            TerminalId::new(),
            TerminalId::new(),
            TerminalId::new(),
            TerminalId::new(),
        );
        t.claim(claim(known, 1, 0));
        t.claim(claim(vanished, 2, 0));
        let row = |terminal, pane, offset, dead| Listed {
            window: WindowId(pane),
            pane: PaneId(pane),
            pid: Some(7),
            dead,
            cols: 30,
            rows: 6,
            offset,
            terminal,
            name: "found".into(),
        };
        let adopted = t.reconcile(&[
            row(Some(known), 1, None, false),
            row(Some(found), 3, Some(5000), false),
            row(Some(dead), 4, Some(10), true),
            row(None, 0, None, false),
        ]);
        assert_eq!(adopted, vec![(found, PaneId(3), 5000 + RESERVE)]);
        assert!(t.get(known).expect("known").alive);
        assert_eq!(t.get_mut(known).expect("known").screen().cols, 30);
        assert!(!t.get(vanished).expect("vanished").alive);
        let found_term = t.get(found).expect("found");
        assert!(found_term.alive);
        assert_eq!(found_term.end(), 5000);
        assert_eq!(
            found_term.info().native_target.as_deref(),
            Some("pitcrew:@3")
        );
        assert!(!t.get(dead).expect("dead").alive);
    }

    #[test]
    fn only_the_newest_dead_are_kept() {
        let mut t = Terminals::new(64);
        let ids: Vec<TerminalId> = (0..(DEAD_KEPT as u64 + 3))
            .map(|_| TerminalId::new())
            .collect();
        for (n, id) in ids.iter().enumerate() {
            t.claim(claim(*id, n as u64, 0));
        }
        for (n, _) in ids.iter().enumerate() {
            t.window_closed(WindowId(n as u64));
        }
        assert!(t.get(ids[0]).is_none());
        assert!(t.get(ids[2]).is_none());
        assert!(t.get(ids[3]).is_some_and(|x| !x.alive));
        assert!(!t.any_alive());
        // A late output for an evicted pane is not mistaken for another terminal's.
        t.output(PaneId(0), b"x", |_, _| true);
        assert!(t.all().all(|x| x.end() == 0));
    }

    #[test]
    fn a_new_server_reusing_a_dead_terminals_pane_id_gets_its_own_output() {
        let mut t = Terminals::new(1024);
        let (old, new) = (TerminalId::new(), TerminalId::new());
        t.claim(claim(old, 1, 0));
        t.output(PaneId(1), b"old", |_, _| true);
        t.window_closed(WindowId(1));
        // A later server numbers its panes from the start again.
        t.forget_unclaimed();
        t.output(PaneId(1), b"first ", |_, _| true);
        t.claim(claim(new, 1, 0));
        t.output(PaneId(1), b"words", |_, _| true);
        assert_eq!(t.get(old).expect("old").read(0, 100).data, b"old");
        assert_eq!(t.get(new).expect("new").read(0, 100).data, b"first words");
    }

    #[test]
    fn screens_follow_cursor_movement_and_resizes() {
        let mut t = Terminals::new(1024);
        let id = TerminalId::new();
        t.claim(claim(id, 1, 0));
        t.output(
            PaneId(1),
            b"\x1b[2J\x1b[3;5Hprompt> \x1b[1;1Htop\rT\x1b[3;13H",
            |_, _| true,
        );
        let screen = t.get_mut(id).expect("term").screen();
        assert_eq!(screen.rows.len(), 5);
        assert_eq!(screen.rows[0], "Top");
        assert_eq!(screen.rows[2], "    prompt>");
        assert_eq!((screen.cursor_row, screen.cursor_col), (2, 12));
        t.pane_size(PaneId(1), 40, 3);
        let screen = t.get_mut(id).expect("term").screen();
        assert_eq!((screen.cols, screen.rows.len()), (40, 3));
        t.set_size(id, 0, 5000);
        let screen = t.get_mut(id).expect("term").screen();
        assert_eq!((screen.cols, screen.rows.len()), (1, usize::from(MAX_SIZE)));
    }

    #[test]
    fn the_screen_model_catches_up_lazily_and_restarts_after_an_overflow() {
        let mut t = Terminals::new(64);
        let id = TerminalId::new();
        t.claim(claim(id, 1, 0));
        t.output(PaneId(1), b"\x1b[3;3Hkept", |_, _| true);
        assert_eq!(t.get_mut(id).expect("term").screen().rows[2], "  kept");
        t.output(PaneId(1), b"\x1b[4;1Hmore", |_, _| true);
        let screen = t.get_mut(id).expect("term").screen();
        assert_eq!(
            (screen.rows[2].as_str(), screen.rows[3].as_str()),
            ("  kept", "more")
        );
        // More output than the buffer keeps arrives before the next look: the model starts
        // again from the oldest byte kept.
        t.output(PaneId(1), &[b'.'; 200], |_, _| true);
        t.output(PaneId(1), b"\x1b[1;1Hfresh", |_, _| true);
        let screen = t.get_mut(id).expect("term").screen();
        assert!(screen.rows[0].starts_with("fresh"), "{screen:?}");
        assert!(
            !screen.rows.iter().any(|r| r.contains("kept")),
            "{screen:?}"
        );
    }

    #[test]
    fn names_and_variables() {
        assert_eq!(window_name(" agent\n%exit\t#{x} "), "agent %exit #{x}");
        assert_eq!(window_name(&"n".repeat(300)).len(), 100);
        assert_eq!(window_name("\u{85}\u{9b}x"), "x");
        for good in ["A", "_x", "PATH", "a1_B2"] {
            assert!(is_env_name(good), "{good}");
        }
        for bad in ["", "1A", "A=B", "A B", "Ä", "A-B"] {
            assert!(!is_env_name(bad), "{bad}");
        }
    }
}
