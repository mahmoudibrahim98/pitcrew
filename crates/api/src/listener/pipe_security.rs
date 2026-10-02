//! Windows security for the named pipe: a descriptor that makes the current user the pipe's owner
//! and its only grantee, and what the client-side check reads: a pipe's owner, the current user,
//! and the owner our own token gives the objects it creates.
//!
//! This is the crate's only `unsafe` code. It calls Win32 functions and hands the descriptor to
//! tokio's `create_with_security_attributes_raw`.
//!
//! Soundness:
//! - Every out-pointer passed to Win32 points at a live local of the right type, and every buffer
//!   is passed with its true length.
//! - The `TOKEN_USER` or `TOKEN_OWNER` read comes from a buffer that `GetTokenInformation` filled
//!   with that very class and that is aligned for it (`u64` storage). The SID it points into lives
//!   inside that buffer, which outlives its use.
//! - The owner SID or DACL `GetSecurityInfo` returns points into the descriptor it allocates;
//!   that descriptor is freed only after their last use. A DACL entry is read as the structure
//!   its header's type names.
//! - Memory Win32 allocates with `LocalAlloc` (strings, descriptors) is freed exactly once with
//!   `LocalFree`: strings right after copying them, queried descriptors after their last use, the
//!   pipe's own descriptor on `Drop`.
//! - The token handle we open is closed exactly once by `OwnedHandle`. A pipe handle we are given
//!   is borrowed (`AsHandle`), so it stays open for the duration of the call.
//! - The descriptor is never mutated after creation, and Win32 only reads it, so sharing it
//!   across threads (`Send`, `Sync`) is sound.
//!
//! `unsafe_code` is allowed for this module only, where it is declared.

use std::ffi::c_void;
use std::io;
use std::os::windows::io::{AsHandle, AsRawHandle as _};
use std::ptr;
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, LocalFree};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, GetSecurityInfo,
    SDDL_REVISION_1, SE_KERNEL_OBJECT,
};
use windows_sys::Win32::Security::{
    GetTokenInformation, OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID,
    SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER, TokenUser,
};
// `TokenOwner`/`TOKEN_OWNER` back `default_owner_sid`, read only by tests: production code
// requires exactly the current user (see `default_owner_sid`'s doc comment).
#[cfg(test)]
use windows_sys::Win32::Security::{TOKEN_OWNER, TokenOwner};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

/// An owned security descriptor: the current user owns the pipe and is the only one granted
/// access (`GENERIC_ALL`).
#[derive(Debug)]
pub(super) struct PipeSecurity {
    descriptor: PSECURITY_DESCRIPTOR,
}

// SAFETY: see the module comment; the descriptor is immutable and only read by Win32.
unsafe impl Send for PipeSecurity {}
// SAFETY: as above.
unsafe impl Sync for PipeSecurity {}

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
        let sddl: Vec<u16> = format!("O:{sid}D:P(A;;GA;;;{sid})")
            .encode_utf16()
            .chain(Some(0))
            .collect();
        let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
        // SAFETY: `sddl` is NUL-terminated; `descriptor` is a valid out-pointer; the size
        // out-pointer may be null.
        let ok = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                ptr::null_mut(),
            )
        };
        if ok == 0 || descriptor.is_null() {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { descriptor })
    }

    /// Creates a pipe instance with this descriptor.
    pub(super) fn create(
        &self,
        options: &ServerOptions,
        name: &str,
    ) -> io::Result<NamedPipeServer> {
        let mut attributes = SECURITY_ATTRIBUTES {
            nLength: u32::try_from(size_of::<SECURITY_ATTRIBUTES>()).unwrap_or(u32::MAX),
            lpSecurityDescriptor: self.descriptor,
            bInheritHandle: 0,
        };
        // SAFETY: `attributes` is a valid `SECURITY_ATTRIBUTES` whose descriptor lives as long as
        // `self`; the call only reads it.
        unsafe {
            options.create_with_security_attributes_raw(
                name,
                ptr::from_mut(&mut attributes).cast::<c_void>(),
            )
        }
    }
}

impl Drop for PipeSecurity {
    fn drop(&mut self) {
        // SAFETY: allocated by `ConvertStringSecurityDescriptorToSecurityDescriptorW`, freed once.
        unsafe {
            LocalFree(self.descriptor);
        }
    }
}

