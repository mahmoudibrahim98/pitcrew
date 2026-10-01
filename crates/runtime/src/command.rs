//! Formatting for the tmux command parser, written directly to control-mode stdin.
//!
//! This is not shell quoting or quoting for tmux's process argv. The low-level
//! builder protects argument boundaries; callers still choose trusted commands
//! and flags (commands such as `run-shell` intentionally interpret their values).
//! Use `Name` for names, `FormatLiteral` for other format-expanding options, and `Text`
//! only where tmux does not expand formats. Requires tmux 3.2 or newer.

use std::fmt::{self, Write};

use pitcrew_protocol::runner::Key;

use crate::control::{PaneId, SessionId, WindowId};
use crate::keys::tmux_key;

/// A single tmux argument, never a fragment of command syntax.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Argument<'a> {
    /// Text for an argument which does NOT expand tmux formats, such as `send-keys -l`.
    Text(&'a str),
    /// A user name (`-n`, `rename-window`): rejects C0 controls and DEL, doubles `#`.
    Name(&'a str),
    /// User text for one format-expansion pass (such as `-c`).
    /// Doubles every `#` before quoting. Not shell quoting or recursive expansion protection.
    /// `display-message` also expands strftime sequences: first escape `%` as `%%`.
    FormatLiteral(&'a str),
    /// A deliberately authored format expression. Never put untrusted text here.
    Format(&'static str),
    /// A trusted option or option terminator, such as `-t` or `--`.
    Flag(&'static str),
    /// An unambiguous pane target.
    Pane(PaneId),
    /// An unambiguous window target.
    Window(WindowId),
    /// An unambiguous session target.
    Session(SessionId),
    /// A protocol key, formatted for `send-keys` without `-l`.
    Key(Key),
    /// An unsigned numeric argument.
    Number(u64),
}

/// A value that cannot be represented safely as a tmux command argument.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormatError {
    InvalidCommandName,
    /// tmux strings are NUL-terminated; silently truncating input would be incorrect.
    Nul,
    /// Names must not contain C0 controls or DEL, which tmux can emit verbatim.
    ControlInName,
}

impl fmt::Display for FormatError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidCommandName => {
                "tmux command names must contain only lowercase ASCII letters and hyphens"
            }
            Self::Nul => "tmux text arguments cannot contain NUL",
            Self::ControlInName => "tmux names cannot contain C0 controls or DEL",
        })
    }
}

impl std::error::Error for FormatError {}

/// One command, including its terminating LF. Fields cannot bypass quoting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    line: String,
}

impl Command {
    /// Select a trusted command name, such as `list-panes` or `send-keys`.
    pub fn new(name: &str) -> Result<Self, FormatError> {
        if !name.as_bytes().first().is_some_and(u8::is_ascii_lowercase)
            || !name.bytes().all(|b| b.is_ascii_lowercase() || b == b'-')
        {
            return Err(FormatError::InvalidCommandName);
        }
        Ok(Self {
            line: name.to_owned(),
        })
    }

    /// Append one quoted argument, with format escaping when requested.
    pub fn arg(mut self, arg: Argument<'_>) -> Result<Self, FormatError> {
        let value = match arg {
            Argument::Text(text) => quote_argument(text)?,
            Argument::Name(text) => {
                if text.bytes().any(|byte| byte <= 0x1f || byte == 0x7f) {
                    return Err(FormatError::ControlInName);
                }
                quote_argument(&text.replace('#', "##"))?
            }
            Argument::FormatLiteral(text) => quote_argument(&text.replace('#', "##"))?,
            Argument::Format(format) => quote_argument(format)?,
            Argument::Flag(flag) => quote_argument(flag)?,
            Argument::Key(key) => quote_argument(tmux_key(key))?,
            Argument::Pane(id) => quote_argument(&id.to_string())?,
            Argument::Window(id) => quote_argument(&id.to_string())?,
            Argument::Session(id) => quote_argument(&id.to_string())?,
            Argument::Number(number) => quote_argument(&number.to_string())?,
        };
        self.line.push(' ');
        self.line.push_str(&value);
        Ok(self)
    }

