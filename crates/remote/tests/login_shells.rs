//! The wrapped command under real login shells, the way sshd runs it: `<shell> -c <command>`.
//!
//! Shells come from `PITCREW_TEST_SHELLS` (`:`-separated paths; each must work) or else from
//! whichever of sh, bash, dash, zsh, ksh, mksh, fish, tcsh and csh are on `PATH`. Each is
//! checked twice:
//! - as the login shell, in front of the real `/bin/sh`;
//! - for the POSIX ones, also as the `/bin/sh` that decodes and runs the command.
//!
//! Run with `--nocapture` to see which shells were checked.

#![cfg(unix)]
#![allow(clippy::unwrap_used)]

use pitcrew_remote::quote::remote_command;
use std::path::{Path, PathBuf};
use std::process::Command;

const CANDIDATES: [&str; 9] = [
    "sh", "bash", "dash", "zsh", "ksh", "mksh", "fish", "tcsh", "csh",
];

fn shells() -> Vec<PathBuf> {
    if let Some(list) = std::env::var_os("PITCREW_TEST_SHELLS").filter(|v| !v.is_empty()) {
        let shells: Vec<PathBuf> = std::env::split_paths(&list).collect();
        for shell in &shells {
            assert!(
                shell.is_file(),
                "PITCREW_TEST_SHELLS names {shell:?}, which is missing"
            );
        }
        return shells;
    }
    let path = std::env::var_os("PATH").unwrap_or_default();
    CANDIDATES
        .iter()
        .filter_map(|name| {
            std::env::split_paths(&path)
                .map(|dir| dir.join(name))
                .find(|p| p.is_file())
        })
        .collect()
}

fn name(shell: &Path) -> String {
    shell.file_name().unwrap().to_string_lossy().into_owned()
}

/// Every byte value a `&str` argument can carry in one char (except NUL), and the review's
/// fish break-outs.
fn hostile() -> Vec<String> {
    let mut words: Vec<String> = [
        "it's",
        "\\'; touch pwned; #",
        "x\\",
        "; touch pwned; #",
        "!!",
        "!x",
        "a\nb",
        "$HOME",
        "$(echo no)",
        "`echo no`",
        "%s",
        "\\101",
        "-rf",
        "",
        "日本語 ünïcödé 🦀",
        "trailing newline\n",
    ]
    .map(str::to_owned)
    .to_vec();
    words.push((1u8..=127).map(char::from).collect());
    words
}

fn run(shell: &Path, script: &str) -> std::process::Output {
    Command::new(shell)
        .arg("-c")
        .arg(script)
        .env("HOME", "/nonexistent-home")
        .output()
        .unwrap()
}

fn check(shell: &Path, script: &str, want: &[String], what: &str) {
    let out = run(shell, script);
    assert!(
        out.status.success(),
        "{what} under {}: {:?}\nstderr: {}",
        name(shell),
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    let words: Vec<String> = String::from_utf8(out.stdout)
        .unwrap()
        .split_terminator('\0')
        .map(str::to_owned)
        .collect();
    assert_eq!(words, want, "{what} under {}", name(shell));
}

fn argv(words: &[String]) -> Vec<String> {
    let mut argv = vec!["printf".to_owned(), "%s\\0".to_owned()];
    argv.extend(words.iter().cloned());
    argv
}

#[test]
fn hostile_argv_survives_every_login_shell() {
    let words = hostile();
    let command = remote_command(&argv(&words)).unwrap();
    // Close to the size limit, for shells with small word buffers.
    let big = vec!["x'\\!\n".repeat(3_500)];
    let big_command = remote_command(&argv(&big)).unwrap();
    assert!(big_command.len() > 100_000);
    let mut checked = Vec::new();
    for shell in shells() {
        check(&shell, &command, &words, "hostile argv");
        check(&shell, &big_command, &big, "a 17 KB argument");
        checked.push(name(&shell));
    }
    println!("login shells checked: {checked:?}");
}

/// The single-quoted script the login shell hands to `/bin/sh`, run by each POSIX shell as if
/// it were `/bin/sh`.
#[test]
fn the_inner_script_runs_under_every_posix_sh() {
    let words = hostile();
    let command = remote_command(&argv(&words)).unwrap();
    let inner = command
        .strip_prefix("/bin/sh -c '")
        .and_then(|s| s.strip_suffix('\''))
        .unwrap();
    let mut checked = Vec::new();
    for shell in shells() {
        let name = name(&shell);
        if matches!(name.as_str(), "fish" | "tcsh" | "csh") {
            continue;
        }
        check(&shell, inner, &words, "the inner script");
        checked.push(name);
    }
    println!("POSIX shells checked as /bin/sh: {checked:?}");
}
