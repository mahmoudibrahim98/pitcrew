//! Deploying the helper: [`deploy`].
//!
//! At most two ssh calls, each running `helper.sh` under the deploy lock:
//! 1. `check` verifies a copy already installed under the version (its sha256, computed on the
//!    machine, against the expected one; then `--version`) and switches `current` to it. A
//!    re-run with the same version and hash ends here: it only verifies. A damaged copy is
//!    removed, and so is replaced in step 2. `check` also finds a missing hash tool, an unsafe
//!    directory or a busy lock before anything is uploaded.
//! 2. `install` streams the helper over ssh's stdin into `bin/<version>/pitcrewd.tmp.<random>`
//!    (`umask 077`, so the file is 0600 and its directories 0700 from the start: never
//!    readable by others), then checks its size, its sha256 and `--version` (deleting it if any
//!    fails), `chmod 700`s it, renames it into place, switches `current` and removes old
//!    versions.
//!
//! An upload that is cut off never lands in place: the file stays a temporary one, removed by
//! the script on the way out, or by the next deploy if the script itself was killed.

use super::script::{self, Call, Report};
use super::{HelperError, Target};
use pitcrew_protocol::model::MachineInfo;
use sha2::{Digest as _, Sha256};
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

/// The largest helper accepted: 256 MiB. A static `pitcrewd` is a few tens of MiB.
pub const MAX_HELPER_SIZE: usize = 256 * 1024 * 1024;

/// A platform PitCrew builds the helper for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Platform {
    /// Linux on x86_64: a static musl build.
    LinuxX86_64,
    /// Linux on aarch64: a static musl build.
    LinuxAarch64,
    /// macOS on either architecture: a universal build.
    MacOs,
}

impl Platform {
    /// Every platform.
    pub const ALL: [Self; 3] = [Self::LinuxX86_64, Self::LinuxAarch64, Self::MacOs];

    /// The platform of a probed machine (`uname -s` and `uname -m`, normalised by the probe).
    ///
    /// # Errors
    /// [`HelperError::UnsupportedPlatform`] for anything else: other operating systems, 32-bit
    /// ARM, POWER, RISC-V, an unknown answer.
    pub fn detect(info: &MachineInfo) -> Result<Self, HelperError> {
        match (info.os.as_str(), info.arch.as_str()) {
            ("linux", "x86_64") => Ok(Self::LinuxX86_64),
            ("linux", "aarch64") => Ok(Self::LinuxAarch64),
            ("macos", "x86_64" | "aarch64") => Ok(Self::MacOs),
            (os, arch) => Err(HelperError::UnsupportedPlatform {
                os: script::clean(os),
                arch: script::clean(arch),
            }),
        }
    }

    /// The build target, as the release names it (stream P): e.g.
    /// `x86_64-unknown-linux-musl`, `universal-apple-darwin`.
    #[must_use]
    pub fn target(self) -> &'static str {
        match self {
            Self::LinuxX86_64 => "x86_64-unknown-linux-musl",
            Self::LinuxAarch64 => "aarch64-unknown-linux-musl",
            Self::MacOs => "universal-apple-darwin",
        }
    }

    /// The release artefact to deploy: `pitcrewd-<target>`, as listed in `SHA256SUMS`.
    #[must_use]
    pub fn artefact(self) -> &'static str {
        match self {
            Self::LinuxX86_64 => "pitcrewd-x86_64-unknown-linux-musl",
            Self::LinuxAarch64 => "pitcrewd-aarch64-unknown-linux-musl",
            Self::MacOs => "pitcrewd-universal-apple-darwin",
        }
    }
}

impl fmt::Display for Platform {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::LinuxX86_64 => "Linux on x86_64",
            Self::LinuxAarch64 => "Linux on aarch64",
            Self::MacOs => "macOS",
        })
    }
}

