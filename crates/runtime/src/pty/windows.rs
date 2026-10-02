//! Windows security and process control for the PTY runtime and pitcrew-ptyd: who owns a pipe,
//! who is on its other end (user and integrity level), a pipe only the current user at the
//! current integrity level can open, and Job Objects.
//!
//! **This is the only `unsafe` code in pitcrew-runtime and pitcrew-ptyd.** The Win32 calls below
//! have no safe binding in the dependency tree. Every function here is safe to call; each
//! `unsafe` block says why it is sound. In general:
//! - Every out-pointer passed to Win32 points at a live local of the right type, and every
//!   buffer is passed with its true length.
//! - A `TOKEN_USER` or `TOKEN_MANDATORY_LABEL` is read from a buffer `GetTokenInformation`
//!   filled with exactly that class, aligned for it (`u64` storage). The SID it points to lies
//!   inside that buffer, which outlives its use.
//! - Memory Win32 allocates with `LocalAlloc` (strings, descriptors) is freed exactly once with
//!   `LocalFree`, after its last use. Pointers into a descriptor (its owner, its DACL and the
//!   DACL's entries) are used only while it lives.
//! - A DACL entry is read as the structure its header's type names.
//! - Handles we open are owned by `OwnedHandle` and closed exactly once. Handles we are given
//!   are borrowed (`AsHandle`), so they stay open for the call.
//! - A descriptor is never changed after creation and Win32 only reads it, so sharing it between
//!   threads is sound.
//! - Impersonating a pipe's client is undone on the same thread before the function returns;
//!   should that fail, the process aborts rather than go on as the client.

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
    ACCESS_ALLOWED_ACE, ACE_HEADER, ACL, DACL_SECURITY_INFORMATION, GetAce,
    GetSecurityDescriptorControl, GetTokenInformation, LABEL_SECURITY_INFORMATION,
    OBJECT_SECURITY_INFORMATION, OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID,
    RevertToSelf, SE_DACL_PROTECTED, SECURITY_ATTRIBUTES, SECURITY_DESCRIPTOR_CONTROL,
    TOKEN_INFORMATION_CLASS, TOKEN_MANDATORY_LABEL, TOKEN_QUERY, TOKEN_USER, TokenIntegrityLevel,
    TokenUser,
};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, IsProcessInJob, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
    SetInformationJobObject, TerminateJobObject,
};
use windows_sys::Win32::System::Pipes::{GetNamedPipeClientProcessId, ImpersonateNamedPipeClient};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, GetCurrentThread, OpenProcess, OpenProcessToken, OpenThreadToken,
    PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SET_QUOTA, PROCESS_TERMINATE,
};

/// The integrity level of a normal, non-elevated process.
pub const MEDIUM_INTEGRITY: u32 = 0x2000;
/// The integrity level of an elevated process.
pub const HIGH_INTEGRITY: u32 = 0x3000;
/// The integrity level of a sandboxed (low) process.
pub const LOW_INTEGRITY: u32 = 0x1000;

/// Who a process or a pipe's client is: its user's SID, and its integrity level (the RID of its
/// mandatory label: [`MEDIUM_INTEGRITY`] for a normal process, [`HIGH_INTEGRITY`] elevated).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    /// The user's SID, e.g. `S-1-5-21-…`.
    pub user: String,
    /// The integrity level.
    pub integrity: u32,
}

/// The current user's SID, e.g. `S-1-5-21-…`.
///
/// # Errors
///
/// If our own process token cannot be read.
pub fn current_user_sid() -> io::Result<String> {
    current_identity().map(|identity| identity.user)
}

/// This process's user and integrity level.
///
/// # Errors
///
/// If our own process token cannot be read.
pub fn current_identity() -> io::Result<Identity> {
    let mut token: HANDLE = ptr::null_mut();
    // SAFETY: `GetCurrentProcess` returns a pseudo-handle that is always valid and needs no
    // closing; `token` is a valid out-pointer.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `token` was just opened, and nothing else owns it.
    let token = unsafe { OwnedHandle::from_raw_handle(token) };
    token_identity(&token)
}

/// True if this process runs elevated (at high integrity or above).
pub fn is_elevated() -> bool {
    current_identity().is_ok_and(|identity| identity.integrity >= HIGH_INTEGRITY)
}

