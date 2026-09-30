//! Line framing for JSONL transcripts: forwards from a byte offset, and backwards from the end.
//!
//! Both directions bound memory: a line longer than [`MAX_LINE_BYTES`] is never buffered, only
//! measured and reported.

use std::io::{self, Read, Seek, SeekFrom};

/// Lines longer than this are skipped and reported, never buffered.
pub const MAX_LINE_BYTES: usize = 16 * 1024 * 1024;

/// A partial last line up to this size is carried in the cursor, so the next read does not read
/// its bytes again.
pub(crate) const MAX_CARRIED_BYTES: usize = 64 * 1024;

const BLOCK: usize = 64 * 1024;

/// Why a line was skipped.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SkipReason {
    /// Longer than [`MAX_LINE_BYTES`].
    TooLong,
    /// Not valid UTF-8.
    InvalidUtf8,
    /// Not a JSON object.
    Malformed(String),
}

/// A line the parser skipped. Reads never fail because of one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkippedLine {
    /// Byte offset of the line.
    pub offset: u64,
    /// Length in bytes, without the newline.
    pub len: u64,
    /// Why it was skipped.
    pub reason: SkipReason,
}

/// One complete line.
#[derive(Debug)]
pub(crate) enum Line<'a> {
    /// The line's bytes, without the newline or a trailing `\r`.
    Data(&'a [u8]),
    /// A line longer than [`MAX_LINE_BYTES`], with its length.
    TooLong(u64),
}

/// An incomplete last line, left for the next read.
///
/// Three kinds: its bytes are carried (`bytes`); it is known to be too long (`too_long`), so the
/// rest is skipped; or it was too big to carry but may still fit, so the rest is only measured and
/// the line is read again once, if and only if it turns out short enough to parse.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Pending {
    /// Bytes of it already read.
    pub len: u64,
    /// Those bytes, when small enough to carry.
    pub bytes: Option<Vec<u8>>,
    /// Whether it is already known to be too long.
    pub too_long: bool,
}

/// The outcome of a forward read.
#[derive(Debug)]
pub(crate) struct Forward {
    /// Offset just after the last complete line.
    pub end: u64,
    /// The incomplete last line, if any.
    pub pending: Option<Pending>,
    /// Bytes read from the file by this call.
    pub bytes_read: u64,
}

/// How the current line is being collected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    /// Buffering its bytes.
    Buffer,
    /// Too long: only counting.
    Skip,
    /// Its start was not kept: counting, then reading it again if it fits.
    Measure,
}

