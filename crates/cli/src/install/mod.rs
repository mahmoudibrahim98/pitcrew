//! `pitcrew hooks status|diff|install|uninstall`: wires `pitcrew hook` into each agent CLI's own
//! configuration **safely**. See `docs/build/briefs/I-hook-install.md`.
//!
//! - [`claude`]: the `hooks` section of `settings.json`, edited surgically ([`jsontext`]) so
//!   unrelated keys, formatting and comments survive a round trip byte-for-byte.
//! - [`codex`]: the `notify` key of `config.toml`, edited with `toml_edit` (keeps comments and
//!   formatting). A pre-existing foreign `notify` is reported, never replaced, unless `--chain`.
//! - [`opencode`]: a small auto-discovered plugin file; nothing to merge, so install/uninstall is
//!   just writing or removing one file we marked as ours.
//!
//! Every entry we write is marked so it is unambiguously ours (an exact suffix of the command we
//! run, or a marker comment), so `install` is idempotent, `uninstall` removes only what we wrote,
//! and nothing a person or another tool wrote is ever touched.

mod claude;
mod codex;
mod difftext;
mod jsontext;
mod opencode;

use crate::config::Env;
use crate::error::{Error, Kind, Result};
use crate::{HooksAction, Io};
use std::fmt::Write as _;
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Which agent CLI a `hooks` command acts on.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Target {
    Claude,
    Codex,
    OpenCode,
}

impl Target {
    pub(crate) const ALL: [Self; 3] = [Self::Claude, Self::Codex, Self::OpenCode];

    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::OpenCode => "opencode",
        }
    }

    pub(crate) fn parse(s: &str) -> Result<Self> {
        match s.to_ascii_lowercase().as_str() {
            "claude" => Ok(Self::Claude),
            "codex" => Ok(Self::Codex),
            "opencode" => Ok(Self::OpenCode),
            _ => Err(Error::invalid(format!(
                "unknown engine {s:?}; use claude, codex or opencode"
            ))),
        }
    }
}

/// Where one engine's installation stands.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Status {
    /// Nothing of ours is present.
    Missing,
    /// Claude only: some but not all of the five events are wired up.
    Partial,
    /// Fully installed (chained after a foreign Codex `notify`, for Codex).
    Installed,
    /// Something of ours would collide with content we did not write; nothing was changed.
    Conflicting,
}

impl Status {
    fn label(self) -> &'static str {
        match self {
            Self::Missing => "missing",
            Self::Partial => "partial",
            Self::Installed => "installed",
            Self::Conflicting => "conflicting",
        }
    }
}

/// One file a plan would change.
pub(crate) struct Change {
    path: PathBuf,
    /// The file's current bytes, or `None` if it does not exist yet.
    before: Option<Vec<u8>>,
    /// Its content after the change; meaningless when `delete` is set.
    after: Vec<u8>,
    /// Remove the file instead of writing `after` (OpenCode uninstall; the Codex chain wrapper).
    delete: bool,
    /// Set the file's permissions so it can be run directly (the Unix chain wrapper).
    executable: bool,
}

/// One engine's situation, and what `install` (or `uninstall`) would do about it.
pub(crate) struct Plan {
    target: Target,
    status: Status,
    /// What `status`/`diff` shows beyond the one-word status.
    detail: String,
    /// Empty when there is nothing to do.
    changes: Vec<Change>,
}

/// Reads an environment variable that is set and non-empty, as a (possibly lossily converted)
/// `String`: these are directory names, never secret, so losing an unpaired surrogate on a
/// mis-encoded Windows variable is an acceptable, visible degradation.
fn env_str(env: Env<'_>, name: &str) -> Option<String> {
    env(name)
        .filter(|v| !v.is_empty())
        .map(|v| v.to_string_lossy().into_owned())
}

/// The user's home folder: `HOME`, else `USERPROFILE`.
fn user_home(env: Env<'_>) -> Option<PathBuf> {
    env_str(env, "HOME")
        .or_else(|| env_str(env, "USERPROFILE"))
        .map(PathBuf::from)
}

/// `$<var_name>`, else `<home>/<default_subdir>`.
fn config_dir(env: Env<'_>, var_name: &str, default_subdir: &str) -> Result<PathBuf> {
    if let Some(dir) = env_str(env, var_name) {
        return Ok(PathBuf::from(dir));
    }
    user_home(env).map(|h| h.join(default_subdir)).ok_or_else(|| {
        Error::invalid(format!(
            "cannot find the home directory for {var_name}'s default; set {var_name} or HOME"
        ))
    })
}

