//! What a CLI the runner starts for a session the hub named gets in its environment:
//! [`SessionEnv`], so the CLI (its hooks, `pitcrew`) reaches the hub as the session's agent.

use pitcrew_protocol::ids::SessionId;
use std::fmt;

/// The environment of a CLI the runner starts for a session the hub named (`StartSession`'s
/// `session`): how the CLI, its hooks and `pitcrew` reach the hub, and as whom.
///
/// The host implements it over the hub's sessions. The daemon gives a session that runs as an
/// agent `PITCREW_TOKEN_FILE`, a private file holding an **agent** token bound to that agent and
/// its owner (never a person's token), and where the hub listens (`PITCREW_SOCKET` or
/// `PITCREW_URL`); a session without an agent gets no token.
///
/// # Rules for implementations
///
/// - **Paths and addresses, never secrets.** A terminal runtime keeps the variables a program was
///   started with (tmux's `new-window -e`), so a token travels as a file the variable names.
/// - **Bound to the session the hub stored**, not to anything a command says: the token's caller
///   comes from the hub's record of `session`.
/// - **Never call back into the runner**: it is asked while a start runs.
pub trait SessionEnv: Send + Sync + fmt::Debug {
    /// The variables to set for `session`'s CLI, as `(name, value)`. An error refuses the start,
    /// with the reason.
    ///
    /// # Errors
    ///
    /// Why the CLI cannot be given what it needs (the session is not stored, its agent's token
    /// cannot be written); the start is refused with it.
    fn env_for(&self, session: SessionId) -> Result<Vec<(String, String)>, String>;
}
