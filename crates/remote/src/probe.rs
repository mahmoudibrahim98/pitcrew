//! What a machine is: one ssh call running a small POSIX-sh script.
//!
//! The report sits between two markers that carry a random tag for each call, so neither a
//! login banner nor a value the machine reports can fake them. A report counts only when both
//! markers are there, every key appears once, and the script exits 0. The call is bounded by
//! [`PROBE_LIMITS`], so a hung `stat` on a dead NFS mount cannot block it forever.
//!
//! The probe also reads the login shell (`$SHELL`) and refuses hosts whose shell cannot carry
//! PitCrew's commands safely (xonsh; see [`crate::quote`]). Its own command is fixed text, so
//! running it through such a shell to find out is harmless.

use crate::{Limits, Ssh, SshError};
use pitcrew_protocol::model::{MachineInfo, Scheduler};
use std::time::Duration;

/// Bounds for [`Ssh::probe`]: 1 MiB of output, and 30 seconds not counting time spent on
/// prompts.
pub const PROBE_LIMITS: Limits = Limits {
    max_output: Some(1024 * 1024),
    timeout: Some(Duration::from_secs(30)),
};

/// The probe script. It runs as `sh -c SCRIPT sh <tag>`.
pub const SCRIPT: &str = concat!(
    r#"printf '@@pitcrew-probe-begin-%s\n' "$1"; "#,
    r#"printf 'os=%s\n' "$(uname -s 2>/dev/null)"; "#,
    r#"printf 'arch=%s\n' "$(uname -m 2>/dev/null)"; "#,
    r#"printf 'hostname=%s\n' "$(uname -n 2>/dev/null)"; "#,
    r#"printf 'home=%s\n' "$HOME"; "#,
    r#"printf 'shell=%s\n' "$SHELL"; "#,
    r#"if command -v tmux >/dev/null 2>&1; then echo tmux_found=1; printf 'tmux=%s\n' "$(tmux -V 2>/dev/null)"; else echo tmux_found=0; fi; "#,
    "if command -v sbatch >/dev/null 2>&1; then echo sbatch=1; else echo sbatch=0; fi; ",
    "if command -v squeue >/dev/null 2>&1; then echo squeue=1; else echo squeue=0; fi; ",
    // GNU stat, then GNU df, then the BSD/macOS route: the device from df, its type from mount.
    r#"fs=$(stat -f -c %T "$HOME" 2>/dev/null); "#,
    r#"if [ -z "$fs" ]; then fs=$(df -PT "$HOME" 2>/dev/null | awk 'NR==2 {print $2}'); fi; "#,
    r#"if [ -z "$fs" ]; then dev=$(df -P "$HOME" 2>/dev/null | awk 'NR==2 {print $1}'); "#,
    r#"fs=$(mount 2>/dev/null | awk -v d="$dev" '$1 == d' | sed -n 's/.*(\([^,)]*\).*/\1/p' | head -n 1); fi; "#,
    r#"printf 'fs=%s\n' "$fs"; "#,
    r#"printf '@@pitcrew-probe-end-%s\n' "$1""#,
);

/// What the probe found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Probe {
    /// The facts shared with the rest of PitCrew. `home_on_network_fs` is conservative: it is
    /// true when the filesystem type is unknown (`home_fs` is `None`), since treating a network
    /// filesystem as local is the mistake that costs data.
    pub info: MachineInfo,
    /// `$HOME` on the machine.
    pub home: Option<String>,
    /// `tmux -V`, e.g. `3.3a`, when tmux exists and answered.
    pub tmux_version: Option<String>,
    /// Whether `sbatch` is on `PATH`.
    pub has_sbatch: bool,
    /// Whether `squeue` is on `PATH`.
    pub has_squeue: bool,
    /// The filesystem type of `$HOME`, e.g. `ext2/ext3`, `nfs`, `lustre`, `apfs`; `None` when
    /// the machine could not tell.
    pub home_fs: Option<String>,
    /// The login shell (`$SHELL`), when set.
    pub login_shell: Option<String>,
}

/// Login shells that may not keep single-quoted text literal, so PitCrew's commands could be
/// changed on the way to `/bin/sh` (see [`crate::quote`]).
const UNSAFE_SHELLS: [&str; 1] = ["xonsh"];

/// Whether `shell` (a path or a name) is one PitCrew refuses.
#[must_use]
pub fn is_unsafe_shell(shell: &str) -> bool {
    let name = shell.rsplit(['/', '\\']).next().unwrap_or(shell);
    let name = name.strip_suffix(".exe").unwrap_or(name);
    UNSAFE_SHELLS
        .iter()
        .any(|bad| name == *bad || name.starts_with(&format!("{bad}-")))
}

impl Ssh {
    /// Probes `host` within [`PROBE_LIMITS`]. `host` also stands in for the host name if the
    /// machine reports none.
    ///
    /// # Errors
    /// The ssh call fails or breaks the limits, the script exits non-zero, the report is
    /// missing, cut off or malformed, or the login shell is one PitCrew cannot use safely
    /// ([`SshError::UnsupportedShell`]).
    pub async fn probe(&self, host: &str) -> Result<Probe, SshError> {
        self.probe_with(host, PROBE_LIMITS).await
    }