/// The user and integrity level of the client on the other end of a server's pipe instance,
/// from the client's own token: the server impersonates it (at the identification level the
/// client allows) just long enough to read that token. The client must have written something
/// the server has read (Windows takes the identity of the last message read).
///
/// # Errors
///
/// If the client cannot be impersonated or its token read.
pub fn pipe_client_identity(pipe: &impl AsHandle) -> io::Result<Identity> {
    // SAFETY: `pipe` is borrowed for the call, so its handle is open. This makes the calling
    // thread act as the client until `RevertToSelf` below, which runs before returning.
    if unsafe { ImpersonateNamedPipeClient(pipe.as_handle().as_raw_handle()) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut token: HANDLE = ptr::null_mut();
    // SAFETY: `GetCurrentThread` is a pseudo-handle that is always valid; `token` is a valid
    // out-pointer. `OpenAsSelf` (1) checks access against our own token, as the client allows
    // only identification.
    let opened = unsafe { OpenThreadToken(GetCurrentThread(), TOKEN_QUERY, 1, &mut token) };
    let failed = (opened == 0).then(io::Error::last_os_error);
    // SAFETY: no arguments; it ends the impersonation started above, on this thread.
    if unsafe { RevertToSelf() } == 0 {
        // Going on while acting as the client would be wrong for everything that follows.
        std::process::abort();
    }
    if let Some(e) = failed {
        return Err(e);
    }
    // SAFETY: `token` was just opened, and nothing else owns it.
    let token = unsafe { OwnedHandle::from_raw_handle(token) };
    token_identity(&token)
}

fn token_identity(token: &OwnedHandle) -> io::Result<Identity> {
    let user = token_sid(token, TokenUser)?;
    let label = token_sid(token, TokenIntegrityLevel)?;
    let integrity = integrity_rid(&label).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{label} is not an integrity level"),
        )
    })?;
    Ok(Identity { user, integrity })
}

/// The integrity level an integrity SID (`S-1-16-<level>`) names.
pub fn integrity_rid(sid: &str) -> Option<u32> {
    sid.strip_prefix("S-1-16-")?.parse().ok()
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
    token_sid(&token, TokenUser)
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

/// A SID a token holds, as a string: its user's (`TokenUser`) or its integrity label's
/// (`TokenIntegrityLevel`). Any other class is refused.
fn token_sid(token: &OwnedHandle, class: TOKEN_INFORMATION_CLASS) -> io::Result<String> {
    if class != TokenUser && class != TokenIntegrityLevel {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a SID class",
        ));
    }
    let mut len = 0u32;
    // SAFETY: a size query: null buffer, zero length, valid length out-pointer. It fails with
    // ERROR_INSUFFICIENT_BUFFER and sets `len`.
    unsafe { GetTokenInformation(token.as_raw_handle(), class, ptr::null_mut(), 0, &mut len) };
    if len == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut buffer = vec![0u64; (len as usize).div_ceil(size_of::<u64>())];
    // SAFETY: `buffer` holds at least `len` bytes and is aligned for `TOKEN_USER` and
    // `TOKEN_MANDATORY_LABEL` (both hold pointers).
    let ok = unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            class,
            buffer.as_mut_ptr().cast::<c_void>(),
            len,
            &mut len,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    let sid: PSID = if class == TokenUser {
        // SAFETY: filled by the call above with the `TokenUser` class, i.e. a `TOKEN_USER`.
        unsafe { (*buffer.as_ptr().cast::<TOKEN_USER>()).User.Sid }
    } else {
        // SAFETY: filled by the call above with the `TokenIntegrityLevel` class, i.e. a
        // `TOKEN_MANDATORY_LABEL`.
        unsafe { (*buffer.as_ptr().cast::<TOKEN_MANDATORY_LABEL>()).Label.Sid }
    };
    if sid.is_null() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "the token has no such SID",
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

/// A kernel object's DACL, read part by part rather than as SDDL text: SDDL writes some SIDs as
/// aliases (the built-in Administrator as `LA`, say), so comparing its text with a SID string is
/// wrong. Each SID here is in its full form (`S-1-5-21-…`), which names one SID exactly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dacl {
    /// Protected: it inherits no entries from a parent (`SE_DACL_PROTECTED`, `P` in SDDL).
    pub protected: bool,
    /// Its entries, in order.
    pub entries: Vec<Ace>,
}

