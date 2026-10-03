//! Windows: who this process is, who owns an object and what its DACL and mandatory label hold,
//! SIDs from text, security descriptors for named pipes, and private file storage.
//!
//! The one copy of this code. The API's pipe (pitcrew-api), the askpass pipe (pitcrew-remote)
//! and pitcrew-ptyd's pipe (pitcrew-runtime) keep their own policy (the descriptor they ask for,
//! what they check) and call these. The runner uses native file security for private storage.
//! A DACL is compared by SID, never as SDDL text, which names
//! some SIDs by alias.
//!
//! **The crate's only `unsafe` code.** The Win32 calls below have no safe binding in the
//! dependency tree, and tokio takes a descriptor only through
//! `create_with_security_attributes_raw`. Every function here that is not an `unsafe fn` is safe
//! to call; each `unsafe fn` says what its caller must uphold, and each `unsafe` block why it is
//! sound. In general:
//! - Every out-pointer passed to Win32 points at a live local of the right type, and every
//!   buffer is passed with its true length.
//! - A `TOKEN_USER`, `TOKEN_OWNER` or `TOKEN_MANDATORY_LABEL` is read from a buffer that
//!   `GetTokenInformation` filled with exactly that class, aligned for it (`u64` storage). The
//!   SID it points to lies inside that buffer, which outlives its use.
//! - Memory Win32 allocates with `LocalAlloc` (strings, SIDs, descriptors) is freed exactly once
//!   with `LocalFree`, after its last use. Pointers into a descriptor (its owner, its DACL and
//!   the DACL's entries) are used only while it lives.
//! - A DACL entry is read as the structure its header's type names.
//! - Handles we open are owned by `OwnedHandle` and closed exactly once. Handles we are given
//!   are borrowed (`AsHandle`), so they stay open for the call.
//! - A descriptor is never changed after it is made, and Win32 only reads it, so sharing it
//!   between threads (`Send`, `Sync`) is sound.

#![allow(unsafe_code)]

use std::ffi::{OsStr, c_void};
use std::fs::File;
use std::io;
use std::os::windows::ffi::OsStrExt as _;
use std::os::windows::fs::{MetadataExt as _, OpenOptionsExt as _};
use std::os::windows::io::{AsHandle, AsRawHandle as _, FromRawHandle as _, OwnedHandle};
use std::path::Path;
use std::ptr;

use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
use windows_sys::Win32::Foundation::{GENERIC_READ, GENERIC_WRITE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Foundation::{HANDLE, LocalFree};
use windows_sys::Win32::Security::Authorization::{
    ConvertSecurityDescriptorToStringSecurityDescriptorW, ConvertSidToStringSidW,
    ConvertStringSecurityDescriptorToSecurityDescriptorW, ConvertStringSidToSidW, GetSecurityInfo,
    SDDL_REVISION_1, SE_FILE_OBJECT, SE_KERNEL_OBJECT, SetSecurityInfo,
};
use windows_sys::Win32::Security::{
    ACCESS_ALLOWED_ACE, ACE_HEADER, ACL, DACL_SECURITY_INFORMATION, GetAce,
    GetSecurityDescriptorControl, GetSecurityDescriptorDacl, GetSecurityDescriptorOwner,
    GetTokenInformation, LABEL_SECURITY_INFORMATION, OBJECT_SECURITY_INFORMATION,
    OWNER_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID,
    SE_DACL_PROTECTED, SECURITY_ATTRIBUTES, SECURITY_DESCRIPTOR_CONTROL, TOKEN_INFORMATION_CLASS,
    TOKEN_MANDATORY_LABEL, TOKEN_OWNER, TOKEN_QUERY, TOKEN_USER, TokenIntegrityLevel, TokenOwner,
    TokenUser, UNPROTECTED_DACL_SECURITY_INFORMATION,
};
use windows_sys::Win32::Storage::FileSystem::{
    CREATE_NEW, CreateDirectoryW, CreateFileW, FILE_ATTRIBUTE_REPARSE_POINT,
    FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ, FILE_SHARE_WRITE,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

pub use crate::sddl::{
    HIGH_INTEGRITY, LOW_INTEGRITY, MEDIUM_INTEGRITY, SYSTEM_INTEGRITY, integrity_rid, parse_label,
};

/// Full access to a file or pipe, as its DACL reads back what was granted as `GA` (generic all):
/// `FILE_ALL_ACCESS` (`FA` in SDDL).
pub const FILE_ALL_ACCESS: u32 = windows_sys::Win32::Storage::FileSystem::FILE_ALL_ACCESS;

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
/// If our own process token cannot be read.
pub fn current_user_sid() -> io::Result<String> {
    token_sid(&own_token()?, TokenSid::User)
}

/// This process's user and integrity level.
///
/// # Errors
/// If our own process token cannot be read.
pub fn current_identity() -> io::Result<Identity> {
    token_identity(&own_token()?)
}

/// True if this process runs elevated (at high integrity or above).
pub fn is_elevated() -> bool {
    current_identity().is_ok_and(|identity| identity.integrity >= HIGH_INTEGRITY)
}

/// The owner our process token gives the objects it creates without naming one (`TokenOwner`).
/// Unelevated, that is the user itself; elevated, typically the Administrators group
/// (`S-1-5-32-544`), unless policy makes it the user. So a pipe that must be the user's names
/// its owner.
///
/// # Errors
/// If our own process token cannot be read.
pub fn default_owner_sid() -> io::Result<String> {
    token_sid(&own_token()?, TokenSid::Owner)
}

/// The user and integrity level a token holds (a process's, or a thread's while it impersonates
/// a pipe's client), opened with `TOKEN_QUERY`.
///
/// # Errors
/// If it cannot be read (it is not a token, say).
pub fn token_identity(token: &impl AsHandle) -> io::Result<Identity> {
    let user = token_sid(token, TokenSid::User)?;
    let label = token_sid(token, TokenSid::IntegrityLabel)?;
    let integrity = integrity_rid(&label).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{label} is not an integrity level"),
        )
    })?;
    Ok(Identity { user, integrity })
}