/// Reads complete lines from `start` to the end of `file`, calling `on_line` with each line's
/// offset. With `resume` (from the previous read's [`Forward::pending`]), reading continues after
/// the bytes already seen.
pub(crate) fn read_forward<F: Read + Seek>(
    file: &mut F,
    start: u64,
    resume: Option<Pending>,
    mut on_line: impl FnMut(u64, Line<'_>),
) -> io::Result<Forward> {
    let (mut line, mut line_len, mut mode) = match resume {
        Some(Pending {
            len,
            bytes: Some(bytes),
            ..
        }) if bytes.len() as u64 == len => (bytes, len, Mode::Buffer),
        Some(Pending {
            len,
            too_long: true,
            ..
        }) => (Vec::new(), len, Mode::Skip),
        Some(Pending {
            len, bytes: None, ..
        }) => (Vec::new(), len, Mode::Measure),
        _ => (Vec::new(), 0, Mode::Buffer),
    };
    let mut phys = start + line_len;
    file.seek(SeekFrom::Start(phys))?;

    let mut line_start = start;
    let mut buf = vec![0u8; BLOCK];
    let mut bytes_read = 0u64;
    loop {
        let n = match file.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        bytes_read += n as u64;
        phys += n as u64;
        let mut chunk = &buf[..n];
        while !chunk.is_empty() {
            let newline = chunk.iter().position(|&b| b == b'\n');
            let seg = &chunk[..newline.unwrap_or(chunk.len())];
            line_len += seg.len() as u64;
            match mode {
                Mode::Buffer if line.len() + seg.len() > MAX_LINE_BYTES => {
                    mode = Mode::Skip;
                    line = Vec::new();
                }
                Mode::Buffer => line.extend_from_slice(seg),
                Mode::Measure if line_len > MAX_LINE_BYTES as u64 => mode = Mode::Skip,
                Mode::Measure | Mode::Skip => {}
            }
            let Some(i) = newline else { break };
            match mode {
                Mode::Buffer => on_line(line_start, Line::Data(strip_cr(&line))),
                Mode::Skip => on_line(line_start, Line::TooLong(line_len)),
                Mode::Measure => {
                    let mut whole = vec![0u8; usize::try_from(line_len).unwrap_or(0)];
                    file.seek(SeekFrom::Start(line_start))?;
                    file.read_exact(&mut whole)?;
                    file.seek(SeekFrom::Start(phys))?;
                    bytes_read += line_len;
                    on_line(line_start, Line::Data(strip_cr(&whole)));
                }
            }
            line_start += line_len + 1;
            line.clear();
            line_len = 0;
            mode = Mode::Buffer;
            chunk = &chunk[i + 1..];
        }
    }

    let pending = match mode {
        _ if line_len == 0 => None,
        Mode::Skip => Some(Pending {
            len: line_len,
            bytes: None,
            too_long: true,
        }),
        Mode::Buffer if line.len() <= MAX_CARRIED_BYTES => Some(Pending {
            len: line_len,
            bytes: Some(line),
            too_long: false,
        }),
        Mode::Buffer | Mode::Measure => Some(Pending {
            len: line_len,
            bytes: None,
            too_long: false,
        }),
    };
    Ok(Forward {
        end: line_start,
        pending,
        bytes_read,
    })
}
fn strip_cr(line: &[u8]) -> &[u8] {
    line.strip_suffix(b"\r").unwrap_or(line)
}

/// A complete line read backwards.
#[derive(Debug)]
pub(crate) enum OwnedLine {
    /// The line's bytes, without the newline or a trailing `\r`.
    Data(Vec<u8>),
    /// A line longer than [`MAX_LINE_BYTES`], with its length.
    TooLong(u64),
}

/// Walks a file's lines from the end towards the start, reading it in blocks.
#[derive(Debug)]
pub(crate) struct Backward<'f, F> {
    file: &'f mut F,
    len: u64,
    block: Vec<u8>,
    block_start: u64,
}

impl<'f, F: Read + Seek> Backward<'f, F> {
    /// `len` is the file's size; nothing past it is read.
    pub(crate) fn new(file: &'f mut F, len: u64) -> Self {
        Self {
            file,
            len,
            block: Vec::new(),
            block_start: 0,
        }
    }

    /// The largest line boundary (0, or just after a newline) at or before `limit`. A partial line
    /// before `limit` is left out.
    pub(crate) fn align(&mut self, limit: u64) -> io::Result<u64> {
        Ok(self
            .newline_before(limit.min(self.len))?
            .map_or(0, |p| p + 1))
    }

    /// The line that ends at boundary `end`, with its start offset; `None` at the start of the
    /// file.
    pub(crate) fn prev_line(&mut self, end: u64) -> io::Result<Option<(u64, OwnedLine)>> {
        if end == 0 {
            return Ok(None);
        }
        let content_end = end - 1;
        let start = self.newline_before(content_end)?.map_or(0, |p| p + 1);
        let len = content_end - start;
        if len > MAX_LINE_BYTES as u64 {
            return Ok(Some((start, OwnedLine::TooLong(len))));
        }
        let mut bytes = self.read_range(start, content_end)?;
        if bytes.last() == Some(&b'\r') {
            bytes.pop();
        }
        Ok(Some((start, OwnedLine::Data(bytes))))
    }

