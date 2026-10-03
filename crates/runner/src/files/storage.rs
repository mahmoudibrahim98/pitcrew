//! Platform handles and private backup retention. No subprocess output is exposed.
use super::{Checked, FileError, Result, hash, link};
use std::fs::{self, File};
use std::io::Write as _;
use std::path::Path;

pub(super) fn open(path: &Path, directory: bool) -> Result<File> {
    open_at(path, directory, None)
}
pub(super) fn open_at(path: &Path, directory: bool, parent: Option<&File>) -> Result<File> {
    #[cfg(unix)]
    {
        use rustix::fs::{Mode, OFlags};
        let mut flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
        if directory {
            flags |= OFlags::DIRECTORY;
        }
        let fd = match parent {
            Some(parent) => rustix::fs::openat(
                parent,
                path.file_name().ok_or_else(FileError::invalid)?,
                flags,
                Mode::empty(),
            ),
            None => rustix::fs::open(path, flags, Mode::empty()),
        }
        .map_err(|_| FileError::forbidden())?;
        Ok(File::from(fd))
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        let _ = (directory, parent);
        Ok(fs::OpenOptions::new()
            .read(true)
            .share_mode(1 | 2)
            .custom_flags(0x0020_0000 | 0x0200_0000)
            .open(path)?)
    }
}
pub(super) fn same(file: &File, path: &Path) -> Result<bool> {
    let other = open(path, file.metadata()?.is_dir())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let a = file.metadata()?;
        let b = other.metadata()?;
        Ok(a.dev() == b.dev() && a.ino() == b.ino())
    }
    #[cfg(windows)]
    {
        let a = winapi_util::file::information(file)?;
        let b = winapi_util::file::information(&other)?;
        Ok(
            a.volume_serial_number() == b.volume_serial_number()
                && a.file_index() == b.file_index(),
        )
    }
}
pub(super) fn links(file: &File) -> Result<u64> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        Ok(file.metadata()?.nlink())
    }
    #[cfg(windows)]
    {
        Ok(winapi_util::file::information(file)?.number_of_links())
    }
}
pub(super) fn permissions(
    file: &File,
    source: &Path,
    target_file: &File,
    target: &Path,
) -> Result<()> {
    target_file.set_permissions(file.metadata()?.permissions())?;
    #[cfg(unix)]
    {
        let _ = (source, target);
    }
    #[cfg(windows)]
    pitcrew_trust::windows::copy_file_dacl(source, target)?;
    Ok(())
}

