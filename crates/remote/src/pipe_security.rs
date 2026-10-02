//! Windows: the askpass pipe's security descriptor. The current user owns the pipe and is the
//! only one granted access, so another user can neither open it nor read it. A pipe made without
//! one gets the default descriptor, which lets Everyone read it (and, elevated, makes the
//! Administrators group its owner).
//!
//! The same descriptor as the API's pipe (`crates/api/src/listener/pipe_security.rs`), written
//! again here: the two crates do not depend on each other.
//!
//! **Unsafe code.** This module and `job.rs` are the only ones of the crate that use it: the
//! workspace denies `unsafe_code`, and these two alone allow it, because the Win32 calls below
//! have no safe binding in the dependency tree, and tokio takes the descriptor only through
//! `create_with_security_attributes_raw`. Every function here is safe to call. Soundness:
//! - Every out-pointer passed to Win32 points at a live local of the right type, and every buffer
//!   is passed with its true length.
//! - The `TOKEN_USER` read comes from a buffer that `GetTokenInformation` filled with that very
//!   class and that is aligned for it (`u64` storage). The SID it points into lives inside that
//!   buffer, which outlives its use.
//! - Memory Win32 allocates with `LocalAlloc` (strings, descriptors) is freed exactly once with
//!   `LocalFree`: strings right after copying them, queried descriptors after their last use, the
//!   pipe's own descriptor on `Drop`.
//! - The token handle is closed exactly once by `OwnedHandle`. A pipe handle we are given is
//!   borrowed (`AsHandle`), so it stays open for the duration of the call.
//! - The descriptor is never changed after it is made, and Win32 only reads it, so sharing it
//!   across threads (`Send`, `Sync`) is sound.

#![allow(unsafe_code)]

use std::ffi::c_void;
use std::io;
use std::ptr;
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, LocalFree};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::{
    GetTokenInformation, PSECURITY_DESCRIPTOR, PSID, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER,
    TokenUser,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

/// An owned security descriptor: the current user owns the pipe and is the only one granted
/// access (`GENERIC_ALL`).
#[derive(Debug)]
pub(crate) struct PipeSecurity {
    descriptor: PSECURITY_DESCRIPTOR,
}

// SAFETY: see the module comment; the descriptor is immutable and only read by Win32.
unsafe impl Send for PipeSecurity {}
// SAFETY: as above.
unsafe impl Sync for PipeSecurity {}

impl PipeSecurity {
    /// The current user as owner (`O:`), and a protected DACL (`P`, so nothing is inherited)
    /// with one entry: the current user. The owner is named, so that an elevated PitCrew's pipe
    /// is the user's too; any process may name its own user as owner.
    pub(crate) fn current_user_only() -> io::Result<Self> {
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
    pub(crate) fn create(
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
    unsafe { GetTokenInformation(token.0, TokenUser, ptr::null_mut(), 0, &mut len) };
    if len == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut buffer = vec![0u64; (len as usize).div_ceil(size_of::<u64>())];
    // SAFETY: `buffer` holds at least `len` bytes and is aligned for `TOKEN_USER` (it holds
    // pointers).
    let ok = unsafe {
        GetTokenInformation(
            token.0,
            TokenUser,
            buffer.as_mut_ptr().cast::<c_void>(),
            len,
            &mut len,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: filled by the call above with the `TokenUser` class, i.e. a `TOKEN_USER`.
    let sid: PSID = unsafe { (*buffer.as_ptr().cast::<TOKEN_USER>()).User.Sid };
    if sid.is_null() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "the process token has no user SID",
        ));
    }
    let mut wide: *mut u16 = ptr::null_mut();
    // SAFETY: `sid` points into `buffer`, which is alive; `wide` is a valid out-pointer.
    if unsafe { ConvertSidToStringSidW(sid, &mut wide) } == 0 || wide.is_null() {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `wide` is a NUL-terminated string from `LocalAlloc`, used once.
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

/// A kernel object's owner and DACL (a pipe's, through any handle to it opened with
/// `READ_CONTROL`, which one opened for reading has), in SDDL. For tests.
#[cfg(test)]
pub(crate) fn owner_and_dacl(object: &impl std::os::windows::io::AsHandle) -> io::Result<String> {
    use std::os::windows::io::AsRawHandle as _;
    use windows_sys::Win32::Security::Authorization::{
        ConvertSecurityDescriptorToStringSecurityDescriptorW, GetSecurityInfo, SE_KERNEL_OBJECT,
    };
    use windows_sys::Win32::Security::{DACL_SECURITY_INFORMATION, OWNER_SECURITY_INFORMATION};

    let handle: HANDLE = object.as_handle().as_raw_handle();
    let info = OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION;
    let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
    // SAFETY: `handle` is borrowed from a live object; unused out-pointers are null, as allowed;
    // `descriptor` is a valid out-pointer and receives a `LocalAlloc` block, freed below.
    let status = unsafe {
        GetSecurityInfo(
            handle,
            SE_KERNEL_OBJECT,
            info,
            ptr::null_mut(),
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
    // SAFETY: `descriptor` came from the call above; `wide` is a valid out-pointer.
    let ok = unsafe {
        ConvertSecurityDescriptorToStringSecurityDescriptorW(
            descriptor,
            SDDL_REVISION_1,
            info,
            &mut wide,
            ptr::null_mut(),
        )
    };
    // Read the error before `LocalFree` can overwrite it.
    let failed = (ok == 0 || wide.is_null()).then(io::Error::last_os_error);
    // SAFETY: allocated by `GetSecurityInfo`, freed once, after its last use.
    unsafe {
        LocalFree(descriptor);
    }
    if let Some(e) = failed {
        return Err(e);
    }
    // SAFETY: a NUL-terminated `LocalAlloc` string from the call above, used once.
    Ok(unsafe { take_local_string(wide) })
}

/// What the askpass server's pipe must have: the current user as owner, and a protected DACL
/// with one entry, allowing the current user. For tests.
#[cfg(test)]
pub(crate) fn assert_current_user_only(sddl: &str) {
    let sid = current_user_sid().expect("the current user's SID");
    assert!(sid.starts_with("S-1-"), "{sid}");
    assert!(sddl.starts_with(&format!("O:{sid}D:P(")), "{sddl}");
    assert_eq!(sddl.matches('(').count(), 1, "{sddl}");
    assert!(sddl.contains("(A;"), "{sddl}");
    assert!(sddl.ends_with(&format!(";;;{sid})")), "{sddl}");
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
        let sddl = owner_and_dacl(&plain).unwrap();
        assert!(sddl.matches('(').count() > 1, "{sddl}");
    }
}
