//! The machine check over SSH (or WSL), before PitCrew's helper is there: a few lines of POSIX sh
//! ([`check_lines`]) that ask each tool for its version and `df` for the free space of `$HOME`,
//! and the same rows as the hub's own check (api-v1.md, "Machine setup"; [`MachineCheck`]).
//!
//! - **One login:** the lines ride in the probe's own call ([`Ssh::probe_and_check`]), not a call
//!   of their own: Windows' OpenSSH has no ControlMaster, so every call is a login, and a host that
//!   asks for a password or a one-time code (typical of HPC) would ask once more. [`SCRIPT`] is
//!   the same lines between the check's own markers, for a check alone.
//! - **What runs:** `command -v` and `<tool> --version` for `claude`, `codex`, `opencode`, `git`,
//!   `gh`, `sbatch`, `squeue` and `scancel` (each under `timeout 10` where there is `timeout`, with
//!   no input), `tmux -V`, and `df -Pk "$HOME"`. Nothing else: no package manager, no `sudo`, no
//!   download, nothing written. The report sits between markers that carry a random tag for the
//!   call, as the probe's does; its keys start with `check_`, so they never meet the probe's.
//! - **Rows:** each tool found and answering is `ok` with its first line; found but failing is
//!   `warn`; not there is `missing` with [`MachineCheckFix::InstallPage`] (the client opens the
//!   tool's install page from its own table). tmux older than 3.2 is `warn`. The disk row warns
//!   under 5 GB free in `$HOME`. SLURM's row is there only where `sbatch` is. The helper's row is
//!   the caller's, who knows what the probe found ([`helper_row`]).

#[cfg(doc)]
use crate::Ssh;
use crate::SshError;
use pitcrew_protocol::machine_setup::{
    MachineCheck, MachineCheckFix, MachineCheckItem, MachineCheckRow, MachineCheckStatus,
};
use std::collections::HashMap;

/// The tools whose version the script asks, in its order.
pub const TOOLS: [&str; 8] = [
    "claude", "codex", "opencode", "git", "gh", "sbatch", "squeue", "scancel",
];

/// The check's lines, without markers: each key starts with `check_`. The probe's script carries
/// them too ([`crate::probe::CHECKED_SCRIPT`]), so the check is no login of its own.
macro_rules! check_lines {
    () => {
        concat!(
            r#"to=; if command -v timeout >/dev/null 2>&1; then to='timeout 10'; fi; "#,
            r#"for t in claude codex opencode git gh sbatch squeue scancel; do "#,
            r#"if command -v "$t" >/dev/null 2>&1; then v=$($to "$t" --version </dev/null 2>&1); c=$?; "#,
            r#"printf 'check_%s=1\ncheck_%s_code=%s\n' "$t" "$t" "$c"; "#,
            r#"printf 'check_%s_version=%s\n' "$t" "$(printf '%s\n' "$v" | awk 'NF {print; exit}')"; "#,
            r#"else printf 'check_%s=0\n' "$t"; fi; done; "#,
            r#"if command -v tmux >/dev/null 2>&1; then echo check_tmux=1; "#,
            r#"printf 'check_tmux_version=%s\n' "$(tmux -V </dev/null 2>/dev/null | awk 'NF {print; exit}')"; "#,
            r#"else echo check_tmux=0; fi; "#,
            r#"printf 'check_disk_kb=%s\n' "$(df -Pk "$HOME" 2>/dev/null | awk 'NR == 2 {print $4}')"; "#,
        )
    };
}
pub(crate) use check_lines;

/// The check alone, between its own markers. It runs as `sh -c SCRIPT sh <tag>`.
pub const SCRIPT: &str = concat!(
    r#"printf '@@pitcrew-check-begin-%s\n' "$1"; "#,
    check_lines!(),
    r#"printf '@@pitcrew-check-end-%s\n' "$1""#,
);

