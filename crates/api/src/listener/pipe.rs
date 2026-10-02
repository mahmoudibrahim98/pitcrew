//! A Windows named pipe that only the current user can open, and never from another machine.

use super::pipe_security::PipeSecurity;
use std::io;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};

/// The pipe's address: its name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PipeAddr(pub Arc<str>);

/// A named pipe listener. Each accepted connection is one pipe instance; a new instance is
/// created before the connected one is handed out.
#[derive(Debug)]
pub struct NamedPipe {
    name: Arc<str>,
    security: PipeSecurity,
    next: NamedPipeServer,
}

impl NamedPipe {
    /// Creates the first instance of `name` (e.g. `\\.\pipe\pitcrewd-<user>`).
    ///
    /// # Errors
    /// The pipe already exists (another daemon, or someone squatting the name), or it cannot be
    /// created.
    pub fn bind(name: &str) -> io::Result<Self> {
        let security = PipeSecurity::current_user_only()?;
        let next = create(&security, name, true)?;
        Ok(Self {
            name: name.into(),
            security,
            next,
        })
    }

    /// The pipe's name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }
}

fn create(security: &PipeSecurity, name: &str, first: bool) -> io::Result<NamedPipeServer> {
    let mut options = ServerOptions::new();
    options
        .first_pipe_instance(first)
        .reject_remote_clients(true)
        .access_inbound(true)
        .access_outbound(true);
    security.create(&options, name)
}

impl axum::serve::Listener for NamedPipe {
    type Io = NamedPipeServer;
    type Addr = PipeAddr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        loop {
            if let Err(e) = self.next.connect().await {
                tracing::warn!(error = %e, "a pipe client failed to connect");
                match create(&self.security, &self.name, false) {
                    Ok(fresh) => self.next = fresh,
                    Err(e) => {
                        tracing::error!(error = %e, "could not create a pipe instance");
                        tokio::time::sleep(Duration::from_secs(1)).await;
                    }
                }
                continue;
            }
            // Open the next instance before handing this one out, so new clients never find
            // the pipe missing.
            loop {
                match create(&self.security, &self.name, false) {
                    Ok(fresh) => {
                        let connected = std::mem::replace(&mut self.next, fresh);
                        return (connected, PipeAddr(self.name.clone()));
                    }
                    Err(e) => {
                        tracing::error!(error = %e, "could not create a pipe instance");
                        tokio::time::sleep(Duration::from_secs(1)).await;
                    }
                }
            }
        }
    }

    fn local_addr(&self) -> io::Result<Self::Addr> {
        Ok(PipeAddr(self.name.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::listener::pipe_security::{Ace, Dacl, current_user_sid, dacl, owner_sid};

    #[tokio::test]
    async fn the_current_user_owns_it_and_alone_has_access() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let name = format!(r"\\.\pipe\pitcrewd-unit-{}-{nanos}", std::process::id());
        let pipe = NamedPipe::bind(&name).unwrap();
        let sid = current_user_sid().unwrap();
        // Compared as SIDs: the DACL's SDDL text names some users by alias (the built-in
        // Administrator, as CI's runner is, by `LA`).
        assert_eq!(
            dacl(&pipe.next).unwrap(),
            Dacl {
                protected: true,
                entries: vec![Ace::Allow(sid.clone())],
            }
        );
        // Named explicitly, so even an elevated daemon's pipe is owned by the user.
        assert_eq!(owner_sid(&pipe.next).unwrap(), sid);
        // A client reads the same owner through its own handle.
        let client = tokio::net::windows::named_pipe::ClientOptions::new()
            .open(&name)
            .unwrap();
        assert_eq!(owner_sid(&client).unwrap(), sid);
        crate::client::check_pipe_server(&client).unwrap();
    }
}
