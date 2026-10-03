//! Bounded file access. Paths and I/O errors never enter error messages or logs.
mod storage;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use pitcrew_protocol::api::ErrorCode;
use pitcrew_protocol::files::{
    FileContent, FileEncoding, FileEntry, FileKind, FileList, MAX_ENTRIES, MAX_FILE_BYTES,
    WriteFile,
};
use sha2::{Digest, Sha256};
use std::fs::{self, File, Metadata};
use std::io::{self, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

/// A fixed reason and optional details safe to return to a client.
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct FileError {
    /// HTTP error code.
    pub code: ErrorCode,
    /// Fixed message, never an OS error.
    pub message: &'static str,
    /// Size for a too-large file.
    pub size: Option<u64>,
    /// Conflict's current revision (including an absent file).
    pub current_revision: Option<Option<String>>,
}
impl FileError {
    fn new(code: ErrorCode, message: &'static str) -> Self {
        Self {
            code,
            message,
            size: None,
            current_revision: None,
        }
    }
    fn forbidden() -> Self {
        Self::new(ErrorCode::Forbidden, "Unsafe file or directory.")
    }
    fn invalid() -> Self {
        Self::new(ErrorCode::Invalid, "Invalid relative path or content.")
    }
    fn conflict(revision: Option<String>) -> Self {
        Self {
            current_revision: Some(revision),
            ..Self::new(ErrorCode::Conflict, "File revision changed.")
        }
    }
    fn large(size: u64) -> Self {
        Self {
            size: Some(size),
            ..Self::new(ErrorCode::TooLarge, "File exceeds the size cap.")
        }
    }
}
impl From<io::Error> for FileError {
    fn from(e: io::Error) -> Self {
        match e.kind() {
            io::ErrorKind::NotFound => {
                Self::new(ErrorCode::NotFound, "File or directory not found.")
            }
            io::ErrorKind::PermissionDenied => Self::forbidden(),
            _ => Self::new(ErrorCode::Unavailable, "File operation unavailable."),
        }
    }
}
type Result<T> = std::result::Result<T, FileError>;

/// Validate before any filesystem access. Empty is allowed only for listing the root.
pub fn validate_path(path: &str, list: bool, write: bool) -> Result<()> {
    if path.is_empty() {
        return if list {
            Ok(())
        } else {
            Err(FileError::invalid())
        };
    }
    if path.contains(['\0', '\\']) || path.starts_with('/') {
        return Err(FileError::invalid());
    }
    for part in path.split('/') {
        if matches!(part, "" | "." | "..") {
            return Err(FileError::invalid());
        }
        #[cfg(windows)]
        if part.contains(':') || part.ends_with(['.', ' ']) || reserved(part) {
            return Err(FileError::invalid());
        }
        if write && (part == ".git" || cfg!(windows) && part.eq_ignore_ascii_case(".git")) {
            return Err(FileError::forbidden());
        }
    }
    Ok(())
}
#[cfg(windows)]
fn reserved(part: &str) -> bool {
    let name = part
        .split('.')
        .next()
        .unwrap_or("")
        .trim_end_matches(' ')
        .to_uppercase();
    matches!(
        name.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) || ["COM", "LPT"].iter().any(|prefix| {
        name.strip_prefix(prefix).is_some_and(|n| {
            matches!(
                n,
                "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
            )
        })
    })
}
fn link(meta: &Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt as _;
        meta.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        meta.file_type().is_symlink()
    }
}
fn hash(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
fn content(path: &str, bytes: Vec<u8>) -> FileContent {
    let revision = hash(&bytes);
    let size = bytes.len() as u64;
    let (encoding, content) = match String::from_utf8(bytes) {
        Ok(s) => (FileEncoding::Utf8, s),
        Err(e) => (FileEncoding::Base64, STANDARD.encode(e.as_bytes())),
    };
    let media_type = match Path::new(path).extension().and_then(|s| s.to_str()) {
        Some("txt" | "md" | "rs" | "ts" | "js" | "toml" | "yaml" | "yml") => "text/plain",
        Some("json") => "application/json",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("pdf") => "application/pdf",
        _ => "application/octet-stream",
    }
    .into();
    FileContent {
        size,
        media_type,
        revision,
        encoding,
        content,
    }
}
fn bytes(file: &mut File) -> Result<Vec<u8>> {
    let size = file.metadata()?.len();
    if size > MAX_FILE_BYTES {
        return Err(FileError::large(size));
    }
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(FileError::large(bytes.len() as u64));
    }
    Ok(bytes)
}