/// Below this much free space in `$HOME` the disk row warns.
const LOW_DISK_KB: u64 = 5_000_000;
/// The longest line kept from a tool.
const MAX_LINE: usize = 120;

/// Parses [`SCRIPT`]'s output for the call tagged `tag` into rows.
///
/// # Errors
/// [`SshError::UnexpectedOutput`] if a marker is missing or a key appears twice.
pub fn parse(stdout: &str, tag: &str) -> Result<MachineCheck, SshError> {
    let values = crate::report::parse(stdout, "check", tag).map_err(SshError::UnexpectedOutput)?;
    Ok(rows(&values))
}

/// A report's `check_` value for `key`.
fn value<'a>(values: &HashMap<&'a str, &'a str>, key: &str) -> Option<&'a str> {
    values.get(format!("check_{key}").as_str()).copied()
}

/// One plain line: control characters dropped, at most [`MAX_LINE`] characters.
fn plain(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control() && !pitcrew_protocol::text::is_hidden(*c))
        .take(MAX_LINE)
        .collect::<String>()
        .trim()
        .to_owned()
}

fn row(
    id: MachineCheckItem,
    status: MachineCheckStatus,
    detail: impl Into<String>,
    version: Option<String>,
    fix: Option<MachineCheckFix>,
) -> MachineCheckRow {
    MachineCheckRow {
        id,
        status,
        detail: detail.into(),
        version,
        fix,
    }
}

/// What the script said of `tool`: `None` when it is not there, else its first line and whether
/// `--version` succeeded.
fn tool(values: &HashMap<&str, &str>, tool: &str) -> Option<(String, bool)> {
    if value(values, tool) != Some("1") {
        return None;
    }
    let line = plain(value(values, &format!("{tool}_version")).unwrap_or(""));
    let ok = value(values, &format!("{tool}_code")) == Some("0");
    Some((line, ok))
}

/// A tool's row: `ok` with its version line, `warn` when it did not answer, `missing` (with its
/// install page) when it is not there.
fn tool_row(
    values: &HashMap<&str, &str>,
    id: MachineCheckItem,
    name: &str,
    missing: &str,
) -> MachineCheckRow {
    match tool(values, name) {
        None => row(
            id,
            MachineCheckStatus::Missing,
            missing,
            None,
            Some(MachineCheckFix::InstallPage),
        ),
        Some((line, true)) if !line.is_empty() => {
            row(id, MachineCheckStatus::Ok, line.clone(), Some(line), None)
        }
        Some(_) => row(
            id,
            MachineCheckStatus::Warn,
            format!("Found, but `{name} --version` did not work."),
            None,
            None,
        ),
    }
}

/// The rows for a report's `check_` values (the check's own, or the probe's with them).
pub(crate) fn rows(values: &HashMap<&str, &str>) -> MachineCheck {
    let mut rows = vec![
        tool_row(
            values,
            MachineCheckItem::CliClaude,
            "claude",
            "Claude Code (claude) is not on PATH.",
        ),
        tool_row(
            values,
            MachineCheckItem::CliCodex,
            "codex",
            "Codex (codex) is not on PATH.",
        ),
        tool_row(
            values,
            MachineCheckItem::CliOpencode,
            "opencode",
            "OpenCode (opencode) is not on PATH.",
        ),
        tmux_row(values),
        tool_row(
            values,
            MachineCheckItem::Git,
            "git",
            "Not found on PATH: PitCrew reads projects' branches with it.",
        ),
        tool_row(
            values,
            MachineCheckItem::Gh,
            "gh",
            "Not found on PATH: only GitHub's integration needs it.",
        ),
        disk_row(values),
    ];
    if let Some(slurm) = slurm_row(values) {
        rows.push(slurm);
    }
    MachineCheck { rows }
}

