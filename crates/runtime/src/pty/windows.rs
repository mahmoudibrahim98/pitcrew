//! Windows process control and peer checks for the PTY runtime and pitcrew-ptyd: who is on the
//! other end of a pipe (its process's user, and its own token by impersonation), the descriptor of
//! ptyd's pipe (the current user alone, at the current integrity level), Job Objects, and the
//! lock file that gives one ptyd endpoint to one daemon.
//!
//! The current user, owners, DACLs, labels and descriptors are `pitcrew_trust::windows`, the one
//! copy of that code behind every PitCrew pipe; they are re-exported here for pitcrew-ptyd.
//!
//! **This is the only `unsafe` code in pitcrew-runtime and pitcrew-ptyd.** The Win32 calls below
//! have no safe binding in the dependency tree. Every function here is safe to call; each
//! `unsafe` block says why it is sound. In general:
//! - Every out-pointer passed to Win32 points at a live local of the right type.
//! - Handles we open are owned by `OwnedHandle` (or a `File`) and closed exactly once. Handles we
//!   are given are borrowed (`AsHandle`), so they stay open for the call.
//! - Impersonating a pipe's client is undone on the same thread before the function returns;
//!   should that fail, the process aborts rather than go on as the client.
//! - A file is locked and unlocked only through a handle we opened for synchronous I/O, so the
//!   `OVERLAPPED` passed (for the offset) is not used after the call returns.

#![allow(unsafe_code)]

use std::fs::File;
use std::io;
use std::os::windows::io::{AsHandle, AsRawHandle as _, FromRawHandle as _, OwnedHandle};
use std::path::Path;
use std::ptr;

use pitcrew_trust::windows::SecurityDescriptor;
use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
use windows_sys::Win32::Foundation::{ERROR_IO_PENDING, ERROR_LOCK_VIOLATION, HANDLE};
use windows_sys::Win32::Security::{RevertToSelf, TOKEN_QUERY};
use windows_sys::Win32::Storage::FileSystem::{
    LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY, LockFileEx, UnlockFileEx,
};
use windows_sys::Win32::System::IO::OVERLAPPED;
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, IsProcessInJob, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
    SetInformationJobObject, TerminateJobObject,
};
use windows_sys::Win32::System::Pipes::{GetNamedPipeClientProcessId, ImpersonateNamedPipeClient};
use windows_sys::Win32::System::Threading::{
    GetCurrentThread, OpenProcess, OpenProcessToken, OpenThreadToken,
    PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SET_QUOTA, PROCESS_TERMINATE,
};

pub use pitcrew_trust::windows::{
    Ace, Dacl, HIGH_INTEGRITY, Identity, LOW_INTEGRITY, MEDIUM_INTEGRITY, current_identity,
    current_user_sid, dacl, integrity_rid, is_elevated, label_integrity, owner_sid, parse_label,
};

/// What [`PipeSecurity`] grants its one user (`GA`, generic all), as a pipe's DACL reads it
/// back: `FILE_ALL_ACCESS` (`FA` in SDDL).
pub const PIPE_FULL_ACCESS: u32 = pitcrew_trust::windows::FILE_ALL_ACCESS;

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
    pitcrew_trust::windows::token_identity(&token)
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
    pitcrew_trust::windows::token_user_sid(&token)
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

/// A security descriptor that makes the current user a pipe's owner and its only grantee, at
/// the current integrity level: ptyd's policy, on `pitcrew_trust`'s descriptor.
#[derive(Debug)]
pub struct PipeSecurity(SecurityDescriptor);

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
        SecurityDescriptor::from_sddl(&Self::sddl(&identity.user, identity.integrity)).map(Self)
    }

    /// The descriptor, in SDDL, for a user's SID and an integrity level.
    pub fn sddl(sid: &str, integrity: u32) -> String {
        let label = pitcrew_trust::sddl::label(integrity);
        format!("O:{sid}D:P(A;;GA;;;{sid})S:(ML;;NWNR;;;{label})")
    }

    /// Creates a pipe instance with this descriptor.
    ///
    /// # Errors
    ///
    /// As [`ServerOptions::create`]: with `first_pipe_instance`, `PermissionDenied` when the
    /// name is taken.
    pub fn create(&self, options: &ServerOptions, name: &str) -> io::Result<NamedPipeServer> {
        self.0.create_pipe(options, name)
    }
}

