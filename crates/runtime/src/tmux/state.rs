//! What the runtime knows about its terminals: each one's pane, liveness, output history and
//! screen model. Updated from `%output`, `%window-close` and `%layout-change` on the reader
//! thread, and read by the runtime's calls.
//!
//! - **The screen model** of each terminal ([`crate::screen`]) has its own lock, and is fed
//!   lazily from the replay buffer when the screen is read: output nobody looks at costs no
//!   emulation, and one terminal's screen never holds up another terminal or the reader thread.
//!   Its work is bounded as that module describes.
//! - **Panes and tags.** Within one server's life a pane id never changes owner, so a known
//!   live terminal is always found by its own pane, whatever tag that pane carries now; a tag
//!   is only used to adopt a pane the runtime does not know (after a restart), and only if no
//!   other pane carries the same id.
//! - **Lost output.** After the control client was replaced, a terminal's numbering skips one
//!   offset, so a reader at the old end reads `truncated`: output may have been printed while
//!   no client was attached. The history before stays readable.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};

use pitcrew_interfaces::runtime::{OutputChunk, Screen, TerminalInfo};
use pitcrew_protocol::ids::TerminalId;

use super::SESSION;
use super::conn::lock;
use crate::control::{PaneId, WindowId};
use crate::replay::ReplayBuffer;
pub(crate) use crate::screen::MAX_SIZE;
use crate::screen::{RESIZES_KEPT, ScreenModel, Unseen, clamp, columns};

/// How far ahead of the output the stored resume offset is kept. A restart after a crash
/// resumes numbering at most this far past the last byte a reader saw.
pub(crate) const RESERVE: u64 = 1 << 20;

/// Dead terminals kept readable, newest deaths first.
const DEAD_KEPT: usize = 16;
/// Output kept per pane that is not (yet) a known terminal: a new window's first bytes can
/// arrive before its creator records it, and output after a reconnect waits for reconcile.
const UNCLAIMED_BYTES: usize = 256 << 10;
/// Such panes tracked at once.
const UNCLAIMED_PANES: usize = 32;

pub(crate) struct Term {
    pub(crate) id: TerminalId,
    pub(crate) name: String,
    pub(crate) window: WindowId,
    pub(crate) pane: PaneId,
    pub(crate) pid: Option<u32>,
    pub(crate) alive: bool,
    /// False from a lost connection until reconcile finds the pane again.
    attached: bool,
    buffer: ReplayBuffer,
    /// The offset `buffer` was started at.
    origin: u64,
    /// History from before output was lost, readable until the next loss.
    previous: Option<ReplayBuffer>,
    screen: Arc<Mutex<ScreenModel>>,
    /// The latest size: (cols, rows).
    size: (u16, u16),
    /// Size changes not yet known to be applied: (offset, cols, rows).
    resizes: VecDeque<(u64, u16, u16)>,
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
        if let Some(previous) = &self.previous
            && from < previous.end()
        {
            let mut chunk = previous.read(from, max);
            chunk.end = self.buffer.end();
            return chunk;
        }
        self.buffer.read(from, max)
    }

    fn resized(&mut self, cols: u16, rows: u16) {
        let size = (columns(cols), clamp(rows));
        if size == self.size {
            return;
        }
        self.size = size;
        if self.resizes.len() >= RESIZES_KEPT {
            self.resizes.pop_front();
        }
        self.resizes.push_back((self.buffer.end(), size.0, size.1));
    }

    /// Numbering skips one offset: whatever was printed while detached is not in the stream.
    fn lose_output(&mut self, history: usize) {
        let next = self.buffer.end().saturating_add(1);
        let fresh = ReplayBuffer::with_capacity_at(history, next);
        self.previous = Some(std::mem::replace(&mut self.buffer, fresh));
        self.origin = next;
    }

    /// The output a screen model that has seen everything before `from` still needs.
    fn unseen(&mut self, from: u64) -> Unseen {
        while self.resizes.front().is_some_and(|r| r.0 < from) {
            self.resizes.pop_front();
        }
        let mut pieces = Vec::new();
        let mut restart = false;
        let mut at = from;
        if let Some(previous) = &self.previous
            && at < previous.end()
        {
            let chunk = previous.read(at, usize::MAX);
            restart = chunk.truncated;
            pieces.push((chunk.offset, chunk.data));
            at = self.origin;
        }
        if self.buffer.start() > at.max(self.origin) {
            // The buffer has dropped bytes the model never saw: start again from what is kept.
            pieces.clear();
            restart = true;
        }
        let chunk = self.buffer.read(at, usize::MAX);
        pieces.push((chunk.offset, chunk.data));
        Unseen::new(
            pieces,
            restart,
            self.resizes.iter().copied().collect(),
            self.buffer.end(),
            self.size,
        )
    }
}

