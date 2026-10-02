//! Named keys for tmux and for a PTY.

use pitcrew_protocol::runner::Key;

/// The key name accepted by tmux `send-keys` (without `-l`).
pub fn tmux_key(key: Key) -> &'static str {
    match key {
        Key::Enter => "Enter",
        Key::Escape => "Escape",
        Key::Tab => "Tab",
        Key::Up => "Up",
        Key::Down => "Down",
        Key::Left => "Left",
        Key::Right => "Right",
        Key::Backspace => "BSpace",
        Key::CtrlC => "C-c",
    }
}

/// Bytes for a conventional VT-compatible PTY in normal cursor-key mode. Backspace is DEL.
///
/// Arrow keys use normal cursor mode (CSI). A program that set application cursor mode
/// (`DECCKM`, `ESC [ ? 1 h`) expects [`pty_key_in`] with `application_cursor` set; pitcrew-ptyd
/// follows the mode.
pub fn pty_key(key: Key) -> &'static [u8] {
    pty_key_in(key, false)
}

/// Bytes for a PTY, with arrows in application cursor mode (SS3: `ESC O A`) when the program
/// has asked for it, else in normal mode (CSI: `ESC [ A`), as a terminal sends them.
pub fn pty_key_in(key: Key, application_cursor: bool) -> &'static [u8] {
    match (key, application_cursor) {
        (Key::Enter, _) => b"\r",
        (Key::Escape, _) => b"\x1b",
        (Key::Tab, _) => b"\t",
        (Key::Up, false) => b"\x1b[A",
        (Key::Down, false) => b"\x1b[B",
        (Key::Left, false) => b"\x1b[D",
        (Key::Right, false) => b"\x1b[C",
        (Key::Up, true) => b"\x1bOA",
        (Key::Down, true) => b"\x1bOB",
        (Key::Left, true) => b"\x1bOD",
        (Key::Right, true) => b"\x1bOC",
        (Key::Backspace, _) => b"\x7f",
        (Key::CtrlC, _) => b"\x03",
    }
}