/// Closes a handle when dropped.
struct OwnedHandle(HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        // SAFETY: the handle came from `OpenProcessToken` and is closed once.
        unsafe {
            CloseHandle(self.0);
        }
    }
}

/// The current user's SID as a string, e.g. `S-1-5-21-…`.
pub(crate) fn current_user_sid() -> io::Result<String> {
    own_token_sid(TokenSid::User)
}

/// The owner our process token gives the objects it creates without naming one (`TokenOwner`),
/// as a string. Unelevated, that is the user itself; elevated, typically the Administrators group
/// (`S-1-5-32-544`), unless policy makes it the user.
///
/// Test-only: a real pipe always names the current user explicitly
/// (`PipeSecurity::current_user_only`), and `client::check_pipe_server` now requires exactly
/// that, so nothing outside tests reads the default owner. Kept to exercise that a pipe with only
/// this (no explicit owner) passes `check_pipe_server` just when it happens to equal the current
/// user, and is rejected otherwise.
#[cfg(test)]
pub(crate) fn default_owner_sid() -> io::Result<String> {
    own_token_sid(TokenSid::DefaultOwner)
}

/// The SID of a kernel object's owner, such as a pipe's (read through any handle to it opened
/// with `READ_CONTROL`, which a handle opened for reading has).
///
/// A pipe's owner is its creator's user, or whoever its descriptor names; naming another user
/// takes the restore privilege. So another user's pipe cannot claim the current user as owner,
/// and unlike the server's process id, the owner cannot be recycled.
pub(crate) fn owner_sid(object: &impl AsHandle) -> io::Result<String> {
    let handle: HANDLE = object.as_handle().as_raw_handle();
    let mut owner: PSID = ptr::null_mut();
    let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
    // SAFETY: `handle` is borrowed from a live object; `owner` and `descriptor` are valid
    // out-pointers and the unused ones are null, as allowed. `owner` points into `descriptor`, a
    // `LocalAlloc` block freed below after the last use of `owner`.
    let status = unsafe {
        GetSecurityInfo(
            handle,
            SE_KERNEL_OBJECT,
            OWNER_SECURITY_INFORMATION,
            &mut owner,
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            &mut descriptor,
        )
    };
    if status != 0 {
        return Err(io::Error::from_raw_os_error(
            i32::try_from(status).unwrap_or(-1),
        ));
    }
    let mut wide: *mut u16 = ptr::null_mut();
    let failed = if owner.is_null() {
        Some(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "the object has no owner",
        ))
    } else {
        // SAFETY: `owner` points into `descriptor`, which is still alive; `wide` is a valid
        // out-pointer.
        let ok = unsafe { ConvertSidToStringSidW(owner, &mut wide) };
        // Read the error before `LocalFree` can overwrite it.
        (ok == 0 || wide.is_null()).then(io::Error::last_os_error)
    };
    // SAFETY: allocated by `GetSecurityInfo`, freed once, after the last use of `owner`.
    unsafe {
        LocalFree(descriptor);
    }
    if let Some(e) = failed {
        return Err(e);
    }
    // SAFETY: a NUL-terminated `LocalAlloc` string from `ConvertSidToStringSidW`, used once.
    Ok(unsafe { take_local_string(wide) })
}

/// A SID our own process token holds.
#[derive(Clone, Copy, Debug)]
enum TokenSid {
    /// The user the process runs as (`TokenUser`).
    User,
    /// The default owner of the objects it creates (`TokenOwner`). Test-only: see
    /// `default_owner_sid`'s doc comment.
    #[cfg(test)]
    DefaultOwner,
}

