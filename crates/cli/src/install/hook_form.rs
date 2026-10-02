//! Claude Code 2.1.139 introduced hook `args` (official CHANGELOG.md).

use crate::config::Env;
use crate::error::{Error, Result};
use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub(crate) enum HookForm {
    #[default]
    Auto,
    Exec,
    Shell,
}

const MINIMUM: (u64, u64, u64) = (2, 1, 139);
const TIMEOUT: Duration = Duration::from_millis(750);
const OUTPUT_LIMIT: u64 = 4096;

fn supported(text: &str) -> bool {
    let Some(version) = text.split_whitespace().next() else {
        return false;
    };
    let parts: Vec<_> = version.split('.').collect();
    if parts.len() != 3
        || parts
            .iter()
            .any(|p| p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()))
    {
        return false;
    }
    match (
        parts[0].parse::<u64>(),
        parts[1].parse::<u64>(),
        parts[2].parse::<u64>(),
    ) {
        (Ok(major), Ok(minor), Ok(patch)) => (major, minor, patch) >= MINIMUM,
        _ => false,
    }
}

fn version(env: Env<'_>) -> Option<String> {
    let path = env("PATH")?;
    let names: &[&str] = if cfg!(windows) {
        &["claude.exe", "claude.cmd", "claude.bat"]
    } else {
        &["claude"]
    };
    let exe = std::env::split_paths(&path)
        .filter(|dir| dir.is_absolute())
        .flat_map(|dir| names.iter().map(move |name| dir.join(name)))
        .find(|path| path.is_file())?;
    let mut command = Command::new(exe);
    command
        .arg("--version")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .stdout(Stdio::piped());
    let mut child = command.spawn().ok()?;
    let stdout = child.stdout.take()?;
    // A bounded reader prevents a full pipe from hanging the version probe. The main thread
    // never waits indefinitely, even if an unexpected descendant keeps the pipe open.
    let (send, receive) = std::sync::mpsc::sync_channel(1);
    let reader = std::thread::Builder::new()
        .name("claude-version".into())
        .spawn(move || {
            let mut bytes = Vec::new();
            if stdout
                .take(OUTPUT_LIMIT + 1)
                .read_to_end(&mut bytes)
                .is_ok()
                && bytes.len() <= OUTPUT_LIMIT as usize
            {
                let _ = send.send(bytes);
            }
        });
    if reader.is_err() {
        let _ = child.kill();
        let _ = child.wait();
        return None;
    }
    let deadline = Instant::now() + TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    return None;
                }
                let bytes = receive
                    .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                    .ok()?;
                return String::from_utf8(bytes).ok();
            }
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

pub(crate) fn select(form: HookForm, env: Env<'_>) -> Result<(HookForm, &'static str)> {
    if form == HookForm::Shell {
        return Ok((HookForm::Shell, "shell form explicitly selected"));
    }
    let supports_exec = version(env).is_some_and(|v| supported(&v));
    if supports_exec {
        return Ok((HookForm::Exec, "exec form (Claude Code >=2.1.139 detected)"));
    }
    if form == HookForm::Exec {
        return Err(Error::invalid(
            "--hook-form exec requires a verified Claude Code >=2.1.139; use auto or shell when the CLI is missing, old, unreadable, or times out",
        ));
    }
    Ok((
        HookForm::Shell,
        "shell form: Claude Code missing, older than 2.1.139, unreadable, or version probe timed out",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_known_stable_versions_at_or_above_the_introduction_use_exec() {
        for version in ["2.1.139 (Claude Code)", "2.1.140", "2.2.0", "3.0.0"] {
            assert!(supported(version), "{version}");
        }
        for version in [
            "",
            "unknown",
            "2.1.138",
            "2.0.999",
            "2.1.139-beta",
            "2.1",
            "2.1.139.1",
            "warning 2.1.139",
        ] {
            assert!(!supported(version), "{version}");
        }
    }
}