    /// [`Ssh::probe`] with other limits.
    ///
    /// # Errors
    /// As [`Ssh::probe`].
    pub async fn probe_with(&self, host: &str, limits: Limits) -> Result<Probe, SshError> {
        let tag = crate::askpass::random::<8>().map_err(SshError::Setup)?;
        let tag = crate::askpass::to_hex(&tag);
        let output = self
            .run_limited(host, &["sh", "-c", SCRIPT, "sh", &tag], limits)
            .await?;
        if !output.success() {
            return Err(SshError::UnexpectedOutput(format!(
                "the probe exited with {:?}: {}",
                output.code,
                crate::ssh::last_line(&String::from_utf8_lossy(&output.stderr))
            )));
        }
        let probe = parse(&output.stdout_text(), &tag, host)?;
        if let Some(shell) = probe.login_shell.as_deref().filter(|s| is_unsafe_shell(s)) {
            return Err(SshError::UnsupportedShell(shell.to_owned()));
        }
        Ok(probe)
    }
}

/// Parses the script's output for the call tagged `tag`. Lines before the begin marker (login
/// banners) and after the end marker are ignored, as are unknown keys and lines without `=`.
///
/// # Errors
/// [`SshError::UnexpectedOutput`] if a marker is missing or a key appears twice (a value with
/// a newline in it, such as a strange `$HOME`, must not stand in for a later key).
pub fn parse(stdout: &str, tag: &str, fallback_hostname: &str) -> Result<Probe, SshError> {
    let values = crate::report::parse(stdout, "probe", tag).map_err(SshError::UnexpectedOutput)?;
    let get = |key: &str| values.get(key).copied().filter(|v| !v.is_empty());
    let flag = |key: &str| get(key) == Some("1");

    let has_sbatch = flag("sbatch");
    let has_squeue = flag("squeue");
    let home_fs = get("fs").map(str::to_owned);
    let info = MachineInfo {
        hostname: get("hostname").unwrap_or(fallback_hostname).to_owned(),
        os: normalize_os(get("os")),
        arch: normalize_arch(get("arch")),
        has_tmux: flag("tmux_found"),
        scheduler: (has_sbatch && has_squeue).then_some(Scheduler::Slurm),
        home_on_network_fs: !home_fs.as_deref().is_some_and(is_local_fs),
    };
    Ok(Probe {
        info,
        home: get("home").map(str::to_owned),
        tmux_version: get("tmux")
            .map(|v| v.strip_prefix("tmux ").unwrap_or(v).to_owned())
            .filter(|_| flag("tmux_found")),
        has_sbatch,
        has_squeue,
        home_fs,
        login_shell: get("shell").map(str::to_owned),
    })
}

fn normalize_os(uname: Option<&str>) -> String {
    let Some(uname) = uname else {
        return "unknown".to_owned();
    };
    let lower = uname.to_ascii_lowercase();
    match lower.as_str() {
        "linux" => "linux".to_owned(),
        "darwin" => "macos".to_owned(),
        s if s.starts_with("cygwin") || s.starts_with("mingw") || s.starts_with("msys") => {
            "windows".to_owned()
        }
        _ => lower,
    }
}

fn normalize_arch(uname: Option<&str>) -> String {
    match uname {
        None => "unknown".to_owned(),
        Some("x86_64" | "amd64") => "x86_64".to_owned(),
        Some("aarch64" | "arm64") => "aarch64".to_owned(),
        Some(other) => other.to_owned(),
    }
}

/// Filesystems known to keep files on this machine's own disks (or memory), as `stat -f -c
/// %T`, `df -T` or `mount` name them. It is an allowlist: everything else counts as possibly
/// networked (shared, slower, and not safe for SQLite locking; ADR-0004), including what `stat`
/// cannot name (`UNKNOWN (0x…)`), every FUSE filesystem (`fuseblk` covers sshfs, glusterfs,
/// DAOS dfuse and s3fs alike), and VM or cluster filesystems such as `9p`, `virtiofs`,
/// `vboxsf`, `gfs2` and `ocfs2`.
const LOCAL_FS: [&str; 29] = [
    "ext2/ext3", // GNU stat, for ext2, ext3 and ext4.
    "ext2",
    "ext3",
    "ext4",
    "xfs",
    "btrfs",
    "zfs",
    "tmpfs",
    "ramfs",
    "f2fs",
    "bcachefs",
    "overlay",
    "overlayfs",
    "apfs",
    "hfs",
    "hfsplus",
    "ufs",
    "ffs",
    "jfs",
    "reiserfs",
    "nilfs2",
    "hammer",
    "hammer2",
    "exfat",
    "vfat",
    "msdos",
    "ntfs3",
    "squashfs",
    "erofs",
];