/// Reads one SID from our own process token, as a string.
fn own_token_sid(which: TokenSid) -> io::Result<String> {
    let class = match which {
        TokenSid::User => TokenUser,
        #[cfg(test)]
        TokenSid::DefaultOwner => TokenOwner,
    };
    let mut token: HANDLE = ptr::null_mut();
    // SAFETY: `GetCurrentProcess` returns a pseudo-handle that is always valid and needs no
    // closing; `token` is a valid out-pointer.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let token = OwnedHandle(token);

    let mut len = 0u32;
    // SAFETY: a size query: null buffer, zero length, valid length out-pointer. It fails with
    // ERROR_INSUFFICIENT_BUFFER and sets `len`.
    unsafe { GetTokenInformation(token.0, class, ptr::null_mut(), 0, &mut len) };
    if len == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut buffer = vec![0u64; (len as usize).div_ceil(size_of::<u64>())];
    // SAFETY: `buffer` holds at least `len` bytes and is aligned for `TOKEN_USER` and
    // `TOKEN_OWNER` (both hold pointers).
    let ok = unsafe {
        GetTokenInformation(
            token.0,
            class,
            buffer.as_mut_ptr().cast::<c_void>(),
            len,
            &mut len,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    let sid: PSID = match which {
        // SAFETY: filled by the call above with a `TokenUser` class, i.e. a `TOKEN_USER`.
        TokenSid::User => unsafe { (*buffer.as_ptr().cast::<TOKEN_USER>()).User.Sid },
        // SAFETY: filled by the call above with a `TokenOwner` class, i.e. a `TOKEN_OWNER`.
        #[cfg(test)]
        TokenSid::DefaultOwner => unsafe { (*buffer.as_ptr().cast::<TOKEN_OWNER>()).Owner },
    };
    if sid.is_null() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("the process token has no {which:?} SID"),
        ));
    }

    let mut wide: *mut u16 = ptr::null_mut();
    // SAFETY: `sid` points into `buffer`, which is alive; `wide` is a valid out-pointer.
    if unsafe { ConvertSidToStringSidW(sid, &mut wide) } == 0 || wide.is_null() {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `wide` is a NUL-terminated string from `LocalAlloc`, freed once here.
    Ok(unsafe { take_local_string(wide) })
}

/// Copies a NUL-terminated wide string allocated with `LocalAlloc`, then frees it.
///
/// # Safety
/// `wide` must be a valid, NUL-terminated, `LocalAlloc`-allocated string not used afterwards.
unsafe fn take_local_string(wide: *mut u16) -> String {
    // SAFETY: guaranteed by the caller.
    unsafe {
        let mut n = 0;
        while *wide.add(n) != 0 {
            n += 1;
        }
        let text = String::from_utf16_lossy(std::slice::from_raw_parts(wide, n));
        LocalFree(wide.cast::<c_void>());
        text
    }
}

/// A kernel object's DACL, read part by part rather than as SDDL text: SDDL writes some SIDs as
/// aliases (the built-in Administrator as `LA`, say), so comparing its text with a SID string is
/// wrong. Each SID here is in its full form (`S-1-5-21-…`), which names one SID exactly. For
/// tests.
#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Dacl {
    /// Protected: it inherits no entries from a parent (`SE_DACL_PROTECTED`, `P` in SDDL).
    pub(crate) protected: bool,
    /// Its entries, in order.
    pub(crate) entries: Vec<Ace>,
}

/// One entry of a [`Dacl`].
#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Ace {
    /// Allows access to this SID (`ACCESS_ALLOWED_ACE_TYPE`).
    Allow(String),
    /// Denies access to this SID (`ACCESS_DENIED_ACE_TYPE`).
    Deny(String),
    /// Any other kind of entry, by its type number.
    Other(u8),
}

/// The DACL of a kernel object (such as a pipe). For tests.
#[cfg(test)]
pub(crate) fn dacl(object: &impl AsHandle) -> io::Result<Dacl> {
    use windows_sys::Win32::Security::{ACL, DACL_SECURITY_INFORMATION};

    let handle: HANDLE = object.as_handle().as_raw_handle();
    let mut acl: *mut ACL = ptr::null_mut();
    let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
    // SAFETY: `handle` is borrowed from a live object; `acl` and `descriptor` are valid
    // out-pointers and the unused ones are null. `acl` points into `descriptor`, a `LocalAlloc`
    // block freed below after the last use of `acl`.
    let status = unsafe {
        GetSecurityInfo(
            handle,
            SE_KERNEL_OBJECT,
            DACL_SECURITY_INFORMATION,
            ptr::null_mut(),
            ptr::null_mut(),
            &mut acl,
            ptr::null_mut(),
            &mut descriptor,
        )
    };
    if status != 0 {
        return Err(io::Error::from_raw_os_error(
            i32::try_from(status).unwrap_or(-1),
        ));
    }
    // SAFETY: `descriptor` came from the call above and is still alive; `acl` is its DACL or
    // null.
    let read = unsafe { read_dacl(descriptor, acl) };
    // SAFETY: allocated by `GetSecurityInfo`, freed once, after the last use of `acl`.
    unsafe {
        LocalFree(descriptor);
    }
    read
}