    /// Offset of the last newline in `[0, before)`.
    fn newline_before(&mut self, before: u64) -> io::Result<Option<u64>> {
        let mut hi = before;
        while hi > 0 {
            let cached = self.block_start < hi && hi <= self.block_start + self.block.len() as u64;
            if !cached {
                self.load_block(hi)?;
            }
            let rel = usize::try_from(hi - self.block_start).unwrap_or(self.block.len());
            if let Some(i) = self.block[..rel].iter().rposition(|&b| b == b'\n') {
                return Ok(Some(self.block_start + i as u64));
            }
            hi = self.block_start;
        }
        Ok(None)
    }

    fn load_block(&mut self, hi: u64) -> io::Result<()> {
        let lo = hi.saturating_sub(BLOCK as u64);
        self.block
            .resize(usize::try_from(hi - lo).unwrap_or(BLOCK), 0);
        self.file.seek(SeekFrom::Start(lo))?;
        self.file.read_exact(&mut self.block)?;
        self.block_start = lo;
        Ok(())
    }

    fn read_range(&mut self, lo: u64, hi: u64) -> io::Result<Vec<u8>> {
        let block_end = self.block_start + self.block.len() as u64;
        if self.block_start <= lo && hi <= block_end {
            let a = usize::try_from(lo - self.block_start).unwrap_or(0);
            let b = usize::try_from(hi - self.block_start).unwrap_or(0);
            return Ok(self.block[a..b].to_vec());
        }
        let mut out = vec![0u8; usize::try_from(hi - lo).unwrap_or(0)];
        self.file.seek(SeekFrom::Start(lo))?;
        self.file.read_exact(&mut out)?;
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn forward(data: &[u8]) -> (Vec<(u64, String)>, Forward) {
        let mut out = Vec::new();
        let fwd = read_forward(&mut Cursor::new(data), 0, None, |off, line| {
            let s = match line {
                Line::Data(b) => String::from_utf8_lossy(b).into_owned(),
                Line::TooLong(n) => format!("<too long {n}>"),
            };
            out.push((off, s));
        })
        .expect("read");
        (out, fwd)
    }

    #[test]
    fn forward_stops_before_a_partial_line() {
        let (lines, fwd) = forward(b"a\r\nbc\npartial");
        assert_eq!(lines, vec![(0, "a".into()), (3, "bc".into())]);
        assert_eq!(fwd.end, 6);
        assert_eq!(
            fwd.pending.and_then(|p| p.bytes).as_deref(),
            Some(&b"partial"[..])
        );
    }

    #[test]
    fn a_large_partial_line_is_read_again_only_once_complete() {
        let big = vec![b'x'; MAX_CARRIED_BYTES + 10];
        let mut data = b"a\n".to_vec();
        data.extend_from_slice(&big);
        let (_, first) = forward(&data);
        let pending = first.pending.expect("pending");
        assert_eq!(
            (pending.len, pending.bytes.is_none(), pending.too_long),
            (big.len() as u64, true, false)
        );

        data.extend_from_slice(b"yz\nb\n");
        let mut lines = Vec::new();
        let second = read_forward(
            &mut Cursor::new(&data),
            first.end,
            Some(pending),
            |off, line| {
                if let Line::Data(b) = line {
                    lines.push((off, b.len()));
                }
            },
        )
        .expect("read");
        assert_eq!(lines, vec![(2, big.len() + 2), (big.len() as u64 + 5, 1)]);
        // The rest of the file, plus the completed line once more.
        assert_eq!(second.bytes_read, 5 + big.len() as u64 + 2);
    }

    #[test]
    fn backward_walks_lines_in_reverse() {
        let data = b"one\ntwo\r\n\nthree\npart".to_vec();
        let mut cur = Cursor::new(data);
        let mut back = Backward::new(&mut cur, 20);
        let mut end = back.align(u64::MAX >> 1).expect("align");
        assert_eq!(end, 16);
        let mut seen = Vec::new();
        while let Some((start, line)) = back.prev_line(end).expect("line") {
            if let OwnedLine::Data(b) = line {
                seen.push((start, String::from_utf8(b).expect("utf8")));
            }
            end = start;
        }
        assert_eq!(
            seen,
            vec![
                (10, "three".into()),
                (9, String::new()),
                (4, "two".into()),
                (0, "one".into())
            ]
        );
    }
}
