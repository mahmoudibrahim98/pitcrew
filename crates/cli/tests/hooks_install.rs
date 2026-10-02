//! Installation probes only a synthetic Claude binary on a private PATH, on every platform.
mod common;

use common::{checked, pitcrew_command};
use serde_json::{Value, json};
use std::error::Error;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::OnceLock;
use std::time::Instant;

type Result<T = ()> = std::result::Result<T, Box<dyn Error>>;

fn fake_binary() -> Result<&'static Path> {
    static FAKE: OnceLock<std::result::Result<(tempfile::TempDir, PathBuf), String>> =
        OnceLock::new();
    let result = FAKE.get_or_init(|| {
        let tmp = tempfile::tempdir().map_err(|e| e.to_string())?;
        let source = tmp.path().join("fake.rs");
        std::fs::write(
            &source,
            r#"
            use std::io::Write;
            fn main() {
                assert_eq!(std::env::args().nth(1).as_deref(), Some("--version"));
                let dir = std::env::current_exe().unwrap().parent().unwrap().to_owned();
                std::fs::write(dir.join("probed"), "yes").unwrap();
                let text = std::fs::read_to_string(dir.join("version.txt")).unwrap();
                match text.as_str() {
                    "timeout" => std::thread::sleep(std::time::Duration::from_secs(5)),
                    "failed" => std::process::exit(1),
                    "invalid-utf8" => { std::io::stdout().write_all(&[255]).unwrap(); },
                    "huge" => { println!("{}", "x".repeat(8192)); },
                    _ => println!("{text}"),
                }
            }
        "#,
        )
        .map_err(|e| e.to_string())?;
        let binary = tmp.path().join(if cfg!(windows) {
            "claude.exe"
        } else {
            "claude"
        });
        let output = Command::new("rustc")
            .arg(&source)
            .arg("-o")
            .arg(&binary)
            .output()
            .map_err(|e| e.to_string())?;
        if !output.status.success() {
            return Err(String::from_utf8_lossy(&output.stderr).into_owned());
        }
        Ok((tmp, binary))
    });
    match result {
        Ok((_, binary)) => Ok(binary),
        Err(message) => Err(message.clone().into()),
    }
}

fn run(home: &Path, bin: &Path, args: &[&str]) -> Result<Output> {
    let mut command = pitcrew_command(home);
    command.env("PATH", bin).args(args);
    Ok(checked(&mut command).output()?)
}

fn settings(home: &Path) -> Result<Value> {
    Ok(serde_json::from_slice(&std::fs::read(
        home.join(".claude/settings.json"),
    )?)?)
}

#[test]
fn auto_selects_only_supported_versions_and_every_failure_falls_back() -> Result {
    for (version, exec) in [
        ("2.1.139 (Claude Code)", true),
        ("2.1.140", true),
        ("2.1.138", false),
        ("unknown", false),
        ("failed", false),
        ("invalid-utf8", false),
        ("huge", false),
        ("timeout", false),
        ("missing", false),
    ] {
        let tmp = tempfile::tempdir()?;
        let bin = tmp.path().join("bin");
        let home = tmp.path().join("home");
        std::fs::create_dir(&bin)?;
        if version != "missing" {
            std::fs::copy(
                fake_binary()?,
                bin.join(if cfg!(windows) {
                    "claude.exe"
                } else {
                    "claude"
                }),
            )?;
            std::fs::write(bin.join("version.txt"), version)?;
        }
        let start = Instant::now();
        let output = run(
            &home,
            &bin,
            &["hooks", "install", "--engine", "claude", "--yes"],
        )?;
        assert!(
            output.status.success(),
            "{version}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            start.elapsed().as_secs() < 4,
            "version probe must be bounded"
        );
        let value = settings(&home)?;
        let hook = &value["hooks"]["Stop"][0]["hooks"][0];
        assert_eq!(hook.get("args").is_some(), exec, "{version}");
        let text = String::from_utf8_lossy(&output.stdout);
        assert!(
            text.contains(if exec { "exec form" } else { "shell form" }),
            "{text}"
        );
        if exec {
            let command = hook["command"].as_str().ok_or("missing command")?;
            assert!(Path::new(command).is_absolute());
            assert_eq!(
                std::fs::canonicalize(command)?,
                std::fs::canonicalize(env!("CARGO_BIN_EXE_pitcrew"))?
            );
            if cfg!(unix) {
                assert_eq!(
                    command,
                    std::fs::canonicalize(env!("CARGO_BIN_EXE_pitcrew"))?
                        .to_string_lossy()
                        .as_ref()
                );
            }
            assert_eq!(hook["args"], json!(["hook", "claude", "Stop"]));
            if cfg!(windows) {
                assert!(
                    hook["command"]
                        .as_str()
                        .is_some_and(|p| p.ends_with("pitcrew.exe"))
                );
            }
        }
    }
    Ok(())
}

