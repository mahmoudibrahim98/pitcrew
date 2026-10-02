//! Windows security and process control for the PTY runtime and pitcrew-ptyd: who owns a pipe,
//! who is on its other end, a pipe only the current user can open, and Job Objects.
//!
//! **This is the only `unsafe` code in pitcrew-runtime and pitcrew-ptyd.** The Win32 calls below
//! have no safe binding in the dependency tree. Every function here is safe to call; each
//! `unsafe` block says why it is sound. In general:
//! - Every out-pointer passed to Win32 points at a live local of the right type, and every
//!   buffer is passed with its true length.
//! - A `TOKEN_USER` is read from a buffer `GetTokenInformation` filled with exactly that class,
//!   aligned for it (`u64` storage). The SID it points to lies inside that buffer, which outlives
//!   its use.
//! - Memory Win32 allocates with `LocalAlloc` (strings, descriptors) is freed exactly once with
//!   `LocalFree`, after its last use.
//! - Handles we open are owned by `OwnedHandle` and closed exactly once. Handles we are given
//!   are borrowed (`AsHandle`), so they stay open for the call.
//! - A descriptor is never changed after creation and Win32 only reads it, so sharing it between
//!   threads is sound.

#![allow(unsafe_code)]

use std::ffi::c_void;
use std::io;
use std::os::windows::io::{AsHandle, AsRawHandle as _, FromRawHandle as _, OwnedHandle};
use std::ptr;

use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
use windows_sys::Win32::Foundation::{HANDLE, LocalFree};
use windows_sys::Win32::Security::Authorization::{
    ConvertSecurityDescriptorToStringSecurityDescriptorW, ConvertSidToStringSidW,
    ConvertStringSecurityDescriptorToSecurityDescriptorW, GetSecurityInfo, SDDL_REVISION_1,
    SE_KERNEL_OBJECT,
};
use windows_sys::Win32::Security::{
    DACL_SECURITY_INFORMATION, GetTokenInformation, OWNER_SECURITY_INFORMATION,
    PSECURITY_DESCRIPTOR, PSID, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER, TokenUser,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
    SetInformationJobObject, TerminateJobObject,
};
use windows_sys::Win32::System::Pipes::GetNamedPipeClientProcessId;
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
    PROCESS_SET_QUOTA, PROCESS_TERMINATE,
};

/// The current user's SID, e.g. `S-1-5-21-…`.
///
/// # Errors
///
/// If our own process token cannot be read.
pub fn current_user_sid() -> io::Result<String> {
    let mut token: HANDLE = ptr::null_mut();
    // SAFETY: `GetCurrentProcess` returns a pseudo-handle that is always valid and needs no
    // closing; `token` is a valid out-pointer.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `token` was just opened, and nothing else owns it.
    let token = unsafe { OwnedHandle::from_raw_handle(token) };
    token_user_sid(&token)
}

/// The SID of the user of the process with id `pid`.
///
/// # Errors
///
/// If the process cannot be opened or its token read (it has ended, or we may not look).
pub fn process_user_sid(pid: u32) -> io::Result<String> {
    // SAFETY: a plain call; the result is checked before use.
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if process.is_null() {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `process` was just opened, and nothing else owns it.
    let process = unsafe { OwnedHandle::from_raw_handle(process) };
    let mut token: HANDLE = ptr::null_mut();
    // SAFETY: `process` is a live handle with query access; `token` is a valid out-pointer.
    if unsafe { OpenProcessToken(process.as_raw_handle(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `token` was just opened, and nothing else owns it.
    let token = unsafe { OwnedHandle::from_raw_handle(token) };
    token_user_sid(&token)
}

/// The process id of the client on the other end of a server's pipe instance.
///
/// # Errors
///
/// If the pipe is not connected.
pub fn pipe_client_pid(pipe: &impl AsHandle) -> io::Result<u32> {
    let mut pid = 0u32;
    // SAFETY: `pipe` is borrowed for the call, so its handle is open; `pid` is a valid
    // out-pointer.
    if unsafe { GetNamedPipeClientProcessId(pipe.as_handle().as_raw_handle(), &mut pid) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(pid)
}

/// The SID of the user a token belongs to.
fn token_user_sid(token: &OwnedHandle) -> io::Result<String> {
    let mut len = 0u32;
    // SAFETY: a size query: null buffer, zero length, valid length out-pointer. It fails with
    // ERROR_INSUFFICIENT_BUFFER and sets `len`.
    unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            ptr::null_mut(),
            0,
            &mut len,
        )
    };
    if len == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut buffer = vec![0u64; (len as usize).div_ceil(size_of::<u64>())];
    // SAFETY: `buffer` holds at least `len` bytes and is aligned for `TOKEN_USER` (pointers).
    let ok = unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
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
            "the token has no user SID",
        ));
    }
    sid_string(sid)
}

