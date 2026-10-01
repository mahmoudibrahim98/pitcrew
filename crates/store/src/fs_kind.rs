//! Detects whether a directory sits on local or network-attached storage, so [`Store::open`]
//! (crate::store) can choose WAL (local) or the NFS-safe journal mode plus a single-host lease
//! (network). See [`FsMode`] to force the choice instead of detecting it.
//!
//! - **Linux:** `statfs`'s `f_type` magic number, mapped to a lowercase type name.
//! - **macOS:** `statfs`'s `f_fstypename`, used directly.
//! - **Windows:** a UNC path (`\\server\share`, `\\?\UNC\...`) is `Network`; a drive letter is
//!   `Local`. `std` cannot tell a mapped network drive from a local one without more than a UNC
//!   check (that needs `GetDriveTypeW`, which this crate's `#![forbid(unsafe_code)]` rules out);
//!   force [`FsMode::Network`] for those.
//! - **Other platforms:** always `Unknown`, so [`FsMode::Auto`] treats them as network.
//!
//! A magic number or name this module does not recognise becomes [`FsKind::Unknown`], which
//! [`FsMode::Auto`] treats the same as `Network`: guessing wrong toward "local" is the unsafe
//! direction (WAL on a filesystem that cannot support it corrupts the database), so an unknown
//! type is always routed through the safe, slower path.

use std::path::Path;

/// What kind of filesystem holds a directory, from [`detect`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum FsKind {
    /// A local disk: WAL is safe.
    Local,
    /// A network filesystem, by its reported type name (e.g. `nfs4`, `cifs`).
    Network {
        /// The type name, lowercase.
        name: String,
    },
    /// Detection did not recognise the type: an unmapped magic number, an error reading it, or a
    /// platform this module does not detect on. Handled exactly like `Network` (see the module
    /// docs), but kept distinct so callers can report what happened.
    Unknown {
        /// What is known, for diagnosis (a hex magic number, or the error).
        name: String,
    },
}

impl FsKind {
    /// Whether the NFS-safe journal mode and lease should be used for this kind. True for both
    /// `Network` and `Unknown`.
    #[must_use]
    pub fn is_network(&self) -> bool {
        !matches!(self, FsKind::Local)
    }
}

/// How a [`Store`](crate::Store) decides between WAL and the NFS-safe mode.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum FsMode {
    /// Detect the directory's filesystem with [`detect`] and choose accordingly (the default).
    #[default]
    Auto,
    /// Always use WAL, whatever [`detect`] would say. For local disks this module does not
    /// recognise, or tests.
    Local,
    /// Always use the NFS-safe journal mode and the single-host lease, whatever [`detect`] would
    /// say. For network filesystems this module does not recognise, or tests.
    Network,
}

/// Detects the filesystem of `dir` (the store's parent directory). See the [module docs](self)
/// for the method per platform. Never panics; a detection error becomes [`FsKind::Unknown`].
#[must_use]
pub fn detect(dir: &Path) -> FsKind {
    imp::detect(dir)
}

/// Type names (lowercase) known to be local disks, or local-like (`tmpfs`, `overlay`): safe for
/// WAL. `ext2`/`ext3`/`ext4` share one Linux magic number, so all three are listed even though the
/// magic table below can only ever produce one of them.
#[cfg_attr(windows, allow(dead_code))]
const LOCAL_NAMES: &[&str] = &[
    "ext2",
    "ext3",
    "ext4",
    "xfs",
    "btrfs",
    "zfs",
    "tmpfs",
    "f2fs",
    "bcachefs",
    "overlay",
    "overlayfs",
    "apfs",
    "hfs",
    "hfsplus",
    "exfat",
    "msdos",
    "vfat",
    "ntfs",
    "ufs",
];

/// Type names (lowercase), named by the brief, that are always network, whatever prefix rule
/// below would already catch.
#[cfg_attr(windows, allow(dead_code))]
const NETWORK_NAMES: &[&str] = &[
    "nfs", "nfs3", "nfs4", "cifs", "smb", "smb2", "smb3", "smbfs", "lustre", "gpfs", "beegfs",
    "9p", "virtiofs", "gfs2", "ocfs2", "vboxsf",
];

/// A name starting with any of these (after lowercasing) is network: catches `fuse`, `fuseblk`
/// and any FUSE-backed remote mount (sshfs, rclone, s3fs, ...), and stray `nfs*`/`smb*` variants
/// the exact list above missed.
#[cfg_attr(windows, allow(dead_code))]
const NETWORK_PREFIXES: &[&str] = &["fuse", "nfs", "smb"];

/// Classifies a type name (from a magic number or `f_fstypename`) as local, network or unknown.
#[cfg_attr(windows, allow(dead_code))]
fn classify(raw_name: &str) -> FsKind {
    let lower = raw_name.to_ascii_lowercase();
    if NETWORK_NAMES.contains(&lower.as_str())
        || NETWORK_PREFIXES.iter().any(|p| lower.starts_with(p))
    {
        FsKind::Network {
            name: raw_name.to_owned(),
        }
    } else if LOCAL_NAMES.contains(&lower.as_str()) {
        FsKind::Local
    } else {
        FsKind::Unknown {
            name: raw_name.to_owned(),
        }
    }
}

