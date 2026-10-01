//! Windows: a Job Object that holds ssh and everything it starts (askpass programs, `ProxyJump`
//! hops, `Match exec` commands).
//!
//! Terminating the job ends them all at once; that is how a cancel, a timeout or a dropped call
//! stops ssh. The job is created with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, so when its last
//! handle closes (the call ends, or PitCrew exits or crashes and the OS closes its handles)
//! Windows ends them too. No ssh child can outlive the call and send empty credentials to a
//! jump host.
//!
//! **Unsafe code.** This is the only module of the crate that uses it: the workspace denies
//! `unsafe_code`, and this module alone allows it, because the four Win32 calls below have no
//! safe binding in the dependency tree. Each call has a `SAFETY` comment. The job handle lives
//! in an `OwnedHandle`, so it is closed exactly once.

#![allow(unsafe_code)]

use std::io;
use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle, RawHandle};
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
    SetInformationJobObject, TerminateJobObject,
};

/// A job that kills its processes when told to, and when its handle closes.
#[derive(Debug)]
pub(crate) struct Job(OwnedHandle);

impl Job {
    /// A new, empty job.
    pub(crate) fn new() -> io::Result<Self> {
        // SAFETY: both arguments may be null (default security, no name). The result is
        // checked before use.
        let raw = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if raw.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `raw` is a valid job handle that we just created and nothing else owns.
        let job = Self(unsafe { OwnedHandle::from_raw_handle(raw) });
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let size = u32::try_from(std::mem::size_of_val(&limits))
            .map_err(|_| io::Error::other("job limits do not fit a u32"))?;
        // SAFETY: the handle is a live job handle; the pointer and size describe `limits`,
        // which is the structure this information class expects and outlives the call.
        let ok = unsafe {
            SetInformationJobObject(
                job.raw(),
                JobObjectExtendedLimitInformation,
                std::ptr::from_ref(&limits).cast(),
                size,
            )
        };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(job)
    }

    /// Puts a process in the job. Its later children join it too.
    ///
    /// `process` must be the handle of a live child process that the caller owns (tokio's
    /// `Child::raw_handle`, which is `None` once the child is reaped).
    pub(crate) fn assign(&self, process: RawHandle) -> io::Result<()> {
        // SAFETY: the job handle is live; `process` is a live process handle by this
        // function's contract. The call only reads both.
        let ok = unsafe { AssignProcessToJobObject(self.raw(), process) };
        if ok == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// Ends every process in the job.
    pub(crate) fn terminate(&self) {
        // SAFETY: the job handle is live. Terminating an empty or finished job is harmless.
        unsafe {
            TerminateJobObject(self.raw(), 1);
        }
    }

    fn raw(&self) -> windows_sys::Win32::Foundation::HANDLE {
        self.0.as_raw_handle()
    }
}