/// Checks a helper version before it names a directory on the machine: 1 to 64 characters of
/// `0-9 A-Z a-z . _ + -`, starting with a digit (so it is never `current`, `previous` or a
/// hidden name), e.g. `1.4.0` or `1.5.0-rc.1+build.7`.
///
/// # Errors
/// [`HelperError::InvalidArgument`] naming the problem.
pub fn validate_version(version: &str) -> Result<(), HelperError> {
    let ok = (1..=64).contains(&version.len())
        && version.starts_with(|c: char| c.is_ascii_digit())
        && version
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '+' | '-'));
    if ok {
        Ok(())
    } else {
        Err(HelperError::InvalidArgument(format!(
            "helper version {:?}: 1 to 64 characters of 0-9 A-Z a-z . _ + -, starting with a digit",
            script::clean(version)
        )))
    }
}

/// The helper to deploy: its bytes, version, platform and the sha256 the desktop has compiled
/// in. Cheap to clone.
#[derive(Clone)]
pub struct Helper {
    platform: Platform,
    version: String,
    sha256: String,
    bytes: Arc<[u8]>,
}

impl Helper {
    /// Checks the helper before anything is sent: the version (see [`validate_version`]), the
    /// hash (64 hex digits), the size (1 byte to [`MAX_HELPER_SIZE`]), and that `bytes` hash to
    /// `sha256`.
    ///
    /// # Errors
    /// [`HelperError::InvalidArgument`] or [`HelperError::LocalHashMismatch`].
    pub fn new(
        platform: Platform,
        version: &str,
        sha256: &str,
        bytes: impl Into<Arc<[u8]>>,
    ) -> Result<Self, HelperError> {
        validate_version(version)?;
        if sha256.len() != 64 || !sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(HelperError::InvalidArgument(
                "the expected sha256 must be 64 hex digits".to_owned(),
            ));
        }
        let bytes = bytes.into();
        if bytes.is_empty() || bytes.len() > MAX_HELPER_SIZE {
            return Err(HelperError::InvalidArgument(format!(
                "the helper is {} bytes; it must be 1 to {MAX_HELPER_SIZE}",
                bytes.len()
            )));
        }
        let sha256 = sha256.to_ascii_lowercase();
        if crate::askpass::to_hex(&Sha256::digest(&bytes)) != sha256 {
            return Err(HelperError::LocalHashMismatch);
        }
        Ok(Self {
            platform,
            version: version.to_owned(),
            sha256,
            bytes,
        })
    }

    /// What it is built for.
    #[must_use]
    pub fn platform(&self) -> Platform {
        self.platform
    }

    /// Its version, as `pitcrewd --version` prints it.
    #[must_use]
    pub fn version(&self) -> &str {
        &self.version
    }

    /// Its sha256, lower-case hex.
    #[must_use]
    pub fn sha256(&self) -> &str {
        &self.sha256
    }

    /// Its bytes.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Its size in bytes.
    #[must_use]
    pub fn len(&self) -> u64 {
        u64::try_from(self.bytes.len()).unwrap_or(u64::MAX)
    }

    /// Never true: [`Helper::new`] refuses empty helpers.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }
}

impl fmt::Debug for Helper {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Helper")
            .field("platform", &self.platform)
            .field("version", &self.version)
            .field("sha256", &self.sha256)
            .field("len", &self.bytes.len())
            .finish()
    }
}

/// How far an upload got.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Progress {
    /// Bytes of the helper handed to ssh so far.
    pub sent: u64,
    /// The helper's size.
    pub total: u64,
}

/// Where a [`deploy`] has got to, for a live log (`DeployOptions::step`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum DeployStep {
    /// Looking for a copy of this version already there, and verifying it (sha256 computed on
    /// the machine, then `--version`).
    Checking,
    /// A copy was there and verified: nothing is uploaded.
    AlreadyInstalled,
    /// None was: the upload begins ([`Progress`] follows).
    Uploading,
    /// Every byte is sent: the machine checks the size, the sha256 and `--version`, then switches
    /// `current` to it.
    Verifying,
    /// Uploaded, verified and switched to.
    Installed,
}

