//! The `apply_patch` format (`*** Begin Patch` ... `*** End Patch`) that Codex and OpenCode's
//! patch tool use, split into files and turned into capped `FileEdit`s.

use crate::bound::{Diff, MAX_DIFF_BYTES, MAX_PATH_BYTES, MAX_TARGET_CHARS};
use crate::text::{first_line, truncate_chars};
use pitcrew_interfaces::source::TranscriptItem;
use pitcrew_protocol::model::TimestampMs;

/// At most this many files are taken from one patch.
pub(crate) const MAX_PATCH_FILES: usize = 100;
/// The diffs of one patch share this many bytes; each file also has [`MAX_DIFF_BYTES`].
pub(crate) const MAX_PATCH_DIFF_BYTES: usize = 256 * 1024;

/// What a patch does to one file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Change {
    Add,
    Delete,
    Update,
}

/// One file's section of a patch. The body is kept as a byte range of the patch text, so a large
/// patch is not copied while it is counted.
#[derive(Debug)]
struct PatchFile<'a> {
    change: Change,
    path: &'a str,
    move_to: Option<&'a str>,
    body: (usize, usize),
    added: u32,
    removed: u32,
}

impl PatchFile<'_> {
    fn dest(&self) -> &str {
        self.move_to.unwrap_or(self.path)
    }
}

/// A patch, split into files.
#[derive(Debug)]
pub(crate) struct Patch<'a> {
    text: &'a str,
    files: Vec<PatchFile<'a>>,
}

impl<'a> Patch<'a> {
    /// Anything that does not start with `*** Begin Patch` has no files. Lines outside a file
    /// section are ignored, and a patch without `*** End Patch` keeps the files it has.
    pub(crate) fn parse(text: &'a str) -> Self {
        let mut files = Vec::new();
        let mut cur: Option<PatchFile<'a>> = None;
        let mut began = false;
        let mut pos = 0;
        for raw in text.split_inclusive('\n') {
            pos += raw.len();
            let line = raw.trim_end_matches(['\n', '\r']);
            if !began {
                match line.trim() {
                    "" => continue,
                    "*** Begin Patch" => {
                        began = true;
                        continue;
                    }
                    _ => break,
                }
            }
            if line.trim_end() == "*** End Patch" {
                break;
            }
            let header = [
                ("*** Add File: ", Change::Add),
                ("*** Delete File: ", Change::Delete),
                ("*** Update File: ", Change::Update),
            ]
            .into_iter()
            .find_map(|(prefix, change)| line.strip_prefix(prefix).map(|p| (change, p.trim())));
            if let Some((change, path)) = header {
                files.extend(cur.take());
                if files.len() >= MAX_PATCH_FILES {
                    break;
                }
                cur = (!path.is_empty()).then_some(PatchFile {
                    change,
                    path,
                    move_to: None,
                    body: (pos, pos),
                    added: 0,
                    removed: 0,
                });
                continue;
            }
            let Some(file) = cur.as_mut() else { continue };
            if let Some(to) = line.strip_prefix("*** Move to: ") {
                let to = to.trim();
                if file.body.0 == file.body.1 && file.change == Change::Update && !to.is_empty() {
                    file.move_to = Some(to);
                    file.body = (pos, pos);
                }
                continue;
            }
            match line.as_bytes().first() {
                Some(b'+') => file.added = file.added.saturating_add(1),
                Some(b'-') => file.removed = file.removed.saturating_add(1),
                _ => {}
            }
            file.body.1 = pos;
        }
        files.extend(cur);
        Self { text, files }
    }

    /// Whether the patch names no file.
    pub(crate) fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// A tool target: the first file, and how many more.
    pub(crate) fn target(&self) -> String {
        let Some(first) = self.files.first() else {
            return String::new();
        };
        let first = first_line(first.dest(), MAX_TARGET_CHARS);
        match self.files.len() {
            1 => first,
            n => format!("{first} (+{} more)", n - 1),
        }
    }

    /// One `FileEdit` per file, the diffs sharing [`MAX_PATCH_DIFF_BYTES`].
    pub(crate) fn file_edits(&self, at: TimestampMs, offset: u64, out: &mut Vec<TranscriptItem>) {
        let mut budget = MAX_PATCH_DIFF_BYTES;
        for file in &self.files {
            let path = truncate_chars(file.path, MAX_PATH_BYTES);
            let dest = truncate_chars(file.dest(), MAX_PATH_BYTES);
            let diff = (budget > 0).then(|| {
                let (old, new) = match file.change {
                    Change::Add => ("/dev/null", dest.as_str()),
                    Change::Delete => (path.as_str(), "/dev/null"),
                    Change::Update => (path.as_str(), dest.as_str()),
                };
                let mut diff = Diff::new(old, new, budget.min(MAX_DIFF_BYTES));
                if file.change == Change::Add {
                    diff.push(&format!("@@ -0,0 +1,{} @@", file.added));
                }
                let body = self.text.get(file.body.0..file.body.1).unwrap_or("");
                for line in body.lines().filter(|l| !l.starts_with("*** ")) {
                    diff.push(line);
                }
                let diff = diff.finish();
                budget = budget.saturating_sub(diff.len());
                diff
            });
            out.push(TranscriptItem::FileEdit {
                at,
                path: dest,
                added: file.added,
                removed: file.removed,
                diff,
                offset,
            });
        }
    }
}
