//! The Orchestrator in this process (api-v1.md, "Orchestrator"; hub-work's `orchestrator`): the
//! loop that follows answers from their transcripts ([`follow`]), the scratch folders its sessions
//! start in ([`scratch_folder`]), and whether an agent CLI is installed here ([`installed`]).
//!
//! - **Following**: about once a second ([`FOLLOW_EVERY`]), on the blocking pool,
//!   `WorkService::follow_orchestrator` reads each answering turn's transcript through the runner
//!   link and ends the turn when its transcript does, or when it passes its bounds. With nothing
//!   answering a look costs a lock and nothing else. The loop holds the work model only while it
//!   looks, so it ends with the daemon.
//! - **The scratch folder** of a person's sessions, `scratch/orchestrator-<member id>` in the
//!   state directory: private (0700), made if missing. For Claude Code it holds
//!   `.claude/settings.json` ([`CLAUDE_SETTINGS`]): `pitcrew`'s read verbs are allowed without a
//!   prompt, and editing files and the web are denied. Rewritten at every start, so it is always
//!   this version's. The CLI's token is the hard limit (it may only read); these settings keep the
//!   CLI from changing files as well, and anything else it tries waits for a person in its
//!   terminal, where the answer's time limit ends it.
//! - **Installed** means on the daemon's `PATH`, which the terminals' runtime passes to the CLIs
//!   it starts (with `PATHEXT` on Windows).

use pitcrew_hub_work::WorkService;
use pitcrew_protocol::model::Engine;
use std::path::{Path, PathBuf};
use std::sync::Weak;
use std::time::Duration;

/// How often answering turns are looked at.
pub const FOLLOW_EVERY: Duration = Duration::from_secs(1);

/// What a Claude Code session in a scratch folder may do: run `pitcrew`'s read verbs without
/// asking; never edit a file or reach the web.
pub const CLAUDE_SETTINGS: &str = r#"{
  "permissions": {
    "allow": [
      "Bash(pitcrew whoami)",
      "Bash(pitcrew search:*)",
      "Bash(pitcrew session list:*)",
      "Bash(pitcrew session show:*)",
      "Bash(pitcrew recap blocks:*)",
      "Bash(pitcrew recap days:*)",
      "Bash(pitcrew activity:*)",
      "Bash(pitcrew task list:*)",
      "Bash(pitcrew task show:*)"
    ],
    "deny": ["Edit", "MultiEdit", "Write", "NotebookEdit", "WebFetch", "WebSearch"]
  }
}"#;

/// Follows the Orchestrator's answers until the daemon stops. See the [module docs](self).
pub async fn follow(work: Weak<WorkService>) {
    loop {
        let Some(looking) = work.upgrade() else {
            return;
        };
        match tokio::task::spawn_blocking(move || looking.follow_orchestrator()).await {
            Ok(Ok(_)) => {}
            Ok(Err(e)) => tracing::warn!(error = %e, "cannot follow the Orchestrator's answers"),
            Err(e) => tracing::warn!(error = %e, "following the Orchestrator's answers failed"),
        }
        tokio::time::sleep(FOLLOW_EVERY).await;
    }
}

/// The scratch folder `name` under `root`, made if missing (private to this user), with
/// [`CLAUDE_SETTINGS`] in it; its path, resolved. `name` is letters, digits, `-` and `_` only.
///
/// # Errors
/// Why it cannot be made.
pub fn scratch_folder(root: &Path, name: &str) -> Result<PathBuf, String> {
    let plain = !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if !plain {
        return Err(format!("{name:?} is not a scratch folder's name"));
    }
    let folder = root.join(name);
    let claude = folder.join(".claude");
    for dir in [root, folder.as_path(), claude.as_path()] {
        crate::dispatch::private_dir(dir)
            .map_err(|e| format!("cannot make the scratch folder {}: {e}", dir.display()))?;
    }
    let settings = claude.join("settings.json");
    let value: serde_json::Value =
        serde_json::from_str(CLAUDE_SETTINGS).map_err(|e| format!("the settings: {e}"))?;
    crate::state::write_json(&settings, &value)
        .map_err(|e| format!("cannot write {}: {e}", settings.display()))?;
    folder
        .canonicalize()
        .map_err(|e| format!("cannot resolve {}: {e}", folder.display()))
}

/// The program each engine runs, as the runner starts it.
fn program(engine: Engine) -> Option<&'static str> {
    match engine {
        Engine::Claude => Some("claude"),
        Engine::Codex => Some("codex"),
        Engine::OpenCode => Some("opencode"),
        _ => None,
    }
}

/// Whether `engine`'s CLI is on this daemon's `PATH`.
#[must_use]
pub fn installed(engine: Engine) -> bool {
    let (Some(program), Some(path)) = (program(engine), std::env::var_os("PATH")) else {
        return false;
    };
    std::env::split_paths(&path).any(|dir| on(&dir, program))
}

/// Whether `dir` holds `program`, as something this user may run.
fn on(dir: &Path, program: &str) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::metadata(dir.join(program))
            .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        let extensions = std::env::var("PATHEXT").unwrap_or_else(|_| ".EXE;.CMD;.BAT".into());
        extensions
            .split(';')
            .filter(|e| !e.is_empty())
            .any(|e| dir.join(format!("{program}{e}")).is_file())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch folder is private, holds the Claude Code settings, and only plain names make one.
    #[test]
    fn a_scratch_folder_is_private_and_holds_the_settings() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("scratch");
        let folder = scratch_folder(&root, "orchestrator-01JB000000000000000MEM0001").unwrap();
        assert_eq!(
            folder,
            root.join("orchestrator-01JB000000000000000MEM0001")
                .canonicalize()
                .unwrap()
        );
        let settings: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(folder.join(".claude/settings.json")).unwrap(),
        )
        .unwrap();
        assert!(
            settings["permissions"]["deny"]
                .as_array()
                .unwrap()
                .contains(&"Write".into())
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(&root), 0o700);
            assert_eq!(mode(&folder), 0o700);
            assert_eq!(mode(&folder.join(".claude/settings.json")), 0o600);
        }
        // Made again: the same folder, the settings rewritten.
        std::fs::write(folder.join(".claude/settings.json"), "{}").unwrap();
        assert_eq!(
            scratch_folder(&root, "orchestrator-01JB000000000000000MEM0001").unwrap(),
            folder
        );
        assert!(
            std::fs::read_to_string(folder.join(".claude/settings.json"))
                .unwrap()
                .contains("pitcrew session list")
        );
        for bad in ["", "..", "a/b", "a\\b", "x y"] {
            assert!(scratch_folder(&root, bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn installed_means_on_the_path() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(!on(tmp.path(), "claude"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let file = tmp.path().join("claude");
            std::fs::write(&file, "#!/bin/sh\n").unwrap();
            assert!(!on(tmp.path(), "claude"), "not runnable");
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).unwrap();
            assert!(on(tmp.path(), "claude"));
        }
    }
}