/// Set (in debug builds only) to make `install`/`diff` act as if `pitcrew` were installed at this
/// path, so tests can exercise quoting of spaces and quotes without depending on where the test
/// binary itself happens to live.
const EXE_OVERRIDE_VAR: &str = "PITCREW_INSTALL_EXE_FOR_TEST";

/// The absolute path of the current `pitcrew` executable, to write into each CLI's config.
fn exe_path(env: Env<'_>) -> Result<String> {
    #[cfg(debug_assertions)]
    if let Some(path) = env_str(env, EXE_OVERRIDE_VAR) {
        return Ok(path);
    }
    let _ = env;
    std::env::current_exe()
        .map_err(|e| Error::internal(format!("cannot find pitcrew's own executable path: {e}")))
        .map(|p| p.to_string_lossy().into_owned())
}

/// Quotes one argument for POSIX `sh`: wrapped in single quotes, with embedded single quotes
/// escaped as `'\''`. Safe for any text, including spaces and double quotes.
#[must_use]
pub(crate) fn shell_quote_unix(s: &str) -> String {
    if !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'/' | b'_' | b'-' | b'.' | b':'))
    {
        s.to_owned()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

/// Quotes one argument for `cmd.exe`: wrapped in double quotes when it contains whitespace or a
/// quote (Windows paths cannot contain a `"`, so this is mainly defensive).
#[must_use]
pub(crate) fn cmd_quote_windows(s: &str) -> String {
    if !s.is_empty() && !s.bytes().any(|b| matches!(b, b' ' | b'\t' | b'"')) {
        s.to_owned()
    } else {
        format!("\"{}\"", s.replace('"', "\"\""))
    }
}

/// Quotes the pitcrew executable's path as one shell word, for the platform this `install` runs
/// on (the platform the config will be read on).
#[must_use]
pub(crate) fn quote_exe_path(path: &str) -> String {
    if cfg!(windows) {
        cmd_quote_windows(path)
    } else {
        shell_quote_unix(path)
    }
}

fn read_optional(path: &Path) -> Result<Option<Vec<u8>>> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(Error::internal(format!("cannot read {}: {e}", path.display()))),
    }
}

