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
    pub time: u64,
    pub number: u64,
    pub flags: u64,
    /// True for `%error`, false for `%end`.
    pub failed: bool,
    /// Lines between the guards, with only the terminating LF removed.
    pub lines: Vec<Vec<u8>>,
}

/// A complete reply or asynchronous notification from tmux.
#[derive(Debug, Clone, PartialEq, Eq)]
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
    Exit {
        reason: Option<Vec<u8>>,
    },
    /// Unknown or malformed notifications are preserved, without the leading `%`.
    /// Non-protocol lines are also kept here. `args` excludes one separating space.
    Other {
        name: String,
        args: Vec<u8>,
    },
}

/// One parser per control-mode connection. Feeding empty input is a no-op.
#[derive(Debug, Default)]
pub struct ControlParser {
    pending: Vec<u8>,
    reply: Option<CommandReply>,
}

impl ControlParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// Accept arbitrary transport chunks, including splits inside UTF-8 or octal escapes.
    pub fn feed(&mut self, mut bytes: &[u8]) -> Vec<Notification> {
        let mut notifications = Vec::new();
        while let Some(end) = bytes.iter().position(|&byte| byte == b'\n') {
            self.pending.extend_from_slice(&bytes[..end]);
            let line = std::mem::take(&mut self.pending);
            if let Some(notification) = self.line(line) {
                notifications.push(notification);
            }
            bytes = &bytes[end + 1..];
        }
        self.pending.extend_from_slice(bytes);
        notifications
    }

    fn line(&mut self, line: Vec<u8>) -> Option<Notification> {
        let (name, args) = split(&line);
        if let Some(reply) = &mut self.reply {
            // tmux never interleaves notifications in a reply. Even lines starting
            // with '%' are response data unless they are the matching end guard.
            if matches!(name, b"%end" | b"%error")
                && guard(args)
                    .is_some_and(|(time, number, _)| time == reply.time && number == reply.number)
            {
                reply.failed = name == b"%error";
                return self.reply.take().map(Notification::CommandReply);
            }
            reply.lines.push(line);
            return None;
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
            return None;
        }
        Some(
            parse_notification(name, args).unwrap_or_else(|| Notification::Other {
                name: String::from_utf8_lossy(name.strip_prefix(b"%").unwrap_or(name)).into_owned(),
                args: args.to_vec(),
            }),
        )
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