/// One entry of a [`Dacl`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ace {
    /// Allows access to this SID (`ACCESS_ALLOWED_ACE_TYPE`).
    Allow(String),
    /// Denies access to this SID (`ACCESS_DENIED_ACE_TYPE`).
    Deny(String),
    /// Any other kind of entry, by its type number.
    Other(u8),
}

// From `Win32_System_SystemServices`, a feature needed for nothing else.
const ACCESS_ALLOWED_ACE_TYPE: u8 = 0;
const ACCESS_DENIED_ACE_TYPE: u8 = 1;

/// The DACL of a kernel object (such as a pipe), read through any handle opened with
/// `READ_CONTROL`: for checks and tests.
///
/// # Errors
///
/// If it cannot be read, or the object has no DACL at all (which would let anyone in).
pub fn dacl(object: &impl AsHandle) -> io::Result<Dacl> {
    let mut acl: *mut ACL = ptr::null_mut();
    let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
    // SAFETY: the handle is borrowed from a live object; `acl` and `descriptor` are valid
    // out-pointers and the unused ones are null, as allowed. `acl` points into `descriptor`,
    // freed below after the last use of `acl`.
    let status = unsafe {
        GetSecurityInfo(
            object.as_handle().as_raw_handle(),
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
///
/// `descriptor` must be a live security descriptor, and `acl` null or that descriptor's DACL.
unsafe fn read_dacl(descriptor: PSECURITY_DESCRIPTOR, acl: *const ACL) -> io::Result<Dacl> {
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
                let sid = sid_string(sid.cast::<c_void>())?;
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

/// The integrity level a kernel object's mandatory label gives it (such as a pipe's), read
/// through any handle opened with `READ_CONTROL`; `None` if it has no label, which Windows
/// treats as [`MEDIUM_INTEGRITY`].
///
/// # Errors
///
/// If it cannot be read.
pub fn label_integrity(object: &impl AsHandle) -> io::Result<Option<u32>> {
    security_sddl(object, LABEL_SECURITY_INFORMATION).map(|sddl| parse_label(&sddl))
}

/// The integrity level in an SDDL mandatory label (`S:(ML;;NWNR;;;HI)`), if there is one.
pub fn parse_label(sddl: &str) -> Option<u32> {
    let start = sddl.find("(ML;")?;
    let ace = &sddl[start + 1..];
    let ace = &ace[..ace.find(')')?];
    match ace.rsplit(';').next()? {
        "LW" => Some(LOW_INTEGRITY),
        "ME" => Some(MEDIUM_INTEGRITY),
        "MP" => Some(MEDIUM_INTEGRITY + 0x100),
        "HI" => Some(HIGH_INTEGRITY),
        "SI" => Some(0x4000),
        sid => integrity_rid(sid),
    }
}

/// Parts of a kernel object's security descriptor, in SDDL.
fn security_sddl(object: &impl AsHandle, info: OBJECT_SECURITY_INFORMATION) -> io::Result<String> {
    let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
    // SAFETY: the handle is borrowed from a live object; unused out-pointers are null;
    // `descriptor` is a valid out-pointer and receives a `LocalAlloc` block, freed below.
    let status = unsafe {
        GetSecurityInfo(
            object.as_handle().as_raw_handle(),
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
    // SAFETY: `descriptor` came from the call above and is still alive; `wide` is a valid
    // out-pointer.
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

/// A security descriptor that makes the current user a pipe's owner and its only grantee, at
/// the current integrity level.
#[derive(Debug)]
pub struct PipeSecurity {
    descriptor: PSECURITY_DESCRIPTOR,
}

// SAFETY: see the module comment; the descriptor is never changed and only read by Win32.
unsafe impl Send for PipeSecurity {}
// SAFETY: as above.
unsafe impl Sync for PipeSecurity {}

impl PipeSecurity {
    /// The current user as owner (`O:`); a protected DACL (`P`: nothing is inherited) with one
    /// entry, the current user with full access (nobody else, SYSTEM and Administrators
    /// included, is granted anything); and a mandatory label at this process's integrity level
    /// that refuses both reads and writes from below (`NWNR`). So an elevated ptyd's pipe
    /// cannot be opened by the same user's ordinary (medium) processes.
    ///
    /// # Errors
    ///
    /// If our token cannot be read or the descriptor cannot be built.
    pub fn current_user_only() -> io::Result<Self> {
        let me = current_identity()?;
        Self::for_identity(&me)
    }

    /// The same descriptor for another user's SID or integrity level: for tests (a process may
    /// label an object at or below its own level only).
    ///
    /// # Errors
    ///
    /// If the descriptor cannot be built.
    pub fn for_identity(identity: &Identity) -> io::Result<Self> {
        let text = Self::sddl(&identity.user, identity.integrity);
        let sddl: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
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

    /// The descriptor, in SDDL, for a user's SID and an integrity level.
    pub fn sddl(sid: &str, integrity: u32) -> String {
        let label = match integrity {
            LOW_INTEGRITY => "LW".to_owned(),
            MEDIUM_INTEGRITY => "ME".to_owned(),
            HIGH_INTEGRITY => "HI".to_owned(),
            0x4000 => "SI".to_owned(),
            other => format!("S-1-16-{other}"),
        };
        format!("O:{sid}D:P(A;;GA;;;{sid})S:(ML;;NWNR;;;{label})")
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

    /// Whether the process with id `pid` is in this job.
    ///
    /// # Errors
    ///
    /// If the process cannot be opened (it has ended, say).
    pub fn contains(&self, pid: u32) -> io::Result<bool> {
        // SAFETY: a plain call; the result is checked before use.
        let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if process.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `process` was just opened, and nothing else owns it.
        let process = unsafe { OwnedHandle::from_raw_handle(process) };
        let mut inside = 0;
        // SAFETY: both handles are live for the call; `inside` is a valid out-pointer.
        let ok =
            unsafe { IsProcessInJob(process.as_raw_handle(), self.0.as_raw_handle(), &mut inside) };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(inside != 0)
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
            assert_eq!(
                dacl(&server).expect("dacl"),
                Dacl {
                    protected: true,
                    entries: vec![Ace::Allow(sid.clone())],
                }
            );
            assert_eq!(owner_sid(&server).expect("owner"), sid);
            // The name is taken: a second first instance is refused.
            assert!(
                security
                    .create(ServerOptions::new().first_pipe_instance(true), &name)
                    .is_err()
            );
            // The pipe carries this process's integrity level, whatever it is (medium here,
            // high on an elevated CI runner).
            let me = current_identity().expect("identity");
            assert_eq!(me.user, sid);
            assert!(me.integrity >= MEDIUM_INTEGRITY, "{me:?}");
            assert_eq!(is_elevated(), me.integrity >= HIGH_INTEGRITY);
            assert_eq!(label_integrity(&server).expect("label"), Some(me.integrity));
            let mut client = ClientOptions::new()
                .security_qos_flags(
                    windows_sys::Win32::Storage::FileSystem::SECURITY_SQOS_PRESENT
                        | windows_sys::Win32::Storage::FileSystem::SECURITY_IDENTIFICATION,
                )
                .open(&name)
                .expect("client");
            let mut server = server;
            server.connect().await.expect("connect");
            assert_eq!(owner_sid(&client).expect("owner"), sid);
            assert_eq!(label_integrity(&client).expect("label"), Some(me.integrity));
            let pid = pipe_client_pid(&server).expect("client pid");
            assert_eq!(pid, std::process::id());
            assert_eq!(process_user_sid(pid).expect("client sid"), sid);
            // The client's own token, through impersonation, once it has written something.
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            client.write_all(b"hello").await.expect("write");
            let mut got = [0u8; 5];
            server.read_exact(&mut got).await.expect("read");
            assert_eq!(pipe_client_identity(&server).expect("client identity"), me);
            // Impersonation is over: this thread is itself again.
            assert_eq!(current_identity().expect("identity"), me);
        });
    }

    #[test]
    fn a_pipe_labelled_below_us_is_seen_as_such() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        rt.block_on(async {
            let name = name("label");
            let me = current_identity().expect("identity");
            // A process may label an object at or below its own level: low is always allowed.
            let low = PipeSecurity::for_identity(&Identity {
                user: me.user.clone(),
                integrity: LOW_INTEGRITY,
            })
            .expect("descriptor");
            let server = low
                .create(ServerOptions::new().first_pipe_instance(true), &name)
                .expect("pipe");
            assert_eq!(
                label_integrity(&server).expect("label"),
                Some(LOW_INTEGRITY)
            );
            // We can still open it (writing down is allowed), and see its level from there.
            let client = ClientOptions::new().open(&name).expect("client");
            assert_eq!(
                label_integrity(&client).expect("label"),
                Some(LOW_INTEGRITY)
            );
        });
    }

    #[test]
    fn labels_and_descriptors_parse() {
        assert_eq!(parse_label("S:(ML;;NWNR;;;HI)"), Some(HIGH_INTEGRITY));
        assert_eq!(parse_label("S:(ML;;NW;;;ME)"), Some(MEDIUM_INTEGRITY));
        assert_eq!(parse_label("S:(ML;;NWNR;;;LW)"), Some(LOW_INTEGRITY));
        assert_eq!(parse_label("S:(ML;;NW;;;S-1-16-8448)"), Some(0x2100));
        assert_eq!(parse_label("S:"), None);
        assert_eq!(parse_label(""), None);
        assert_eq!(parse_label("D:P(A;;FA;;;S-1-5-21-1)"), None);
        assert_eq!(integrity_rid("S-1-16-12288"), Some(HIGH_INTEGRITY));
        assert_eq!(integrity_rid("S-1-5-21-1"), None);
        assert_eq!(
            PipeSecurity::sddl("S-1-5-21-1", MEDIUM_INTEGRITY),
            "O:S-1-5-21-1D:P(A;;GA;;;S-1-5-21-1)S:(ML;;NWNR;;;ME)"
        );
        assert_eq!(
            PipeSecurity::sddl("S-1-5-21-1", HIGH_INTEGRITY),
            "O:S-1-5-21-1D:P(A;;GA;;;S-1-5-21-1)S:(ML;;NWNR;;;HI)"
        );
        assert!(PipeSecurity::sddl("S-1-5-21-1", 0x2100).ends_with("S-1-16-8448)"));
    }

    #[test]
    fn a_job_ends_its_processes() {
        let job = Job::new().expect("job");
        let mut child = std::process::Command::new("cmd.exe")
            .args(["/d", "/c", "ping -n 30 127.0.0.1 >NUL"])
            .spawn()
            .expect("child");
        job.assign_pid(child.id()).expect("assign");
        assert!(job.contains(child.id()).expect("in the job"));
        job.terminate();
        wait_until("the job ends its process", || {
            child.try_wait().expect("wait").is_some()
        });
    }

    #[test]
    fn a_job_holds_and_ends_a_grandchild_with_its_own_console() {
        // PowerShell starts ping with `Start-Process`, which gives it a console of its own (so
        // closing a pseudo console would not end it), prints its id, and waits.
        let job = Job::new().expect("job");
        let mut child = std::process::Command::new("powershell.exe")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "$p = Start-Process -PassThru -WindowStyle Hidden ping -ArgumentList '-n','60','127.0.0.1'; \
                 [Console]::Out.WriteLine($p.Id); [Console]::Out.Flush(); Start-Sleep 60",
            ])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("powershell");
        job.assign_pid(child.id()).expect("assign");
        let mut line = String::new();
        let stdout = child.stdout.take().expect("stdout");
        std::io::BufRead::read_line(&mut std::io::BufReader::new(stdout), &mut line)
            .expect("the grandchild's id");
        let grandchild: u32 = line.trim().parse().expect("a pid");
        assert!(
            job.contains(grandchild).expect("look"),
            "the grandchild left the job"
        );
        job.terminate();
        wait_until("the job ends the child", || {
            child.try_wait().expect("wait").is_some()
        });
        // Gone, or exiting: either it cannot be opened any more, or it is no longer running.
        wait_until("the job ends the grandchild", || !running(grandchild));
    }

    /// Whether process `pid` still runs.
    fn running(pid: u32) -> bool {
        std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"])
            .output()
            .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains(&format!("\"{pid}\"")))
    }

    fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while !done() {
            assert!(std::time::Instant::now() < deadline, "never: {what}");
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }
}