/// The SID of the user a token holds, opened with `TOKEN_QUERY`.
///
/// # Errors
/// If it cannot be read (it is not a token, say).
pub fn token_user_sid(token: &impl AsHandle) -> io::Result<String> {
    token_sid(token, TokenSid::User)
}

/// Our own process's token, for queries.
fn own_token() -> io::Result<OwnedHandle> {
    let mut token: HANDLE = ptr::null_mut();
    // SAFETY: `GetCurrentProcess` returns a pseudo-handle that is always valid and needs no
    // closing; `token` is a valid out-pointer.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `token` was just opened, and nothing else owns it.
    Ok(unsafe { OwnedHandle::from_raw_handle(token) })
}

/// A SID a token holds.
#[derive(Clone, Copy, Debug)]
enum TokenSid {
    /// The user it runs as (`TokenUser`).
    User,
    /// The default owner of the objects it creates (`TokenOwner`).
    Owner,
    /// Its mandatory label (`TokenIntegrityLevel`).
    IntegrityLabel,
}

impl TokenSid {
    fn class(self) -> TOKEN_INFORMATION_CLASS {
        match self {
            Self::User => TokenUser,
            Self::Owner => TokenOwner,
            Self::IntegrityLabel => TokenIntegrityLevel,
        }
    }
}

/// One SID a token holds, as a string.
fn token_sid(token: &impl AsHandle, which: TokenSid) -> io::Result<String> {
    let handle = token.as_handle().as_raw_handle();
    let class = which.class();
    let mut len = 0u32;
    // SAFETY: a size query: null buffer, zero length, valid length out-pointer. The handle is
    // borrowed, so open. It fails with ERROR_INSUFFICIENT_BUFFER and sets `len`.
    unsafe { GetTokenInformation(handle, class, ptr::null_mut(), 0, &mut len) };
    if len == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut buffer = vec![0u64; (len as usize).div_ceil(size_of::<u64>())];
    // SAFETY: `buffer` holds at least `len` bytes and is aligned for `TOKEN_USER`,
    // `TOKEN_OWNER` and `TOKEN_MANDATORY_LABEL` (each holds a pointer).
    let ok = unsafe {
        GetTokenInformation(
            handle,
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
        // SAFETY: filled by the call above with the `TokenUser` class, i.e. a `TOKEN_USER`.
        TokenSid::User => unsafe { (*buffer.as_ptr().cast::<TOKEN_USER>()).User.Sid },
        // SAFETY: filled with the `TokenOwner` class, i.e. a `TOKEN_OWNER`.
        TokenSid::Owner => unsafe { (*buffer.as_ptr().cast::<TOKEN_OWNER>()).Owner },
        // SAFETY: filled with the `TokenIntegrityLevel` class, i.e. a `TOKEN_MANDATORY_LABEL`.
        TokenSid::IntegrityLabel => unsafe {
            (*buffer.as_ptr().cast::<TOKEN_MANDATORY_LABEL>()).Label.Sid
        },
    };
    if sid.is_null() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("the token has no {which:?} SID"),
        ));
    }
    // SAFETY: `sid` points into `buffer`, which is alive until after the call.
    unsafe { sid_string(sid) }
}