/// Bounds and callbacks for [`deploy`].
#[derive(Clone)]
pub struct DeployOptions {
    /// How long each of the two calls may take once it holds the lock: for the upload, the
    /// transfer and the checks. Time spent on prompts does not count. Default 15 minutes.
    pub timeout: Duration,
    /// How long to wait for another deploy's lock. Default 1 minute.
    pub lock_wait: Duration,
    /// When a lock counts as stale, however its holder looks (rounded up to whole minutes). It
    /// must be longer than `lock_wait` plus `timeout`, so a slow but live deploy is never
    /// broken. Default 30 minutes.
    pub stale_lock: Duration,
    /// Called as the upload goes.
    pub progress: Option<Arc<dyn Fn(Progress) + Send + Sync>>,
    /// Called at each [`DeployStep`], in order.
    pub step: Option<Arc<dyn Fn(DeployStep) + Send + Sync>>,
}

impl Default for DeployOptions {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(15 * 60),
            lock_wait: Duration::from_secs(60),
            stale_lock: Duration::from_secs(30 * 60),
            progress: None,
            step: None,
        }
    }
}

impl fmt::Debug for DeployOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DeployOptions")
            .field("timeout", &self.timeout)
            .field("lock_wait", &self.lock_wait)
            .field("stale_lock", &self.stale_lock)
            .field("progress", &self.progress.is_some())
            .field("step", &self.step.is_some())
            .finish()
    }
}

/// Whole seconds, rounded up.
pub(crate) fn seconds(d: Duration) -> u64 {
    d.as_secs() + u64::from(d.subsec_nanos() > 0)
}

/// Whole minutes for `find -mmin`, rounded up, at least 1.
pub(crate) fn minutes(d: Duration) -> u64 {
    seconds(d).div_ceil(60).max(1)
}

impl DeployOptions {
    fn check(&self) -> Result<(), HelperError> {
        if self.timeout.is_zero() {
            return Err(HelperError::InvalidArgument(
                "the deploy timeout is zero".to_owned(),
            ));
        }
        if minutes(self.stale_lock) * 60 <= seconds(self.lock_wait) + seconds(self.timeout) {
            return Err(HelperError::InvalidArgument(
                "stale_lock must be longer than lock_wait plus timeout".to_owned(),
            ));
        }
        Ok(())
    }
}

/// The tool that computed the sha256 on the machine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum HashTool {
    /// `sha256sum`.
    Sha256sum,
    /// `shasum -a 256`.
    Shasum,
    /// `openssl dgst -sha256`.
    Openssl,
}

impl HashTool {
    fn from_report(name: Option<&str>) -> Option<Self> {
        match name? {
            "sha256sum" => Some(Self::Sha256sum),
            "shasum" => Some(Self::Shasum),
            "openssl" => Some(Self::Openssl),
            _ => None,
        }
    }
}

/// A deploy that succeeded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Deployed {
    /// The version `current` now points to.
    pub version: String,
    /// Its sha256 as computed on the machine (equal to the expected one).
    pub sha256: String,
    /// Where it is: `<root>/bin/<version>/pitcrewd`.
    pub path: String,
    /// Whether it was uploaded; false when a verified copy was already there.
    pub uploaded: bool,
    /// What computed the sha256.
    pub hash_tool: HashTool,
    /// The version kept as the previous one, if any.
    pub previous: Option<String>,
    /// Versions removed, keeping only the current and previous ones.
    pub removed: Vec<String>,
    /// Whether `current` was switched atomically. False only where `mv` has neither `-T` nor
    /// `-h` (busybox); the link was then replaced in place, missing for an instant.
    pub atomic: bool,
    /// The first line of `pitcrewd --version` on the machine.
    pub version_line: String,
}

