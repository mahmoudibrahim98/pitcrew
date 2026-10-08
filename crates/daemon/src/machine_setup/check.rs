//! The machine check (`GET /v1/machines/{id}/check`): what this machine has for running agents.
//!
//! Each row runs at most one tool's version command (`claude --version`, `tmux -V`, …), found on
//! the daemon's `PATH`, bounded ([`VERSION_LIMIT`]), all at once. Nothing is installed, changed or
//! read besides what those commands print and the free space of the state directory's
//! filesystem. A row for a tool that is not there carries [`MachineCheckFix::InstallPage`]: the
//! client opens that tool's install page from its own table. SLURM's row is there only where
//! `sbatch` is; tmux's is left out on Windows, where PitCrew's terminals never use it.

use super::disk;
use super::tools::{Tools, first_line, major_minor, version_word};
use pitcrew_protocol::machine_setup::{
    MachineCheck, MachineCheckFix, MachineCheckItem, MachineCheckRow, MachineCheckStatus,
};
use pitcrew_protocol::runner::Capability;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// How long one version command may take. Node-based CLIs can take a few seconds on a cold start.
pub const VERSION_LIMIT: Duration = Duration::from_secs(15);

/// Below this much free space the disk row warns.
const LOW_DISK: u64 = 5_000_000_000;
/// Below this much it warns more strongly.
const VERY_LOW_DISK: u64 = 1_000_000_000;

/// What the check needs to know of the hub.
#[derive(Clone, Debug)]
pub struct CheckEnv {
    /// Where tools are found.
    pub tools: Tools,
    /// The folder whose filesystem's free space counts (the state directory).
    pub state: PathBuf,
    /// What PitCrew's terminals run in here (`tmux`, `pty`), if anything.
    pub runtime: Option<Capability>,
}

/// The rows for `only`, or for every item when `None`, in [`MachineCheckItem::ALL`]'s order.
pub async fn check(env: &CheckEnv, only: Option<MachineCheckItem>) -> MachineCheck {
    let items: Vec<MachineCheckItem> = MachineCheckItem::ALL
        .into_iter()
        .filter(|item| only.is_none_or(|o| o == *item))
        .collect();
    let mut tasks = tokio::task::JoinSet::new();
    for (index, item) in items.into_iter().enumerate() {
        let env = env.clone();
        tasks.spawn(async move { (index, row(&env, item).await) });
    }
    let mut rows: Vec<(usize, MachineCheckRow)> = Vec::new();
    while let Some(joined) = tasks.join_next().await {
        match joined {
            Ok((index, Some(row))) => rows.push((index, row)),
            Ok((_, None)) => {}
            Err(e) => tracing::error!(error = %e, "a machine check row failed"),
        }
    }
    rows.sort_by_key(|(index, _)| *index);
    MachineCheck {
        rows: rows.into_iter().map(|(_, row)| row).collect(),
    }
}

/// `item`'s row, or `None` where it does not apply here.
async fn row(env: &CheckEnv, item: MachineCheckItem) -> Option<MachineCheckRow> {
    match item {
        MachineCheckItem::CliClaude => Some(cli(env, item, "claude", "Claude Code").await),
        MachineCheckItem::CliCodex => Some(cli(env, item, "codex", "Codex").await),
        MachineCheckItem::CliOpencode => Some(cli(env, item, "opencode", "OpenCode").await),
        MachineCheckItem::Tmux => {
            if cfg!(windows) {
                None
            } else {
                Some(tmux(env).await)
            }
        }
        MachineCheckItem::Git => Some(
            tool(
                env,
                item,
                "git",
                &["--version"],
                "Not found on PATH: PitCrew reads projects' branches with it.",
            )
            .await,
        ),
        MachineCheckItem::Gh => Some(
            tool(
                env,
                item,
                "gh",
                &["--version"],
                "Not found on PATH: only GitHub's integration needs it.",
            )
            .await,
        ),
        MachineCheckItem::Disk => Some(disk_row(&env.state).await),
        MachineCheckItem::Slurm => slurm(env).await,
        // Only a check made before the helper is installed (over SSH) has this row: on the hub's
        // own machine, the hub is the helper.
        MachineCheckItem::Helper => None,
        _ => None,
    }
}

