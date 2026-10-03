//! `unsafe` code is allowed in one module only, `src/windows.rs`; `lib.rs` denies it everywhere
//! else. This test fails if another source file of the crate allows it.

use std::path::{Path, PathBuf};

/// Every `.rs` file under `dir`.
fn sources(dir: &Path, found: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            sources(&path, found);
        } else if path.extension().is_some_and(|e| e == "rs") {
            found.push(path);
        }
    }
}

#[test]
fn only_the_windows_module_allows_unsafe_code() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    sources(&root.join("src"), &mut files);
    sources(&root.join("tests"), &mut files);
    assert!(files.len() >= 4, "found only {} files", files.len());
    let allowed = root.join("src").join("windows.rs");
    // Built from parts, so this file does not match itself.
    let lint = ["unsafe", "_code"].concat();
    let allowing: Vec<String> = files
        .iter()
        .filter(|file| **file != allowed)
        .filter(|file| {
            std::fs::read_to_string(file)
                .expect("source")
                .lines()
                .map(str::trim)
                .filter(|line| !line.starts_with("//"))
                .any(|line| line.contains("allow(") && line.contains(&lint))
        })
        .map(|file| file.display().to_string())
        .collect();
    assert!(allowing.is_empty(), "unsafe code allowed in {allowing:?}");
    let lib = std::fs::read_to_string(root.join("src/lib.rs")).expect("lib.rs");
    assert!(
        lib.contains(&format!("#![deny({lint})]")),
        "lib.rs must deny it"
    );
    let windows = std::fs::read_to_string(&allowed).expect("windows.rs");
    assert!(windows.contains(&format!("#![allow({lint})]")));
}