/// Reads a descriptor's DACL into a [`Dacl`].
///
/// # Safety
/// `descriptor` must be a live security descriptor, and `acl` null or that descriptor's DACL.
#[cfg(test)]
unsafe fn read_dacl(
    descriptor: PSECURITY_DESCRIPTOR,
    acl: *const windows_sys::Win32::Security::ACL,
) -> io::Result<Dacl> {
    use windows_sys::Win32::Security::{
        ACCESS_ALLOWED_ACE, ACE_HEADER, GetAce, GetSecurityDescriptorControl, SE_DACL_PROTECTED,
        SECURITY_DESCRIPTOR_CONTROL,
    };
    // From `Win32_System_SystemServices`, a feature needed for nothing else.
    const ACCESS_ALLOWED_ACE_TYPE: u8 = 0;
    const ACCESS_DENIED_ACE_TYPE: u8 = 1;

    let mut control: SECURITY_DESCRIPTOR_CONTROL = 0;
    let mut revision = 0u32;
    // SAFETY: `descriptor` is live (guaranteed by the caller); both out-pointers are valid.
    if unsafe { GetSecurityDescriptorControl(descriptor, &mut control, &mut revision) } == 0 {
        return Err(io::Error::last_os_error());
    }
    if acl.is_null() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "the object has no DACL, so anyone may open it",
        ));
    }
    // SAFETY: `acl` is the descriptor's live DACL (guaranteed by the caller).
    let count = unsafe { (*acl).AceCount };
    let mut entries = Vec::with_capacity(usize::from(count));
    for index in 0..u32::from(count) {
        let mut ace: *mut c_void = ptr::null_mut();
        // SAFETY: `acl` is live and `index` is below its entry count; `ace` is a valid
        // out-pointer, and receives a pointer into the DACL.
        if unsafe { GetAce(acl, index, &mut ace) } == 0 || ace.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: every entry starts with an `ACE_HEADER`, and entries are 4-byte aligned.
        let kind = unsafe { (*ace.cast::<ACE_HEADER>()).AceType };
        entries.push(match kind {
            ACCESS_ALLOWED_ACE_TYPE | ACCESS_DENIED_ACE_TYPE => {
                // SAFETY: entries of both types are laid out as `ACCESS_ALLOWED_ACE` (an
                // `ACCESS_DENIED_ACE` is the same), and the SID begins at `SidStart`, inside the
                // entry, which lives as long as the DACL.
                let sid = unsafe { &raw mut (*ace.cast::<ACCESS_ALLOWED_ACE>()).SidStart };
                let mut wide: *mut u16 = ptr::null_mut();
                // SAFETY: `sid` points into the live DACL; `wide` is a valid out-pointer.
                if unsafe { ConvertSidToStringSidW(sid.cast::<c_void>(), &mut wide) } == 0
                    || wide.is_null()
                {
                    return Err(io::Error::last_os_error());
                }
                // SAFETY: a NUL-terminated `LocalAlloc` string from the call above, used once.
                let sid = unsafe { take_local_string(wide) };
                if kind == ACCESS_ALLOWED_ACE_TYPE {
                    Ace::Allow(sid)
                } else {
                    Ace::Deny(sid)
                }
            }
            other => Ace::Other(other),
        });
    }
    Ok(Dacl {
        protected: (control & SE_DACL_PROTECTED) != 0,
        entries,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::windows::named_pipe::ClientOptions;

    /// A pipe created without a descriptor is owned by the token's default owner, which is what
    /// `default_owner_sid` reads: the user itself unelevated, typically the Administrators group
    /// elevated. `check_pipe_server` requires exactly the current user, so such a pipe passes
    /// only when that default happens to be the same SID (unelevated); run elevated, it is
    /// correctly rejected, the same as any pipe that never named us as its owner.
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