    /// A complete, single physical line ready to write to control-mode stdin.
    pub fn to_line(&self) -> String {
        let mut line = self.line.clone();
        line.push('\n');
        line
    }

    /// Send text with key-name lookup and format expansion disabled.
    ///
    /// In copy mode, tmux still dispatches characters through the mode's key table.
    /// Query [`Self::pane_in_mode`] and cancel copy mode before writing, checking
    /// each reply. The runtime must serialize this sequence and account for other
    /// clients changing modes concurrently.
    /// Newlines are delivered as input to the pane, just like other text. What
    /// the program in that pane does with input is outside the command formatter.
    pub fn send_literal(pane: PaneId, text: &str) -> Result<Self, FormatError> {
        Self::new("send-keys")?
            .arg(Argument::Flag("-l"))?
            .arg(Argument::Flag("-t"))?
            .arg(Argument::Pane(pane))?
            .arg(Argument::Flag("--"))?
            .arg(Argument::Text(text))
    }

    /// Send named keys in order, letting tmux handle the pane's terminal modes.
    /// An empty key list produces no command.
    pub fn send_keys(pane: PaneId, keys: &[Key]) -> Result<Option<Self>, FormatError> {
        if keys.is_empty() {
            return Ok(None);
        }
        let mut command = Self::new("send-keys")?
            .arg(Argument::Flag("-t"))?
            .arg(Argument::Pane(pane))?
            .arg(Argument::Flag("--"))?;
        for &key in keys {
            command = command.arg(Argument::Key(key))?;
        }
        Ok(Some(command))
    }

    /// Send arbitrary bytes (including NUL and invalid UTF-8) with `send-keys -H`.
    /// Cancel copy mode first, as for [`Self::send_literal`]. Empty input is a no-op.
    pub fn send_bytes(pane: PaneId, bytes: &[u8]) -> Result<Option<Self>, FormatError> {
        if bytes.is_empty() {
            return Ok(None);
        }
        let mut command = Self::new("send-keys")?
            .arg(Argument::Flag("-H"))?
            .arg(Argument::Flag("-t"))?
            .arg(Argument::Pane(pane))?
            .arg(Argument::Flag("--"))?;
        for byte in bytes {
            command = command.arg(Argument::Text(&format!("{byte:02x}")))?;
        }
        Ok(Some(command))
    }

    /// Query the mode depth as a decimal integer (`0` means normal input).
    pub fn pane_in_mode(pane: PaneId) -> Result<Self, FormatError> {
        Self::new("display-message")?
            .arg(Argument::Flag("-p"))?
            .arg(Argument::Flag("-t"))?
            .arg(Argument::Pane(pane))?
            .arg(Argument::Format("#{pane_in_mode}"))
    }

    /// Leave the current copy mode. Check the reply, then query again before input.
    /// Other pane modes may not support `cancel`; do not send text on failure.
    pub fn cancel_copy_mode(pane: PaneId) -> Result<Self, FormatError> {
        Self::new("send-keys")?
            .arg(Argument::Flag("-X"))?
            .arg(Argument::Flag("-t"))?
            .arg(Argument::Pane(pane))?
            .arg(Argument::Flag("--"))?
            .arg(Argument::Text("cancel"))
    }
}

/// Quote one UTF-8 argument for the tmux control-mode command parser.
///
/// tmux expands variables in double quotes, so dollars and backslashes must be
/// escaped as well as quotes. Control bytes use fixed-width octal escapes so
/// the wire command stays one line. Format expansion is command-specific; this
/// function alone does not suppress flags such as `send-keys -F`.
pub fn quote_argument(text: &str) -> Result<String, FormatError> {
    if text.contains('\0') {
        return Err(FormatError::Nul);
    }
    let mut quoted = String::from("\"");
    for character in text.chars() {
        match character {
            '\\' | '"' | '$' | '~' => {
                quoted.push('\\');
                quoted.push(character);
            }
            '\u{1}'..='\u{1f}' | '\u{7f}' => {
                // Writing to a String cannot fail.
                let _ = write!(quoted, "\\{:03o}", character as u32);
            }
            _ => quoted.push(character),
        }
    }
    quoted.push('"');
    Ok(quoted)
}
