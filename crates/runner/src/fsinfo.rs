//! File facts the watcher compares: size, mtime, identity; and network filesystem detection.

use pitcrew_protocol::model::TimestampMs;
use std::fs::Metadata;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// What a `stat` tells the watcher.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FileStat {
    pub size: u64,
    pub mtime: TimestampMs,
    /// Changes when the file is replaced by a new one (a new inode). Compare with [`same_file`].
    pub identity: Option<String>,
}

/// Whether two identities name the same file. On Unix only the inode counts: the device number
/// changes across reboots and remounts (NFS, btrfs), which is not a replacement. Unknown
/// identities match anything, leaving the size check to judge.
pub(crate) fn same_file(a: Option<&str>, b: Option<&str>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => inode_part(a) == inode_part(b),
        _ => true,
    }
}

/// `dev:ino` → `ino`; other forms are compared whole.
fn inode_part(identity: &str) -> &str {
    identity.rsplit(':').next().unwrap_or(identity)
}

pub(crate) fn stat(path: &Path) -> std::io::Result<FileStat> {
    let meta = std::fs::metadata(path)?;
    Ok(FileStat {
        size: meta.len(),
        mtime: meta.modified().map_or(0, millis),
        identity: identity(&meta),
    })
}

pub(crate) fn millis(t: SystemTime) -> TimestampMs {
    t.duration_since(UNIX_EPOCH).map_or(0, |d| {
        TimestampMs::try_from(d.as_millis()).unwrap_or(TimestampMs::MAX)
    })
}

/// `dev:ino`. The device is kept for diagnosis only; [`same_file`] ignores it.
#[cfg(unix)]
fn identity(meta: &Metadata) -> Option<String> {
    use std::os::unix::fs::MetadataExt;
    Some(format!("{}:{}", meta.dev(), meta.ino()))
}

/// Without inode numbers in stable std, the creation time stands in. It misses a replacement that
/// keeps the creation time (Windows file tunnelling); the size check still catches most of those.
#[cfg(not(unix))]
fn identity(meta: &Metadata) -> Option<String> {
    meta.created().ok().map(|t| format!("c{}", millis(t)))
}

/// Filesystem types where change notifications are missing or unreliable, so the watcher polls.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
const NETWORK_FS: &[&str] = &[
    "nfs",
    "nfs4",
    "cifs",
    "smb3",
    "smbfs",
    "9p",
    "drvfs",
    "virtiofs",
    "lustre",
    "gpfs",
    "beegfs",
    "ceph",
    "glusterfs",
    "afs",
    "panfs",
    "pvfs2",
    "orangefs",
    "davfs",
];

/// Whether `path` is on a network filesystem. Linux reads `/proc/self/mountinfo`; elsewhere this
/// returns false (force polling with [`crate::PollMode::Always`]).
pub(crate) fn is_network_fs(path: &Path) -> bool {
    #[cfg(target_os = "linux")]
    {
        let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        std::fs::read_to_string("/proc/self/mountinfo")
            .ok()
            .and_then(|info| fs_type(&info, &path.to_string_lossy()))
            .is_some_and(|t| is_network_type(&t))
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = path;
        false
    }
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn is_network_type(fs: &str) -> bool {
    let fs = fs.strip_prefix("fuse.").unwrap_or(fs);
    NETWORK_FS.contains(&fs) || matches!(fs, "sshfs" | "rclone" | "s3fs")
}

/// The filesystem type of the longest mount point containing `path`, from mountinfo text.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn fs_type(mountinfo: &str, path: &str) -> Option<String> {
    let mut best: Option<(usize, String)> = None;
    for line in mountinfo.lines() {
        let fields: Vec<&str> = line.split(' ').collect();
        let (Some(mount), Some(sep)) = (fields.get(4), fields.iter().position(|f| *f == "-"))
        else {
            continue;
        };
        let Some(fs) = fields.get(sep + 1) else {
            continue;
        };
        let mount = unescape(mount);
        let inside = path == mount
            || mount == "/"
            || path
                .strip_prefix(mount.as_str())
                .is_some_and(|rest| rest.starts_with('/'));
        if inside && best.as_ref().is_none_or(|(len, _)| mount.len() >= *len) {
            best = Some((mount.len(), (*fs).to_owned()));
        }
    }
    best.map(|(_, fs)| fs)
}

/// Mountinfo escapes space, tab, newline and backslash as `\ooo` octal.
fn unescape(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let octal = b
            .get(i + 1..i + 4)
            .filter(|d| b[i] == b'\\' && d.iter().all(|c| (b'0'..=b'7').contains(c)))
            .map(|d| d.iter().fold(0u32, |n, c| n * 8 + u32::from(c - b'0')));
        match octal.and_then(|n| u8::try_from(n).ok()) {
            Some(c) => {
                out.push(c);
                i += 4;
            }
            None => {
                out.push(b[i]);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    const INFO: &str = "\
22 1 8:1 / / rw,relatime shared:1 - ext4 /dev/sda1 rw
40 22 0:40 / /home rw,relatime shared:2 - nfs4 server:/home rw
41 40 0:41 / /home/lab\\040share rw - fuse.sshfs remote: rw
42 22 0:42 / /mnt/c rw,noatime - 9p drvfs rw
43 22 0:43 / /homework rw - ext4 /dev/sdb1 rw";

    #[test]
    fn picks_the_longest_mount_point() {
        assert_eq!(fs_type(INFO, "/var/tmp").as_deref(), Some("ext4"));
        assert_eq!(fs_type(INFO, "/home/sam/.claude").as_deref(), Some("nfs4"));
        assert_eq!(fs_type(INFO, "/home").as_deref(), Some("nfs4"));
        assert_eq!(fs_type(INFO, "/homework/x").as_deref(), Some("ext4"));
        assert_eq!(
            fs_type(INFO, "/home/lab share/x").as_deref(),
            Some("fuse.sshfs")
        );
        assert_eq!(fs_type(INFO, "/mnt/c/Users").as_deref(), Some("9p"));
    }

    #[test]
    fn network_types() {
        for t in ["nfs4", "cifs", "fuse.sshfs", "9p", "lustre"] {
            assert!(is_network_type(t), "{t}");
        }
        for t in ["ext4", "xfs", "btrfs", "tmpfs", "overlay", "fuse.portal"] {
            assert!(!is_network_type(t), "{t}");
        }
    }

    #[test]
    fn stat_sees_growth_and_replacement() {
        let dir = tempfile::tempdir().expect("tempdir");
        let p = dir.path().join("t.jsonl");
        std::fs::write(&p, b"a\n").expect("write");
        let a = stat(&p).expect("stat");
        std::fs::write(dir.path().join("new"), b"b\n").expect("write");
        std::fs::rename(dir.path().join("new"), &p).expect("rename");
        let b = stat(&p).expect("stat");
        assert_eq!(a.size, b.size);
        #[cfg(unix)]
        assert!(!same_file(a.identity.as_deref(), b.identity.as_deref()));
        assert!(same_file(a.identity.as_deref(), a.identity.as_deref()));
    }

    #[test]
    fn only_the_inode_decides_sameness() {
        // A reboot or remount changes the device number, not the file.
        assert!(same_file(Some("64769:42"), Some("2049:42")));
        assert!(!same_file(Some("64769:42"), Some("64769:43")));
        assert!(same_file(None, Some("1:2")));
        assert!(same_file(Some("c100"), Some("c100")));
        assert!(!same_file(Some("c100"), Some("c200")));
    }
}
