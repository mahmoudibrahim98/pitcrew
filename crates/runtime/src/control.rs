//! Incremental parsing of the LF-delimited `tmux -C` protocol.
//!
//! Reply lines, pane output, names and unknown arguments stay byte-oriented:
//! terminal output and tmux names need not be valid UTF-8. Incomplete lines and
//! replies are retained until complete. Drop the parser when its connection ends.

use std::fmt;

macro_rules! tmux_id {
    ($name:ident, $prefix:literal, $doc:literal) => {
        #[doc = $doc]
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub struct $name(pub u64);

        impl $name {
            fn parse(bytes: &[u8]) -> Option<Self> {
                Some(Self(number(bytes.strip_prefix($prefix.as_bytes())?)?))
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}{}", $prefix, self.0)
            }
        }
    };
}

tmux_id!(PaneId, "%", "A tmux pane ID, distinct from a pane index.");
tmux_id!(
    WindowId,
    "@",
    "A tmux window ID, distinct from a window index."
);
tmux_id!(
    SessionId,
    "$",
    "A tmux session ID, distinct from a session name."
);

/// One complete command response, including its `%begin` metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandReply {
    /// Opening guard timestamp, in seconds since the Unix epoch.
    pub time: u64,
    /// tmux's command sequence number.
    pub number: u64,
    /// Guard flags. All three guard fields must match before a reply closes.
    pub flags: u64,
    /// True for `%error`, false for `%end`.
    pub failed: bool,
    /// Untrusted text between the guards, with only the terminating LF removed.
    /// Pane content can forge guards. Never use `capture-pane` replies for an
    /// untrusted screen; use a terminal model driven by `%output` instead.
    pub lines: Vec<Vec<u8>>,
}

/// A complete reply or asynchronous notification from tmux.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Notification {
    CommandReply(CommandReply),
    Output {
        pane: PaneId,
        data: Vec<u8>,
    },
    ExtendedOutput {
        pane: PaneId,
        age: u64,
        /// Future fields between age and the standalone `:` delimiter.
        extra: Vec<Vec<u8>>,
        data: Vec<u8>,
    },
    WindowAdd {
        window: WindowId,
    },
    WindowClose {
        window: WindowId,
    },
    UnlinkedWindowAdd {
        window: WindowId,
    },
    UnlinkedWindowClose {
        window: WindowId,
    },
    WindowRenamed {
        window: WindowId,
        name: Vec<u8>,
    },
    SessionChanged {
        session: SessionId,
        name: Vec<u8>,
    },
    SessionsChanged,
    LayoutChange {
        window: WindowId,
        layout: Vec<u8>,
        visible_layout: Vec<u8>,
        flags: Vec<u8>,
    },
    Pause {
        pane: PaneId,
    },
    Continue {
        pane: PaneId,
    },
    PaneModeChanged {
        pane: PaneId,
    },
    Exit {
        reason: Option<Vec<u8>>,
    },
    /// Unknown or malformed records. `name` excludes the optional leading `%`;
    /// `args` excludes one separating space. Both fields preserve arbitrary bytes.
    Other {
        notification: bool,
        name: Vec<u8>,
        args: Vec<u8>,
    },
}

/// Storage limits. LF is excluded from the line limit; every reply body line
/// charges its wire bytes, one LF, and 32 bytes of allocation overhead.
/// The optional transport CR counts towards both limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct ParserLimits {
    pub max_line_bytes: usize,
    pub max_reply_bytes: usize,
}

impl ParserLimits {
    pub const fn new(max_line_bytes: usize, max_reply_bytes: usize) -> Self {
        Self {
            max_line_bytes,
            max_reply_bytes,
        }
    }
}

impl Default for ParserLimits {
    fn default() -> Self {
        Self {
            max_line_bytes: 1024 * 1024,
            max_reply_bytes: 4 * 1024 * 1024,
        }
    }
}

/// The connection cannot be parsed safely. Drop its parser and reconnect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum DesyncError {
    LineTooLong {
        limit: usize,
    },
    ReplyTooLarge {
        limit: usize,
    },
    /// Reports both conditions when EOF falls inside a reply body line.
    UnexpectedEof {
        unfinished_reply: bool,
        partial_line_bytes: usize,
    },
}

