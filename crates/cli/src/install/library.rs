//! The hub retains this opaque plan between preview and confirmation.
use super::*;
use pitcrew_protocol::onboarding::{HooksDiffFile, HooksEngine};

/// A preview that can only be constructed by the existing CLI installer.
/// Debug output deliberately omits private configuration values.
pub struct Installation {
    plans: Vec<Plan>,
}
impl std::fmt::Debug for Installation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Installation")
            .field("engines", &self.plans.len())
            .finish_non_exhaustive()
    }
}
impl Installation {
    /// Plan hooks for CLIs found on PATH or through existing homes, using the CLI's automatic hook form and no chaining.
    /// # Errors
    /// Invalid executable path or non-text/oversized configuration.
    pub fn preview(env: Env<'_>, exe: &Path) -> Result<Self> {
        if !exe.is_absolute()
            || !exe.is_file()
            || !is_our_exe_name(&exe.to_string_lossy())
                && !exe
                    .file_name()
                    .is_some_and(|n| is_our_exe_name(&n.to_string_lossy()))
        {
            return Err(Error::invalid(
                "The installed pitcrew executable is unavailable.",
            ));
        }
        let exe = exe
            .to_str()
            .ok_or_else(|| Error::invalid("Executable path is not UTF-8."))?;
        let mut plans = Vec::new();
        for target in Target::ALL {
            let home = match target {
                Target::Claude => super::config_dir(env, "CLAUDE_CONFIG_DIR", ".claude")?,
                Target::Codex => super::config_dir(env, "CODEX_HOME", ".codex")?,
                Target::OpenCode => opencode::plugin_dir(env)?
                    .parent()
                    .map(Path::to_path_buf)
                    .ok_or_else(|| Error::invalid("No OpenCode home."))?,
            };
            let data_home = target == Target::OpenCode
                && super::env_str(env, "XDG_DATA_HOME")
                    .map(PathBuf::from)
                    .or_else(|| super::user_home(env).map(|h| h.join(".local/share")))
                    .is_some_and(|h| h.join("opencode").is_dir());
            if found(env, target.name()) || home.is_dir() || data_home {
                // Bound input before the existing planners read it.
                let path = match target {
                    Target::Claude => claude::path(env),
                    Target::Codex => codex::path(env),
                    Target::OpenCode => opencode::path(env),
                }?;
                if std::fs::metadata(&path).is_ok_and(|m| m.len() > 1024 * 1024) {
                    return Err(Error::invalid("A hook configuration exceeds 1 MiB."));
                }
                plans.push(plan_install(target, env, exe, false, HookForm::Auto));
            }
        }
        let installation = Self { plans };
        installation.files()?;
        Ok(installation)
    }
    /// Exact replacements; content must never be logged.
    /// # Errors
    /// A configuration is not UTF-8.
    pub fn files(&self) -> Result<Vec<HooksDiffFile>> {
        self.plans
            .iter()
            .flat_map(|p| &p.changes)
            .map(|c| {
                let text = |bytes: &[u8]| {
                    String::from_utf8(bytes.to_vec())
                        .map_err(|_| Error::invalid("A hook configuration is not UTF-8."))
                };
                Ok(HooksDiffFile {
                    path: c.path.to_string_lossy().into_owned(),
                    before: c.before.as_deref().map(text).transpose()?,
                    after: text(&c.after)?,
                })
            })
            .collect()
    }
    /// Discovered engines, including conflicts and already-installed hooks.
    #[must_use]
    pub fn engines(&self) -> Vec<HooksEngine> {
        self.plans
            .iter()
            .map(|p| HooksEngine {
                engine: p.target.name().into(),
                status: p.status.label().into(),
                detail: p.detail.clone(),
            })
            .collect()
    }
    /// Engines skipped because their existing configuration conflicts.
    #[must_use]
    pub fn skipped(&self) -> Vec<String> {
        self.plans
            .iter()
            .filter(|p| p.status == Status::Conflicting)
            .map(|p| p.target.name().to_owned())
            .collect()
    }
    /// Retained bytes for the hub's memory budget.
    #[must_use]
    pub fn bytes(&self) -> usize {
        self.plans
            .iter()
            .flat_map(|p| &p.changes)
            .map(|c| c.after.len() + c.before.as_ref().map_or(0, Vec::len))
            .sum()
    }
    /// Apply the exact plan, refusing changed files before any write, and skipping applied files.
    /// # Errors
    /// A stale file or installer I/O error. Conflicting engines are skipped. Partial writes can be resumed with this same plan.
    pub fn apply(&self) -> Result<()> {
        let changes: Vec<_> = self
            .plans
            .iter()
            .filter(|p| p.status != Status::Conflicting)
            .flat_map(|p| &p.changes)
            .collect();
        for c in &changes {
            let current = read_optional(&c.path)?;
            if current != c.before && current.as_deref() != Some(c.after.as_slice()) {
                return Err(Error::new(
                    Kind::Conflict,
                    "A configuration changed since the preview. Preview again.",
                ));
            }
        }
        for c in changes {
            if read_optional(&c.path)?.as_deref() != Some(c.after.as_slice()) {
                apply_change(c)?;
            }
        }
        Ok(())
    }
}
fn found(env: Env<'_>, name: &str) -> bool {
    env("PATH").is_some_and(|path| {
        std::env::split_paths(&path)
            .filter(|dir| dir.is_absolute())
            .any(|dir| {
                let names = if cfg!(windows) {
                    vec![
                        format!("{name}.exe"),
                        format!("{name}.cmd"),
                        format!("{name}.bat"),
                    ]
                } else {
                    vec![name.into()]
                };
                names.iter().any(|n| dir.join(n).is_file())
            })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_preview_stale_refusal_backup_and_idempotence() {
        let temp = tempfile::tempdir().unwrap();
        let bin = temp.path().join("bin");
        let home = temp.path().join("home");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(home.join(".codex")).unwrap();
        let suffix = if cfg!(windows) { ".exe" } else { "" };
        let exe = bin.join(format!("pitcrew{suffix}"));
        std::fs::write(&exe, "synthetic executable").unwrap();
        std::fs::write(
            bin.join(format!("codex{suffix}")),
            "synthetic CLI, never run",
        )
        .unwrap();
        let path = home.join(".codex/config.toml");
        let original = "# synthetic configuration\nmodel = 'example-model'\n";
        std::fs::write(&path, original).unwrap();
        let env = |name: &str| match name {
            "PATH" => Some(bin.clone().into_os_string()),
            "HOME" | "USERPROFILE" => Some(home.clone().into_os_string()),
            _ => None,
        };
        let plan = Installation::preview(&env, &exe).unwrap();
        assert_eq!(plan.engines().len(), 1);
        let files = plan.files().unwrap();
        assert_eq!(files[0].before.as_deref(), Some(original));
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            original,
            "preview must not write"
        );
        std::fs::write(&path, "# concurrent edit\n").unwrap();
        assert_eq!(plan.apply().unwrap_err().kind, Kind::Conflict);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "# concurrent edit\n"
        );
        assert!(our_backups(&path).is_empty());
        std::fs::write(&path, original).unwrap();
        plan.apply().unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), files[0].after);
        assert_eq!(
            std::fs::read_to_string(&our_backups(&path)[0]).unwrap(),
            original
        );
        plan.apply().unwrap();
        assert_eq!(our_backups(&path).len(), 1);
        assert!(
            Installation::preview(&env, &exe)
                .unwrap()
                .files()
                .unwrap()
                .is_empty()
        );
        std::fs::write(&path, "notify = ['foreign-command']\n").unwrap();
        let conflict = Installation::preview(&env, &exe).unwrap();
        assert_eq!(conflict.engines()[0].status, "conflicting");
        conflict.apply().unwrap();
        assert_eq!(conflict.skipped(), vec!["codex"]);
    }
    #[test]
    fn homes_detect_engines_without_path_and_conflicts_do_not_block_other_plans() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        std::fs::create_dir_all(home.join(".codex")).unwrap();
        std::fs::create_dir_all(home.join(".config/opencode")).unwrap();
        let suffix = if cfg!(windows) { ".exe" } else { "" };
        let exe = temp.path().join(format!("pitcrew{suffix}"));
        std::fs::write(&exe, "synthetic executable, never run").unwrap();
        let config = home.join(".codex/config.toml");
        std::fs::write(&config, "notify = ['foreign-command']\n").unwrap();
        let env = |name: &str| match name {
            "HOME" | "USERPROFILE" => Some(home.clone().into_os_string()),
            _ => None,
        };
        let plan = Installation::preview(&env, &exe).unwrap();
        assert_eq!(plan.engines().len(), 2);
        assert_eq!(plan.skipped(), vec!["codex"]);
        let files = plan.files().unwrap();
        assert_eq!(files.len(), 1);
        assert!(files[0].path.ends_with("pitcrew.js"));
        plan.apply().unwrap();
        assert_eq!(
            std::fs::read_to_string(&config).unwrap(),
            "notify = ['foreign-command']\n"
        );
        assert_eq!(
            std::fs::read_to_string(&files[0].path).unwrap(),
            files[0].after
        );
        plan.apply().unwrap();
    }
}