/// A terminal's screen now. Takes its own lock, and `terminals` only briefly, so it never holds
/// up other terminals. Lock order: a screen model, then `terminals`; never the reverse.
///
/// Bounded, but it may emulate up to the work budget: call it from a blocking thread, not an
/// async executor's.
pub(crate) fn screen(terminals: &Mutex<Terminals>, id: TerminalId) -> Option<Screen> {
    let model = Arc::clone(&lock(terminals).terms.get(&id)?.screen);
    let mut model = lock(&model);
    let unseen = lock(terminals).terms.get_mut(&id)?.unseen(model.screened);
    Some(model.show(unseen, &id))
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

/// What a pane's `@pitcrew-terminal` says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tag {
    None,
    Id(TerminalId),
    /// Something that is not a terminal id.
    Invalid,
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
    pub(crate) tag: Tag,
    pub(crate) name: String,
}

/// The `list-panes -F` format [`parse_listed`] reads. The window name is last: it may hold
/// spaces and `|`.
pub(crate) const LIST_FORMAT: &str = "#{window_id} #{pane_id} #{pane_pid} #{pane_dead} #{pane_width} #{pane_height} #{@pitcrew-offset}|#{@pitcrew-terminal}|#{window_name}";

/// Reads one line of [`LIST_FORMAT`]. The window and pane ids come first and must parse: a line
/// without them is not a pane. The option values can be set by anything that reaches the
/// server (a program in a pane can), so a bad one only makes that field unknown.
pub(crate) fn parse_listed(line: &[u8]) -> Option<Listed> {
    let line = String::from_utf8_lossy(line);
    let mut fields = line.splitn(7, ' ');
    let window = WindowId::parse(fields.next()?.as_bytes())?;
    let pane = PaneId::parse(fields.next()?.as_bytes())?;
    let pid = fields.next().and_then(|p| p.parse().ok());
    let dead = fields.next() == Some("1");
    let cols = fields.next().and_then(|c| c.parse().ok()).unwrap_or(80);
    let rows = fields.next().and_then(|r| r.parse().ok()).unwrap_or(24);
    let mut rest = fields.next().unwrap_or_default().splitn(3, '|');
    let offset = rest.next().and_then(|o| o.parse().ok());
    let tag = match rest.next() {
        None | Some("") => Tag::None,
        Some(id) => id.parse().map_or(Tag::Invalid, Tag::Id),
    };
    Some(Listed {
        window,
        pane,
        pid,
        dead,
        cols,
        rows,
        offset,
        tag,
        name: rest.next().unwrap_or_default().to_owned(),
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
    unclaimed: HashMap<PaneId, ReplayBuffer>,
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
        self.unclaimed
            .entry(pane)
            .or_insert_with(|| ReplayBuffer::new(UNCLAIMED_BYTES))
            .append(data);
    }

    fn take_unclaimed(&mut self, pane: PaneId) -> Vec<u8> {
        self.unclaimed_order.retain(|p| *p != pane);
        self.unclaimed
            .remove(&pane)
            .map(|kept| kept.read(0, usize::MAX).data)
            .unwrap_or_default()
    }

    /// Records a terminal, with any output its pane produced before. Numbering starts at
    /// `claim.offset`; nobody has read the earlier bytes, so they are renumbered from there.
    pub(crate) fn claim(&mut self, claim: Claim) -> TerminalInfo {
        let early = self.take_unclaimed(claim.pane);
        if let Some(old) = self.terms.get(&claim.id)
            && self.panes.get(&old.pane) == Some(&claim.id)
        {
            self.panes.remove(&old.pane);
        }
        let mut term = Term {
            id: claim.id,
            name: claim.name,
            window: claim.window,
            pane: claim.pane,
            pid: claim.pid,
            alive: claim.alive,
            attached: true,
            buffer: ReplayBuffer::with_capacity_at(self.history, claim.offset),
            origin: claim.offset,
            previous: None,
            screen: Arc::new(Mutex::new(ScreenModel::new(
                claim.cols,
                claim.rows,
                claim.offset,
            ))),
            size: (columns(claim.cols), clamp(claim.rows)),
            resizes: VecDeque::new(),
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

    /// A pane's size changed (`%layout-change`). Applied to the screen when it is next read.
    pub(crate) fn pane_size(&mut self, pane: PaneId, cols: u16, rows: u16) {
        if let Some(term) = self.panes.get(&pane).and_then(|id| self.terms.get_mut(id)) {
            term.resized(cols, rows);
        }
    }

    pub(crate) fn set_size(&mut self, id: TerminalId, cols: u16, rows: u16) {
        if let Some(term) = self.terms.get_mut(&id) {
            term.resized(cols, rows);
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

    /// The control client is going away or being replaced. Until reconcile has found each live
    /// terminal's pane again, their output waits with the unclaimed output (pane ids repeat
    /// across servers), and a terminal found again marks its output lost in between.
    pub(crate) fn detach(&mut self) {
        self.unclaimed.clear();
        self.unclaimed_order.clear();
        for term in self.terms.values_mut().filter(|t| t.alive) {
            term.attached = false;
            if self.panes.get(&term.pane) == Some(&term.id) {
                self.panes.remove(&term.pane);
            }
        }
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

    /// Brings the records up to date with `list-panes` of the same server.
    ///
    /// - A live terminal keeps its own pane (found again after a reconnect); one whose pane is
    ///   gone has ended. Its pane's tag is not consulted: a program could have changed it.
    /// - Another pane tagged with an id the runtime does not know is adopted, numbering on from
    ///   its stored offset, unless the id is on more than one pane (none is adopted then).
    /// - A pane id listed more than once is not to be believed: a raw newline in an option value
    ///   starts a forged row. A known terminal on such a pane keeps what is known of it (its
    ///   window, its liveness), and such a pane is never adopted.
    ///
    /// Returns the adopted terminals, whose resume offsets the caller stores again before
    /// anyone reads them.
    pub(crate) fn reconcile(&mut self, listed: &[Listed]) -> Vec<(TerminalId, PaneId, u64)> {
        let mut times: HashMap<PaneId, usize> = HashMap::new();
        for row in listed {
            *times.entry(row.pane).or_default() += 1;
        }
        let forged: HashSet<PaneId> = times
            .into_iter()
            .filter(|&(_, n)| n > 1)
            .map(|(pane, _)| pane)
            .collect();
        for pane in &forged {
            tracing::warn!(%pane, "a pane is listed more than once; not believing its rows");
        }
        let rows: HashMap<PaneId, &Listed> = listed
            .iter()
            .filter(|row| !forged.contains(&row.pane))
            .map(|row| (row.pane, row))
            .collect();
        let known: Vec<(TerminalId, PaneId)> = self
            .terms
            .values()
            .filter(|t| t.alive)
            .map(|t| (t.id, t.pane))
            .collect();
        let known_panes: HashSet<PaneId> = known.iter().map(|&(_, pane)| pane).collect();
        for (id, pane) in known {
            match rows.get(&pane) {
                Some(row) if !row.dead => self.found(id, pane, Some(row)),
                // Its pane is there, but what the rows say about it cannot be trusted.
                None if forged.contains(&pane) => self.found(id, pane, None),
                _ => self.died(id),
            }
        }
        let unknown =
            |row: &&Listed| !known_panes.contains(&row.pane) && !forged.contains(&row.pane);
        let mut copies: HashMap<TerminalId, usize> = HashMap::new();
        for row in listed.iter().filter(unknown) {
            if let Tag::Id(id) = row.tag {
                *copies.entry(id).or_default() += 1;
            }
        }
        let mut adopted = Vec::new();
        for row in listed.iter().filter(unknown) {
            let Tag::Id(id) = row.tag else { continue };
            if copies.get(&id).is_some_and(|&n| n > 1) {
                tracing::warn!(%id, pane = %row.pane, "a terminal id is on several panes; adopting none");
                continue;
            }
            if self.terms.contains_key(&id) {
                tracing::warn!(%id, pane = %row.pane, "a pane carries a known terminal's id; ignoring it");
                continue;
            }
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
        adopted
    }

    /// A live terminal's pane is listed: map it again, after a reconnect with a gap first, and
    /// take what its row says when the row can be trusted.
    fn found(&mut self, id: TerminalId, pane: PaneId, row: Option<&Listed>) {
        let reattached = self.terms.get(&id).is_some_and(|t| !t.attached);
        let early = if reattached {
            self.take_unclaimed(pane)
        } else {
            Vec::new()
        };
        let history = self.history;
        let Some(term) = self.terms.get_mut(&id) else {
            return;
        };
        if reattached {
            term.lose_output(history);
            term.buffer.append(&early);
            term.attached = true;
        }
        if let Some(row) = row {
            term.window = row.window;
            term.pid = row.pid.or(term.pid);
            term.resized(row.cols, row.rows);
        }
        self.panes.insert(pane, id);
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

    fn row(tag: Tag, pane: u64, offset: Option<u64>, dead: bool) -> Listed {
        Listed {
            window: WindowId(pane),
            pane: PaneId(pane),
            pid: Some(7),
            dead,
            cols: 30,
            rows: 6,
            offset,
            tag,
            name: "found".into(),
        }
    }

    fn shown(t: &Mutex<Terminals>, id: TerminalId) -> Screen {
        screen(t, id).expect("screen")
    }

    fn data(t: &Terminals, id: TerminalId, from: u64) -> OutputChunk {
        t.get(id).expect("term").read(from, usize::MAX)
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
    fn listed_panes_need_ids_and_tolerate_bad_option_values() {
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
                tag: Tag::Id(id),
                name: "agent | one".into(),
            })
        );
        let untagged = parse_listed(b"@0 %0  1 80 24 ||pitcrew-start").expect("untagged");
        assert_eq!(
            (untagged.tag, untagged.offset, untagged.dead, untagged.pid),
            (Tag::None, None, true, None)
        );
        // A forged or damaged value makes only that field unknown.
        let bad = parse_listed(b"@0 %4 99 0 80 24 x|term_nonsense|").expect("bad values");
        assert_eq!(
            (bad.pane, bad.offset, bad.tag),
            (PaneId(4), None, Tag::Invalid)
        );
        assert!(parse_listed(b"@0 %4 99 0 80 24 \xff|\xff|\xff").is_some());
        for bad in [&b"0 %0 99 0 80 24 ||x"[..], b"@0 0 1", b"", b"garbage"] {
            assert_eq!(parse_listed(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn early_output_is_kept_and_renumbered_when_claimed() {
        let t = Mutex::new(Terminals::new(1024));
        let id = TerminalId::new();
        {
            let mut t = lock(&t);
            t.output(PaneId(5), b"early ", |_, _| true);
            t.claim(claim(id, 5, 100));
            t.output(PaneId(5), b"late", |_, _| true);
            let chunk = data(&t, id, 0);
            assert_eq!((chunk.offset, chunk.end, chunk.truncated), (100, 110, true));
            assert_eq!(chunk.data, b"early late");
        }
        assert_eq!(shown(&t, id).rows[0], "early late");
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
        assert_eq!(t.take_unclaimed(PaneId(100)).len(), UNCLAIMED_BYTES);
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
    fn reconcile_adopts_finds_and_buries() {
        let mut t = Terminals::new(1024);
        let (known, vanished, found, dead) = (
            TerminalId::new(),
            TerminalId::new(),
            TerminalId::new(),
            TerminalId::new(),
        );
        t.claim(claim(known, 1, 0));
        t.claim(claim(vanished, 2, 0));
        let adopted = t.reconcile(&[
            row(Tag::Id(known), 1, None, false),
            row(Tag::Id(found), 3, Some(5000), false),
            row(Tag::Id(dead), 4, Some(10), true),
            row(Tag::None, 0, None, false),
        ]);
        assert_eq!(adopted, vec![(found, PaneId(3), 5000 + RESERVE)]);
        assert!(t.get(known).expect("known").alive);
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
    fn tags_cannot_move_a_known_terminal_or_adopt_copies() {
        let mut t = Terminals::new(1024);
        let (mine, unknown) = (TerminalId::new(), TerminalId::new());
        t.claim(claim(mine, 1, 0));
        // Someone copied my tag to pane 2 and wrote garbage into mine, and put one unknown id
        // on two panes.
        let adopted = t.reconcile(&[
            row(Tag::Invalid, 1, None, false),
            row(Tag::Id(mine), 2, None, false),
            row(Tag::Id(unknown), 3, Some(9), false),
            row(Tag::Id(unknown), 4, Some(9), false),
        ]);
        assert!(adopted.is_empty(), "{adopted:?}");
        assert!(t.get(unknown).is_none());
        let term = t.get(mine).expect("mine");
        assert!(term.alive, "an unreadable tag is not a missing pane");
        assert_eq!(term.pane, PaneId(1));
        t.output(PaneId(1), b"to mine", |_, _| true);
        t.output(PaneId(2), b"to the copy", |_, _| true);
        assert_eq!(data(&t, mine, 0).data, b"to mine");
        // Without its own pane, it has ended, whatever other panes say.
        t.reconcile(&[row(Tag::Id(mine), 2, None, false)]);
        assert!(!t.get(mine).expect("mine").alive);
    }

    #[test]
    fn a_reconnect_marks_lost_output_and_keeps_the_history() {
        let mut t = Terminals::new(1024);
        let id = TerminalId::new();
        t.claim(claim(id, 1, 0));
        t.output(PaneId(1), b"before", |_, _| true);
        t.detach();
        // Output on the new connection waits until reconcile has found the pane again.
        t.output(PaneId(1), b"early ", |_, _| true);
        assert_eq!(t.get(id).expect("term").end(), 6);
        t.reconcile(&[row(Tag::Id(id), 1, None, false)]);
        t.output(PaneId(1), b"after", |_, _| true);
        let old = data(&t, id, 0);
        assert_eq!(
            (old.offset, old.data.as_slice(), old.end, old.truncated),
            (0, &b"before"[..], 18, false)
        );
        let gap = data(&t, id, 6);
        assert_eq!(
            (gap.offset, gap.data.as_slice(), gap.truncated),
            (7, &b"early after"[..], true)
        );
        // A reconcile on the same connection marks nothing.
        t.reconcile(&[row(Tag::Id(id), 1, None, false)]);
        assert_eq!(t.get(id).expect("term").end(), 18);
    }

    #[test]
    fn a_new_server_reusing_a_dead_terminals_pane_id_gets_its_own_output() {
        let mut t = Terminals::new(1024);
        let (old, new) = (TerminalId::new(), TerminalId::new());
        t.claim(claim(old, 1, 0));
        t.output(PaneId(1), b"old", |_, _| true);
        t.window_closed(WindowId(1));
        t.detach();
        t.output(PaneId(1), b"first ", |_, _| true);
        t.claim(claim(new, 1, 0));
        t.output(PaneId(1), b"words", |_, _| true);
        assert_eq!(data(&t, old, 0).data, b"old");
        assert_eq!(data(&t, new, 0).data, b"first words");
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
        t.output(PaneId(0), b"x", |_, _| true);
        assert!(t.all().all(|x| x.end() == 0));
    }

    #[test]
    fn screens_follow_cursor_movement_and_resizes_in_order() {
        let t = Mutex::new(Terminals::new(1024));
        let id = TerminalId::new();
        lock(&t).claim(claim(id, 1, 0));
        lock(&t).output(
            PaneId(1),
            b"\x1b[2J\x1b[3;5Hprompt> \x1b[1;1Htop\rT\x1b[3;13H",
            |_, _| true,
        );
        let s = shown(&t, id);
        assert_eq!(s.rows.len(), 5);
        assert_eq!(s.rows[0], "Top");
        assert_eq!(s.rows[2], "    prompt>");
        assert_eq!((s.cursor_row, s.cursor_col), (2, 12));
        // Output drawn before a resize lands at the old size, even when read after it: a long
        // line wraps at 20 columns, then the screen grows to 40.
        lock(&t).output(PaneId(1), b"\x1b[5;1H0123456789abcdefghij", |_, _| true);
        lock(&t).pane_size(PaneId(1), 40, 3);
        lock(&t).output(PaneId(1), b"\x1b[3;1H0123456789abcdefghijKLMNOP", |_, _| {
            true
        });
        let s = shown(&t, id);
        assert_eq!((s.cols, s.rows.len()), (40, 3));
        assert_eq!(s.rows[2], "0123456789abcdefghijKLMNOP");
        lock(&t).set_size(id, 0, 5000);
        let s = shown(&t, id);
        assert_eq!((s.cols, s.rows.len()), (2, usize::from(MAX_SIZE)));
    }

    #[test]
    fn the_screen_model_catches_up_lazily_and_restarts_after_an_overflow() {
        let t = Mutex::new(Terminals::new(64));
        let id = TerminalId::new();
        lock(&t).claim(claim(id, 1, 0));
        lock(&t).output(PaneId(1), b"\x1b[3;3Hkept", |_, _| true);
        assert_eq!(shown(&t, id).rows[2], "  kept");
        lock(&t).output(PaneId(1), b"\x1b[4;1Hmore", |_, _| true);
        let s = shown(&t, id);
        assert_eq!((s.rows[2].as_str(), s.rows[3].as_str()), ("  kept", "more"));
        lock(&t).output(PaneId(1), &[b'.'; 200], |_, _| true);
        lock(&t).output(PaneId(1), b"\x1b[1;1Hfresh", |_, _| true);
        let s = shown(&t, id);
        assert!(s.rows[0].starts_with("fresh"), "{s:?}");
        assert!(!s.rows.iter().any(|r| r.contains("kept")), "{s:?}");
    }

    #[test]
    fn the_screen_continues_across_lost_output() {
        let t = Mutex::new(Terminals::new(1024));
        let id = TerminalId::new();
        lock(&t).claim(claim(id, 1, 0));
        lock(&t).output(PaneId(1), b"\x1b[1;1Hone", |_, _| true);
        {
            let mut t = lock(&t);
            t.detach();
            t.reconcile(&[row(Tag::Id(id), 1, None, false)]);
            t.output(PaneId(1), b"\x1b[2;1Htwo", |_, _| true);
        }
        let s = shown(&t, id);
        assert_eq!((s.rows[0].as_str(), s.rows[1].as_str()), ("one", "two"));
    }

    #[test]
    fn huge_counts_cost_no_more_than_the_screen_size() {
        let t = Mutex::new(Terminals::new(1 << 20));
        let id = TerminalId::new();
        lock(&t).claim(claim(id, 1, 0));
        let flood: Vec<u8> = b"\x1b[65535L\x1b[65535T\x1b[65535@"
            .iter()
            .copied()
            .cycle()
            .take(64 << 10)
            .collect();
        lock(&t).output(PaneId(1), &flood, |_, _| true);
        lock(&t).output(PaneId(1), b"\x1b[1;1Hstill here", |_, _| true);
        let started = std::time::Instant::now();
        let s = shown(&t, id);
        assert!(
            started.elapsed() < std::time::Duration::from_secs(2),
            "{:?}",
            started.elapsed()
        );
        assert_eq!(s.rows[0], "still here");
    }

    #[test]
    fn a_busy_screen_model_holds_up_no_other_terminal() {
        let t = Mutex::new(Terminals::new(1024));
        let (a, b) = (TerminalId::new(), TerminalId::new());
        lock(&t).claim(claim(a, 1, 0));
        lock(&t).claim(claim(b, 2, 0));
        let model = Arc::clone(&lock(&t).terms.get(&a).expect("a").screen);
        std::thread::scope(|scope| {
            // As if `a`'s screen were being computed from a flood.
            let busy = lock(&model);
            let (tx, rx) = std::sync::mpsc::channel();
            let t = &t;
            scope.spawn(move || {
                // The reader thread records output, and `b`'s screen is read meanwhile.
                lock(t).output(PaneId(2), b"hello", |_, _| true);
                let _ = tx.send(shown(t, b));
            });
            let got = rx.recv_timeout(std::time::Duration::from_secs(10));
            drop(busy);
            assert_eq!(got.expect("b was held up").rows[0], "hello");
        });
    }

    #[test]
    fn duplicated_pane_rows_change_nothing_known() {
        let mut t = Terminals::new(1024);
        let (mine, other) = (TerminalId::new(), TerminalId::new());
        t.claim(claim(mine, 1, 0));
        // A newline in an option value made a second row for pane 1, saying it is dead and in
        // another window, and a row for a pane 5 twice, tagged with a new id.
        let mut forged = row(Tag::None, 1, None, true);
        forged.window = WindowId(9);
        let adopted = t.reconcile(&[
            row(Tag::Id(mine), 1, None, false),
            forged,
            row(Tag::Id(other), 5, Some(3), false),
            row(Tag::Id(other), 5, Some(3), false),
        ]);
        assert!(adopted.is_empty(), "{adopted:?}");
        assert!(t.get(other).is_none());
        let term = t.get(mine).expect("mine");
        assert!(term.alive);
        assert_eq!(term.window, WindowId(1));
        // It is still found by its pane after a reconnect, with the gap marked.
        t.detach();
        t.reconcile(&[
            row(Tag::Id(mine), 1, None, true),
            row(Tag::Id(mine), 1, None, false),
        ]);
        t.output(PaneId(1), b"x", |_, _| true);
        assert!(t.get(mine).expect("mine").alive);
        assert_eq!(data(&t, mine, 1).data, b"x");
    }

    #[test]
    fn a_wide_character_on_one_column_does_not_break_the_screen() {
        let t = Mutex::new(Terminals::new(1024));
        let id = TerminalId::new();
        let mut narrow = claim(id, 1, 0);
        narrow.cols = 1;
        lock(&t).claim(narrow);
        lock(&t).output(PaneId(1), "雪x".as_bytes(), |_, _| true);
        let s = shown(&t, id);
        assert_eq!(s.cols, 2);
        assert!(s.rows.iter().any(|r| r.contains('雪')), "{s:?}");
        lock(&t).set_size(id, 1, 5);
        lock(&t).output(PaneId(1), "\r\n雪".as_bytes(), |_, _| true);
        assert_eq!(shown(&t, id).cols, 2);
    }

    #[test]
    fn a_screen_model_that_panics_starts_again_past_that_output() {
        let t = Mutex::new(Terminals::new(1024));
        let id = TerminalId::new();
        lock(&t).claim(claim(id, 1, 0));
        lock(&t).output(PaneId(1), b"breaks it", |_, _| true);
        let model = Arc::clone(&lock(&t).terms.get(&id).expect("term").screen);
        lock(&model).panic_once = true;
        // The read that panics still answers, with an empty screen...
        assert!(shown(&t, id).rows.iter().all(String::is_empty));
        // ...and the model goes on from after that output.
        lock(&t).output(PaneId(1), b"next", |_, _| true);
        assert_eq!(shown(&t, id).rows[0], "next");
    }

    #[test]
    fn a_backlog_over_the_work_budget_is_read_from_its_tail() {
        let head = |cols, rows| {
            let t = Mutex::new(Terminals::new(4 << 20));
            let id = TerminalId::new();
            let mut big = claim(id, 1, 0);
            (big.cols, big.rows) = (cols, rows);
            lock(&t).claim(big);
            lock(&t).output(PaneId(1), b"\x1b[3;1HHEAD", |_, _| true);
            // Then output that changes nothing on screen (NUL is ignored).
            lock(&t).output(PaneId(1), &vec![0; 600 << 10], |_, _| true);
            shown(&t, id).rows.iter().any(|r| r.contains("HEAD"))
        };
        assert!(head(80, 24), "a small screen reads the whole backlog");
        assert!(
            !head(1000, 1000),
            "a huge one starts again from the last 256 KiB"
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