/// Runs one `hooks` subcommand.
pub(crate) fn dispatch(action: HooksAction, env: Env<'_>, io: &mut Io<'_>, json: bool) -> Result<()> {
    let (engine, action, yes, chain) = match action {
        HooksAction::Status { engine } => (engine.engine, Action::Status, false, false),
        HooksAction::Diff { engine, chain } => (engine.engine, Action::Diff, false, chain),
        HooksAction::Install { engine, yes, chain } => (engine.engine, Action::Install, yes, chain),
        HooksAction::Uninstall { engine, yes } => (engine.engine, Action::Uninstall, yes, false),
    };
    let targets: Vec<Target> = match engine {
        Some(name) => vec![Target::parse(&name)?],
        None => Target::ALL.to_vec(),
    };
    run(action, &targets, env, io, json, yes, chain)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Action {
    Status,
    Diff,
    Install,
    Uninstall,
}

fn run(
    action: Action,
    targets: &[Target],
    env: Env<'_>,
    io: &mut Io<'_>,
    json: bool,
    yes: bool,
    chain: bool,
) -> Result<()> {
    let plans = if action == Action::Uninstall {
        targets
            .iter()
            .map(|&t| plan_uninstall(t, env))
            .collect::<Result<Vec<_>>>()?
    } else {
        let exe = exe_path(env)?;
        targets
            .iter()
            .map(|&t| plan_install(t, env, &exe, chain))
            .collect::<Result<Vec<_>>>()?
    };

    match action {
        Action::Status => print_status(io, &plans, json),
        Action::Diff => print_diff(io, &plans, json),
        Action::Install | Action::Uninstall => {
            apply_plans(io, &plans, json, yes, action == Action::Uninstall)
        }
    }
}

fn plan_install(target: Target, env: Env<'_>, exe: &str, chain: bool) -> Result<Plan> {
    match target {
        Target::Claude => claude::plan_install(env, exe),
        Target::Codex => codex::plan_install(env, exe, chain),
        Target::OpenCode => opencode::plan_install(env, exe),
    }
}

fn plan_uninstall(target: Target, env: Env<'_>) -> Result<Plan> {
    match target {
        Target::Claude => claude::plan_uninstall(env),
        Target::Codex => codex::plan_uninstall(env),
        Target::OpenCode => opencode::plan_uninstall(env),
    }
}

fn write_err(e: std::io::Error) -> Error {
    Error::internal(format!("cannot write the output: {e}"))
}

fn write_text(io: &mut Io<'_>, text: &str) -> Result<()> {
    io.stdout.write_all(text.as_bytes()).map_err(write_err)
}

fn write_json(io: &mut Io<'_>, value: &serde_json::Value) -> Result<()> {
    let mut text = serde_json::to_string_pretty(value)
        .map_err(|e| Error::internal(format!("cannot encode the output: {e}")))?;
    text.push('\n');
    write_text(io, &text)
}

fn render_diff(c: &Change) -> String {
    let path = c.path.display().to_string();
    let before = c
        .before
        .as_deref()
        .map(|b| String::from_utf8_lossy(b).into_owned())
        .unwrap_or_default();
    let after = if c.delete {
        String::new()
    } else {
        String::from_utf8_lossy(&c.after).into_owned()
    };
    difftext::unified(&path, &before, &after)
}

fn print_status(io: &mut Io<'_>, plans: &[Plan], json: bool) -> Result<()> {
    if json {
        let value: Vec<serde_json::Value> = plans
            .iter()
            .map(|p| {
                serde_json::json!({
                    "engine": p.target.name(),
                    "status": p.status.label(),
                    "detail": p.detail,
                })
            })
            .collect();
        return write_json(io, &serde_json::Value::Array(value));
    }
    let mut out = String::new();
    for p in plans {
        let _ = writeln!(out, "{:<9} {:<11} {}", p.target.name(), p.status.label(), p.detail);
    }
    write_text(io, &out)
}

fn print_diff(io: &mut Io<'_>, plans: &[Plan], json: bool) -> Result<()> {
    if json {
        let value: Vec<serde_json::Value> = plans
            .iter()
            .map(|p| {
                let changes: Vec<serde_json::Value> = p
                    .changes
                    .iter()
                    .map(|c| {
                        serde_json::json!({
                            "path": c.path.display().to_string(),
                            "diff": render_diff(c),
                        })
                    })
                    .collect();
                serde_json::json!({
                    "engine": p.target.name(),
                    "status": p.status.label(),
                    "detail": p.detail,
                    "changes": changes,
                })
            })
            .collect();
        return write_json(io, &serde_json::Value::Array(value));
    }
    let mut out = String::new();
    for p in plans {
        let _ = writeln!(out, "## {} \u{2014} {} ({})", p.target.name(), p.status.label(), p.detail);
        if p.changes.is_empty() {
            out.push_str("(no change)\n\n");
            continue;
        }
        for c in &p.changes {
            out.push_str(&render_diff(c));
        }
        out.push('\n');
    }
    write_text(io, &out)
}

fn conflict_error(conflicts: &[&Plan]) -> Error {
    let names: Vec<&str> = conflicts.iter().map(|p| p.target.name()).collect();
    Error::new(
        Kind::Conflict,
        format!("not changed because of a conflict: {}", names.join(", ")),
    )
}

fn apply_plans(io: &mut Io<'_>, plans: &[Plan], json: bool, yes: bool, uninstalling: bool) -> Result<()> {
    let pending: Vec<&Plan> = plans.iter().filter(|p| !p.changes.is_empty()).collect();
    let conflicts: Vec<&Plan> = plans
        .iter()
        .filter(|p| p.status == Status::Conflicting)
        .collect();

    if pending.is_empty() {
        print_status(io, plans, json)?;
        return if conflicts.is_empty() {
            Ok(())
        } else {
            Err(conflict_error(&conflicts))
        };
    }

    if !yes {
        let mut preview = String::new();
        for p in &pending {
            let _ = writeln!(preview, "## {} \u{2014} {}", p.target.name(), p.detail);
            for c in &p.changes {
                preview.push_str(&render_diff(c));
            }
            preview.push('\n');
        }
        write_text(io, &preview)?;
        let verb = if uninstalling { "Remove" } else { "Make" };
        if !confirm(io, &format!("{verb} these changes? [y/N] "))? {
            return Err(Error::invalid("not confirmed; nothing was changed"));
        }
    }

    for p in &pending {
        for c in &p.changes {
            apply_change(c)?;
        }
    }

    print_status(io, plans, json)?;
    if conflicts.is_empty() {
        Ok(())
    } else {
        Err(conflict_error(&conflicts))
    }
}

/// Asks a yes/no question on stdout and reads one line from stdin (capped, since this is a
/// person answering, not a payload).
fn confirm(io: &mut Io<'_>, prompt: &str) -> Result<bool> {
    if !io.stdin_is_terminal {
        return Err(Error::invalid(
            "refusing to proceed without a terminal to confirm on; rerun with --yes",
        ));
    }
    write_text(io, prompt)?;
    io.stdout.flush().map_err(write_err)?;
    let mut answer = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        match io.stdin.read(&mut byte) {
            Ok(0) => break,
            Ok(_) => {
                if byte[0] == b'\n' {
                    break;
                }
                if answer.len() < 16 {
                    answer.push(byte[0]);
                }
            }
            Err(_) => break,
        }
    }
    let text = String::from_utf8_lossy(&answer);
    let text = text.trim().to_ascii_lowercase();
    Ok(text == "y" || text == "yes")
}

fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    path.with_file_name(format!("{name}{suffix}"))
}

fn now_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

#[cfg(unix)]
fn copy_permissions(original: &Path, tmp: &Path) -> Result<()> {
    if let Ok(meta) = std::fs::metadata(original) {
        std::fs::set_permissions(tmp, meta.permissions()).map_err(|e| {
            Error::internal(format!("cannot set permissions on {}: {e}", tmp.display()))
        })?;
    }
    Ok(())
}
#[cfg(not(unix))]
fn copy_permissions(_original: &Path, _tmp: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn make_executable(tmp: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(tmp, std::fs::Permissions::from_mode(0o755))
        .map_err(|e| Error::internal(format!("cannot make {} executable: {e}", tmp.display())))
}
#[cfg(not(unix))]
fn make_executable(_tmp: &Path) -> Result<()> {
    Ok(())
}

/// Backs up the existing file (if any), then writes atomically: a temp file in the same
/// directory, with the original's permissions copied over, renamed into place.
fn apply_change(c: &Change) -> Result<()> {
    let dir = c
        .path
        .parent()
        .ok_or_else(|| Error::internal("the path has no parent directory"))?;
    std::fs::create_dir_all(dir)
        .map_err(|e| Error::internal(format!("cannot create {}: {e}", dir.display())))?;

    if c.before.is_some() {
        let backup = sibling(&c.path, &format!(".pitcrew-backup-{}", now_millis()));
        std::fs::copy(&c.path, &backup).map_err(|e| {
            Error::internal(format!(
                "cannot back up {} to {}: {e}",
                c.path.display(),
                backup.display()
            ))
        })?;
    }

    if c.delete {
        return std::fs::remove_file(&c.path)
            .map_err(|e| Error::internal(format!("cannot remove {}: {e}", c.path.display())));
    }

    let tmp = sibling(&c.path, &format!(".pitcrew-tmp-{}", std::process::id()));
    let result = std::fs::write(&tmp, &c.after)
        .map_err(|e| Error::internal(format!("cannot write {}: {e}", tmp.display())))
        .and_then(|()| copy_permissions(&c.path, &tmp))
        .and_then(|()| if c.executable { make_executable(&tmp) } else { Ok(()) })
        .and_then(|()| {
            std::fs::rename(&tmp, &c.path)
                .map_err(|e| Error::internal(format!("cannot replace {}: {e}", c.path.display())))
        });
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unix_quoting_handles_spaces_and_quotes() {
        assert_eq!(shell_quote_unix("/usr/bin/pitcrew"), "/usr/bin/pitcrew");
        assert_eq!(
            shell_quote_unix("/home/sam/my apps/pitcrew"),
            "'/home/sam/my apps/pitcrew'"
        );
        assert_eq!(
            shell_quote_unix("it's/pitcrew"),
            r"'it'\''s/pitcrew'"
        );
    }

    #[test]
    fn windows_quoting_handles_spaces_and_quotes() {
        assert_eq!(
            cmd_quote_windows(r"C:\Users\sam\pitcrew.exe"),
            r"C:\Users\sam\pitcrew.exe"
        );
        assert_eq!(
            cmd_quote_windows(r"C:\Program Files\PitCrew\pitcrew.exe"),
            "\"C:\\Program Files\\PitCrew\\pitcrew.exe\""
        );
        assert_eq!(cmd_quote_windows("a\"b"), "\"a\"\"b\"");
    }

    #[test]
    fn target_names_parse_case_insensitively() {
        assert!(Target::parse("Claude").is_ok());
        assert!(Target::parse("CODEX").is_ok());
        assert!(Target::parse("opencode").is_ok());
        assert!(Target::parse("gemini").is_err());
    }
}