/// Whether `fs` is on the allowlist of filesystems known to be local (ext2/3/4, xfs, btrfs,
/// zfs, tmpfs, f2fs, bcachefs, overlayfs, apfs, hfs and similar). Anything else may be
/// networked: unknown types, FUSE, 9p, virtiofs, vboxsf, cluster filesystems.
#[must_use]
pub fn is_local_fs(fs: &str) -> bool {
    let fs = fs.trim().to_ascii_lowercase();
    LOCAL_FS.contains(&fs.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    const TAG: &str = "0123456789abcdef";

    fn report(body: &str) -> String {
        format!("@@pitcrew-probe-begin-{TAG}\n{body}@@pitcrew-probe-end-{TAG}\n")
    }

    #[test]
    fn junk_without_markers_is_an_error() {
        assert!(parse("Welcome!\n", TAG, "h").is_err());
    }

    #[test]
    fn a_cut_off_report_is_an_error() {
        let cut = format!("@@pitcrew-probe-begin-{TAG}\nos=Linux\narch=x86_64\n");
        let err = parse(&cut, TAG, "h").unwrap_err();
        assert!(err.to_string().contains("cut off"), "{err}");
    }

    #[test]
    fn markers_from_another_call_do_not_count() {
        let other = "@@pitcrew-probe-begin-ffff\nos=Linux\n@@pitcrew-probe-end-ffff\n";
        assert!(parse(other, TAG, "h").is_err());
        // An end marker without this call's tag does not end the report.
        let fake_end = format!(
            "@@pitcrew-probe-begin-{TAG}\nhome=/x\n@@pitcrew-probe-end-\nfs=nfs\n\
             @@pitcrew-probe-end-{TAG}\n"
        );
        assert_eq!(
            parse(&fake_end, TAG, "h").unwrap().home_fs.as_deref(),
            Some("nfs")
        );
    }

    #[test]
    fn a_repeated_key_is_an_error() {
        // A $HOME with a newline must not pass for the fs line.
        let text = report("home=/x\nfs=ext4\nfs=nfs\n");
        let err = parse(&text, TAG, "h").unwrap_err();
        assert!(err.to_string().contains("twice"), "{err}");
    }

    #[test]
    fn missing_values_default() {
        let p = parse(&report("weird line\n=\nos=\n"), TAG, "alias").unwrap();
        assert_eq!(p.info.hostname, "alias");
        assert_eq!(p.info.os, "unknown");
        assert_eq!(p.info.arch, "unknown");
        assert!(!p.info.has_tmux);
        assert_eq!(p.info.scheduler, None);
        // Unknown filesystem: assume it may be networked.
        assert_eq!(p.home_fs, None);
        assert!(p.info.home_on_network_fs);
    }

    #[test]
    fn known_local_filesystems() {
        for fs in [
            "ext2/ext3",
            "ext4",
            "EXT4",
            "xfs",
            "btrfs",
            "zfs",
            "tmpfs",
            "f2fs",
            "bcachefs",
            "overlayfs",
            "overlay",
            "apfs",
            "hfs",
            " xfs ",
        ] {
            assert!(is_local_fs(fs), "{fs:?}");
        }
    }

    /// Everything not on the allowlist may be networked, named or not.
    #[test]
    fn everything_else_may_be_networked() {
        for fs in [
            "nfs",
            "NFS4",
            "lustre",
            "gpfs",
            "cifs",
            "UNKNOWN (0x19830326)",
            "UNKNOWN (0x5346414f)",
            "fuseblk",
            "fuse",
            "fuse.sshfs",
            "fuse.glusterfs",
            "fuse.dfuse",
            "fuse.s3fs",
            "9p",
            "v9fs",
            "virtiofs",
            "gfs2",
            "ocfs2",
            "vboxsf",
            "",
            "ext4 nfs",
        ] {
            assert!(!is_local_fs(fs), "{fs:?}");
            let p = parse(&report(&format!("fs={fs}\n")), TAG, "h").unwrap();
            assert!(p.info.home_on_network_fs, "{fs:?}");
        }
        let p = parse(&report("fs=xfs\n"), TAG, "h").unwrap();
        assert!(!p.info.home_on_network_fs);
    }

    #[test]
    fn unsafe_login_shells() {
        for shell in [
            "xonsh",
            "/usr/bin/xonsh",
            "/opt/bin/xonsh-0.14",
            "xonsh.exe",
        ] {
            assert!(is_unsafe_shell(shell), "{shell}");
        }
        for shell in [
            "/bin/bash",
            "/usr/bin/fish",
            "/bin/tcsh",
            "/bin/zsh",
            "",
            "xonshy",
        ] {
            assert!(!is_unsafe_shell(shell), "{shell}");
        }
        let p = parse(&report("shell=/usr/bin/xonsh\n"), TAG, "h").unwrap();
        assert_eq!(p.login_shell.as_deref(), Some("/usr/bin/xonsh"));
    }

    #[test]
    fn the_script_brackets_its_report() {
        assert!(SCRIPT.starts_with(r#"printf '@@pitcrew-probe-begin-%s\n' "$1";"#));
        assert!(SCRIPT.ends_with(r#"printf '@@pitcrew-probe-end-%s\n' "$1""#));
    }
}