fn tmux_row(values: &HashMap<&str, &str>) -> MachineCheckRow {
    let id = MachineCheckItem::Tmux;
    if value(values, "tmux") != Some("1") {
        return row(
            id,
            MachineCheckStatus::Missing,
            "Not found on PATH: the tmux launcher needs it (direct and SLURM do not).",
            None,
            Some(MachineCheckFix::InstallPage),
        );
    }
    let line = plain(value(values, "tmux_version").unwrap_or(""));
    match crate::helper::parse_tmux_version(&line) {
        Some(version) if version >= crate::helper::MIN_TMUX => {
            row(id, MachineCheckStatus::Ok, line.clone(), Some(line), None)
        }
        Some(_) => row(
            id,
            MachineCheckStatus::Warn,
            format!("{line}: the tmux launcher needs 3.2 or newer."),
            Some(line),
            Some(MachineCheckFix::InstallPage),
        ),
        None => row(
            id,
            MachineCheckStatus::Warn,
            "Found, but its version could not be read.",
            None,
            None,
        ),
    }
}

fn disk_row(values: &HashMap<&str, &str>) -> MachineCheckRow {
    let id = MachineCheckItem::Disk;
    match value(values, "disk_kb").and_then(|kb| kb.parse::<u64>().ok()) {
        Some(kb) if kb >= LOW_DISK_KB => row(
            id,
            MachineCheckStatus::Ok,
            format!("{} free in the home folder", human_kb(kb)),
            None,
            None,
        ),
        Some(kb) => row(
            id,
            MachineCheckStatus::Warn,
            format!("Low: {} free in the home folder.", human_kb(kb)),
            None,
            None,
        ),
        None => row(
            id,
            MachineCheckStatus::Warn,
            "Free space could not be read.",
            None,
            None,
        ),
    }
}

/// Kilobytes (of 1024 bytes, as `df -k`) for people, in decimal units.
fn human_kb(kb: u64) -> String {
    let bytes = kb.saturating_mul(1024);
    if bytes >= 100_000_000_000 {
        format!("{} GB", bytes / 1_000_000_000)
    } else if bytes >= 1_000_000_000 {
        let tenths = bytes / 100_000_000;
        format!("{}.{} GB", tenths / 10, tenths % 10)
    } else {
        format!("{} MB", bytes / 1_000_000)
    }
}

fn slurm_row(values: &HashMap<&str, &str>) -> Option<MachineCheckRow> {
    let id = MachineCheckItem::Slurm;
    let (line, ok) = tool(values, "sbatch")?;
    let missing: Vec<&str> = ["squeue", "scancel"]
        .into_iter()
        .filter(|name| tool(values, name).is_none())
        .collect();
    Some(if !ok || line.is_empty() {
        row(
            id,
            MachineCheckStatus::Warn,
            "Found, but `sbatch --version` did not work.",
            None,
            None,
        )
    } else if missing.is_empty() {
        row(id, MachineCheckStatus::Ok, line.clone(), Some(line), None)
    } else {
        row(
            id,
            MachineCheckStatus::Warn,
            format!(
                "{line}, but {} is not on PATH: PitCrew cannot follow or stop its jobs.",
                missing.join(" and ")
            ),
            Some(line),
            None,
        )
    })
}