/// A SID as a string.
fn sid_string(sid: PSID) -> io::Result<String> {
    let mut wide: *mut u16 = ptr::null_mut();
    // SAFETY: the callers pass a SID that stays alive for this call; `wide` is a valid
    // out-pointer.
    if unsafe { ConvertSidToStringSidW(sid, &mut wide) } == 0 || wide.is_null() {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `wide` is a NUL-terminated `LocalAlloc` string, not used afterwards.
    Ok(unsafe { take_local_string(wide) })
}

/// The SID of a kernel object's owner, such as a pipe's, read through any handle to it opened
/// with `READ_CONTROL` (a pipe client opened for reading has it).
///
/// A pipe's owner is its creator's user, or whoever its descriptor names; naming another user
/// takes the restore privilege. So another user's pipe cannot claim the current user as owner.
///
/// # Errors
///
/// If the owner cannot be read.
pub fn owner_sid(object: &impl AsHandle) -> io::Result<String> {
    let mut owner: PSID = ptr::null_mut();
    let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
    // SAFETY: the handle is borrowed from a live object; `owner` and `descriptor` are valid
    // out-pointers and the unused ones are null, as allowed. `owner` points into `descriptor`,
    // freed below after the last use of `owner`.
    let status = unsafe {
        GetSecurityInfo(
            object.as_handle().as_raw_handle(),
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
    let sid = if owner.is_null() {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "the object has no owner",
        ))
    } else {
        sid_string(owner)
    };
    // SAFETY: allocated by `GetSecurityInfo`, freed once, after the last use of `owner`.
    unsafe {
        LocalFree(descriptor);
    }
    sid
}

/// The DACL of a kernel object (such as a pipe), in SDDL: for checks and tests.
///
/// # Errors
///
/// If it cannot be read.
pub fn dacl_sddl(object: &impl AsHandle) -> io::Result<String> {
    let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
    // SAFETY: the handle is borrowed from a live object; unused out-pointers are null;
    // `descriptor` is a valid out-pointer and receives a `LocalAlloc` block, freed below.
    let status = unsafe {
        GetSecurityInfo(
            object.as_handle().as_raw_handle(),
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
    // SAFETY: `descriptor` came from the call above and is still alive; `wide` is a valid
    // out-pointer.
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
    // SAFETY: a NUL-terminated `LocalAlloc` string from the call above, not used afterwards.
    Ok(unsafe { take_local_string(wide) })
}

/// A security descriptor that makes the current user a pipe's owner and its only grantee.
#[derive(Debug)]
pub struct PipeSecurity {
    descriptor: PSECURITY_DESCRIPTOR,
}

// SAFETY: see the module comment; the descriptor is never changed and only read by Win32.
unsafe impl Send for PipeSecurity {}
// SAFETY: as above.
unsafe impl Sync for PipeSecurity {}

impl PipeSecurity {
    /// The current user as owner (`O:`), and a protected DACL (`P`: nothing is inherited) with
    /// one entry: the current user, with full access. Nobody else, SYSTEM and Administrators
    /// included, is granted anything.
    ///
    /// # Errors
    ///
    /// If our token cannot be read or the descriptor cannot be built.
    pub fn current_user_only() -> io::Result<Self> {
        let sid = current_user_sid()?;
        let sddl: Vec<u16> = Self::sddl(&sid).encode_utf16().chain(Some(0)).collect();
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

    /// The descriptor, in SDDL, for a user's SID.
    pub fn sddl(sid: &str) -> String {
        format!("O:{sid}D:P(A;;GA;;;{sid})")
    }

    /// Creates a pipe instance with this descriptor.
    ///
    /// # Errors
    ///
    /// As [`ServerOptions::create`]: with `first_pipe_instance`, `PermissionDenied` when the
    /// name is taken.
    pub fn create(&self, options: &ServerOptions, name: &str) -> io::Result<NamedPipeServer> {
        let mut attributes = SECURITY_ATTRIBUTES {
            nLength: u32::try_from(size_of::<SECURITY_ATTRIBUTES>()).unwrap_or(u32::MAX),
            lpSecurityDescriptor: self.descriptor,
            bInheritHandle: 0,
        };
        // SAFETY: `attributes` is a valid `SECURITY_ATTRIBUTES` whose descriptor lives as long
        // as `self`; the call only reads it, during the call.
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
        // SAFETY: allocated by `ConvertStringSecurityDescriptorToSecurityDescriptorW`, freed
        // once.
        unsafe {
            LocalFree(self.descriptor);
        }
    }
}

/// A Job Object that holds one terminal's program and everything it starts. Terminating it
/// ends them all; so does closing its last handle (`JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`), so if
/// ptyd itself ends, its terminals' programs end with it.
#[derive(Debug)]
pub struct Job(OwnedHandle);

impl Job {
    /// A new, empty job.
    ///
    /// # Errors
    ///
    /// If Windows refuses.
    pub fn new() -> io::Result<Self> {
        // SAFETY: both arguments may be null (default security, no name). The result is checked
        // before use.
        let raw = unsafe { CreateJobObjectW(ptr::null(), ptr::null()) };
        if raw.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `raw` is a job handle we just created, and nothing else owns it.
        let job = Self(unsafe { OwnedHandle::from_raw_handle(raw) });
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let size = u32::try_from(size_of_val(&limits))
            .map_err(|_| io::Error::other("job limits do not fit a u32"))?;
        // SAFETY: the handle is a live job handle; the pointer and size describe `limits`, the
        // structure this information class expects, which outlives the call.
        let ok = unsafe {
            SetInformationJobObject(
                job.0.as_raw_handle(),
                JobObjectExtendedLimitInformation,
                ptr::from_ref(&limits).cast(),
                size,
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(job)
    }

    /// Puts the process with id `pid` in the job; the processes it starts from then on join it
    /// too. The caller must hold a handle to that process (as the owner of a child does), so
    /// its id cannot have been reused by another process.
    ///
    /// # Errors
    ///
    /// If the process cannot be opened or assigned.
    pub fn assign_pid(&self, pid: u32) -> io::Result<()> {
        // SAFETY: a plain call; the result is checked before use.
        let process = unsafe { OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, pid) };
        if process.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `process` was just opened, and nothing else owns it.
        let process = unsafe { OwnedHandle::from_raw_handle(process) };
        // SAFETY: both handles are live for the call, which only reads them.
        let ok =
            unsafe { AssignProcessToJobObject(self.0.as_raw_handle(), process.as_raw_handle()) };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// Ends every process in the job.
    pub fn terminate(&self) {
        // SAFETY: the job handle is live. Terminating an empty or finished job is harmless.
        unsafe {
            TerminateJobObject(self.0.as_raw_handle(), 1);
        }
    }
}

/// Copies a NUL-terminated wide string allocated with `LocalAlloc`, then frees it.
///
/// # Safety
///
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

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::windows::named_pipe::ClientOptions;

    fn name(test: &str) -> String {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        format!(
            r"\\.\pipe\pitcrew-ptyd-unit-{test}-{}-{nanos}",
            std::process::id()
        )
    }

    #[test]
    fn a_private_pipe_is_ours_alone_and_its_client_is_us() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        rt.block_on(async {
            let name = name("dacl");
            let security = PipeSecurity::current_user_only().expect("descriptor");
            let server = security
                .create(
                    ServerOptions::new()
                        .first_pipe_instance(true)
                        .reject_remote_clients(true),
                    &name,
                )
                .expect("pipe");
            let sid = current_user_sid().expect("sid");
            assert!(sid.starts_with("S-1-"), "{sid}");
            let dacl = dacl_sddl(&server).expect("dacl");
            assert!(dacl.starts_with("D:P"), "{dacl}");
            assert_eq!(dacl.matches("(A;").count(), 1, "{dacl}");
            assert!(dacl.contains(&sid), "{dacl}");
            assert_eq!(owner_sid(&server).expect("owner"), sid);
            // The name is taken: a second first instance is refused.
            assert!(
                security
                    .create(ServerOptions::new().first_pipe_instance(true), &name)
                    .is_err()
            );
            let client = ClientOptions::new().open(&name).expect("client");
            server.connect().await.expect("connect");
            assert_eq!(owner_sid(&client).expect("owner"), sid);
            let pid = pipe_client_pid(&server).expect("client pid");
            assert_eq!(pid, std::process::id());
            assert_eq!(process_user_sid(pid).expect("client sid"), sid);
        });
    }

    #[test]
    fn a_job_ends_its_processes() {
        let job = Job::new().expect("job");
        let mut child = std::process::Command::new("cmd.exe")
            .args(["/d", "/c", "ping -n 30 127.0.0.1 >NUL"])
            .spawn()
            .expect("child");
        job.assign_pid(child.id()).expect("assign");
        job.terminate();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            if child.try_wait().expect("wait").is_some() {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "the job did not end it"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert_eq!(
            PipeSecurity::sddl("S-1-5-21-1"),
            "O:S-1-5-21-1D:P(A;;GA;;;S-1-5-21-1)"
        );
    }
}