/// Maps a Linux `statfs` `f_type` magic number to a lowercase type name. Not exhaustive: a
/// magic number this does not know becomes `None`, and [`classify`]'s caller turns that into
/// [`FsKind::Unknown`] (still treated as network by [`FsMode::Auto`]), never a false `Local`.
///
/// Values are from `linux/magic.h` and, for a few filesystems the kernel header does not list,
/// their well-known magic numbers. `ext2`, `ext3` and `ext4` share `0xEF53`; `virtiofs` uses the
/// FUSE magic number, since it speaks the FUSE wire protocol.
#[cfg(target_os = "linux")]
fn magic_name(magic: i64) -> Option<&'static str> {
    Some(match magic {
        0xEF53 => "ext4",
        0x5846_5342 => "xfs",
        0x9123_683E => "btrfs",
        0x0102_1994 => "tmpfs",
        0xF2F5_2010 => "f2fs",
        0x2FC1_2FC1 => "zfs",
        0xCA45_1A4E => "bcachefs",
        0x794C_7630 => "overlay",
        0x6969 => "nfs",
        0x517B => "smb",
        0xFF53_4D42 => "cifs",
        0xFE53_4D42 => "smb2",
        0x6573_5546 => "fuse",
        0x0BD0_0BD0 => "lustre",
        0x0116_1970 => "gfs2",
        0x7461_636F => "ocfs2",
        0x786F_4256 => "vboxsf",
        0x0102_1997 => "9p",
        _ => return None,
    })
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod imp {
    use super::{FsKind, classify};
    use std::path::Path;

    #[cfg(target_os = "linux")]
    pub(super) fn detect(dir: &Path) -> FsKind {
        match rustix::fs::statfs(dir) {
            // `f_type` is `__fsword_t`, already `i64` on the 64-bit Linux targets this crate
            // builds for.
            Ok(st) => {
                let magic = st.f_type;
                super::magic_name(magic).map_or_else(
                    || FsKind::Unknown {
                        name: format!("0x{magic:x}"),
                    },
                    classify,
                )
            }
            Err(e) => FsKind::Unknown {
                name: format!("statfs failed: {e}"),
            },
        }
    }

    #[cfg(target_os = "macos")]
    pub(super) fn detect(dir: &Path) -> FsKind {
        match rustix::fs::statfs(dir) {
            Ok(st) => classify(&cstr_array_to_string(&st.f_fstypename)),
            Err(e) => FsKind::Unknown {
                name: format!("statfs failed: {e}"),
            },
        }
    }

    /// Converts a NUL-terminated (or full) `c_char` array to a `String`, without unsafe: each
    /// byte is reinterpreted with `as`, not transmuted.
    #[cfg(target_os = "macos")]
    fn cstr_array_to_string(buf: &[std::ffi::c_char]) -> String {
        let bytes: Vec<u8> = buf
            .iter()
            .take_while(|&&c| c != 0)
            .map(|&c| c as u8)
            .collect();
        String::from_utf8_lossy(&bytes).into_owned()
    }
}

#[cfg(windows)]
mod imp {
    use super::FsKind;
    use std::path::{Component, Path, Prefix};

    /// A UNC path (`\\server\share` or `\\?\UNC\server\share`) is network; anything else
    /// (including a drive letter, which may itself be a mapped network drive `std` cannot tell
    /// apart from a local one) defaults to `Local`. Callers who know better force [`FsMode`](
    /// super::FsMode)`::Network`.
    pub(super) fn detect(dir: &Path) -> FsKind {
        let canon = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
        for component in canon.components() {
            if let Component::Prefix(p) = component {
                return match p.kind() {
                    Prefix::UNC(_, _) | Prefix::VerbatimUNC(_, _) => FsKind::Network {
                        name: "unc".to_owned(),
                    },
                    _ => FsKind::Local,
                };
            }
        }
        FsKind::Local
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
mod imp {
    use super::FsKind;
    use std::path::Path;

    pub(super) fn detect(dir: &Path) -> FsKind {
        let _ = dir;
        FsKind::Unknown {
            name: "undetected platform".to_owned(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_names_classify_as_local() {
        for name in [
            "ext4",
            "xfs",
            "btrfs",
            "zfs",
            "tmpfs",
            "f2fs",
            "bcachefs",
            "overlay",
            "overlayfs",
            "apfs",
            "hfs",
            "EXT4",
            "Btrfs",
        ] {
            assert_eq!(classify(name), FsKind::Local, "{name}");
        }
    }

    #[test]
    fn network_names_and_prefixes_classify_as_network() {
        for name in [
            "nfs",
            "nfs4",
            "cifs",
            "smb",
            "smb2",
            "smbfs",
            "lustre",
            "gpfs",
            "beegfs",
            "9p",
            "virtiofs",
            "gfs2",
            "ocfs2",
            "vboxsf",
            "fuse",
            "fuseblk",
            "fuse.sshfs",
            "NFS4",
        ] {
            assert!(
                matches!(classify(name), FsKind::Network { .. }),
                "{name} should be network"
            );
        }
    }

    #[test]
    fn unrecognised_names_are_unknown() {
        assert!(matches!(classify("qnx6"), FsKind::Unknown { .. }));
        assert!(matches!(classify("UNKNOWN"), FsKind::Unknown { .. }));
    }

    #[test]
    fn unknown_and_network_both_count_as_network_for_auto() {
        assert!(!FsKind::Local.is_network());
        assert!(FsKind::Network { name: "nfs".into() }.is_network());
        assert!(
            FsKind::Unknown {
                name: "0xdeadbeef".into()
            }
            .is_network()
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn magic_numbers_map_to_names() {
        assert_eq!(magic_name(0xEF53), Some("ext4"));
        assert_eq!(magic_name(0x6969), Some("nfs"));
        assert_eq!(magic_name(0x517B), Some("smb"));
        assert_eq!(magic_name(0xFF53_4D42), Some("cifs"));
        assert_eq!(magic_name(0x6573_5546), Some("fuse"));
        assert_eq!(magic_name(0x0BAD_F00D), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn detect_on_this_machines_temp_dir_is_local() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(
            detect(dir.path()),
            FsKind::Local,
            "{:?}",
            detect(dir.path())
        );
    }
}