fn row_of(
    item: MachineCheckItem,
    status: MachineCheckStatus,
    detail: impl Into<String>,
    version: Option<String>,
    fix: Option<MachineCheckFix>,
) -> MachineCheckRow {
    MachineCheckRow {
        id: item,
        status,
        detail: detail.into(),
        version,
        fix,
    }
}

/// A tool that is not on `PATH`: missing, with its install page.
fn absent(item: MachineCheckItem, detail: &str) -> MachineCheckRow {
    row_of(
        item,
        MachineCheckStatus::Missing,
        detail,
        None,
        Some(MachineCheckFix::InstallPage),
    )
}

/// A tool found at `path` whose version command `args` ran: `ok` with its first line, else a
/// warning saying why not.
async fn versioned(
    env: &CheckEnv,
    item: MachineCheckItem,
    name: &str,
    path: &Path,
    args: &[&str],
) -> (MachineCheckRow, Option<String>) {
    let ran = env.tools.run(path, args, VERSION_LIMIT).await;
    let line = first_line(&ran.output());
    match line {
        Some(line) if ran.succeeded() => (
            row_of(
                item,
                MachineCheckStatus::Ok,
                line.clone(),
                Some(line.clone()),
                None,
            ),
            Some(line),
        ),
        _ => (
            row_of(
                item,
                MachineCheckStatus::Warn,
                format!(
                    "Found, but `{name} {}` did not work: {}.",
                    args.join(" "),
                    ran.why()
                ),
                None,
                None,
            ),
            None,
        ),
    }
}

async fn tool(
    env: &CheckEnv,
    item: MachineCheckItem,
    name: &str,
    args: &[&str],
    missing: &str,
) -> MachineCheckRow {
    match env.tools.find(name) {
        None => absent(item, missing),
        Some(path) => versioned(env, item, name, &path, args).await.0,
    }
}

async fn cli(env: &CheckEnv, item: MachineCheckItem, name: &str, label: &str) -> MachineCheckRow {
    tool(
        env,
        item,
        name,
        &["--version"],
        &format!("{label} ({name}) is not on PATH."),
    )
    .await
}

async fn tmux(env: &CheckEnv) -> MachineCheckRow {
    let item = MachineCheckItem::Tmux;
    let elsewhere = match env.runtime {
        Some(Capability::Pty) => " PitCrew's terminals run in pitcrew-ptyd instead.",
        Some(Capability::Tmux) => "",
        _ => " PitCrew has no other terminal runtime here, so it cannot open terminals.",
    };
    let Some(path) = env.tools.find("tmux") else {
        return absent(item, &format!("Not found on PATH.{elsewhere}"));
    };
    let (row, line) = versioned(env, item, "tmux", &path, &["-V"]).await;
    let Some(line) = line else {
        return row;
    };
    match version_word(&line).as_deref().and_then(major_minor) {
        Some(version) if version >= (3, 2) => row,
        Some(_) => row_of(
            item,
            MachineCheckStatus::Warn,
            format!("{line}: PitCrew's terminals need 3.2 or newer.{elsewhere}"),
            Some(line),
            Some(MachineCheckFix::InstallPage),
        ),
        None => row_of(
            item,
            MachineCheckStatus::Warn,
            format!("{line}: not a version PitCrew can read."),
            Some(line),
            None,
        ),
    }
}