/// An exclusive lock on a whole file (`LockFileEx`), held until it is dropped, and then released
/// (`UnlockFileEx`) before the file is closed: Windows releases a lock left to the close only
/// "depending on available system resources". The lock belongs to this handle, so another
/// handle, in this process or another, cannot take it meanwhile.
#[derive(Debug)]
pub struct FileLock(File);

impl FileLock {
    /// Opens `path` (creating it if missing) and locks it, without waiting: `None` if another
    /// handle holds a lock on it.
    ///
    /// # Errors
    ///
    /// If the file cannot be opened, or Windows fails the lock for another reason.
    pub fn try_exclusive(path: &Path) -> io::Result<Option<Self>> {
        // Opened here, so the handle is synchronous (std never asks for overlapped I/O).
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        let mut overlapped = OVERLAPPED::default();
        // SAFETY: the handle is open for the call, and synchronous, so the call is over when it
        // returns and only reads `overlapped` (a live local: offset 0) during it. The range is
        // the whole file.
        let locked = unsafe {
            LockFileEx(
                file.as_raw_handle(),
                LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY,
                0,
                u32::MAX,
                u32::MAX,
                &mut overlapped,
            )
        };
        if locked != 0 {
            return Ok(Some(Self(file)));
        }
        let e = io::Error::last_os_error();
        let held = [ERROR_LOCK_VIOLATION, ERROR_IO_PENDING]
            .iter()
            .any(|&code| e.raw_os_error() == i32::try_from(code).ok());
        if held { Ok(None) } else { Err(e) }
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        let mut overlapped = OVERLAPPED::default();
        // SAFETY: as in `try_exclusive`: an open, synchronous handle, a live local, and the
        // range that was locked.
        unsafe {
            UnlockFileEx(
                self.0.as_raw_handle(),
                0,
                u32::MAX,
                u32::MAX,
                &mut overlapped,
            );
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
                    entries: vec![Ace::Allow {
                        sid: sid.clone(),
                        mask: PIPE_FULL_ACCESS,
                    }],
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

    /// ptyd's descriptor: ours alone, labelled at the level asked for. (Labels themselves, and
    /// reading a DACL back, are `pitcrew_trust`'s tests.)
    #[test]
    fn the_descriptor_is_ours_alone_at_a_level() {
        assert_eq!(
            PipeSecurity::sddl("S-1-5-21-1", MEDIUM_INTEGRITY),
            "O:S-1-5-21-1D:P(A;;GA;;;S-1-5-21-1)S:(ML;;NWNR;;;ME)"
        );
        assert_eq!(
            PipeSecurity::sddl("S-1-5-21-1", HIGH_INTEGRITY),
            "O:S-1-5-21-1D:P(A;;GA;;;S-1-5-21-1)S:(ML;;NWNR;;;HI)"
        );
        assert!(PipeSecurity::sddl("S-1-5-21-1", 0x2100).ends_with("S-1-16-8448)"));
        assert_eq!(
            parse_label(&PipeSecurity::sddl("S-1-5-21-1", LOW_INTEGRITY)),
            Some(LOW_INTEGRITY)
        );
    }

    /// One lock per file: a second handle, here in this same process, is refused until the
    /// first lets go; the file is made if missing and kept.
    #[test]
    fn a_file_lock_has_one_holder() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("ptyd-0123456789abcdef.lock");
        let first = FileLock::try_exclusive(&path).expect("lock").expect("free");
        assert!(path.is_file());
        assert!(FileLock::try_exclusive(&path).expect("try").is_none());
        drop(first);
        let again = FileLock::try_exclusive(&path).expect("lock").expect("free");
        assert!(FileLock::try_exclusive(&path).expect("try").is_none());
        drop(again);
        assert!(path.is_file(), "the file stays");
        // A folder that does not exist is an error, not a lock.
        assert!(FileLock::try_exclusive(&tmp.path().join("missing").join("x.lock")).is_err());
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