/// Deploys `helper` to `target` (see the module docs). Idempotent: with the same version and
/// hash already installed, it only verifies, and switches `current` back to it if needed.
///
/// # Errors
/// [`HelperError::WrongPlatform`] and invalid options before any call; then whatever a call or
/// the machine reports (see [`HelperError`]).
pub async fn deploy(
    target: &Target,
    helper: &Helper,
    options: &DeployOptions,
) -> Result<Deployed, HelperError> {
    if helper.platform() != target.platform() {
        return Err(HelperError::WrongPlatform {
            built: helper.platform(),
            machine: target.platform(),
        });
    }
    options.check()?;
    let step = |at: DeployStep| {
        if let Some(step) = &options.step {
            step(at);
        }
    };
    let args = vec![
        helper.version().to_owned(),
        helper.sha256().to_owned(),
        helper.len().to_string(),
        seconds(options.lock_wait).to_string(),
        minutes(options.stale_lock).to_string(),
    ];
    let timeout = options.lock_wait.saturating_add(options.timeout);
    step(DeployStep::Checking);
    let check = script::run(
        target,
        Call {
            command: "check",
            args: args.clone(),
            payload: None,
            progress: None,
            timeout,
        },
    )
    .await?;
    match check.get("state") {
        Some("installed") => {
            let done = deployed(target, helper, &check)?;
            step(DeployStep::AlreadyInstalled);
            return Ok(done);
        }
        Some("absent") => {}
        other => {
            return Err(HelperError::UnexpectedOutput(format!(
                "the check reported state {other:?}"
            )));
        }
    }

    let total = helper.len();
    let skip = u64::try_from(script::SCRIPT.len()).unwrap_or(u64::MAX);
    let progress = options.progress.clone();
    let steps = options.step.clone();
    let verifying = std::sync::atomic::AtomicBool::new(false);
    let on_sent = move |sent: u64| {
        let sent = sent.saturating_sub(skip).min(total);
        if let Some(progress) = &progress {
            progress(Progress { sent, total });
        }
        // Once, when the last byte is handed over: the machine checks it all from here.
        if sent == total
            && !verifying.swap(true, std::sync::atomic::Ordering::Relaxed)
            && let Some(steps) = &steps
        {
            steps(DeployStep::Verifying);
        }
    };
    step(DeployStep::Uploading);
    let install = script::run(
        target,
        Call {
            command: "install",
            args,
            payload: Some(helper.bytes()),
            progress: Some(&on_sent),
            timeout,
        },
    )
    .await?;
    let done = deployed(target, helper, &install)?;
    step(DeployStep::Installed);
    Ok(done)
}