#[test]
fn explicit_forms_diff_migration_and_uninstall_are_safe() -> Result {
    let tmp = tempfile::tempdir()?;
    let bin = tmp.path().join("bin");
    let home = tmp.path().join("home");
    std::fs::create_dir(&bin)?;
    std::fs::copy(
        fake_binary()?,
        bin.join(if cfg!(windows) {
            "claude.exe"
        } else {
            "claude"
        }),
    )?;
    std::fs::write(bin.join("version.txt"), "2.1.139")?;
    let exec = [
        "hooks",
        "install",
        "--engine",
        "claude",
        "--yes",
        "--hook-form",
        "exec",
    ];
    let output = run(&home, &bin, &exec)?;
    assert!(output.status.success());
    assert!(
        bin.join("probed").exists(),
        "explicit exec must also verify support"
    );
    let first = settings(&home)?;
    assert_eq!(
        first["hooks"]["Stop"][0]["hooks"][0]["args"],
        json!(["hook", "claude", "Stop"])
    );
    assert!(run(&home, &bin, &exec)?.status.success());
    assert_eq!(settings(&home)?, first);
    std::fs::remove_file(bin.join("probed"))?;
    let diff = run(
        &home,
        &bin,
        &[
            "hooks",
            "diff",
            "--engine",
            "claude",
            "--hook-form",
            "shell",
        ],
    )?;
    assert!(diff.status.success());
    let text = String::from_utf8_lossy(&diff.stdout);
    assert!(
        text.contains("args") && text.contains("shell form"),
        "{text}"
    );
    assert_eq!(settings(&home)?, first, "diff must not write");
    assert!(
        run(
            &home,
            &bin,
            &[
                "hooks",
                "install",
                "--engine",
                "claude",
                "--yes",
                "--hook-form",
                "shell"
            ]
        )?
        .status
        .success()
    );
    assert!(
        settings(&home)?["hooks"]["Stop"][0]["hooks"][0]
            .get("args")
            .is_none()
    );
    assert!(
        run(
            &home,
            &bin,
            &["hooks", "uninstall", "--engine", "claude", "--yes"]
        )?
        .status
        .success()
    );
    assert_eq!(settings(&home)?, json!({}));
    assert!(
        !bin.join("probed").exists(),
        "shell and uninstall do not probe"
    );
    Ok(())
}

#[test]
fn explicit_exec_never_installs_for_an_older_or_missing_claude() -> Result {
    for version in ["2.1.138", "unknown", "missing"] {
        let tmp = tempfile::tempdir()?;
        let bin = tmp.path().join("bin");
        let home = tmp.path().join("home");
        std::fs::create_dir(&bin)?;
        if version != "missing" {
            std::fs::copy(
                fake_binary()?,
                bin.join(if cfg!(windows) {
                    "claude.exe"
                } else {
                    "claude"
                }),
            )?;
            std::fs::write(bin.join("version.txt"), version)?;
        }
        let output = run(
            &home,
            &bin,
            &[
                "hooks",
                "install",
                "--engine",
                "claude",
                "--yes",
                "--hook-form",
                "exec",
            ],
        )?;
        assert!(!output.status.success());
        assert!(!home.join(".claude/settings.json").exists());
        assert!(String::from_utf8_lossy(&output.stdout).contains("2.1.139"));
    }
    Ok(())
}
