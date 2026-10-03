//! Windows: the askpass pipe's security descriptor. The current user owns the pipe and is the
//! only one granted access, so another user can neither open it nor read it. A pipe made without
//! one gets the default descriptor, which lets Everyone read it (and, elevated, makes the
//! Administrators group its owner).
//!
//! The policy is this crate's: the same descriptor as the API's pipe. The Win32 code behind it
//! (the user's SID, the descriptor, reading an owner and a DACL back) is
//! `pitcrew_trust::windows`, the one copy every PitCrew pipe uses. No `unsafe` code here.

use std::ffi::OsStr;
use std::io;
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};

/// An owned security descriptor: the current user owns the pipe and is the only one granted
/// access (`GENERIC_ALL`).
#[derive(Debug)]
pub(crate) struct PipeSecurity(pitcrew_trust::windows::SecurityDescriptor);

impl PipeSecurity {
    /// The current user as owner (`O:`), and a protected DACL (`P`, so nothing is inherited)
    /// with one entry: the current user. The owner is named, so that an elevated PitCrew's pipe
    /// is the user's too; any process may name its own user as owner.
    pub(crate) fn current_user_only() -> io::Result<Self> {
        let sid = pitcrew_trust::windows::current_user_sid()?;
        pitcrew_trust::windows::SecurityDescriptor::from_sddl(&format!("O:{sid}D:P(A;;GA;;;{sid})"))
            .map(Self)
    }

    /// Creates a pipe instance with this descriptor.
    pub(crate) fn create(
        &self,
        options: &ServerOptions,
        name: impl AsRef<OsStr>,
    ) -> io::Result<NamedPipeServer> {
        self.0.create_pipe(options, name)
    }
}

/// A kernel object's owner and DACL, as read back (a pipe's, through any handle to it opened
/// with `READ_CONTROL`, which one opened for reading has). For tests.
#[cfg(test)]
#[derive(Debug)]
pub(crate) struct OwnerAndDacl {
    /// The owner's SID, in full.
    pub(crate) owner: String,
    /// The DACL, each SID in full and each access mask as stored.
    pub(crate) dacl: pitcrew_trust::windows::Dacl,
}

/// The owner and DACL of `object`. For tests.
#[cfg(test)]
pub(crate) fn owner_and_dacl(
    object: &impl std::os::windows::io::AsHandle,
) -> io::Result<OwnerAndDacl> {
    Ok(OwnerAndDacl {
        owner: pitcrew_trust::windows::owner_sid(object)?,
        dacl: pitcrew_trust::windows::dacl(object)?,
    })
}

/// What the askpass server's pipe must have ([`owner_and_dacl`]): the current user as owner,
/// and a protected DACL with one entry, allowing the current user full access (`GA` as granted
/// reads back as `FILE_ALL_ACCESS`). SIDs are compared as SIDs, not as SDDL text. For tests.
#[cfg(test)]
pub(crate) fn assert_current_user_only(read: &OwnerAndDacl) {
    use pitcrew_trust::windows::{Ace, Dacl, FILE_ALL_ACCESS};
    let me = pitcrew_trust::windows::current_user_sid().expect("the current user's SID");
    assert!(me.starts_with("S-1-"), "{me}");
    assert_eq!(read.owner, me, "the owner: {read:?}");
    assert_eq!(
        read.dacl,
        Dacl {
            protected: true,
            entries: vec![Ace::Allow {
                sid: me.clone(),
                mask: FILE_ALL_ACCESS,
            }],
        },
        "the DACL: {read:?}"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_descriptor_names_the_current_user_alone() {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let name = format!(
            r"\\.\pipe\pitcrew-askpass-unit-{}-{nanos}",
            std::process::id()
        );
        let security = PipeSecurity::current_user_only().unwrap();
        let mut options = ServerOptions::new();
        options.first_pipe_instance(true);
        let server = security.create(&options, &name).unwrap();
        assert_current_user_only(&owner_and_dacl(&server).unwrap());
        // The default descriptor has more entries (Everyone may read).
        let plain = ServerOptions::new()
            .first_pipe_instance(true)
            .create(format!("{name}-plain"))
            .unwrap();
        let read = owner_and_dacl(&plain).unwrap();
        assert!(!read.dacl.protected, "{read:?}");
        assert!(read.dacl.entries.len() > 1, "{read:?}");
    }
}