/// A SID as a string, `S-1-…`.
///
/// # Safety
/// `sid` must point to a valid SID that stays alive for the call.
unsafe fn sid_string(sid: PSID) -> io::Result<String> {
    let mut wide: *mut u16 = ptr::null_mut();
    // SAFETY: `sid` is valid (guaranteed by the caller); `wide` is a valid out-pointer.
    if unsafe { ConvertSidToStringSidW(sid, &mut wide) } == 0 || wide.is_null() {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `wide` is a NUL-terminated `LocalAlloc` string, not used afterwards.
    Ok(unsafe { take_local_string(wide) })
}

/// A SID in its full form (`S-1-…`), from that form or from one of SDDL's aliases (`BA` for the
/// Administrators group, `LA` for the built-in Administrator, as CI's Windows runner is, `WD`
/// for Everyone): SDDL names some SIDs by alias, so its text is never compared as it is.
///
/// # Errors
/// `InvalidInput` for text with a NUL; otherwise if Windows does not read it as a SID.
pub fn canonical_sid(text: &str) -> io::Result<String> {
    let wide = wide_text(text)?;
    let mut sid: PSID = ptr::null_mut();
    // SAFETY: `wide` is NUL-terminated; `sid` is a valid out-pointer and receives a `LocalAlloc`
    // block, freed below.
    if unsafe { ConvertStringSidToSidW(wide.as_ptr(), &mut sid) } == 0 || sid.is_null() {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `sid` came from the call above and is freed only after this call.
    let text = unsafe { sid_string(sid) };
    // SAFETY: allocated by `ConvertStringSidToSidW`, freed once, after its last use.
    unsafe {
        LocalFree(sid);
    }
    text
}

/// The SID of a kernel object's owner, such as a pipe's, read through any handle to it opened
/// with `READ_CONTROL` (a pipe client opened for reading has it).
///
/// A pipe's owner is its creator's user, or whoever its descriptor names; naming another user
/// takes the restore privilege. So another user's pipe cannot claim the current user as owner,
/// and unlike the server's process id, the owner cannot be recycled.
///
/// # Errors
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
        return Err(os_error(status));
    }
    let sid = if owner.is_null() {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "the object has no owner",
        ))
    } else {
        // SAFETY: `owner` points into `descriptor`, which is still alive.
        unsafe { sid_string(owner) }
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
    /// Allows the access in `mask` to `sid` (`ACCESS_ALLOWED_ACE_TYPE`).
    Allow {
        /// The SID, e.g. `S-1-5-21-…`.
        sid: String,
        /// The access mask, as stored: generic rights read back mapped (`GA` on a pipe or file
        /// as [`FILE_ALL_ACCESS`]).
        mask: u32,
    },
    /// Denies the access in `mask` to `sid` (`ACCESS_DENIED_ACE_TYPE`).
    Deny {
        /// The SID.
        sid: String,
        /// The access mask, as stored.
        mask: u32,
    },
    /// Any other kind of entry, by its type number.
    Other(u8),
}

// From `Win32_System_SystemServices`, a feature needed for nothing else.
const ACCESS_ALLOWED_ACE_TYPE: u8 = 0;
const ACCESS_DENIED_ACE_TYPE: u8 = 1;

