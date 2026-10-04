//! Device-only workstream file requests and responses (api-v1.md).
use serde::{Deserialize, Serialize};

/// Maximum decoded file size.
pub const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;
/// Maximum JSON write body.
pub const MAX_BODY_BYTES: usize = 12 * 1024 * 1024;
/// Maximum directory entries returned.
pub const MAX_ENTRIES: usize = 5000;

/// A directory entry, without following links.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct FileEntry {
    /// UTF-8 name.
    pub name: String,
    /// Entry type.
    pub kind: FileKind,
    /// Byte size from metadata.
    pub size: u64,
    /// UTC milliseconds, if available.
    pub modified_at: Option<i64>,
}
/// Supported entry types.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum FileKind {
    /// Regular file.
    File,
    /// Directory.
    Folder,
    /// Symbolic link or reparse point.
    Link,
}
/// A bounded directory listing.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct FileList {
    /// Sorted by UTF-8 bytes.
    pub entries: Vec<FileEntry>,
    /// Entries were omitted at the cap.
    pub truncated: bool,
}
/// How bytes are carried inside JSON.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum FileEncoding {
    /// UTF-8 text.
    Utf8,
    /// Canonical padded base64.
    Base64,
}
/// File bytes and optimistic concurrency revision.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct FileContent {
    /// Decoded size.
    pub size: u64,
    /// Conservative media type.
    pub media_type: String,
    /// Lowercase SHA-256 of exact bytes.
    pub revision: String,
    /// Content encoding.
    pub encoding: FileEncoding,
    /// Text or base64.
    pub content: String,
}
/// Replace a revision, or create exclusively with null revision.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct WriteFile {
    /// Required; null means the target must not exist. The custom deserializer keeps missing invalid.
    #[serde(deserialize_with = "required_revision")]
    pub revision: Option<String>,
    /// Encoding of content.
    pub encoding: FileEncoding,
    /// New bytes.
    pub content: String,
}
fn required_revision<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    Option::<String>::deserialize(d)
}
