//! XZ data is never executable until its decoded bytes have passed the manifest check.
use super::HelperRef;
use pitcrew_remote::{Helper, HelperError, helper::MAX_HELPER_SIZE};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read as _, Write as _};
use std::path::{Path, PathBuf};

fn invalid(error: impl std::fmt::Display) -> HelperError {
    HelperError::InvalidArgument(format!("helper cache: {error}"))
}

fn open_regular(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(
            (rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK).bits() as i32,
        );
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        options.custom_flags(0x0020_0000); // FILE_FLAG_OPEN_REPARSE_POINT
    }
    let file = options.open(path)?;
    let meta = file.metadata()?;
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt as _;
        if meta.file_attributes() & 0x400 != 0 {
            return Err(io::Error::other("reparse point refused"));
        }
    }
    if !meta.is_file() {
        return Err(io::Error::other("not a regular file"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if meta.nlink() != 1 {
            return Err(io::Error::other("hard link refused"));
        }
    }
    Ok(file)
}

pub(super) fn read_verified(found: &HelperRef, path: &Path) -> Result<Helper, HelperError> {
    let mut bytes = Vec::new();
    open_regular(path)
        .map_err(invalid)?
        .take(MAX_HELPER_SIZE as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(invalid)?;
    Helper::new(found.platform, &found.version, &found.sha256, bytes)
}

fn private_dir(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    let made = {
        use std::os::unix::fs::DirBuilderExt as _;
        fs::DirBuilder::new().mode(0o700).create(path)
    };
    #[cfg(windows)]
    let made = pitcrew_trust::windows::create_private_directory(path);
    #[cfg(not(any(unix, windows)))]
    let made = fs::create_dir(path);
    match made {
        Ok(()) => (),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => (),
        Err(e) => return Err(e),
    }
    let meta = fs::symlink_metadata(path)?;
    if !meta.is_dir() || meta.file_type().is_symlink() {
        return Err(io::Error::other("cache directory is not a real directory"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if meta.uid() != rustix::process::geteuid().as_raw() || meta.mode() & 0o077 != 0 {
            return Err(io::Error::other(
                "cache directory must be owned by us and mode 0700",
            ));
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt as _;
        if meta.file_attributes() & 0x400 != 0 {
            return Err(io::Error::other("cache reparse point refused"));
        }
        pitcrew_trust::windows::check_private_object(path)?;
    }
    Ok(())
}

fn cache_root() -> io::Result<PathBuf> {
    #[cfg(unix)]
    let user = rustix::process::geteuid().as_raw().to_string();
    #[cfg(windows)]
    let user = pitcrew_trust::windows::current_user_sid()?;
    #[cfg(not(any(unix, windows)))]
    let user = "user";
    let parent = fs::canonicalize(std::env::temp_dir())?;
    // A shared temporary directory is safe only when sticky; no writable ancestor may
    // let another user replace the private cache directory.
    #[cfg(unix)]
    for ancestor in parent.ancestors() {
        use std::os::unix::fs::MetadataExt as _;
        let meta = fs::metadata(ancestor)?;
        if (meta.uid() != 0 && meta.uid() != rustix::process::geteuid().as_raw())
            || (meta.mode() & 0o022 != 0 && meta.mode() & 0o1000 == 0)
        {
            return Err(io::Error::other("unsafe temporary directory ancestor"));
        }
    }
    Ok(parent.join(format!("pitcrew-helper-cache-{user}")))
}

pub(super) fn load(found: &HelperRef) -> Result<Helper, HelperError> {
    load_at(
        found,
        &cache_root().map_err(invalid)?,
        MAX_HELPER_SIZE as u64,
    )
}

fn load_at(found: &HelperRef, root: &Path, limit: u64) -> Result<Helper, HelperError> {
    private_dir(root).map_err(invalid)?;
    pitcrew_remote::helper::validate_version(&found.version)?;
    if found.sha256.len() != 64 || !found.sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(invalid("invalid manifest hash"));
    }
    let destination = root.join(format!(
        "{}-{}-{}",
        found.platform.artefact(),
        found.version,
        found.sha256
    ));
    if fs::symlink_metadata(&destination).is_ok() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;
            let meta = fs::symlink_metadata(&destination).map_err(invalid)?;
            if meta.uid() != rustix::process::geteuid().as_raw() || meta.mode() & 0o077 != 0 {
                return Err(invalid("cached file must be owned by us and mode 0600"));
            }
        }
        #[cfg(windows)]
        pitcrew_trust::windows::check_private_object(&destination).map_err(invalid)?;
        return read_verified(found, &destination);
    }
    let mut random = [0_u8; 16];
    getrandom::fill(&mut random).map_err(invalid)?;
    let suffix: String = random.iter().map(|b| format!("{b:02x}")).collect();
    let temporary = root.join(format!(".decode-{suffix}"));
    #[cfg(windows)]
    let mut output = pitcrew_trust::windows::create_private_file(&temporary).map_err(invalid)?;
    #[cfg(not(windows))]
    let mut output = {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        options.open(&temporary).map_err(invalid)?
    };
    let result = (|| {
        let input = open_regular(&found.path).map_err(invalid)?;
        let stream =
            xz2::stream::Stream::new_stream_decoder(64 * 1024 * 1024, 0).map_err(invalid)?;
        let mut decoder = xz2::read::XzDecoder::new_stream(input, stream).take(limit + 1);
        let size = io::copy(&mut decoder, &mut output).map_err(invalid)?;
        if size > limit {
            return Err(invalid("decoded helper exceeds size limit"));
        }
        output.flush().map_err(invalid)?;
        output.sync_all().map_err(invalid)?;
        let helper = read_verified(found, &temporary)?;
        drop(output); // Windows does not allow renaming our secured file while open.
        fs::rename(&temporary, &destination).map_err(invalid)?;
        Ok(helper)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use pitcrew_remote::Platform;
    use sha2::{Digest as _, Sha256};

    fn resource(dir: &Path, version: &str, bytes: &[u8]) -> HelperRef {
        let path = dir.join("helper.xz");
        let mut encoded = xz2::write::XzEncoder::new(File::create(&path).unwrap(), 6);
        encoded.write_all(bytes).unwrap();
        encoded.finish().unwrap();
        HelperRef {
            platform: Platform::LinuxAarch64,
            version: version.into(),
            sha256: Sha256::digest(bytes)
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect(),
            path,
        }
    }

    fn entries(root: &Path) -> Vec<PathBuf> {
        fs::read_dir(root)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect()
    }

    #[test]
    fn decoded_hash_checked_before_atomic_install_and_on_every_cache_read() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("cache");
        let mut found = resource(tmp.path(), "1.2.3", b"decoded executable");
        found.sha256 = "0".repeat(64);
        assert!(matches!(
            load_at(&found, &root, 100),
            Err(HelperError::LocalHashMismatch)
        ));
        assert!(
            entries(&root).is_empty(),
            "no failed decode or temporary file survives"
        );
        let found = resource(tmp.path(), "1.2.3", b"decoded executable");
        assert_eq!(
            load_at(&found, &root, 100).unwrap().bytes(),
            b"decoded executable"
        );
        let cached = entries(&root).pop().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;
            assert_eq!(fs::metadata(&cached).unwrap().mode() & 0o777, 0o600);
        }
        #[cfg(windows)]
        pitcrew_trust::windows::check_private_object(&cached).unwrap();
        fs::write(cached, b"tampered").unwrap();
        assert!(matches!(
            load_at(&found, &root, 100),
            Err(HelperError::LocalHashMismatch)
        ));
    }

    #[test]
    fn corrupt_and_oversized_decodes_fail_closed() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("cache");
        let found = resource(tmp.path(), "1.2.3", &[0; 1024]);
        assert!(
            load_at(&found, &root, 100)
                .unwrap_err()
                .to_string()
                .contains("size limit")
        );
        assert!(entries(&root).is_empty());
        fs::write(&found.path, b"corrupt XZ").unwrap();
        assert!(load_at(&found, &root, 2048).is_err());
        assert!(entries(&root).is_empty());
    }

    #[test]
    fn upgrade_invalidates_cache_even_when_hash_is_unchanged() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("cache");
        let old = resource(tmp.path(), "1.2.3", b"same executable");
        load_at(&old, &root, 100).unwrap();
        let mut new = old.clone();
        new.version = "1.2.4".into();
        fs::write(&new.path, b"broken upgraded resource").unwrap();
        assert!(
            load_at(&new, &root, 100).is_err(),
            "must not reuse the previous version"
        );
        let new = resource(tmp.path(), "1.2.4", b"same executable");
        load_at(&new, &root, 100).unwrap();
        assert_eq!(entries(&root).len(), 2);
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_cache_directory_and_entry_are_refused_without_touching_target() {
        use std::os::unix::fs::symlink;
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("cache");
        let found = resource(tmp.path(), "1.2.3", b"executable");
        let target = tmp.path().join("target");
        fs::create_dir(&target).unwrap();
        symlink(&target, &root).unwrap();
        assert!(load_at(&found, &root, 100).is_err());
        assert!(entries(&target).is_empty());
        fs::remove_file(&root).unwrap();
        load_at(&found, &root, 100).unwrap();
        let cached = entries(&root).pop().unwrap();
        fs::remove_file(&cached).unwrap();
        let victim = target.join("victim");
        fs::write(&victim, b"executable").unwrap();
        symlink(&victim, &cached).unwrap();
        assert!(load_at(&found, &root, 100).is_err());
        assert_eq!(fs::read(&victim).unwrap(), b"executable");
    }

    #[cfg(windows)]
    #[test]
    fn junction_cache_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("target");
        fs::create_dir(&target).unwrap();
        let root = tmp.path().join("cache");
        let status = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&root)
            .arg(&target)
            .status()
            .unwrap();
        assert!(status.success());
        let found = resource(tmp.path(), "1.2.3", b"executable");
        assert!(load_at(&found, &root, 100).is_err());
        assert!(entries(&target).is_empty());
    }
}