/// The DACL of a kernel object (such as a pipe), read through any handle opened with
/// `READ_CONTROL`.
///
/// # Errors
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
        return Err(os_error(status));
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
                let (mask, sid) = unsafe {
                    let entry = ace.cast::<ACCESS_ALLOWED_ACE>();
                    ((*entry).Mask, &raw mut (*entry).SidStart)
                };
                // SAFETY: `sid` points into the live DACL.
                let sid = unsafe { sid_string(sid.cast::<c_void>()) }?;
                if kind == ACCESS_ALLOWED_ACE_TYPE {
                    Ace::Allow { sid, mask }
                } else {
                    Ace::Deny { sid, mask }
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
/// If it cannot be read.
pub fn label_integrity(object: &impl AsHandle) -> io::Result<Option<u32>> {
    security_sddl(object, LABEL_SECURITY_INFORMATION).map(|sddl| parse_label(&sddl))
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
        return Err(os_error(status));
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

/// Creates a new file with the current user as owner and a protected, owner-only FullControl DACL.
/// The returned read/write handle denies delete sharing. No existing file is overwritten.
///
/// # Errors
/// If the path exists, contains a NUL, or Windows cannot create the secured file.
pub fn create_private_file(path: &Path) -> io::Result<File> {
    let descriptor = private_descriptor(false)?;
    create_secured_file(path, &descriptor)
}

fn create_secured_file(path: &Path, descriptor: &SecurityDescriptor) -> io::Result<File> {
    let wide = wide_path(path)?;
    let attributes = security_attributes(descriptor);
    // SAFETY: the path is NUL-terminated; attributes and its descriptor are live for the call.
    // CREATE_NEW creates only a new object and the handle is owned below if successful.
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            &attributes,
            CREATE_NEW,
            FILE_FLAG_OPEN_REPARSE_POINT,
            ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the successful call returned a new handle that nothing else owns.
    Ok(unsafe { File::from_raw_handle(handle) })
}

/// Creates a new folder with a protected, owner-only FullControl DACL, inherited by its children.
///
/// # Errors
/// If the path exists, contains a NUL, or Windows cannot create the secured folder.
pub fn create_private_directory(path: &Path) -> io::Result<()> {
    let descriptor = private_descriptor(true)?;
    let wide = wide_path(path)?;
    let attributes = security_attributes(&descriptor);
    // SAFETY: the NUL-terminated path, attributes and descriptor remain live during the call.
    if unsafe { CreateDirectoryW(wide.as_ptr(), &attributes) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn private_descriptor(directory: bool) -> io::Result<SecurityDescriptor> {
    let sid = current_user_sid()?;
    let inherit = if directory { "OICI" } else { "" };
    SecurityDescriptor::from_sddl(&format!("O:{sid}D:P(A;{inherit};FA;;;{sid})"))
}

fn security_attributes(descriptor: &SecurityDescriptor) -> SECURITY_ATTRIBUTES {
    SECURITY_ATTRIBUTES {
        nLength: u32::try_from(size_of::<SECURITY_ATTRIBUTES>()).unwrap_or(u32::MAX),
        lpSecurityDescriptor: descriptor.descriptor,
        bInheritHandle: 0,
    }
}

fn wide_path(path: &Path) -> io::Result<Vec<u16>> {
    let wide: Vec<_> = path.as_os_str().encode_wide().collect();
    if wide.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the path holds a NUL",
        ));
    }
    Ok(wide.into_iter().chain(Some(0)).collect())
}

// Standard security rights, without granting data access just to inspect or change an ACL.
const READ_CONTROL: u32 = 0x0002_0000;
const WRITE_DAC: u32 = 0x0004_0000;
const WRITE_OWNER: u32 = 0x0008_0000;

fn security_file(path: &Path, access: u32) -> io::Result<File> {
    let file = std::fs::OpenOptions::new()
        .access_mode(
            access
                | READ_CONTROL
                | windows_sys::Win32::Storage::FileSystem::FILE_READ_ATTRIBUTES
                | windows_sys::Win32::Storage::FileSystem::SYNCHRONIZE,
        )
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)?;
    let meta = file.metadata()?;
    if meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "a reparse point is refused",
        ));
    }
    Ok(file)
}

/// Sets the current user as owner and a protected, owner-only FullControl DACL on an existing file.
///
/// # Errors
/// If opening or setting security fails, or the object is a directory or reparse point.
pub fn set_private_file(path: &Path) -> io::Result<()> {
    let file = security_file(path, READ_CONTROL | WRITE_DAC)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "a regular file is required",
        ));
    }
    let descriptor = private_descriptor(false)?;
    if descriptor_owner(&file_descriptor(&file)?)? == current_user_sid()? {
        apply_file_descriptor(&file, &descriptor, false, true)
    } else {
        // Keep the first handle alive so the path cannot be replaced while reopening it.
        let writable = security_file(path, WRITE_DAC | WRITE_OWNER)?;
        apply_file_descriptor(&writable, &descriptor, true, true)
    }
}