/// One daemon's file service. The lock serializes writes and backup retention across roots.
#[derive(Debug)]
pub struct Files {
    backups: PathBuf,
    writes: Mutex<()>,
}
impl Files {
    /// `state` is the daemon state directory, never supplied by the client.
    pub fn new(state: &Path) -> Self {
        Self {
            backups: state.join("file-backups"),
            writes: Mutex::new(()),
        }
    }
    /// List without following entry links.
    pub fn list(&self, root: &Path, path: &str) -> Result<FileList> {
        validate_path(path, true, false)?;
        let checked = Checked::walk(root, path, false)?;
        if !checked.last().metadata()?.is_dir() {
            return Err(FileError::invalid());
        }
        let mut entries = std::collections::BTreeMap::new();
        let mut truncated = false;
        for entry in fs::read_dir(&checked.path)? {
            let entry = entry?;
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            let meta = fs::symlink_metadata(entry.path())?;
            let kind = if link(&meta) {
                FileKind::Link
            } else if meta.is_dir() {
                FileKind::Folder
            } else if meta.is_file() {
                FileKind::File
            } else {
                continue;
            };
            let modified_at = meta.modified().ok().map(crate::fsinfo::millis);
            entries.insert(
                name.clone(),
                FileEntry {
                    name,
                    kind,
                    size: meta.len(),
                    modified_at,
                },
            );
            if entries.len() > MAX_ENTRIES {
                entries.pop_last();
                truncated = true;
            }
        }
        checked.verify()?;
        Ok(FileList {
            entries: entries.into_values().collect(),
            truncated,
        })
    }
    /// Read checked, regular bytes only.
    pub fn read(&self, root: &Path, path: &str) -> Result<FileContent> {
        validate_path(path, false, false)?;
        let mut checked = Checked::walk(root, path, false)?;
        if !checked.last().metadata()?.is_file() {
            return Err(FileError::forbidden());
        }
        let data = bytes(checked.last_mut())?;
        checked.verify()?;
        Ok(content(path, data))
    }
    /// Optimistic, private-backed atomic replacement; no directories are implicitly created.
    pub fn write(&self, root: &Path, path: &str, request: WriteFile) -> Result<FileContent> {
        validate_path(path, false, true)?;
        let data = match request.encoding {
            FileEncoding::Utf8 => request.content.into_bytes(),
            FileEncoding::Base64 => STANDARD
                .decode(request.content)
                .map_err(|_| FileError::invalid())?,
        };
        if data.len() as u64 > MAX_FILE_BYTES {
            return Err(FileError::large(data.len() as u64));
        }
        if request.revision.as_ref().is_some_and(|s| {
            s.len() != 64
                || !s
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        }) {
            return Err(FileError::invalid());
        }
        let _lock = self.writes.lock().unwrap_or_else(PoisonError::into_inner);
        let mut checked = Checked::walk(root, path, true)?;
        let resolved = if checked.exists {
            &checked.path
        } else {
            checked.path.parent().ok_or_else(FileError::invalid)?
        };
        if resolved.canonicalize()?.components().any(|component| {
            let name = component.as_os_str().to_string_lossy();
            name == ".git" || cfg!(windows) && name.eq_ignore_ascii_case(".git")
        }) {
            return Err(FileError::forbidden());
        }
        let old = if checked.exists {
            if !checked.last().metadata()?.is_file() || storage::links(checked.last())? > 1 {
                return Err(FileError::forbidden());
            }
            Some(bytes(checked.last_mut())?)
        } else {
            None
        };
        let revision = old.as_deref().map(hash);
        if request.revision != revision {
            return Err(FileError::conflict(revision));
        }
        let parent = checked.path.parent().ok_or_else(FileError::invalid)?;
        let parent_handle = if checked.exists {
            &checked.handles[checked.handles.len() - 2].1
        } else {
            checked.last()
        };
        let mut temp = storage::Temporary::new(parent, parent_handle)?;
        if checked.exists {
            storage::permissions(checked.last(), &checked.path, temp.file(), temp.path())?;
        } else {
            temp.make_private()?;
        }
        temp.file_mut().write_all(&data)?;
        temp.file().sync_all()?;
        checked.verify()?;
        if let Some(old) = old {
            storage::backup(&self.backups, root, path, &old)?;
            // Re-read before replacement, detecting a concurrent writer since the revision check.
            use std::io::{Seek as _, SeekFrom};
            checked.last_mut().seek(SeekFrom::Start(0))?;
            let now = hash(&bytes(checked.last_mut())?);
            if Some(&now) != request.revision.as_ref() {
                return Err(FileError::conflict(Some(now)));
            }
        }
        checked.verify()?;
        if checked.exists && storage::links(checked.last())? > 1 {
            return Err(FileError::forbidden());
        }
        // Windows must release the final read handle before atomic replacement. Ancestors remain held.
        if checked.exists {
            checked.handles.pop();
        }
        match temp.replace(&checked.path, request.revision.is_none()) {
            Err(error) if error.code == ErrorCode::Conflict => {
                return match self.read(root, path) {
                    Ok(current) => Err(FileError::conflict(Some(current.revision))),
                    Err(error) if error.code == ErrorCode::NotFound => {
                        Err(FileError::conflict(None))
                    }
                    Err(error) => Err(error),
                };
            }
            other => other?,
        }
        Ok(content(path, data))
    }
}

