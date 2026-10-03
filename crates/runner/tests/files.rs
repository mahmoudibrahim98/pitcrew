//! Every artifact and link target is under this test's temporary directory.
use pitcrew_protocol::{
    api::ErrorCode,
    files::{FileEncoding, FileKind, MAX_FILE_BYTES, WriteFile},
};
use pitcrew_runner::files::{Files, validate_path};
use std::fs;
use std::path::Path;

type TestResult = Result<(), Box<dyn std::error::Error>>;
fn write(revision: Option<String>, text: &str) -> WriteFile {
    WriteFile {
        revision,
        encoding: FileEncoding::Utf8,
        content: text.into(),
    }
}
#[test]
fn lexical_path_rules_before_io() -> TestResult {
    for path in [
        "/absolute",
        "../x",
        "x/..",
        "x/../y",
        ".",
        "./x",
        "x/.",
        "x//y",
        "x/",
        "\\absolute",
        "x\\y",
        "x\0y",
    ] {
        let Err(error) = validate_path(path, false, false) else {
            panic!("accepted {path:?}")
        };
        assert_eq!(error.code, ErrorCode::Invalid, "{path:?}");
    }
    assert!(validate_path("", true, false).is_ok());
    assert!(validate_path("", false, false).is_err());
    for path in [".git", ".git/config", "src/.git/config"] {
        let Err(error) = validate_path(path, false, true) else {
            panic!("accepted Git write")
        };
        assert_eq!(error.code, ErrorCode::Forbidden);
        assert!(validate_path(path, false, false).is_ok());
    }
    for path in [
        "src/main.rs",
        "hello world.txt",
        "..safe",
        ".github/workflow",
    ] {
        validate_path(path, false, true)?;
    }
    #[cfg(windows)]
    for path in [
        "C:/file",
        "C:file",
        "//server/share",
        "\\\\server\\share",
        "\\\\?\\C:\\file",
        "file:stream",
        "CON",
        "nul.txt",
        "PRN",
        "AUX",
        "COM1.rs",
        "COM9",
        "LPT1",
        "LPT9.log",
        "COM¹",
        "LPT².txt",
        "COM³",
        "CONIN$",
        "CONOUT$",
        "end.",
        "end ",
        "dir./file",
        "CON .txt",
    ] {
        assert!(validate_path(path, false, false).is_err(), "{path:?}");
    }
    #[cfg(windows)]
    assert!(validate_path(".GIT/config", false, true).is_err());
    Ok(())
}
#[test]
fn a_root_inside_git_is_readable_but_never_writable() -> TestResult {
    let tmp = tempfile::tempdir()?;
    let root = tmp.path().join(".git");
    let state = tmp.path().join("state");
    fs::create_dir(&root)?;
    fs::create_dir(&state)?;
    fs::write(root.join("config"), "synthetic git data")?;
    let files = Files::new(&state);
    let read = files.read(&root, "config")?;
    assert!(
        files
            .write(&root, "config", write(Some(read.revision), "wrong"))
            .is_err()
    );
    assert!(files.write(&root, "new", write(None, "wrong")).is_err());
    assert_eq!(fs::read(root.join("config"))?, b"synthetic git data");
    Ok(())
}
#[test]
fn revisions_backups_binary_and_hardlinks() -> TestResult {
    let tmp = tempfile::tempdir()?;
    let root = tmp.path().join("root");
    let state = tmp.path().join("state");
    fs::create_dir(&root)?;
    fs::create_dir(&state)?;
    let files = Files::new(&state);
    let first = files.write(&root, "file.txt", write(None, "first"))?;
    assert!(
        files
            .write(&root, "file.txt", write(None, "wrong"))
            .is_err()
    );
    let second = files.write(
        &root,
        "file.txt",
        write(Some(first.revision.clone()), "second"),
    )?;
    let Err(conflict) = files.write(&root, "file.txt", write(Some(first.revision), "wrong")) else {
        panic!("stale write")
    };
    assert_eq!(conflict.code, ErrorCode::Conflict);
    assert_eq!(conflict.current_revision, Some(Some(second.revision)));
    let backups: Vec<_> = fs::read_dir(state.join("file-backups"))?.collect::<Result<_, _>>()?;
    assert_eq!(backups.len(), 1);
    assert_eq!(fs::read(backups[0].path())?, b"first");
    fs::write(root.join("binary"), [0xff, 0, 0x80])?;
    let binary = files.read(&root, "binary")?;
    assert_eq!(binary.encoding, FileEncoding::Base64);
    assert_eq!(binary.content, "/wCA");
    fs::hard_link(root.join("file.txt"), root.join("alias"))?;
    let old = files.read(&root, "file.txt")?;
    let Err(error) = files.write(&root, "file.txt", write(Some(old.revision), "wrong")) else {
        panic!("hardlink write")
    };
    assert_eq!(error.code, ErrorCode::Forbidden);
    assert_eq!(fs::read(root.join("alias"))?, b"second");
    assert_eq!(
        fs::read_dir(&root)?
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().starts_with(".pitcrew-"))
            .count(),
        0
    );
    Ok(())
}
#[test]
fn caps_and_retention() -> TestResult {
    let tmp = tempfile::tempdir()?;
    let root = tmp.path().join("root");
    let state = tmp.path().join("state");
    fs::create_dir(&root)?;
    fs::create_dir(&state)?;
    let files = Files::new(&state);
    let mut current = files.write(&root, "history", write(None, "0"))?;
    for n in 1..=5 {
        current = files.write(
            &root,
            "history",
            write(Some(current.revision), &n.to_string()),
        )?;
    }
    let mut old: Vec<_> = fs::read_dir(state.join("file-backups"))?
        .map(|e| fs::read(e?.path()))
        .collect::<Result<_, _>>()?;
    old.sort();
    assert_eq!(old, vec![b"2".to_vec(), b"3".to_vec(), b"4".to_vec()]);
    let big = fs::File::create(root.join("large"))?;
    big.set_len(MAX_FILE_BYTES + 1)?;
    let Err(error) = files.read(&root, "large") else {
        panic!("oversized read")
    };
    assert_eq!(error.code, ErrorCode::TooLarge);
    assert_eq!(error.size, Some(MAX_FILE_BYTES + 1));
    let Err(error) = files.write(
        &root,
        "huge",
        write(None, &"x".repeat(MAX_FILE_BYTES as usize + 1)),
    ) else {
        panic!("oversized write")
    };
    assert_eq!(error.code, ErrorCode::TooLarge);
    assert!(!root.join("huge").exists());
    let data = "x".repeat(MAX_FILE_BYTES as usize);
    for n in 0..9 {
        let path = format!("big{n}");
        let current = files.write(&root, &path, write(None, &data))?;
        files.write(&root, &path, write(Some(current.revision), "small"))?;
    }
    let sizes: Vec<_> = fs::read_dir(state.join("file-backups"))?
        .map(|e| e?.metadata().map(|m| m.len()))
        .collect::<Result<_, _>>()?;
    assert!(sizes.iter().sum::<u64>() <= 64 * 1024 * 1024);
    assert!(sizes.iter().all(|&s| s <= MAX_FILE_BYTES));
    Ok(())
}
#[test]
fn squatted_backup_refuses_and_preserves_target() -> TestResult {
    let tmp = tempfile::tempdir()?;
    let root = tmp.path().join("root");
    let state = tmp.path().join("state");
    fs::create_dir(&root)?;
    fs::create_dir(&state)?;
    fs::write(root.join("file"), "old")?;
    fs::write(state.join("file-backups"), "squatted")?;
    let files = Files::new(&state);
    let revision = files.read(&root, "file")?.revision;
    assert!(
        files
            .write(&root, "file", write(Some(revision), "new"))
            .is_err()
    );
    assert_eq!(fs::read(root.join("file"))?, b"old");
    Ok(())
}
#[test]
fn public_or_linked_backup_storage_is_refused() -> TestResult {
    let tmp = tempfile::tempdir()?;
    let root = tmp.path().join("root");
    let state = tmp.path().join("state");
    let outside = tmp.path().join("outside");
    fs::create_dir(&root)?;
    fs::create_dir(&state)?;
    fs::create_dir(&outside)?;
    fs::write(root.join("file"), "old")?;
    let files = Files::new(&state);
    let revision = files.read(&root, "file")?.revision;
    let backups = state.join("file-backups");
    fs::create_dir(&backups)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(&backups, fs::Permissions::from_mode(0o755))?;
    }
    assert!(
        files
            .write(&root, "file", write(Some(revision.clone()), "wrong"))
            .is_err()
    );
    fs::remove_dir(&backups)?;
    #[cfg(unix)]
    std::os::unix::fs::symlink(&outside, &backups)?;
    #[cfg(windows)]
    assert!(
        std::process::Command::new("cmd")
            .args(["/c", "mklink", "/J"])
            .arg(&backups)
            .arg(&outside)
            .output()?
            .status
            .success()
    );
    assert!(
        files
            .write(&root, "file", write(Some(revision), "wrong"))
            .is_err()
    );
    assert_eq!(fs::read(root.join("file"))?, b"old");
    assert_eq!(fs::read_dir(&outside)?.count(), 0);
    Ok(())
}
#[test]
fn hardlinked_backup_is_refused() -> TestResult {
    let tmp = tempfile::tempdir()?;
    let root = tmp.path().join("root");
    let state = tmp.path().join("state");
    fs::create_dir(&root)?;
    fs::create_dir(&state)?;
    let files = Files::new(&state);
    let current = files.write(&root, "file", write(None, "old"))?;
    let current = files.write(&root, "file", write(Some(current.revision), "new"))?;
    let Some(backup) = fs::read_dir(state.join("file-backups"))?.next() else {
        panic!("backup missing")
    };
    fs::hard_link(backup?.path(), tmp.path().join("alias"))?;
    assert!(
        files
            .write(&root, "file", write(Some(current.revision), "wrong"))
            .is_err()
    );
    assert_eq!(fs::read(root.join("file"))?, b"new");
    assert_eq!(fs::read(tmp.path().join("alias"))?, b"old");
    Ok(())
}
#[test]
fn listing_cap_is_sorted_and_reports_truncation() -> TestResult {
    let tmp = tempfile::tempdir()?;
    let state = tmp.path().join("state");
    fs::create_dir(&state)?;
    let root = tmp.path().join("root");
    fs::create_dir(&root)?;
    for n in (0..5002).rev() {
        fs::write(root.join(format!("entry-{n:05}")), "")?;
    }
    let listing = Files::new(&state).list(&root, "")?;
    assert_eq!(listing.entries.len(), 5000);
    assert!(listing.truncated);
    assert_eq!(listing.entries[0].name, "entry-00000");
    assert_eq!(listing.entries[4999].name, "entry-04999");
    Ok(())
}
fn check_link(root: &Path, state: &Path, name: &str) -> TestResult {
    let files = Files::new(state);
    assert!(
        files
            .list(root, "")?
            .entries
            .iter()
            .any(|e| e.name == name && e.kind == FileKind::Link)
    );
    assert!(files.read(root, &format!("{name}/secret")).is_err());
    assert!(
        files
            .write(root, &format!("{name}/secret"), write(None, "wrong"))
            .is_err()
    );
    assert!(files.list(root, name).is_err());
    Ok(())
}
#[test]
fn links_and_junctions_are_listed_but_never_followed() -> TestResult {
    let tmp = tempfile::tempdir()?;
    let root = tmp.path().join("root");
    let state = tmp.path().join("state");
    let outside = tmp.path().join("outside");
    fs::create_dir(&root)?;
    fs::create_dir(&state)?;
    fs::create_dir(&outside)?;
    fs::write(outside.join("secret"), "synthetic")?;
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&outside, root.join("link"))?;
        check_link(&root, &state, "link")?;
    }
    #[cfg(windows)]
    {
        let status = std::process::Command::new("cmd")
            .args(["/c", "mklink", "/J"])
            .arg(root.join("junction"))
            .arg(&outside)
            .output()?;
        assert!(status.status.success());
        check_link(&root, &state, "junction")?;
        match std::os::windows::fs::symlink_dir(&outside, root.join("link")) {
            Ok(()) => check_link(&root, &state, "link")?,
            Err(e) if e.raw_os_error() == Some(1314) => {
                eprintln!("SKIP symbolic link: Windows privilege unavailable; junction tested")
            }
            Err(e) => return Err(e.into()),
        }
    }
    assert_eq!(fs::read(outside.join("secret"))?, b"synthetic");
    Ok(())
}
#[cfg(unix)]
#[test]
fn unix_permissions_and_public_backups() -> TestResult {
    use std::os::unix::fs::PermissionsExt as _;
    let tmp = tempfile::tempdir()?;
    let state = tmp.path().join("state");
    fs::create_dir(&state)?;
    fs::write(tmp.path().join("file"), "old")?;
    fs::set_permissions(tmp.path().join("file"), fs::Permissions::from_mode(0o640))?;
    let files = Files::new(&state);
    let revision = files.read(tmp.path(), "file")?.revision;
    files.write(tmp.path(), "file", write(Some(revision), "new"))?;
    assert_eq!(
        fs::metadata(tmp.path().join("file"))?.permissions().mode() & 0o777,
        0o640
    );
    assert_eq!(
        fs::metadata(state.join("file-backups"))?
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    for e in fs::read_dir(state.join("file-backups"))? {
        assert_eq!(e?.metadata()?.permissions().mode() & 0o777, 0o600);
    }
    fs::set_permissions(
        state.join("file-backups"),
        fs::Permissions::from_mode(0o755),
    )?;
    let revision = files.read(tmp.path(), "file")?.revision;
    assert!(
        files
            .write(tmp.path(), "file", write(Some(revision), "wrong"))
            .is_err()
    );
    assert_eq!(fs::read(tmp.path().join("file"))?, b"new");
    Ok(())
}