fn apply_file_descriptor(
    file: &File,
    descriptor: &SecurityDescriptor,
    owner: bool,
    protected: bool,
) -> io::Result<()> {
    let mut acl = ptr::null_mut();
    let mut sid = ptr::null_mut();
    let mut present = 0;
    let mut defaulted = 0;
    // SAFETY: the descriptor is valid and live; each output points to a local of the correct
    // type. Returned pointers belong to the descriptor, which outlives SetSecurityInfo below.
    let ok = unsafe {
        GetSecurityDescriptorDacl(
            descriptor.descriptor,
            &mut present,
            &mut acl,
            &mut defaulted,
        ) != 0
            && GetSecurityDescriptorOwner(descriptor.descriptor, &mut sid, &mut defaulted) != 0
    };
    if !ok {
        return Err(io::Error::last_os_error());
    }
    if present == 0 || acl.is_null() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "a DACL is required",
        ));
    }
    let info = DACL_SECURITY_INFORMATION
        | if protected {
            PROTECTED_DACL_SECURITY_INFORMATION
        } else {
            UNPROTECTED_DACL_SECURITY_INFORMATION
        }
        | if owner { OWNER_SECURITY_INFORMATION } else { 0 };
    // SAFETY: the file handle is borrowed and live; the SID and ACL point into the live
    // descriptor. The unused group and SACL are null, as permitted by the selected flags.
    let status = unsafe {
        SetSecurityInfo(
            file.as_raw_handle(),
            SE_FILE_OBJECT,
            info,
            if owner { sid } else { ptr::null_mut() },
            ptr::null_mut(),
            acl,
            ptr::null_mut(),
        )
    };
    if status != 0 {
        return Err(os_error(status));
    }
    Ok(())
}

fn file_descriptor(file: &File) -> io::Result<SecurityDescriptor> {
    let mut descriptor = ptr::null_mut();
    // SAFETY: the file handle is live; descriptor is a valid out-pointer. Unused pointers may
    // be null; the returned LocalAlloc descriptor is owned and freed by SecurityDescriptor.
    let status = unsafe {
        GetSecurityInfo(
            file.as_raw_handle(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            &mut descriptor,
        )
    };
    if status != 0 {
        return Err(os_error(status));
    }
    Ok(SecurityDescriptor { descriptor })
}

fn descriptor_dacl(descriptor: &SecurityDescriptor) -> io::Result<Dacl> {
    let mut acl = ptr::null_mut();
    let mut present = 0;
    let mut defaulted = 0;
    // SAFETY: descriptor is live and the output pointers point to locals of the correct types.
    if unsafe {
        GetSecurityDescriptorDacl(
            descriptor.descriptor,
            &mut present,
            &mut acl,
            &mut defaulted,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: acl is null or belongs to the live descriptor, as returned above.
    unsafe { read_dacl(descriptor.descriptor, acl) }
}

/// Copies a regular file's DACL and its inheritance protection to another regular file.
/// The destination owner is unchanged. Both handles deny delete sharing during the copy.
///
/// # Errors
/// If either file cannot be opened, is a reparse point or directory, or its security cannot be read/set.
pub fn copy_file_dacl(source: &Path, target: &Path) -> io::Result<()> {
    let source = security_file(source, READ_CONTROL)?;
    let target = security_file(target, WRITE_DAC)?;
    if !source.metadata()?.is_file() || !target.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "regular files are required",
        ));
    }
    let descriptor = file_descriptor(&source)?;
    let protected = descriptor_dacl(&descriptor)?.protected;
    apply_file_descriptor(&target, &descriptor, false, protected)
}

/// Checks that a file or folder is owned by the current user and has a protected DACL
/// containing only FullControl allow entries for that SID. Empty and null DACLs are refused.
///
/// # Errors
/// If security cannot be read, the object is a reparse point, or any security check fails.
pub fn check_private_object(path: &Path) -> io::Result<()> {
    let file = security_file(path, READ_CONTROL)?;
    let descriptor = file_descriptor(&file)?;
    let sid = current_user_sid()?;
    let dacl = descriptor_dacl(&descriptor)?;
    let own = descriptor_owner(&descriptor)? == sid;
    if !own || !dacl.protected || dacl.entries.is_empty()
        || !dacl.entries.iter().all(|entry| matches!(entry, Ace::Allow { sid: who, mask } if who == &sid && *mask == FILE_ALL_ACCESS)) {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, "owner-only security is required"));
    }
    Ok(())
}

fn descriptor_owner(descriptor: &SecurityDescriptor) -> io::Result<String> {
    let mut owner = ptr::null_mut();
    let mut defaulted = 0;
    // SAFETY: descriptor is live; both output pointers point to correctly typed locals.
    if unsafe { GetSecurityDescriptorOwner(descriptor.descriptor, &mut owner, &mut defaulted) } == 0
    {
        return Err(io::Error::last_os_error());
    }
    if owner.is_null() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "an owner is required",
        ));
    }
    // SAFETY: the non-null owner returned above belongs to the still-live descriptor.
    unsafe { sid_string(owner) }
}