/// Unix creation and replacement are relative to the held parent, never a re-resolved path.
pub(super) struct Temporary {
    #[cfg(unix)]
    file: File,
    #[cfg(unix)]
    parent: File,
    #[cfg(unix)]
    name: String,
    #[cfg(unix)]
    path: std::path::PathBuf,
    #[cfg(windows)]
    temp: tempfile::NamedTempFile,
}
impl Temporary {
    pub(super) fn new(parent_path: &Path, parent: &File) -> Result<Self> {
        #[cfg(unix)]
        {
            use rustix::fs::{Mode, OFlags};
            let name = format!(".pitcrew-{}", ulid::Ulid::generate());
            let file = File::from(
                rustix::fs::openat(
                    parent,
                    name.as_str(),
                    OFlags::WRONLY
                        | OFlags::CREATE
                        | OFlags::EXCL
                        | OFlags::NOFOLLOW
                        | OFlags::CLOEXEC,
                    Mode::from_bits_truncate(0o600),
                )
                .map_err(std::io::Error::from)?,
            );
            Ok(Self {
                file,
                parent: parent.try_clone()?,
                path: parent_path.join(&name),
                name,
            })
        }
        #[cfg(windows)]
        {
            let _ = parent;
            let path = parent_path.join(format!(".pitcrew-{}", ulid::Ulid::generate()));
            let file = pitcrew_trust::windows::create_private_file(&path)?;
            let cleanup = tempfile::TempPath::try_from_path(path)?;
            if link(&file.metadata()?) || links(&file)? != 1 || !same(&file, &cleanup)? {
                return Err(FileError::forbidden());
            }
            private_check(&cleanup, false)?;
            Ok(Self {
                temp: tempfile::NamedTempFile::from_parts(file, cleanup),
            })
        }
    }
    pub(super) fn file(&self) -> &File {
        #[cfg(unix)]
        {
            &self.file
        }
        #[cfg(windows)]
        {
            self.temp.as_file()
        }
    }
    pub(super) fn file_mut(&mut self) -> &mut File {
        #[cfg(unix)]
        {
            &mut self.file
        }
        #[cfg(windows)]
        {
            self.temp.as_file_mut()
        }
    }
    pub(super) fn path(&self) -> &Path {
        #[cfg(unix)]
        {
            &self.path
        }
        #[cfg(windows)]
        {
            self.temp.path()
        }
    }
    pub(super) fn make_private(&self) -> Result<()> {
        #[cfg(windows)]
        private_file(self.path())?;
        Ok(())
    }
    pub(super) fn replace(self, target: &Path, new: bool) -> Result<()> {
        #[cfg(unix)]
        {
            let name = target.file_name().ok_or_else(FileError::invalid)?;
            if new {
                rustix::fs::linkat(
                    &self.parent,
                    self.name.as_str(),
                    &self.parent,
                    name,
                    rustix::fs::AtFlags::empty(),
                )
                .map_err(|e| {
                    if e == rustix::io::Errno::EXIST {
                        FileError::conflict(None)
                    } else {
                        std::io::Error::from(e).into()
                    }
                })?;
            } else {
                rustix::fs::renameat(&self.parent, self.name.as_str(), &self.parent, name)
                    .map_err(std::io::Error::from)?;
            }
            self.parent.sync_all()?;
        }
        #[cfg(windows)]
        {
            let (file, temp) = self.temp.into_parts();
            drop(file);
            if new {
                temp.persist_noclobber(target)
            } else {
                temp.persist(target)
            }
            .map_err(|e| {
                if e.error.kind() == std::io::ErrorKind::AlreadyExists {
                    FileError::conflict(None)
                } else {
                    e.error.into()
                }
            })?;
        }
        Ok(())
    }
}
#[cfg(unix)]
impl Drop for Temporary {
    fn drop(&mut self) {
        let _ = rustix::fs::unlinkat(
            &self.parent,
            self.name.as_str(),
            rustix::fs::AtFlags::empty(),
        );
    }
}
#[cfg(windows)]
pub(super) fn private_file(path: &Path) -> Result<()> {
    pitcrew_trust::windows::set_private_file(path)?;
    Ok(())
}
fn private_dir(path: &Path, parent: &File) -> Result<()> {
    #[cfg(unix)]
    {
        match rustix::fs::mkdirat(
            parent,
            path.file_name().ok_or_else(FileError::forbidden)?,
            rustix::fs::Mode::from_bits_truncate(0o700),
        ) {
            Ok(()) => (),
            Err(rustix::io::Errno::EXIST) => (),
            Err(e) => return Err(std::io::Error::from(e).into()),
        }
        private_check(path, true)?;
    }
    #[cfg(windows)]
    {
        let _ = parent;
        match pitcrew_trust::windows::create_private_directory(path) {
            Ok(()) => (),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => (),
            Err(e) => return Err(e.into()),
        }
        private_check(path, true)?;
    }
    Ok(())
}
fn private_check(path: &Path, directory: bool) -> Result<()> {
    let meta = fs::symlink_metadata(path)?;
    if link(&meta) || directory != meta.is_dir() || !directory && !meta.is_file() {
        return Err(FileError::forbidden());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if meta.uid() != rustix::process::geteuid().as_raw()
            || meta.mode() & 0o777 != if directory { 0o700 } else { 0o600 }
        {
            return Err(FileError::forbidden());
        }
    }
    #[cfg(windows)]
    pitcrew_trust::windows::check_private_object(path)?;
    Ok(())
}

