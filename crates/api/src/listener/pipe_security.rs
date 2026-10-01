//! Windows security for the named pipe: a descriptor that makes the current user the pipe's owner
//! and its only grantee, and the client-side check that a pipe is owned by the current user.
//!
//! This is the crate's only `unsafe` code. It calls Win32 functions and hands the descriptor to
//! tokio's `create_with_security_attributes_raw`.
//!
//! Soundness:
//! - Every out-pointer passed to Win32 points at a live local of the right type, and every buffer
//!   is passed with its true length.
//! - The `TOKEN_USER` read comes from a buffer that `GetTokenInformation` filled and that is
//!   aligned for it (`u64` storage). The SID it points into lives inside that buffer, which
//!   outlives its use.
//! - The owner SID `GetSecurityInfo` returns points into the descriptor it allocates; that
//!   descriptor is freed only after the SID's last use.
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
    /// The owner is named explicitly because clients check it ([`owner_sid`]), and without it an
    /// elevated daemon's pipe would default to being owned by the Administrators group. Any
    /// process may name its own user as owner.
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
    // SAFETY: `GetCurrentProcess` returns a pseudo-handle that needs no closing.
    process_user_sid(unsafe { GetCurrentProcess() })
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

/// The SID of the user a process runs as.
fn process_user_sid(process: HANDLE) -> io::Result<String> {
    let mut token: HANDLE = ptr::null_mut();
    // SAFETY: `process` is a live process handle; `token` is a valid out-pointer.
    if unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token) } == 0 {
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
    // SAFETY: `buffer` holds at least `len` bytes and is aligned for `TOKEN_USER`.
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
    // SAFETY: filled by the call above with a `TOKEN_USER`.
    let user = unsafe { &*buffer.as_ptr().cast::<TOKEN_USER>() };

    let mut wide: *mut u16 = ptr::null_mut();
    // SAFETY: the SID points into `buffer`, which is alive; `wide` is a valid out-pointer.
    if unsafe { ConvertSidToStringSidW(user.User.Sid, &mut wide) } == 0 || wide.is_null() {
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

/// The DACL of a kernel object (such as a pipe), in SDDL. For tests.
#[cfg(test)]
pub(crate) fn dacl_sddl(object: &impl AsHandle) -> io::Result<String> {
    use windows_sys::Win32::Security::Authorization::ConvertSecurityDescriptorToStringSecurityDescriptorW;
    use windows_sys::Win32::Security::DACL_SECURITY_INFORMATION;

    let handle: HANDLE = object.as_handle().as_raw_handle();
    let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
    // SAFETY: `handle` is borrowed from a live object; unused out-pointers are null; `descriptor`
    // is a valid out-pointer and receives a `LocalAlloc` block, freed below.
    let status = unsafe {
        GetSecurityInfo(
            handle,
            SE_KERNEL_OBJECT,
            DACL_SECURITY_INFORMATION,
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
            DACL_SECURITY_INFORMATION,
            &mut wide,
            ptr::null_mut(),
        )
    };
    // Read the error before `LocalFree` can overwrite it.
    let failed = (ok == 0 || wide.is_null()).then(io::Error::last_os_error);
    // SAFETY: allocated by `GetSecurityInfo`, freed once.
    unsafe {
        LocalFree(descriptor);
    }
    if let Some(e) = failed {
        return Err(e);
    }
    // SAFETY: a NUL-terminated `LocalAlloc` string from the call above.
    Ok(unsafe { take_local_string(wide) })
}