async fn disk_row(state: &Path) -> MachineCheckRow {
    let item = MachineCheckItem::Disk;
    match disk::free_bytes(state).await {
        Ok(free) if free >= LOW_DISK => row_of(
            item,
            MachineCheckStatus::Ok,
            format!("{} free", disk::human(free)),
            None,
            None,
        ),
        Ok(free) if free >= VERY_LOW_DISK => row_of(
            item,
            MachineCheckStatus::Warn,
            format!(
                "Low: {} free where PitCrew keeps its state.",
                disk::human(free)
            ),
            None,
            None,
        ),
        Ok(free) => row_of(
            item,
            MachineCheckStatus::Warn,
            format!(
                "Very low: {} free where PitCrew keeps its state; agents and PitCrew may fail \
                 to write.",
                disk::human(free)
            ),
            None,
            None,
        ),
        Err(why) => {
            tracing::debug!(%why, "cannot read the state directory's free space");
            row_of(
                item,
                MachineCheckStatus::Warn,
                "Free space could not be read.",
                None,
                None,
            )
        }
    }
}

async fn slurm(env: &CheckEnv) -> Option<MachineCheckRow> {
    let item = MachineCheckItem::Slurm;
    let sbatch = env.tools.find("sbatch")?;
    let (row, line) = versioned(env, item, "sbatch", &sbatch, &["--version"]).await;
    // `sbatch` is there but did not answer: its warning row, as for any other tool.
    let Some(line) = line else {
        return Some(row);
    };
    let missing: Vec<&str> = ["squeue", "scancel"]
        .into_iter()
        .filter(|name| env.tools.find(name).is_none())
        .collect();
    if missing.is_empty() {
        return Some(row);
    }
    Some(row_of(
        item,
        MachineCheckStatus::Warn,
        format!(
            "{line}, but {} is not on PATH: PitCrew cannot follow or stop its jobs.",
            missing.join(" and ")
        ),
        Some(line),
        None,
    ))
}

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    fn script(dir: &Path, name: &str, body: &str) {
        crate::test_scripts::write_script(&dir.join(name), &format!("#!/bin/sh\n{body}\n"), 0o755);
    }

    fn env(bin: &Path, state: &Path, runtime: Option<Capability>) -> CheckEnv {
        CheckEnv {
            tools: Tools::with_path(OsString::from(bin)),
            state: state.to_path_buf(),
            runtime,
        }
    }

    fn by_id(check: &MachineCheck, item: MachineCheckItem) -> MachineCheckRow {
        check
            .rows
            .iter()
            .find(|row| row.id == item)
            .unwrap_or_else(|| panic!("no {item:?} row in {check:?}"))
            .clone()
    }

    #[tokio::test]
    async fn a_machine_with_nothing_has_install_pages_to_offer() {
        let tmp = tempfile::tempdir().unwrap();
        let bin = tmp.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        let check = check(&env(&bin, tmp.path(), None), None).await;
        let ids: Vec<_> = check.rows.iter().map(|row| row.id).collect();
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
            ],
            "no SLURM row without sbatch, and no helper row on the hub's own machine"
        );
        for row in &check.rows {
            if row.id == MachineCheckItem::Disk {
                assert_eq!(row.fix, None, "{row:?}");
                continue;
            }
            assert_eq!(row.status, MachineCheckStatus::Missing, "{row:?}");
            assert_eq!(row.fix, Some(MachineCheckFix::InstallPage), "{row:?}");
        }
        assert!(
            by_id(&check, MachineCheckItem::CliClaude)
                .detail
                .contains("Claude Code (claude) is not on PATH")
        );
        assert!(
            by_id(&check, MachineCheckItem::Tmux)
                .detail
                .contains("no other terminal runtime")
        );
    }

    #[tokio::test]
    async fn versions_old_tools_and_tools_that_fail() {
        let tmp = tempfile::tempdir().unwrap();
        let bin = tmp.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        script(&bin, "claude", "echo '2.1.3 (Claude Code)'");
        script(&bin, "codex", "echo 'codex-cli 0.50.0' >&2");
        script(&bin, "opencode", "echo 'broken' >&2; exit 2");
        script(&bin, "tmux", "echo 'tmux 3.0a'");
        script(
            &bin,
            "git",
            "printf '\\033[1mgit version 2.43.0\\033[0m\\n'",
        );
        script(
            &bin,
            "gh",
            "echo 'gh version 2.45.0 (2024-03-04)'; echo https://example.com",
        );
        script(&bin, "sbatch", "echo 'slurm 23.02.7'");
        script(&bin, "squeue", "exit 0");
        let check = check(&env(&bin, tmp.path(), Some(Capability::Pty)), None).await;

        let claude = by_id(&check, MachineCheckItem::CliClaude);
        assert_eq!(claude.status, MachineCheckStatus::Ok);
        assert_eq!(claude.detail, "2.1.3 (Claude Code)");
        assert_eq!(claude.version.as_deref(), Some("2.1.3 (Claude Code)"));
        assert_eq!(claude.fix, None);
        assert_eq!(
            by_id(&check, MachineCheckItem::CliCodex).detail,
            "codex-cli 0.50.0",
            "a version on stderr counts"
        );
        let opencode = by_id(&check, MachineCheckItem::CliOpencode);
        assert_eq!(opencode.status, MachineCheckStatus::Warn);
        assert_eq!(
            opencode.detail,
            "Found, but `opencode --version` did not work: it exited with 2."
        );
        let tmux = by_id(&check, MachineCheckItem::Tmux);
        assert_eq!(tmux.status, MachineCheckStatus::Warn);
        assert!(tmux.detail.contains("need 3.2 or newer"), "{tmux:?}");
        assert!(tmux.detail.contains("pitcrew-ptyd instead"), "{tmux:?}");
        assert_eq!(tmux.fix, Some(MachineCheckFix::InstallPage));
        assert_eq!(
            by_id(&check, MachineCheckItem::Git).detail,
            "git version 2.43.0",
            "escapes are dropped"
        );
        assert_eq!(
            by_id(&check, MachineCheckItem::Gh).detail,
            "gh version 2.45.0 (2024-03-04)"
        );
        let slurm = by_id(&check, MachineCheckItem::Slurm);
        assert_eq!(slurm.status, MachineCheckStatus::Warn);
        assert!(slurm.detail.contains("scancel is not on PATH"), "{slurm:?}");

        script(&bin, "tmux", "echo 'tmux 3.4'");
        script(&bin, "scancel", "exit 0");
        let one = super::check(
            &env(&bin, tmp.path(), Some(Capability::Tmux)),
            Some(MachineCheckItem::Tmux),
        )
        .await;
        assert_eq!(one.rows.len(), 1, "only the row asked for: {one:?}");
        assert_eq!(one.rows[0].status, MachineCheckStatus::Ok);
        assert_eq!(one.rows[0].detail, "tmux 3.4");
        let slurm = super::check(&env(&bin, tmp.path(), None), Some(MachineCheckItem::Slurm)).await;
        assert_eq!(slurm.rows[0].status, MachineCheckStatus::Ok);
        assert_eq!(slurm.rows[0].detail, "slurm 23.02.7");

        // An `sbatch` that is there but fails still has its row: a warning, as for any tool.
        script(&bin, "sbatch", "echo 'sbatch: error: broken' >&2; exit 1");
        let failing =
            super::check(&env(&bin, tmp.path(), None), Some(MachineCheckItem::Slurm)).await;
        assert_eq!(failing.rows.len(), 1, "the row is kept: {failing:?}");
        assert_eq!(failing.rows[0].status, MachineCheckStatus::Warn);
        assert_eq!(
            failing.rows[0].detail,
            "Found, but `sbatch --version` did not work: it exited with 1."
        );
        assert_eq!(failing.rows[0].fix, None);
    }

    #[tokio::test]
    async fn the_disk_row_says_what_is_free() {
        let tmp = tempfile::tempdir().unwrap();
        let row = disk_row(tmp.path()).await;
        assert_ne!(row.status, MachineCheckStatus::Missing);
        assert!(row.detail.contains("free"), "{row:?}");
        let gone = disk_row(&tmp.path().join("missing")).await;
        assert_eq!(gone.status, MachineCheckStatus::Warn);
        assert_eq!(gone.detail, "Free space could not be read.");
    }
}