/// A security descriptor made from SDDL, for the named pipes a server creates. The text is the
/// caller's policy: which owner, which entries, which label.
#[derive(Debug)]
pub struct SecurityDescriptor {
    descriptor: PSECURITY_DESCRIPTOR,
}

// SAFETY: see the module comment; the descriptor is never changed and only read by Win32.
unsafe impl Send for SecurityDescriptor {}
// SAFETY: as above.
unsafe impl Sync for SecurityDescriptor {}

impl SecurityDescriptor {
    /// The descriptor `sddl` describes, e.g. `O:<sid>D:P(A;;GA;;;<sid>)`.
    ///
    /// # Errors
    /// `InvalidInput` for text with a NUL; otherwise if Windows does not read it.
    pub fn from_sddl(sddl: &str) -> io::Result<Self> {
        let wide = wide_text(sddl)?;
        let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
        // SAFETY: `wide` is NUL-terminated; `descriptor` is a valid out-pointer; the size
        // out-pointer may be null.
        let ok = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                wide.as_ptr(),
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

    /// Creates a pipe instance named `name` with this descriptor and `options`.
    ///
    /// # Errors
    /// As [`ServerOptions::create`]: with `first_pipe_instance`, `PermissionDenied` when the
    /// name is taken.
    pub fn create_pipe(
        &self,
        options: &ServerOptions,
        name: impl AsRef<OsStr>,
    ) -> io::Result<NamedPipeServer> {
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

impl Drop for SecurityDescriptor {
    fn drop(&mut self) {
        // SAFETY: allocated by `ConvertStringSecurityDescriptorToSecurityDescriptorW`, freed
        // once.
        unsafe {
            LocalFree(self.descriptor);
        }
    }
}

/// `text` as a NUL-terminated wide string; refused if it holds a NUL, which would cut it short.
fn wide_text(text: &str) -> io::Result<Vec<u16>> {
    if text.contains('\0') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the text holds a NUL",
        ));
    }
    Ok(text.encode_utf16().chain(Some(0)).collect())
}

