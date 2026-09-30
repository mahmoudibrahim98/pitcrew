//! Formatting for the tmux command parser, written directly to control-mode stdin.
//!
//! This is not shell quoting or quoting for tmux's process argv. The low-level
//! builder protects argument boundaries; callers still choose trusted commands
//! and flags (commands such as `run-shell` intentionally interpret their values).

use std::fmt::{self, Write};

use pitcrew_protocol::runner::Key;

use crate::control::{PaneId, SessionId, WindowId};
use crate::keys::tmux_key;

/// A single tmux argument, never a fragment of command syntax.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Argument<'a> {
    Text(&'a str),
    Pane(PaneId),
    Window(WindowId),
    Session(SessionId),
    Key(Key),
    Number(u64),
}

/// A value that cannot be represented safely as a tmux command argument.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormatError {
    InvalidCommandName,
    /// tmux strings are NUL-terminated; silently truncating input would be incorrect.
    Nul,
}

impl fmt::Display for FormatError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidCommandName => {
                "tmux command names must contain only lowercase ASCII letters and hyphens"
            }
            Self::Nul => "tmux text arguments cannot contain NUL",
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

    /// Append one quoted argument. Text includes trusted flags such as `-t`.
    pub fn arg(mut self, arg: Argument<'_>) -> Result<Self, FormatError> {
        let value = match arg {
            Argument::Text(text) => quote_argument(text)?,
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

    /// Send text literally to a pane: no key lookup, flag injection, or format expansion.
    ///
    /// Newlines are delivered as input to the pane, just like other text. What
    /// the program in that pane does with input is outside the command formatter.
    pub fn send_literal(pane: PaneId, text: &str) -> Result<Self, FormatError> {
        Self::new("send-keys")?
            .arg(Argument::Text("-l"))?
            .arg(Argument::Text("-t"))?
            .arg(Argument::Pane(pane))?
            .arg(Argument::Text("--"))?
            .arg(Argument::Text(text))
    }

    /// Send named keys in order, letting tmux handle the pane's terminal modes.
    pub fn send_keys(pane: PaneId, keys: &[Key]) -> Result<Self, FormatError> {
        let mut command = Self::new("send-keys")?
            .arg(Argument::Text("-t"))?
            .arg(Argument::Pane(pane))?
            .arg(Argument::Text("--"))?;
        for &key in keys {
            command = command.arg(Argument::Key(key))?;
        }
        Ok(command)
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
                write!(quoted, "\\{:03o}", character as u32).expect("write to String");
            }
            _ => quoted.push(character),
        }
    }
    quoted.push('"');
    Ok(quoted)
}