/// Flat hash-keyed private files. Never trust names or contents left by another process.
pub(super) fn backup(directory: &Path, root: &Path, relative: &str, bytes: &[u8]) -> Result<()> {
    let state = directory.parent().ok_or_else(FileError::forbidden)?;
    let state_held = Checked::walk(state, "", false)?;
    private_dir(directory, state_held.last())?;
    let held = Checked::walk(state, "file-backups", false)?;
    let identity = format!("{}\0{relative}", root.canonicalize()?.to_string_lossy());
    #[cfg(windows)]
    let identity = identity.to_lowercase();
    let key = hash(identity.as_bytes());
    let mut existing = vec![];
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| FileError::forbidden())?;
        let (prefix, suffix) = name.split_once('-').ok_or_else(FileError::forbidden)?;
        if prefix.len() != 64
            || !prefix
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(FileError::forbidden());
        }
        let id = suffix
            .parse::<ulid::Ulid>()
            .map_err(|_| FileError::forbidden())?;
        private_check(&entry.path(), false)?;
        let file = open(&entry.path(), false)?;
        if links(&file)? != 1 || file.metadata()?.len() > super::MAX_FILE_BYTES {
            return Err(FileError::forbidden());
        }
        existing.push((id, prefix.to_owned(), entry.path(), file.metadata()?.len()));
    }
    existing.sort();
    // Strictly newer than every stored backup, even within one clock tick or after a restart.
    let mut next = ulid::Ulid::generate();
    if let Some((last, _, _, _)) = existing.last().filter(|entry| next <= entry.0) {
        next = ulid::Ulid::from(
            u128::from(*last)
                .checked_add(1)
                .ok_or_else(FileError::forbidden)?,
        );
    }
    let mut total: u64 = existing.iter().map(|e| e.3).sum();
    let mut count = existing.iter().filter(|e| e.1 == key).count();
    for (_, prefix, path, size) in existing {
        if total + bytes.len() as u64 > 64 * 1024 * 1024 || prefix == key && count >= 3 {
            held.verify()?;
            #[cfg(windows)]
            fs::remove_file(path)?;
            #[cfg(unix)]
            rustix::fs::unlinkat(
                held.last(),
                path.file_name().ok_or_else(FileError::forbidden)?,
                rustix::fs::AtFlags::empty(),
            )
            .map_err(std::io::Error::from)?;
            total -= size;
            if prefix == key {
                count -= 1;
            }
        }
    }
    held.verify()?;
    let path = directory.join(format!("{key}-{next}"));
    #[cfg(unix)]
    let mut file = {
        use rustix::fs::{Mode, OFlags};
        File::from(
            rustix::fs::openat(
                held.last(),
                path.file_name().ok_or_else(FileError::forbidden)?,
                OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::from_bits_truncate(0o600),
            )
            .map_err(std::io::Error::from)?,
        )
    };
    #[cfg(windows)]
    let mut file = pitcrew_trust::windows::create_private_file(&path)?;
    held.verify()?;
    file.write_all(bytes)?;
    file.sync_all()?;
    held.verify()?;
    Ok(())
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use crate::files::Files;
    use pitcrew_protocol::files::{FileEncoding, WriteFile};
    #[test]
    fn windows_private_acl_and_permissions_survive_replacement()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let tmp = tempfile::tempdir()?;
        let root = tmp.path().join("root");
        let state = tmp.path().join("state");
        fs::create_dir(&root)?;
        fs::create_dir(&state)?;
        let files = Files::new(&state);
        let first = files.write(
            &root,
            "file",
            WriteFile {
                revision: None,
                encoding: FileEncoding::Utf8,
                content: "old".into(),
            },
        )?;
        private_check(&root.join("file"), false)?;
        files.write(
            &root,
            "file",
            WriteFile {
                revision: Some(first.revision),
                encoding: FileEncoding::Utf8,
                content: "new".into(),
            },
        )?;
        private_check(&root.join("file"), false)?;
        private_check(&state.join("file-backups"), true)?;
        for e in fs::read_dir(state.join("file-backups"))? {
            private_check(&e?.path(), false)?;
        }
        Ok(())
    }
}
