//! `unsafe` code is allowed in one module only: `src/pty/windows.rs`. This crate denies it
//! everywhere else and pitcrew-ptyd forbids it; this test fails if any other source file of
//! either crate allows it, so a new `unsafe` block cannot slip in elsewhere.

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
fn only_pty_windows_allows_unsafe_code() {
    let runtime = Path::new(env!("CARGO_MANIFEST_DIR"));
    let ptyd = runtime.join("../ptyd");
    let mut files = Vec::new();
    for dir in [
        runtime.join("src"),
        runtime.join("tests"),
        ptyd.join("src"),
        ptyd.join("tests"),
    ] {
        sources(&dir, &mut files);
    }
    assert!(files.len() > 20, "found only {} files", files.len());
    let allowed = runtime.join("src").join("pty").join("windows.rs");
    // Built from parts, so this file does not match itself.
    let lint = ["unsafe", "_code"].concat();
    let mut allowing = Vec::new();
    for file in &files {
        let text = std::fs::read_to_string(file).expect("source");
        let allows = text
            .lines()
            .map(str::trim)
            .filter(|line| !line.starts_with("//"))
            .any(|line| line.contains("allow(") && line.contains(&lint));
        if allows && file != &allowed {
            allowing.push(file.display().to_string());
        }
    }
    assert!(allowing.is_empty(), "unsafe code allowed in {allowing:?}");
    let lib = std::fs::read_to_string(runtime.join("src/lib.rs")).expect("lib.rs");
    assert!(
        lib.contains(&format!("#![deny({lint})]")),
        "lib.rs must deny it"
    );
    let main = std::fs::read_to_string(ptyd.join("src/main.rs")).expect("main.rs");
    assert!(
        main.contains(&format!("#![forbid({lint})]")),
        "ptyd must forbid it"
    );
    let windows = std::fs::read_to_string(&allowed).expect("windows.rs");
    assert!(windows.contains(&format!("#![allow({lint})]")));
}
