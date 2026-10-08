//! Keeping the programs a test starts away from the real user's folders.
//!
//! `pitcrewd` started without `--homes` watches "this user's" agent homes (`~/.claude`, `~/.codex`,
//! OpenCode's data folder), which hold the person's private transcripts, and it keeps its state in
//! "this user's" data folder; `pitcrew hooks` edits the agents' settings there. Each finds them
//! through the environment, and on Windows not only through `HOME`:
//!
//! - the agent homes come from `USERPROFILE` (or `HOME`), unless `CLAUDE_CONFIG_DIR`, `CODEX_HOME`
//!   or `XDG_DATA_HOME` point elsewhere;
//! - the data folder comes from Windows' known-folder lookup, which (on a default setup, without
//!   folder redirection) expands `%USERPROFILE%` from the process's own environment and ignores
//!   `LOCALAPPDATA`; the harnesses also pass `--state-dir` wherever a state is used;
//! - any other program a test's program starts may read `APPDATA`, `LOCALAPPDATA`, `HOMEDRIVE` and
//!   `HOMEPATH`, or `XDG_CONFIG_HOME` (`pitcrew hooks` does, for OpenCode).
//!
//! [`private_home`] points every one of them at a folder of the test's own, or removes it, and
//! [`check_private_home`] fails a test whose command would still reach the real ones. Every test
//! harness that starts `pitcrewd` or `pitcrew` calls both, so no test can reach a real home on any
//! platform.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The variables naming the user's home and profile folders: [`private_home`] sets each to the
/// test's own folder, or a folder inside it. Removing one is not enough: Windows then falls back
/// to the real profile.
pub const HOME_VARIABLES: [&str; 4] = ["HOME", "USERPROFILE", "APPDATA", "LOCALAPPDATA"];

/// The variables that send a lookup somewhere other than the home: [`private_home`] removes them.
/// A test may set one again, to a temporary folder of its own.
pub const REDIRECT_VARIABLES: [&str; 8] = [
    "CLAUDE_CONFIG_DIR",
    "CODEX_HOME",
    "XDG_DATA_HOME",
    "XDG_CONFIG_HOME",
    // PitCrew's cache folder, where confined runs (board drafts) get their private folders.
    "XDG_CACHE_HOME",
    "OPENCODE_CONFIG_DIR",
    "HOMEDRIVE",
    "HOMEPATH",
];

/// Gives `command` the home folder `home` (a folder in the test's temporary folder; it need not
/// exist): `HOME` and `USERPROFILE` are `home`, `APPDATA` and `LOCALAPPDATA` its `AppData\Roaming`
/// and `AppData\Local`, and every [`REDIRECT_VARIABLES`] is removed.
pub fn private_home<'a>(command: &'a mut Command, home: &Path) -> &'a mut Command {
    let app_data = home.join("AppData");
    command
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("APPDATA", app_data.join("Roaming"))
        .env("LOCALAPPDATA", app_data.join("Local"));
    for name in REDIRECT_VARIABLES {
        command.env_remove(name);
    }
    command
}

/// Panics unless `command`, as it would be started now, reaches no real home: every
/// [`HOME_VARIABLES`] is set, and every [`REDIRECT_VARIABLES`] is removed or set, to a folder
/// inside this process's temporary folder that is none of this process's own home folders. Call it
/// last, just before the command is started, after every variable the test adds.
pub fn check_private_home(command: &Command) {
    if let Err(why) = private_home_problem(command) {
        panic!("this test would start a program that can reach the real user's folders: {why}");
    }
}

/// Why `command` could reach a real home, if it could (see [`check_private_home`]).
fn private_home_problem(command: &Command) -> Result<(), String> {
    let set: HashMap<&OsStr, Option<&OsStr>> = command.get_envs().collect();
    let temp = std::env::temp_dir();
    // This process's own homes: the real ones, as the test runner inherited them.
    let real: Vec<PathBuf> = HOME_VARIABLES
        .iter()
        .filter_map(std::env::var_os)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .collect();
    let private = |name: &str, value: &OsStr| -> Result<(), String> {
        let path = Path::new(value);
        if path == temp || !path.starts_with(&temp) {
            return Err(format!(
                "{name} is {}, which is not inside the temporary folder {}",
                path.display(),
                temp.display()
            ));
        }
        if real.iter().any(|home| home == path) {
            return Err(format!("{name} is {}, a real home", path.display()));
        }
        Ok(())
    };
    for name in HOME_VARIABLES {
        match set.get(OsStr::new(name)) {
            Some(Some(value)) => private(name, value)?,
            Some(None) => return Err(format!("{name} is removed, not set to a folder of its own")),
            None => return Err(format!("{name} is inherited")),
        }
    }
    for name in REDIRECT_VARIABLES {
        match set.get(OsStr::new(name)) {
            Some(Some(value)) => private(name, value)?,
            Some(None) => {}
            None => return Err(format!("{name} is inherited")),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home() -> PathBuf {
        std::env::temp_dir().join("pitcrew-fixtures-home")
    }

    #[test]
    fn a_private_home_passes_and_points_everything_inside_it() {
        let home = home();
        let mut command = Command::new("program");
        private_home(&mut command, &home);
        private_home_problem(&command).unwrap();
        let envs: HashMap<&OsStr, Option<&OsStr>> = command.get_envs().collect();
        for name in HOME_VARIABLES {
            let value = envs[OsStr::new(name)].unwrap();
            assert!(Path::new(value).starts_with(&home), "{name}");
        }
        for name in REDIRECT_VARIABLES {
            assert_eq!(envs[OsStr::new(name)], None, "{name}");
        }
        // A test may point one of the agents' own variables at a temporary folder of its own.
        command.env(
            "CODEX_HOME",
            std::env::temp_dir().join("pitcrew-fixtures-codex"),
        );
        private_home_problem(&command).unwrap();
    }

    #[test]
    fn a_command_that_could_reach_a_real_home_is_refused() {
        let problem = |change: &dyn Fn(&mut Command)| {
            let mut command = Command::new("program");
            private_home(&mut command, &home());
            change(&mut command);
            private_home_problem(&command).unwrap_err()
        };
        // Nothing set at all: every variable is the runner's own.
        let bare = private_home_problem(&Command::new("program")).unwrap_err();
        assert!(bare.contains("inherited"), "{bare}");
        // Removed is not private: Windows falls back to the real profile.
        let removed = problem(&|c| {
            c.env_remove("USERPROFILE");
        });
        assert!(removed.contains("USERPROFILE is removed"), "{removed}");
        // Outside the temporary folder.
        let outside = problem(&|c| {
            c.env("LOCALAPPDATA", Path::new("/").join("pitcrew-elsewhere"));
        });
        assert!(outside.contains("LOCALAPPDATA"), "{outside}");
        // The temporary folder itself.
        let temp = problem(&|c| {
            c.env("APPDATA", std::env::temp_dir());
        });
        assert!(temp.contains("APPDATA"), "{temp}");
        // An agent's variable sent outside.
        let claude = problem(&|c| {
            c.env("CLAUDE_CONFIG_DIR", Path::new("/").join("pitcrew-claude"));
        });
        assert!(claude.contains("CLAUDE_CONFIG_DIR"), "{claude}");
        // This process's own home, wherever it is.
        if let Some(own) = std::env::var_os("HOME").filter(|v| !v.is_empty()) {
            let real = problem(&|c| {
                c.env("HOME", &own);
            });
            assert!(real.contains("HOME"), "{real}");
        }
    }
}