impl fmt::Display for DesyncError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LineTooLong { limit } => write!(f, "tmux line exceeds {limit} bytes; reconnect"),
            Self::ReplyTooLarge { limit } => {
                write!(f, "tmux reply exceeds {limit} bytes; reconnect")
            }
            Self::UnexpectedEof {
                unfinished_reply,
                partial_line_bytes,
            } => write!(
                f,
                "tmux EOF with unfinished reply={unfinished_reply}, partial line bytes={partial_line_bytes}; reconnect"
            ),
        }
    }
}

impl std::error::Error for DesyncError {}

/// One parser per control-mode connection. Feeding empty input is a no-op.
#[derive(Debug, Default)]
pub struct ControlParser {
    pending: Vec<u8>,
    reply: Option<CommandReply>,
    reply_bytes: usize,
    limits: ParserLimits,
    desync: Option<DesyncError>,
}

impl ControlParser {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_limits(limits: ParserLimits) -> Self {
        Self {
            limits,
            ..Self::default()
        }
    }

    /// Accept arbitrary transport chunks, including splits inside UTF-8 or octal escapes.
    /// On error, notifications from this call are discarded and the error is
    /// latched. Buffered input is released; subsequent feeds return that error.
    pub fn feed(&mut self, mut bytes: &[u8]) -> Result<Vec<Notification>, DesyncError> {
        if let Some(error) = self.desync {
            return Err(error);
        }
        let mut notifications = Vec::new();
        while let Some(end) = bytes.iter().position(|&byte| byte == b'\n') {
            self.retain(&bytes[..end])?;
            let line = std::mem::take(&mut self.pending);
            if let Some(notification) = self.line(line)? {
                notifications.push(notification);
            }
            bytes = &bytes[end + 1..];
        }
        self.retain(bytes)?;
        Ok(notifications)
    }

    /// Consume the connection parser at EOF, reporting incomplete state.
    pub fn finish(self) -> Result<(), DesyncError> {
        if let Some(error) = self.desync {
            return Err(error);
        }
        if self.reply.is_some() || !self.pending.is_empty() {
            return Err(DesyncError::UnexpectedEof {
                unfinished_reply: self.reply.is_some(),
                partial_line_bytes: self.pending.len(),
            });
        }
        Ok(())
    }

    fn fail(&mut self, error: DesyncError) -> DesyncError {
        self.pending = Vec::new();
        self.reply = None;
        self.reply_bytes = 0;
        self.desync = Some(error);
        error
    }

    fn retain(&mut self, bytes: &[u8]) -> Result<(), DesyncError> {
        if bytes.len() > self.limits.max_line_bytes - self.pending.len() {
            return Err(self.fail(DesyncError::LineTooLong {
                limit: self.limits.max_line_bytes,
            }));
        }
        self.pending.extend_from_slice(bytes);
        Ok(())
    }

    fn line(&mut self, line: Vec<u8>) -> Result<Option<Notification>, DesyncError> {
        let normalized = if line.starts_with(b"%") {
            line.strip_suffix(b"\r").unwrap_or(&line)
        } else {
            &line
        };
        let (name, args) = split(normalized);
        if let Some(reply) = &mut self.reply {
            // tmux never interleaves notifications in a reply. Even lines starting
            // with '%' are response data unless they are the matching end guard.
            if matches!(name, b"%end" | b"%error")
                && guard(args).is_some_and(|(time, number, flags)| {
                    time == reply.time && number == reply.number && flags == reply.flags
                })
            {
                reply.failed = name == b"%error";
                self.reply_bytes = 0;
                return Ok(self.reply.take().map(Notification::CommandReply));
            }
            let available = self.limits.max_reply_bytes - self.reply_bytes;
            // Include the LF and per-line allocation cost, including empty lines.
            let Some(cost) = line
                .len()
                .checked_add(1 + 32)
                .filter(|&cost| cost <= available)
            else {
                return Err(self.fail(DesyncError::ReplyTooLarge {
                    limit: self.limits.max_reply_bytes,
                }));
            };
            self.reply_bytes += cost;
            reply.lines.push(line);
            return Ok(None);
        }
        if name == b"%begin"
            && let Some((time, number, flags)) = guard(args)
        {
            self.reply = Some(CommandReply {
                time,
                number,
                flags,
                failed: false,
                lines: Vec::new(),
            });
            return Ok(None);
        }
        Ok(Some(parse_notification(name, args).unwrap_or_else(|| {
            Notification::Other {
                notification: name.starts_with(b"%"),
                name: name.strip_prefix(b"%").unwrap_or(name).to_vec(),
                args: args.to_vec(),
            }
        })))
    }
}

