//! Named keys for tmux and a PTY in normal cursor-key mode.

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

/// Bytes for a conventional VT-compatible PTY. Backspace is DEL.
///
/// Arrow keys use normal cursor mode (CSI), not application cursor mode (SS3).
/// A future PTY runtime must account for the application's terminal modes.
pub fn pty_key(key: Key) -> &'static [u8] {
    match key {
        Key::Enter => b"\r",
        Key::Escape => b"\x1b",
        Key::Tab => b"\t",
        Key::Up => b"\x1b[A",
        Key::Down => b"\x1b[B",
        Key::Left => b"\x1b[D",
        Key::Right => b"\x1b[C",
        Key::Backspace => b"\x7f",
        Key::CtrlC => b"\x03",
    }
}