/// Checks a successful report against what was asked for.
fn deployed(target: &Target, helper: &Helper, report: &Report) -> Result<Deployed, HelperError> {
    let unexpected = |why: String| Err(HelperError::UnexpectedOutput(why));
    if report.get("state") != Some("installed") {
        return unexpected(format!("the deploy reported {:?}", report.get("state")));
    }
    let sha256 = report.get("sha256").unwrap_or("");
    if sha256 != helper.sha256() {
        return unexpected(format!("the machine reported sha256 {sha256:?}"));
    }
    if report.get("current") != Some(helper.version()) {
        return unexpected(format!(
            "current points to {:?}, not {}",
            report.get("current"),
            helper.version()
        ));
    }
    let version_line = report.get("version_line").unwrap_or("");
    if !version_line
        .split_whitespace()
        .any(|word| word == helper.version())
    {
        return unexpected(format!("the helper's version line is {version_line:?}"));
    }
    let Some(hash_tool) = HashTool::from_report(report.get("tool")) else {
        return unexpected(format!("unknown hash tool {:?}", report.get("tool")));
    };
    Ok(Deployed {
        version: helper.version().to_owned(),
        sha256: sha256.to_owned(),
        path: target.layout().binary(helper.version()),
        uploaded: report.get("uploaded") == Some("1"),
        hash_tool,
        previous: report.get("previous").map(str::to_owned),
        removed: report
            .get("removed")
            .unwrap_or("")
            .split_whitespace()
            .map(str::to_owned)
            .collect(),
        atomic: report.get("atomic") != Some("0"),
        version_line: version_line.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(os: &str, arch: &str) -> MachineInfo {
        MachineInfo {
            hostname: "box".into(),
            os: os.into(),
            arch: arch.into(),
            has_tmux: false,
            scheduler: None,
            home_on_network_fs: false,
        }
    }

    #[test]
    fn platforms_are_detected_or_refused() {
        let ok = [
            ("linux", "x86_64", Platform::LinuxX86_64),
            ("linux", "aarch64", Platform::LinuxAarch64),
            ("macos", "x86_64", Platform::MacOs),
            ("macos", "aarch64", Platform::MacOs),
        ];
        for (os, arch, want) in ok {
            assert_eq!(Platform::detect(&info(os, arch)).unwrap(), want);
        }
        for (os, arch) in [
            ("linux", "armv7l"),
            ("linux", "ppc64le"),
            ("linux", "riscv64"),
            ("linux", "i686"),
            ("freebsd", "x86_64"),
            ("windows", "x86_64"),
            ("unknown", "unknown"),
            ("linux", "x86\u{1b}[31m"),
        ] {
            let err = Platform::detect(&info(os, arch)).unwrap_err();
            assert!(
                matches!(&err, HelperError::UnsupportedPlatform { .. }),
                "{os}/{arch}"
            );
            assert!(!err.to_string().contains('\u{1b}'));
        }
        assert_eq!(
            Platform::ALL.map(Platform::artefact),
            [
                "pitcrewd-x86_64-unknown-linux-musl",
                "pitcrewd-aarch64-unknown-linux-musl",
                "pitcrewd-universal-apple-darwin",
            ]
        );
        for p in Platform::ALL {
            assert_eq!(p.artefact(), format!("pitcrewd-{}", p.target()));
        }
    }

    #[test]
    fn versions_are_checked() {
        for good in ["0.0.0", "1.2.3", "1.5.0-rc.1+build.7", "2024_10", "9"] {
            validate_version(good).unwrap();
        }
        for bad in [
            "",
            "v1.2.3",
            "current",
            ".1",
            "-1",
            "1/2",
            "1 2",
            "1\n2",
            "1;rm",
            "1$x",
            "1'",
            "1*",
            &"1".repeat(65),
        ] {
            assert!(validate_version(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn helpers_are_checked_before_sending() {
        let bytes = b"#!/bin/sh\necho pitcrewd 1.0.0\n".to_vec();
        let hash = crate::askpass::to_hex(&Sha256::digest(&bytes));
        let helper = Helper::new(Platform::LinuxX86_64, "1.0.0", &hash, bytes.clone()).unwrap();
        assert_eq!(helper.sha256(), hash);
        assert_eq!(helper.len(), u64::try_from(bytes.len()).unwrap());
        // Upper-case hex is accepted and normalised.
        let upper = hash.to_ascii_uppercase();
        let helper = Helper::new(Platform::LinuxX86_64, "1.0.0", &upper, bytes.clone()).unwrap();
        assert_eq!(helper.sha256(), hash);
        assert!(!format!("{helper:?}").contains("echo"));

        let mut other = hash.clone().into_bytes();
        other[0] = if other[0] == b'0' { b'1' } else { b'0' };
        let other = String::from_utf8(other).unwrap();
        assert!(matches!(
            Helper::new(Platform::LinuxX86_64, "1.0.0", &other, bytes.clone()),
            Err(HelperError::LocalHashMismatch)
        ));
        for bad_hash in ["", "abc", &"g".repeat(64), &format!("{hash}0")] {
            assert!(matches!(
                Helper::new(Platform::LinuxX86_64, "1.0.0", bad_hash, bytes.clone()),
                Err(HelperError::InvalidArgument(_))
            ));
        }
        let empty_hash = crate::askpass::to_hex(&Sha256::digest(b""));
        assert!(matches!(
            Helper::new(Platform::LinuxX86_64, "1.0.0", &empty_hash, Vec::new()),
            Err(HelperError::InvalidArgument(_))
        ));
        assert!(Helper::new(Platform::LinuxX86_64, "latest", &hash, bytes).is_err());
    }

    #[test]
    fn options_are_checked() {
        DeployOptions::default().check().unwrap();
        let tight = DeployOptions {
            stale_lock: Duration::from_secs(16 * 60),
            ..DeployOptions::default()
        };
        assert!(tight.check().is_err());
        let zero = DeployOptions {
            timeout: Duration::ZERO,
            ..DeployOptions::default()
        };
        assert!(zero.check().is_err());
        assert_eq!(minutes(Duration::from_secs(1)), 1);
        assert_eq!(minutes(Duration::from_secs(61)), 2);
        assert_eq!(seconds(Duration::from_millis(1500)), 2);
    }
}