fn split(bytes: &[u8]) -> (&[u8], &[u8]) {
    match bytes.iter().position(|&byte| byte == b' ') {
        Some(index) => (&bytes[..index], &bytes[index + 1..]),
        None => (bytes, b""),
    }
}

fn number(bytes: &[u8]) -> Option<u64> {
    if bytes.is_empty() || !bytes.iter().all(u8::is_ascii_digit) {
        return None;
    }
    std::str::from_utf8(bytes).ok()?.parse().ok()
}

fn guard(args: &[u8]) -> Option<(u64, u64, u64)> {
    let (time, rest) = split(args);
    let (command, flags) = split(rest);
    Some((number(time)?, number(command)?, number(flags)?))
}

fn parse_notification(name: &[u8], args: &[u8]) -> Option<Notification> {
    use Notification::*;
    let (id, rest) = split(args);
    Some(match name {
        b"%output" => Output {
            pane: PaneId::parse(id)?,
            data: decode_output(rest),
        },
        b"%extended-output" => {
            let pane = PaneId::parse(id)?;
            let (age, mut rest) = split(rest);
            let age = number(age)?;
            let mut extra = Vec::new();
            let data = loop {
                let (field, tail) = split(rest);
                if field == b":" {
                    break decode_output(tail);
                }
                if field.is_empty() || tail.is_empty() {
                    return None;
                }
                extra.push(field.to_vec());
                rest = tail;
            };
            ExtendedOutput {
                pane,
                age,
                extra,
                data,
            }
        }
        b"%window-add" => WindowAdd {
            window: WindowId::parse(args)?,
        },
        b"%window-close" => WindowClose {
            window: WindowId::parse(args)?,
        },
        b"%unlinked-window-add" => UnlinkedWindowAdd {
            window: WindowId::parse(args)?,
        },
        b"%unlinked-window-close" => UnlinkedWindowClose {
            window: WindowId::parse(args)?,
        },
        b"%window-renamed" => WindowRenamed {
            window: WindowId::parse(id)?,
            name: rest.to_vec(),
        },
        b"%session-changed" => SessionChanged {
            session: SessionId::parse(id)?,
            name: rest.to_vec(),
        },
        b"%sessions-changed" if args.is_empty() => SessionsChanged,
        b"%layout-change" => {
            let window = WindowId::parse(id)?;
            let mut fields = rest.splitn(3, |&b| b == b' ');
            let layout = fields.next()?;
            let visible_layout = fields.next()?;
            let flags = fields.next()?;
            if layout.is_empty() || visible_layout.is_empty() {
                return None;
            }
            LayoutChange {
                window,
                layout: layout.to_vec(),
                visible_layout: visible_layout.to_vec(),
                flags: flags.to_vec(),
            }
        }
        b"%pause" => Pause {
            pane: PaneId::parse(args)?,
        },
        b"%continue" => Continue {
            pane: PaneId::parse(args)?,
        },
        b"%pane-mode-changed" => PaneModeChanged {
            pane: PaneId::parse(args)?,
        },
        b"%exit" => Exit {
            reason: (!args.is_empty()).then(|| args.to_vec()),
        },
        _ => return None,
    })
}

/// Decode exactly three octal digits, once. Invalid escapes are retained verbatim.
fn decode_output(bytes: &[u8]) -> Vec<u8> {
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'\\' && index + 3 < bytes.len() {
            let digits = &bytes[index + 1..index + 4];
            if digits[0] <= b'3' && digits.iter().all(|b| (b'0'..=b'7').contains(b)) {
                output.push((digits[0] - b'0') * 64 + (digits[1] - b'0') * 8 + (digits[2] - b'0'));
                index += 4;
                continue;
            }
        }
        output.push(bytes[index]);
        index += 1;
    }
    output
}
