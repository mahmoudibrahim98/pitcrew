//! Which runtime this machine uses: tmux when it is present and usable, else the PTY runtime.

use pitcrew_interfaces::runtime::{Runtime, RuntimeError, RuntimeKind};
use pitcrew_protocol::runner::Capability;

use crate::background::Background;
use crate::pty::{self, PtyOptions, PtyRuntime, PtySupport};
use crate::tmux::{self, TmuxOptions, TmuxSupport};

/// The runtime chosen for this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Chosen {
    /// tmux 3.2 or newer can run PitCrew's private server.
    Tmux(TmuxSupport),
    /// No usable tmux; pitcrew-ptyd is installed.
    Pty {
        /// pitcrew-ptyd and its endpoint.
        support: PtySupport,
        /// Why tmux is not used, for people.
        no_tmux: String,
    },
}

impl Chosen {
    /// The capability to report: [`Capability::Tmux`] or [`Capability::Pty`].
    pub fn capability(&self) -> Capability {
        match self {
            Self::Tmux(support) => support.capability(),
            Self::Pty { support, .. } => support.capability(),
        }
    }

    /// Which kind of runtime it is.
    pub fn kind(&self) -> RuntimeKind {
        match self {
            Self::Tmux(_) => RuntimeKind::Tmux,
            Self::Pty { .. } => RuntimeKind::Pty,
        }
    }

    /// The runtime itself, from the options detection was given (with tmux's absolute path
    /// filled in).
    ///
    /// # Errors
    ///
    /// As [`crate::TmuxRuntime::new`] or [`PtyRuntime::new`].
    pub fn into_runtime(
        self,
        tmux: TmuxOptions,
        pty: PtyOptions,
    ) -> Result<Box<dyn Runtime>, RuntimeError> {
        match self {
            #[cfg(unix)]
            Self::Tmux(support) => {
                let mut tmux = tmux;
                tmux.tmux = support.tmux;
                Ok(Box::new(crate::TmuxRuntime::new(tmux)?))
            }
            #[cfg(not(unix))]
            Self::Tmux(_) => {
                let _ = tmux;
                Err(RuntimeError::Unavailable(
                    "tmux runs on Unix-like systems only".into(),
                ))
            }
            Self::Pty { .. } => Ok(Box::new(PtyRuntime::new(pty)?)),
        }
    }
}

/// Detects tmux ([`tmux::detect`]), and when it is not usable, the PTY runtime
/// ([`pty::detect`]). Blocks for up to about twice the tmux call timeout: from async code use
/// [`choose_async`].
///
/// # Errors
///
/// `Unavailable` with both reasons when neither is usable.
pub fn choose(tmux: &TmuxOptions, pty: &PtyOptions) -> Result<Chosen, RuntimeError> {
    let no_tmux = match tmux::detect(tmux) {
        Ok(support) => return Ok(Chosen::Tmux(support)),
        Err(e) => e.to_string(),
    };
    match pty::detect(pty) {
        Ok(support) => Ok(Chosen::Pty { support, no_tmux }),
        Err(no_pty) => Err(RuntimeError::Unavailable(format!(
            "no terminal runtime: {no_tmux}; {no_pty}"
        ))),
    }
}

/// [`choose`] on its own thread, as a future that any executor can await without blocking.
pub fn choose_async(
    tmux: TmuxOptions,
    pty: PtyOptions,
) -> Background<Result<Chosen, RuntimeError>> {
    crate::background::spawn(
        "pitcrew-runtime-detect",
        move || choose(&tmux, &pty),
        |why| {
            Err(RuntimeError::Unavailable(format!(
                "cannot start the runtime check: {why}"
            )))
        },
    )
}
