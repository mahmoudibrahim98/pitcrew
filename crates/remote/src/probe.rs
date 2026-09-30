//! What a machine is: one ssh call running a small POSIX-sh script.
//!
//! The script is a single line with no `!`, so it survives a csh or tcsh login shell on its way
//! to `sh -c`. Its report sits between two markers, so banners that login scripts print are
//! ignored.

use crate::{Ssh, SshError};
use pitcrew_protocol::model::{MachineInfo, Scheduler};

const BEGIN: &str = "@@pitcrew-probe-begin";
const END: &str = "@@pitcrew-probe-end";

/// The probe script.
pub const SCRIPT: &str = concat!(
    "echo @@pitcrew-probe-begin; ",
    r#"printf 'os=%s\n' "$(uname -s 2>/dev/null)"; "#,
    r#"printf 'arch=%s\n' "$(uname -m 2>/dev/null)"; "#,
    r#"printf 'hostname=%s\n' "$(uname -n 2>/dev/null)"; "#,
    r#"printf 'home=%s\n' "$HOME"; "#,
    r#"if command -v tmux >/dev/null 2>&1; then echo tmux_found=1; printf 'tmux=%s\n' "$(tmux -V 2>/dev/null)"; else echo tmux_found=0; fi; "#,
    "if command -v sbatch >/dev/null 2>&1; then echo sbatch=1; else echo sbatch=0; fi; ",
    "if command -v squeue >/dev/null 2>&1; then echo squeue=1; else echo squeue=0; fi; ",
    // GNU stat, then GNU df, then the BSD/macOS route: the device from df, its type from mount.
    r#"fs=$(stat -f -c %T "$HOME" 2>/dev/null); "#,
    r#"if [ -z "$fs" ]; then fs=$(df -PT "$HOME" 2>/dev/null | awk 'NR==2 {print $2}'); fi; "#,
    r#"if [ -z "$fs" ]; then dev=$(df -P "$HOME" 2>/dev/null | awk 'NR==2 {print $1}'); "#,
    r#"fs=$(mount 2>/dev/null | awk -v d="$dev" '$1 == d' | sed -n 's/.*(\([^,)]*\).*/\1/p' | head -n 1); fi; "#,
    r#"printf 'fs=%s\n' "$fs"; "#,
    "echo @@pitcrew-probe-end",
);

/// What the probe found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Probe {
    /// The facts shared with the rest of PitCrew.
    pub info: MachineInfo,
    /// `$HOME` on the machine.
    pub home: Option<String>,
    /// `tmux -V`, e.g. `3.3a`, when tmux exists and answered.
    pub tmux_version: Option<String>,
    /// Whether `sbatch` is on `PATH`.
    pub has_sbatch: bool,
    /// Whether `squeue` is on `PATH`.
    pub has_squeue: bool,
    /// The filesystem type of `$HOME`, e.g. `ext2/ext3`, `nfs`, `lustre`, `apfs`.
    pub home_fs: Option<String>,
}

impl Ssh {
    /// Probes `host`. `host` also stands in for the host name if the machine reports none.
    ///
    /// # Errors
    /// The ssh call fails, or the output has no probe report.
    pub async fn probe(&self, host: &str) -> Result<Probe, SshError> {
        let output = self.run(host, &["sh", "-c", SCRIPT]).await?;
        parse(&output.stdout_text(), host)
    }
}

/// Parses the script's output. Unknown keys and junk lines are ignored; a missing end marker
/// is tolerated.
///
/// # Errors
/// [`SshError::UnexpectedOutput`] if there is no begin marker.
pub fn parse(stdout: &str, fallback_hostname: &str) -> Result<Probe, SshError> {
    let mut lines = stdout.lines().map(|l| l.trim_end_matches('\r'));
    if !lines.any(|l| l.trim() == BEGIN) {
        let head: String = stdout.chars().take(200).collect();
        return Err(SshError::UnexpectedOutput(format!(
            "no probe report in {head:?}"
        )));
    }
    let mut values = std::collections::HashMap::new();
    for line in lines.take_while(|l| l.trim() != END) {
        if let Some((key, value)) = line.split_once('=') {
            values.insert(key.trim(), value.trim());
        }
    }
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
        home_on_network_fs: home_fs.as_deref().is_some_and(is_network_fs),
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

/// Filesystems where files live on another machine: shared, slower, and not safe for SQLite
/// locking (ADR-0004).
#[must_use]
pub fn is_network_fs(fs: &str) -> bool {
    let fs = fs.to_ascii_lowercase();
    let fs = fs.strip_prefix("fuse.").unwrap_or(&fs);
    matches!(
        fs,
        "nfs"
            | "nfs4"
            | "smbfs"
            | "smb"
            | "smb2"
            | "smb3"
            | "cifs"
            | "afs"
            | "lustre"
            | "gpfs"
            | "beegfs"
            | "ceph"
            | "cephfs"
            | "glusterfs"
            | "sshfs"
            | "panfs"
            | "wekafs"
            | "webdav"
            | "davfs"
    ) || fs.contains("0x19830326") // BeeGFS, as older coreutils print it.
        || fs.contains("0x47504653") // GPFS.
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn junk_without_markers_is_an_error() {
        assert!(parse("Welcome!\n", "h").is_err());
    }

    #[test]
    fn missing_values_default() {
        let p = parse("@@pitcrew-probe-begin\nweird line\n=\nos=\n", "alias").unwrap();
        assert_eq!(p.info.hostname, "alias");
        assert_eq!(p.info.os, "unknown");
        assert_eq!(p.info.arch, "unknown");
        assert!(!p.info.has_tmux);
        assert_eq!(p.info.scheduler, None);
        assert!(!p.info.home_on_network_fs);
    }

    #[test]
    fn network_filesystems() {
        for fs in [
            "nfs",
            "NFS4",
            "lustre",
            "gpfs",
            "fuse.sshfs",
            "UNKNOWN (0x19830326)",
        ] {
            assert!(is_network_fs(fs), "{fs}");
        }
        for fs in ["ext2/ext3", "xfs", "apfs", "btrfs", "tmpfs", "zfs"] {
            assert!(!is_network_fs(fs), "{fs}");
        }
    }

    #[test]
    fn the_script_is_one_line_without_bangs() {
        assert!(!SCRIPT.contains('\n'));
        assert!(!SCRIPT.contains('!'));
        assert!(SCRIPT.starts_with(&format!("echo {BEGIN};")));
        assert!(SCRIPT.ends_with(&format!("echo {END}")));
    }
}