/// The error a Win32 status code (as `GetSecurityInfo` returns) names.
fn os_error(status: u32) -> io::Error {
    io::Error::from_raw_os_error(i32::try_from(status).unwrap_or(-1))
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

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::windows::named_pipe::ClientOptions;

    #[test]
    fn private_files_and_folders_are_secured_at_creation() -> io::Result<()> {
        let tmp = tempfile::tempdir()?;
        let folder = tmp.path().join("private");
        create_private_directory(&folder).expect("create folder");
        check_private_object(&folder).expect("check folder");
        let path = folder.join("file");
        let mut file = create_private_file(&path).expect("create file");
        use std::io::Write as _;
        file.write_all(b"original")?;
        check_private_object(&path).expect("check file");
        assert_eq!(
            create_private_file(&path).expect_err("exclusive").kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(
            create_private_directory(&folder)
                .expect_err("exclusive")
                .kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(std::fs::read(&path)?, b"original");
        // A normal child inherits only the owner's entries, but is not protected itself.
        let child = folder.join("inherited");
        std::fs::write(&child, b"child")?;
        let acl = descriptor_dacl(&file_descriptor(
            &security_file(&child, READ_CONTROL).expect("open child"),
        )?)?;
        assert!(!acl.protected);
        assert_eq!(
            acl.entries,
            vec![Ace::Allow {
                sid: current_user_sid()?,
                mask: FILE_ALL_ACCESS
            }]
        );
        assert!(check_private_object(&child).is_err());
        set_private_file(&child).expect("set child private");
        check_private_object(&child)?;
        assert_eq!(std::fs::read(child)?, b"child");
        assert!(set_private_file(&folder).is_err());
        assert!(check_private_object(&tmp.path().join("missing")).is_err());
        assert_eq!(
            create_private_file(Path::new("bad\0path"))
                .expect_err("NUL")
                .kind(),
            io::ErrorKind::InvalidInput
        );
        Ok(())
    }

    #[test]
    fn copying_a_file_dacl_preserves_permissions_and_protection() -> io::Result<()> {
        let tmp = tempfile::tempdir()?;
        let source = tmp.path().join("source");
        let target = tmp.path().join("target");
        drop(create_private_file(&source).expect("create source"));
        drop(create_private_file(&target)?);
        let sid = current_user_sid()?;
        for protected in [true, false] {
            let descriptor =
                SecurityDescriptor::from_sddl(&format!("O:{sid}D:P(A;;FA;;;{sid})(A;;FR;;;WD)"))?;
            let file = security_file(&source, WRITE_DAC).expect("open source to set");
            apply_file_descriptor(&file, &descriptor, false, protected).expect("set source");
            drop(file);
            let source_acl =
                descriptor_dacl(&file_descriptor(&security_file(&source, READ_CONTROL)?)?)?;
            assert_eq!(source_acl.protected, protected);
            copy_file_dacl(&source, &target).expect("copy dacl");
            let target_file = security_file(&target, READ_CONTROL)?;
            let target_acl = descriptor_dacl(&file_descriptor(&target_file)?)?;
            assert_eq!(target_acl.protected, protected);
            // Unprotected ACLs may inherit destination-parent entries as Windows requires.
            for entry in &source_acl.entries[..2] {
                assert!(target_acl.entries.contains(entry));
            }
            if protected {
                assert_eq!(target_acl, source_acl);
            }
            assert_eq!(owner_sid(&target_file)?, sid);
            assert!(check_private_object(&target).is_err());
        }
        assert!(copy_file_dacl(tmp.path(), &target).is_err());
        Ok(())
    }

    #[test]
    fn private_check_refuses_public_denied_empty_partial_and_wrong_owner() -> io::Result<()> {
        let tmp = tempfile::tempdir()?;
        let sid = current_user_sid()?;
        for (name, text) in [
            ("public", format!("O:{sid}D:P(A;;FA;;;{sid})(A;;FR;;;WD)")),
            ("denied", format!("O:{sid}D:P(D;;FR;;;WD)(A;;FA;;;{sid})")),
            ("partial", format!("O:{sid}D:P(A;;FR;;;{sid})")),
            ("empty", format!("O:{sid}D:P")),
            ("null", format!("O:{sid}D:NO_ACCESS_CONTROL")),
            ("unprotected", format!("O:{sid}D:(A;;FA;;;{sid})")),
        ] {
            let path = tmp.path().join(name);
            let descriptor = SecurityDescriptor::from_sddl(&text)?;
            let file = create_secured_file(&path, &descriptor).expect(name);
            if name == "unprotected" {
                let writable = security_file(&path, WRITE_DAC)?;
                apply_file_descriptor(&writable, &descriptor, false, false)?;
            }
            assert!(check_private_object(&path).is_err(), "{name}");
            drop(file);
            // Restrictive DACLs can deny even the metadata/security reads needed to repair them.
            if matches!(name, "denied" | "empty" | "partial") {
                continue;
            }
            set_private_file(&path).expect(name);
            check_private_object(&path)?;
        }
        if default_owner_sid()? != sid {
            let path = tmp.path().join("wrong-owner");
            let descriptor = SecurityDescriptor::from_sddl(&format!("D:P(A;;FA;;;{sid})"))?;
            drop(create_secured_file(&path, &descriptor)?);
            assert!(check_private_object(&path).is_err());
            set_private_file(&path)?;
            check_private_object(&path)?;
        }
        Ok(())
    }

    /// A pipe name of this test's own.
    fn pipe_name(test: &str) -> String {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        format!(
            r"\\.\pipe\pitcrew-trust-unit-{test}-{}-{nanos}",
            std::process::id()
        )
    }

    fn block_on<T>(future: impl std::future::Future<Output = T>) -> T {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime")
            .block_on(future)
    }

    /// The current user, this process's integrity level, and the default owner of what it
    /// creates.
    #[test]
    fn the_current_user_and_the_default_owner() {
        let sid = current_user_sid().expect("sid");
        assert!(sid.starts_with("S-1-"), "{sid}");
        let me = current_identity().expect("identity");
        assert_eq!(me.user, sid);
        // Medium here, high on an elevated CI runner.
        assert!(me.integrity >= MEDIUM_INTEGRITY, "{me:?}");
        assert_eq!(is_elevated(), me.integrity >= HIGH_INTEGRITY);
        assert_eq!(
            token_user_sid(&own_token().expect("token")).expect("user"),
            sid
        );
        assert_eq!(
            token_identity(&own_token().expect("token")).expect("id"),
            me
        );
        let owner = default_owner_sid().expect("default owner");
        if is_elevated() {
            // The Administrators group, unless policy makes it the user.
            assert!(owner == sid || owner == "S-1-5-32-544", "{owner}");
        } else {
            assert_eq!(owner, sid);
        }
        // A handle that is not a token is refused, not misread.
        let file = std::fs::File::open(std::env::current_exe().expect("exe")).expect("file");
        assert!(token_user_sid(&file).is_err());
    }

    /// SIDs from text, whatever form SDDL writes them in.
    #[test]
    fn sids_from_text() {
        let me = current_user_sid().expect("sid");
        assert_eq!(canonical_sid(&me).expect("own"), me);
        assert_eq!(canonical_sid("BA").expect("alias"), "S-1-5-32-544");
        assert_eq!(canonical_sid("WD").expect("alias"), "S-1-1-0");
        assert_eq!(canonical_sid("S-1-5-18").expect("system"), "S-1-5-18");
        assert!(canonical_sid("not a sid").is_err());
        assert!(canonical_sid("").is_err());
        assert_eq!(
            canonical_sid("BA\0x").expect_err("a NUL").kind(),
            io::ErrorKind::InvalidInput
        );
    }

    /// A pipe made with a descriptor reads back as it was made, through the server's handle and
    /// a client's: its owner, its DACL entry by entry with the access mask (`GA` as granted
    /// reads back as `FILE_ALL_ACCESS`), denials included, and its label.
    #[test]
    fn owner_dacl_and_label_of_a_pipe() {
        block_on(async {
            let me = current_identity().expect("identity");
            let sid = &me.user;
            let level = crate::sddl::label(me.integrity);
            let text = format!("O:{sid}D:P(A;;GA;;;{sid})S:(ML;;NWNR;;;{level})");
            let descriptor = SecurityDescriptor::from_sddl(&text).expect("descriptor");
            let name = pipe_name("own");
            let mut options = ServerOptions::new();
            options.first_pipe_instance(true);
            let server = descriptor.create_pipe(&options, &name).expect("pipe");
            let ours = Dacl {
                protected: true,
                entries: vec![Ace::Allow {
                    sid: sid.clone(),
                    mask: FILE_ALL_ACCESS,
                }],
            };
            assert_eq!(dacl(&server).expect("dacl"), ours);
            assert_eq!(owner_sid(&server).expect("owner"), *sid);
            assert_eq!(label_integrity(&server).expect("label"), Some(me.integrity));
            // The name is taken: a second first instance is refused.
            assert!(descriptor.create_pipe(&options, &name).is_err());
            let client = ClientOptions::new().open(&name).expect("client");
            server.connect().await.expect("connect");
            assert_eq!(owner_sid(&client).expect("owner"), *sid);
            assert_eq!(dacl(&client).expect("dacl"), ours);
            assert_eq!(label_integrity(&client).expect("label"), Some(me.integrity));

            // A denial first, of anonymous logons (written `AN`, read back as their SID), and a
            // label below us (a process may label an object at or below its own level).
            let text = format!("O:{sid}D:P(D;;GA;;;AN)(A;;GA;;;{sid})S:(ML;;NWNR;;;LW)");
            let denying = SecurityDescriptor::from_sddl(&text).expect("descriptor");
            let server = denying
                .create_pipe(&options, pipe_name("deny"))
                .expect("pipe");
            assert_eq!(
                dacl(&server).expect("dacl"),
                Dacl {
                    protected: true,
                    entries: vec![
                        Ace::Deny {
                            sid: "S-1-5-7".into(),
                            mask: FILE_ALL_ACCESS,
                        },
                        Ace::Allow {
                            sid: sid.clone(),
                            mask: FILE_ALL_ACCESS,
                        },
                    ],
                }
            );
            assert_eq!(
                label_integrity(&server).expect("label"),
                Some(LOW_INTEGRITY)
            );
        });
    }

    /// A pipe made without a descriptor is owned by the token's default owner, and its default
    /// DACL is not ours alone.
    #[test]
    fn a_pipe_with_default_security() {
        block_on(async {
            let server = ServerOptions::new()
                .first_pipe_instance(true)
                .create(pipe_name("default"))
                .expect("pipe");
            assert_eq!(
                owner_sid(&server).expect("owner"),
                default_owner_sid().expect("default owner")
            );
            // More entries than ours alone (Everyone may read).
            let read = dacl(&server).expect("dacl");
            assert!(read.entries.len() > 1, "{read:?}");
        });
    }

    #[test]
    fn bad_descriptors_are_refused() {
        assert!(SecurityDescriptor::from_sddl("not sddl").is_err());
        assert_eq!(
            SecurityDescriptor::from_sddl("D:P\0(A;;GA;;;WD)")
                .expect_err("a NUL")
                .kind(),
            io::ErrorKind::InvalidInput
        );
    }
}