/// The helper's row, from what the probe found: `ok` when it runs, `warn` when it is installed
/// but not running, `missing` (installing it is the connect wizard's next steps) otherwise.
#[must_use]
pub fn helper_row(found: Option<(&str, bool)>) -> MachineCheckRow {
    let id = MachineCheckItem::Helper;
    match found {
        Some((version, true)) => row(
            id,
            MachineCheckStatus::Ok,
            format!("pitcrewd {} runs here", plain(version)),
            Some(plain(version)),
            None,
        ),
        Some((version, false)) => row(
            id,
            MachineCheckStatus::Warn,
            format!(
                "pitcrewd {} is installed but not running: connecting starts it.",
                plain(version)
            ),
            Some(plain(version)),
            Some(MachineCheckFix::InstallHelper),
        ),
        None => row(
            id,
            MachineCheckStatus::Missing,
            "Not installed: connecting installs it in ~/.pitcrew.",
            None,
            Some(MachineCheckFix::InstallHelper),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TAG: &str = "0123456789abcdef";

    /// A report of `body`'s lines, each key given the `check_` prefix.
    fn report(body: &str) -> String {
        let body: String = body.lines().map(|line| format!("check_{line}\n")).collect();
        format!("banner\n@@pitcrew-check-begin-{TAG}\n{body}@@pitcrew-check-end-{TAG}\n")
    }

    fn by_id(check: &MachineCheck, id: MachineCheckItem) -> &MachineCheckRow {
        check.rows.iter().find(|r| r.id == id).unwrap()
    }

    #[test]
    fn a_report_becomes_rows() {
        let text = report(
            "claude=1\nclaude_code=0\nclaude_version=2.1.3 (Claude Code)\n\
             codex=1\ncodex_code=127\ncodex_version=\n\
             opencode=0\ngit=1\ngit_code=0\ngit_version=git version 2.43.0\n\
             gh=0\nsbatch=1\nsbatch_code=0\nsbatch_version=slurm 23.02.7\n\
             squeue=1\nsqueue_code=0\nsqueue_version=slurm 23.02.7\nscancel=0\n\
             tmux=1\ntmux_version=tmux 3.0a\ndisk_kb=2097152\n",
        );
        let check = parse(&text, TAG).unwrap();
        let ids: Vec<_> = check.rows.iter().map(|r| r.id).collect();
        assert_eq!(
            ids,
            [
                MachineCheckItem::CliClaude,
                MachineCheckItem::CliCodex,
                MachineCheckItem::CliOpencode,
                MachineCheckItem::Tmux,
                MachineCheckItem::Git,
                MachineCheckItem::Gh,
                MachineCheckItem::Disk,
                MachineCheckItem::Slurm,
            ]
        );
        let claude = by_id(&check, MachineCheckItem::CliClaude);
        assert_eq!(claude.status, MachineCheckStatus::Ok);
        assert_eq!(claude.detail, "2.1.3 (Claude Code)");
        assert_eq!(
            by_id(&check, MachineCheckItem::CliCodex).status,
            MachineCheckStatus::Warn
        );
        let opencode = by_id(&check, MachineCheckItem::CliOpencode);
        assert_eq!(opencode.status, MachineCheckStatus::Missing);
        assert_eq!(opencode.fix, Some(MachineCheckFix::InstallPage));
        let tmux = by_id(&check, MachineCheckItem::Tmux);
        assert_eq!(tmux.status, MachineCheckStatus::Warn);
        assert!(tmux.detail.contains("3.2 or newer"), "{tmux:?}");
        assert_eq!(
            by_id(&check, MachineCheckItem::Disk).status,
            MachineCheckStatus::Warn
        );
        let slurm = by_id(&check, MachineCheckItem::Slurm);
        assert!(slurm.detail.contains("scancel is not on PATH"), "{slurm:?}");
    }

    #[test]
    fn no_sbatch_no_slurm_row_and_a_banner_cannot_fake_a_report() {
        let check = parse(&report("disk_kb=99999999\n"), TAG).unwrap();
        assert!(check.rows.iter().all(|r| r.id != MachineCheckItem::Slurm));
        assert_eq!(
            by_id(&check, MachineCheckItem::Disk).detail,
            "102 GB free in the home folder"
        );
        assert!(parse("claude=1\n", TAG).is_err());
        let other = "@@pitcrew-check-begin-ffff\nclaude=1\n@@pitcrew-check-end-ffff\n";
        assert!(parse(other, TAG).is_err());
        let twice = report("claude=1\nclaude=0\n");
        assert!(parse(&twice, TAG).is_err());
    }

    #[test]
    fn escapes_in_a_version_are_dropped() {
        let check = parse(
            &report("git=1\ngit_code=0\ngit_version=git version 2.43.0\u{1b}[0m\u{202e}\n"),
            TAG,
        )
        .unwrap();
        assert_eq!(
            by_id(&check, MachineCheckItem::Git).detail,
            "git version 2.43.0[0m"
        );
    }

    #[test]
    fn the_helper_row_follows_the_probe() {
        assert_eq!(
            helper_row(Some(("0.1.0", true))).status,
            MachineCheckStatus::Ok
        );
        let stopped = helper_row(Some(("0.1.0", false)));
        assert_eq!(stopped.status, MachineCheckStatus::Warn);
        assert_eq!(stopped.fix, Some(MachineCheckFix::InstallHelper));
        let none = helper_row(None);
        assert_eq!(none.status, MachineCheckStatus::Missing);
        assert_eq!(none.fix, Some(MachineCheckFix::InstallHelper));
    }

    /// The script asks only for versions and free space: no package manager, privilege or
    /// download tool appears in it, and it writes nowhere but `/dev/null`.
    #[test]
    fn the_script_installs_nothing() {
        let words: Vec<&str> = SCRIPT
            .split(|c: char| !(c.is_ascii_alphanumeric() || c == '-'))
            .collect();
        for forbidden in [
            "apt", "apt-get", "brew", "port", "yum", "dnf", "pacman", "zypper", "apk", "snap",
            "npm", "npx", "pip", "pip3", "sudo", "su", "doas", "curl", "wget", "install", "rm",
        ] {
            assert!(
                !words.contains(&forbidden),
                "{forbidden:?} in the check script"
            );
        }
        let redirects = SCRIPT.replace(">/dev/null", "").replace("2>&1", "");
        assert!(
            !redirects.contains('>'),
            "a redirect to a file: {redirects}"
        );
    }

    /// The script itself, run by this machine's `sh` with stand-ins first on its `PATH` (no ssh):
    /// it reports what they print, and none of the tripwires (package managers and the like)
    /// runs.
    #[cfg(unix)]
    #[test]
    fn the_script_runs_in_any_sh_and_reports_the_stand_ins() {
        use std::os::unix::fs::PermissionsExt as _;
        let tmp = tempfile::tempdir().unwrap();
        let bin = tmp.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        let tripped = tmp.path().join("tripped");
        let write = |name: &str, body: &str| {
            let path = bin.join(name);
            std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        };
        write("claude", "echo '2.1.3 (Claude Code)'");
        write(
            "gh",
            "printf '\\ngh version 2.45.0 (2024-03-04)\\nhttps://example.com\\n'",
        );
        write("codex", "echo 'boom' >&2; exit 3");
        for name in ["apt-get", "brew", "npm", "pip", "sudo", "curl", "wget"] {
            write(
                name,
                &format!("echo {name} >> '{}'; exit 1", tripped.display()),
            );
        }
        let mut path = bin.as_os_str().to_owned();
        path.push(":/usr/bin:/bin");
        let out = std::process::Command::new("/bin/sh")
            .args(["-c", SCRIPT, "sh", TAG])
            .env("PATH", &path)
            .env("HOME", tmp.path())
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
        let check = parse(&String::from_utf8_lossy(&out.stdout), TAG).unwrap();
        assert_eq!(
            by_id(&check, MachineCheckItem::CliClaude).detail,
            "2.1.3 (Claude Code)"
        );
        assert_eq!(
            by_id(&check, MachineCheckItem::Gh).detail,
            "gh version 2.45.0 (2024-03-04)",
            "the first line with something on it"
        );
        assert_eq!(
            by_id(&check, MachineCheckItem::CliCodex).status,
            MachineCheckStatus::Warn
        );
        assert_eq!(
            by_id(&check, MachineCheckItem::CliOpencode).status,
            MachineCheckStatus::Missing
        );
        assert_ne!(
            by_id(&check, MachineCheckItem::Disk).detail,
            "Free space could not be read."
        );
        assert!(
            !tripped.exists(),
            "{}",
            std::fs::read_to_string(&tripped).unwrap_or_default()
        );
    }
}