/// Every ancestor remains open; Windows handles forbid deletion, Unix identities are rechecked.
struct Checked {
    path: PathBuf,
    handles: Vec<(PathBuf, File)>,
    exists: bool,
}
impl Checked {
    fn walk(root: &Path, path: &str, create: bool) -> Result<Self> {
        Self::walk_with(root, path, create, &mut |_| {})
    }
    fn walk_with(
        root: &Path,
        path: &str,
        create: bool,
        hook: &mut dyn FnMut(&Path),
    ) -> Result<Self> {
        let mut checked = Self {
            path: root.to_path_buf(),
            handles: vec![],
            exists: true,
        };
        checked.push(root, true, hook)?;
        let parts: Vec<_> = path.split('/').filter(|s| !s.is_empty()).collect();
        for (i, part) in parts.iter().enumerate() {
            checked.path.push(part);
            let final_part = i + 1 == parts.len();
            let p = checked.path.clone();
            match checked.push(&p, !final_part, hook) {
                Err(e) if create && final_part && e.code == ErrorCode::NotFound => {
                    checked.exists = false;
                }
                other => other?,
            }
        }
        checked.verify()?;
        Ok(checked)
    }
    fn push(&mut self, path: &Path, directory: bool, hook: &mut dyn FnMut(&Path)) -> Result<()> {
        let meta = fs::symlink_metadata(path)?;
        if link(&meta) || directory && !meta.is_dir() {
            return Err(FileError::forbidden());
        }
        #[cfg(windows)]
        let file = storage::open_at(path, directory, self.handles.last().map(|(_, f)| f))?;
        hook(path);
        #[cfg(unix)]
        let file = storage::open_at(path, directory, self.handles.last().map(|(_, f)| f))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;
            let opened = file.metadata()?;
            if meta.dev() != opened.dev() || meta.ino() != opened.ino() {
                return Err(FileError::forbidden());
            }
        }
        if link(&file.metadata()?) || !storage::same(&file, path)? {
            return Err(FileError::forbidden());
        }
        self.handles.push((path.to_path_buf(), file));
        Ok(())
    }
    fn last(&self) -> &File {
        &self.handles[self.handles.len() - 1].1
    }
    fn last_mut(&mut self) -> &mut File {
        let i = self.handles.len() - 1;
        &mut self.handles[i].1
    }
    fn verify(&self) -> Result<()> {
        for (path, file) in &self.handles {
            if link(&fs::symlink_metadata(path)?) || !storage::same(file, path)? {
                return Err(FileError::forbidden());
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    type TestResult = std::result::Result<(), Box<dyn std::error::Error>>;
    #[test]
    fn check_open_swap_is_refused() -> TestResult {
        let tmp = tempfile::tempdir()?;
        let root = tmp.path().join("root");
        let outside = tmp.path().join("outside");
        fs::create_dir(&root)?;
        fs::create_dir(&outside)?;
        fs::create_dir(root.join("dir"))?;
        fs::write(root.join("dir/file"), "inside")?;
        fs::write(outside.join("file"), "synthetic outside")?;
        let mut moved = false;
        let result = Checked::walk_with(&root, "dir/file", false, &mut |p| {
            if p != root.join("dir") {
                return;
            }
            moved = fs::rename(p, root.join("moved")).is_ok();
            #[cfg(unix)]
            if moved {
                std::os::unix::fs::symlink(&outside, p).expect("create synthetic link");
            }
        });
        #[cfg(unix)]
        {
            assert!(moved);
            assert!(result.is_err());
        }
        #[cfg(windows)]
        {
            assert!(!moved);
            assert!(result.is_ok());
        }
        assert_eq!(fs::read(outside.join("file"))?, b"synthetic outside");
        Ok(())
    }
    #[test]
    fn held_ancestor_swap_is_refused_or_prevented() -> TestResult {
        let tmp = tempfile::tempdir()?;
        let root = tmp.path().join("root");
        fs::create_dir(&root)?;
        fs::create_dir(root.join("dir"))?;
        fs::write(root.join("dir/file"), "inside")?;
        let mut moved = false;
        let result = Checked::walk_with(&root, "dir/file", false, &mut |p| {
            if p == root.join("dir/file") {
                moved = fs::rename(root.join("dir"), root.join("moved")).is_ok();
            }
        });
        #[cfg(unix)]
        {
            assert!(moved);
            assert!(result.is_err());
        }
        #[cfg(windows)]
        {
            assert!(!moved);
            assert!(result.is_ok());
        }
        Ok(())
    }
}
