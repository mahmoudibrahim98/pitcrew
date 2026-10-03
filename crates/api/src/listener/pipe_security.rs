//! Windows security for the named pipe: the descriptor that makes the current user the pipe's
//! owner and its only grantee (this crate's policy), and what the client-side check reads (a
//! pipe's owner, the current user), from `pitcrew_trust::windows`, the one copy of the Win32 code
//! behind every PitCrew pipe. No `unsafe` code here.

use std::ffi::OsStr;
use std::io;
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};

#[cfg(test)]
pub(crate) use pitcrew_trust::windows::{Ace, Dacl, FILE_ALL_ACCESS, dacl, default_owner_sid};
pub(crate) use pitcrew_trust::windows::{current_user_sid, owner_sid};

/// An owned security descriptor: the current user owns the pipe and is the only one granted
/// access (`GENERIC_ALL`).
#[derive(Debug)]
pub(super) struct PipeSecurity(pitcrew_trust::windows::SecurityDescriptor);

impl PipeSecurity {
    /// The current user as owner (`O:`), and a protected DACL (`P`, so nothing is inherited)
    /// with one entry: the current user.
    ///
    /// The owner is named explicitly because clients now require exactly it
    /// (`client::check_pipe_server`), and without it an elevated daemon's pipe would default to
    /// being owned by the Administrators group instead. Any process may name its own user as
    /// owner.
    pub(super) fn current_user_only() -> io::Result<Self> {
        let sid = current_user_sid()?;
        pitcrew_trust::windows::SecurityDescriptor::from_sddl(&format!("O:{sid}D:P(A;;GA;;;{sid})"))
            .map(Self)
    }

    /// Creates a pipe instance with this descriptor.
    pub(super) fn create(
        &self,
        options: &ServerOptions,
        name: impl AsRef<OsStr>,
    ) -> io::Result<NamedPipeServer> {
        self.0.create_pipe(options, name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::windows::named_pipe::ClientOptions;

    /// A pipe created without a descriptor is owned by the token's default owner: the user
    /// itself unelevated, typically the Administrators group elevated. `check_pipe_server`
    /// requires exactly the current user, so such a pipe passes only when that default happens
    /// to be the same SID (unelevated); run elevated, it is correctly rejected, the same as any
    /// pipe that never named us as its owner.
    #[tokio::test]
    async fn a_pipe_with_default_security_passes_check_pipe_server_only_as_the_current_user() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let name = format!(r"\\.\pipe\pitcrew-owner-{}-{nanos}", std::process::id());
        let server = ServerOptions::new()
            .first_pipe_instance(true)
            .create(&name)
            .unwrap();
        let default_owner = default_owner_sid().unwrap();
        assert!(default_owner.starts_with("S-1-"), "{default_owner}");
        assert_eq!(owner_sid(&server).unwrap(), default_owner);
        let client = ClientOptions::new().open(&name).unwrap();
        assert_eq!(owner_sid(&client).unwrap(), default_owner);
        let result = crate::client::check_pipe_server(&client);
        if default_owner == current_user_sid().unwrap() {
            result.unwrap();
        } else {
            assert_eq!(result.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
        }
    }
}
