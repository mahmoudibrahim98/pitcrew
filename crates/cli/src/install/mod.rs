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
//! Every entry we write is marked so it is unambiguously ours — by the identity of the program in
//! its command (not just a text suffix a foreign command could coincidentally share) or by a
//! marker comment — so `install` is idempotent, `uninstall` removes only what we wrote, and
//! nothing a person or another tool wrote is ever touched. **This tool edits a person's own
//! configuration files: when anything is not certain, it refuses and reports instead of
//! guessing**, including refusing to proceed if a file changed on disk since it was last read.
//!
//! One engine failing to plan (a malformed file, say) never blocks the others: each engine's plan
//! is computed independently, and a failure becomes that one engine's `Conflicting` status rather
//! than aborting the whole command (`unwrap_or_conflict`).

mod claude;
mod codex;
mod difftext;
mod hook_form;
mod jsontext;
pub(crate) use hook_form::HookForm;
mod opencode;

use crate::config::Env;
use crate::error::{Error, Kind, Result};
use crate::{HooksAction, Io};
use std::fmt::Write as _;
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
    /// Fully installed.
    Installed,
    /// Installed, but an executable path or hook form changed, or owned hooks are duplicated;
    /// `install` refreshes them.
    Stale,
    /// Something of ours would collide with content we did not write, or the file could not be
    /// read/parsed/trusted; nothing was changed. The detail says why.
    Conflicting,
}

impl Status {
    fn label(self) -> &'static str {
        match self {
            Self::Missing => "missing",
            Self::Partial => "partial",
            Self::Installed => "installed",
            Self::Stale => "stale",
            Self::Conflicting => "conflicting",
        }
    }
}

/// One file a plan would change.
pub(crate) struct Change {
    path: PathBuf,
    /// The file's current bytes, or `None` if it does not exist yet. Checked again, immediately
    /// before writing, against what is actually on disk.
    before: Option<Vec<u8>>,
    /// Its content after the change; meaningless when `delete` is set.
    after: Vec<u8>,
    /// Remove the file instead of writing `after` (OpenCode uninstall; the Codex chain's
    /// sidecar, on uninstall).
    delete: bool,
    /// Set the file's permissions so it can be run directly. Unused today (nothing this module
    /// writes needs to be executable since the Codex chain stopped using a wrapper script), kept
    /// so `apply_change` already does the right thing if that ever changes again.
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

/// The user's home folder: on Windows, `USERPROFILE` (what every native tool there uses), else
/// `HOME`; falling back to the other if the preferred one is unset.
fn user_home(env: Env<'_>) -> Option<PathBuf> {
    let (first, second) = if cfg!(windows) {
        ("USERPROFILE", "HOME")
    } else {
        ("HOME", "USERPROFILE")
    };
    env_str(env, first)
        .or_else(|| env_str(env, second))
        .map(PathBuf::from)
}

/// `$<var_name>`, else `<home>/<default_subdir>`.
fn config_dir(env: Env<'_>, var_name: &str, default_subdir: &str) -> Result<PathBuf> {
    if let Some(dir) = env_str(env, var_name) {
        return Ok(PathBuf::from(dir));
    }
    user_home(env)
        .map(|h| h.join(default_subdir))
        .ok_or_else(|| {
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

/// Whether `word` safely names a single program and nothing else: either wrapped start-to-end in
/// one matching pair of quotes (`'…'` or `"…"`), with the quote character never appearing again
/// inside except as the escape this module's own quoting produces (`'\''`, `""`), or free of
/// whitespace and of the characters a shell gives a second meaning to (`; & | < > ( ) $` and a
/// backtick). Without this check, `quoted_word_file_name`'s "take the last path segment" would
/// read the program name out of something like `afplay ding.aiff; ~/bin/pitcrew` — a multi-word
/// foreign command that merely ends by mentioning `pitcrew` — and wrongly call it ours; checking
/// only the first and last byte of a *quoted* word has exactly the same hole one level up, e.g.
/// `'/usr/bin/afplay' ding.aiff; '/home/u/bin/pitcrew'` starts and ends with `'` but is still two
/// words with an unescaped `'` closing the first one early.
#[must_use]
pub(crate) fn is_single_shell_word(word: &str) -> bool {
    if word.len() >= 2 {
        let bytes = word.as_bytes();
        let inner = &bytes[1..bytes.len() - 1];
        if bytes[0] == b'\'' && bytes[bytes.len() - 1] == b'\'' {
            return no_unescaped_quote(inner, b'\'', b"'\\''");
        }
        if bytes[0] == b'"' && bytes[bytes.len() - 1] == b'"' {
            return no_unescaped_quote(inner, b'"', b"\"\"");
        }
    }
    !word.is_empty()
        && !word.bytes().any(|b| {
            b.is_ascii_whitespace()
                || matches!(
                    b,
                    b';' | b'&' | b'|' | b'<' | b'>' | b'(' | b')' | b'$' | b'`'
                )
        })
}

/// Whether `quote` only ever appears in `inner` as part of `escape` (the full byte sequence this
/// module's own quoting uses for one embedded literal quote character) — never bare, which would
/// mean the word's own closing quote came early and whatever follows is a second, unquoted word.
fn no_unescaped_quote(inner: &[u8], quote: u8, escape: &[u8]) -> bool {
    let mut i = 0;
    while i < inner.len() {
        if inner[i] == quote {
            if inner[i..].starts_with(escape) {
                i += escape.len();
            } else {
                return false;
            }
        } else {
            i += 1;
        }
    }
    true
}

/// The file name of the program a (possibly quoted) command-line word names, case-insensitively
/// comparable: used to check that a hook's program is actually `pitcrew`/`pitcrew.exe`, not just
/// some other command that happens to end the same way. Understands the two quoting styles this
/// module itself writes (`'…'` with `'\''`, `"…"` with `""`); an unquoted word is taken as-is.
/// Callers that have not already checked [`is_single_shell_word`] should: this function alone
/// does not protect against a multi-word command that merely ends by mentioning a path.
#[must_use]
pub(crate) fn quoted_word_file_name(word: &str) -> String {
    let inner = if word.len() >= 2 && word.starts_with('\'') && word.ends_with('\'') {
        word[1..word.len() - 1].replace("'\\''", "'")
    } else if word.len() >= 2 && word.starts_with('"') && word.ends_with('"') {
        word[1..word.len() - 1].replace("\"\"", "\"")
    } else {
        word.to_owned()
    };
    // Deliberately not `std::path::Path`: its separator rules follow the *compiled* target, not
    // whichever platform's path this text happens to be — a Windows path quoted into a Claude
    // Code command can be inspected by code built for either platform. Accepting either
    // separator, on either platform, gets the file name right regardless.
    inner
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(&inner)
        .to_owned()
}

/// Whether a file name is ours: `pitcrew`, optionally with a `.exe`/`.EXE` suffix, nothing else.
/// Case-insensitive so it still matches a Windows path however it was typed.
#[must_use]
pub(crate) fn is_our_exe_name(file_name: &str) -> bool {
    file_name.eq_ignore_ascii_case("pitcrew") || file_name.eq_ignore_ascii_case("pitcrew.exe")
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

/// A leading UTF-8 byte-order mark, which neither `serde_json` nor `toml_edit` accepts as part of
/// a valid document.
pub(crate) const BOM: char = '\u{feff}';

/// Splits a leading BOM off `text`, if there is one, so it can be handed to a parser that does
/// not accept one; pair with [`with_bom`] to put it back before writing.
#[must_use]
pub(crate) fn split_bom(text: &str) -> (bool, &str) {
    text.strip_prefix(BOM)
        .map_or((false, text), |rest| (true, rest))
}

/// Puts a BOM back on the front of `text` if `had_bom` says there was one and it is not there
/// already.
#[must_use]
pub(crate) fn with_bom(had_bom: bool, mut text: String) -> String {
    if had_bom && !text.starts_with(BOM) {
        text.insert(0, BOM);
    }
    text
}

fn read_optional(path: &Path) -> Result<Option<Vec<u8>>> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(Error::internal(format!(
            "cannot read {}: {e}",
            path.display()
        ))),
    }
}

/// Runs one `hooks` subcommand.
pub(crate) fn dispatch(
    action: HooksAction,
    env: Env<'_>,
    io: &mut Io<'_>,
    json: bool,
) -> Result<()> {
    let (engine, action, yes, chain, hook_form) = match action {
        HooksAction::Status { engine } => {
            (engine.engine, Action::Status, false, false, HookForm::Auto)
        }
        HooksAction::Diff {
            engine,
            chain,
            hook_form,
        } => (engine.engine, Action::Diff, false, chain, hook_form),
        HooksAction::Install {
            engine,
            yes,
            chain,
            hook_form,
        } => (engine.engine, Action::Install, yes, chain, hook_form),
        HooksAction::Uninstall { engine, yes } => {
            (engine.engine, Action::Uninstall, yes, false, HookForm::Auto)
        }
    };
    let targets: Vec<Target> = match engine {
        Some(name) => vec![Target::parse(&name)?],
        None => Target::ALL.to_vec(),
    };
    run(
        action,
        &targets,
        env,
        io,
        Options {
            json,
            yes,
            chain,
            hook_form,
        },
    )
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Action {
    Status,
    Diff,
    Install,
    Uninstall,
}

struct Options {
    json: bool,
    yes: bool,
    chain: bool,
    hook_form: HookForm,
}

fn run(
    action: Action,
    targets: &[Target],
    env: Env<'_>,
    io: &mut Io<'_>,
    options: Options,
) -> Result<()> {
    let Options {
        json,
        yes,
        chain,
        hook_form,
    } = options;
    let changing = action == Action::Install || action == Action::Uninstall;
    if changing && json && !yes {
        return Err(Error::invalid(
            "--json needs --yes with install/uninstall: a confirmation prompt cannot be mixed \
             into JSON output",
        ));
    }

    let plans: Vec<Plan> = if action == Action::Uninstall {
        targets.iter().map(|&t| plan_uninstall(t, env)).collect()
    } else {
        let exe = exe_path(env)?;
        targets
            .iter()
            .map(|&t| plan_install(t, env, &exe, chain, hook_form))
            .collect()
    };

    match action {
        Action::Status => print_status(io, &plans, json),
        Action::Diff => print_diff(io, &plans, json),
        Action::Install | Action::Uninstall => {
            apply_plans(io, &plans, json, yes, action == Action::Uninstall)
        }
    }
}

/// Turns a planning failure into that one engine's `Conflicting` status instead of letting it
/// abort the whole command: a malformed Claude `settings.json`, say, must never hide whatever
/// Codex's and OpenCode's own status is.
fn unwrap_or_conflict(target: Target, result: Result<Plan>) -> Plan {
    result.unwrap_or_else(|e| Plan {
        target,
        status: Status::Conflicting,
        detail: e.message,
        changes: vec![],
    })
}

fn plan_install(target: Target, env: Env<'_>, exe: &str, chain: bool, hook_form: HookForm) -> Plan {
    let result = match target {
        Target::Claude => claude::plan_install_with_form(env, exe, hook_form),
        Target::Codex => codex::plan_install(env, exe, chain),
        Target::OpenCode => opencode::plan_install(env, exe),
    };
    unwrap_or_conflict(target, result)
}

fn plan_uninstall(target: Target, env: Env<'_>) -> Plan {
    let result = match target {
        Target::Claude => claude::plan_uninstall(env),
        Target::Codex => codex::plan_uninstall(env),
        Target::OpenCode => opencode::plan_uninstall(env),
    };
    unwrap_or_conflict(target, result)
}

/// The original `notify` command recorded when Codex's `notify` was chained (`install --chain`),
/// for `pitcrew hook codex notify --chain` to run after delivering our own hook. `None` if there
/// is no chain, or its record cannot be read — silently: a hook must never fail loudly just
/// because the thing it forwards to is briefly unavailable.
#[must_use]
pub(crate) fn codex_chained_original(env: Env<'_>) -> Option<Vec<String>> {
    codex::chained_original(env)
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
        let _ = writeln!(
            out,
            "{:<9} {:<11} {}",
            p.target.name(),
            p.status.label(),
            p.detail
        );
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
        let _ = writeln!(
            out,
            "## {} \u{2014} {} ({})",
            p.target.name(),
            p.status.label(),
            p.detail
        );
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

fn apply_plans(
    io: &mut Io<'_>,
    plans: &[Plan],
    json: bool,
    yes: bool,
    uninstalling: bool,
) -> Result<()> {
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

/// If `path` is a symlink, the file it resolves to (so a write lands on the real file and the
/// link itself is left alone — `rename`-ing something new onto the link's own path would replace
/// the link with a plain file instead); otherwise `path` unchanged. Refuses a link that cannot be
/// resolved (broken, or a loop) rather than guessing what to do with it.
fn resolve_write_target(path: &Path) -> Result<PathBuf> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => std::fs::canonicalize(path).map_err(|e| {
            Error::invalid(format!(
                "{} is a symlink that cannot be resolved ({e}); point it at a real file \
                     first",
                path.display()
            ))
        }),
        _ => Ok(path.to_owned()),
    }
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

/// Opens `path` for writing, failing if it already exists (so a concurrent/predicted name never
/// clobbers someone else's file), private from the moment it is created (0600 on Unix — there is
/// no window where the temp file is readable by anyone else before permissions are tightened).
fn create_private(path: &Path) -> Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    options
        .open(path)
        .map_err(|e| Error::internal(format!("cannot create {}: {e}", path.display())))
}

/// An unpredictable sibling file name: a monotonic clock reading, the process id, and the
/// address of a fresh heap allocation (randomised by ASLR) mixed together. Not a cryptographic
/// requirement — these files live in the user's own config directory, not a shared one — but a
/// predictable name is an unforced TOCTOU risk `create_new` alone does not fully remove.
fn unpredictable_suffix() -> u64 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let marker = Box::new(0u8);
    let addr = std::ptr::from_ref(marker.as_ref()) as u64;
    nanos ^ (u64::from(std::process::id())).rotate_left(32) ^ addr
}

/// Syncs a handle data was just written through. Not `std::fs::File::open(path)` followed by
/// `sync_all` on *that* new handle: on Windows, `FlushFileBuffers` (what `sync_all` calls there)
/// needs a handle opened for writing, and `File::open` opens read-only — every sync would
/// silently do nothing, or fail, there, unlike POSIX `fsync`, which does not care which mode the
/// fd was opened with.
fn sync_handle(f: &std::fs::File, path: &Path) -> Result<()> {
    f.sync_all()
        .map_err(|e| Error::internal(format!("cannot sync {}: {e}", path.display())))
}

/// Syncs a directory by opening it (read-only is correct and the only option here: a directory
/// itself cannot be opened for writing) and flushing that handle. POSIX-only: `sync_handle`'s
/// Windows caveat does not apply to a plain directory sync the way it does to a file we wrote
/// through, and Windows has no equivalent operation to perform here anyway.
#[cfg(unix)]
fn sync_dir(dir: &Path) -> Result<()> {
    std::fs::File::open(dir)
        .map_err(|e| Error::internal(format!("cannot open {}: {e}", dir.display())))
        .and_then(|f| sync_handle(&f, dir))
}
#[cfg(not(unix))]
fn sync_dir(_dir: &Path) -> Result<()> {
    Ok(())
}

/// Backups made by us (`<name>.pitcrew-backup-<millis>`) for one file, newest first.
fn our_backups(path: &Path) -> Vec<PathBuf> {
    let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) else {
        return Vec::new();
    };
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return Vec::new();
    };
    let prefix = format!("{name}.pitcrew-backup-");
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .and_then(|n| n.strip_prefix(&prefix))
                // Only our own timestamp suffix (all digits) — never anything a person or
                // another tool happened to name starting the same way.
                .is_some_and(|suffix| {
                    !suffix.is_empty() && suffix.bytes().all(|b| b.is_ascii_digit())
                })
        })
        .collect();
    found.sort(); // the millis suffix sorts chronologically as text; newest last
    found.reverse();
    found
}

/// Keeps at most this many of our own backups per file; older ones are deleted (best-effort — a
/// failure here never fails the write that prompted it).
const MAX_BACKUPS: usize = 5;

fn prune_backups(path: &Path) {
    for old in our_backups(path).into_iter().skip(MAX_BACKUPS) {
        let _ = std::fs::remove_file(old);
    }
}

/// Re-reads `path` and backs it up (if it exists) — privately (0600 on Unix: `settings.json` can
/// hold API keys under `env`, same as the file it is a copy of) — syncing the backup to disk,
/// before any write is attempted. The caller still must confirm this matches what the plan was
/// built from.
fn read_and_back_up(path: &Path, target: &Path) -> Result<Option<Vec<u8>>> {
    let now = read_optional(target)?;
    if let Some(bytes) = &now {
        let backup = sibling(path, &format!(".pitcrew-backup-{}", now_millis()));
        let mut f = create_private(&backup)?;
        use std::io::Write as _;
        f.write_all(bytes)
            .map_err(|e| Error::internal(format!("cannot back up to {}: {e}", backup.display())))?;
        sync_handle(&f, &backup)?;
        drop(f);
        prune_backups(path);
    }
    Ok(now)
}

/// Re-checks the file against what the plan was built from, backs it up, then writes atomically:
/// a private temp file in the resolved target's own directory (following a symlink at `c.path`,
/// so the link itself survives — the backup is still named after `c.path`, where the person
/// would look for it), fsynced, renamed into place, with the directory fsynced too (Unix) so the
/// rename itself survives a crash.
fn apply_change(c: &Change) -> Result<()> {
    let target = resolve_write_target(&c.path)?;
    let dir = target
        .parent()
        .ok_or_else(|| Error::internal("the path has no parent directory"))?;
    std::fs::create_dir_all(dir)
        .map_err(|e| Error::internal(format!("cannot create {}: {e}", dir.display())))?;

    let now = read_and_back_up(&c.path, &target)?;
    if now != c.before {
        return Err(Error::new(
            Kind::Conflict,
            format!(
                "{} changed on disk since it was read (a confirmed install/uninstall always \
                 acts on what it just showed you); rerun to see the current diff",
                c.path.display()
            ),
        ));
    }

    if c.delete {
        return std::fs::remove_file(&target)
            .map_err(|e| Error::internal(format!("cannot remove {}: {e}", target.display())))
            .and_then(|()| sync_dir(dir));
    }

    let tmp = sibling(
        &target,
        &format!(".pitcrew-tmp-{:016x}", unpredictable_suffix()),
    );
    // Written, permissioned and synced through one handle — synced before it is closed, and
    // closed (dropped) before the rename: Windows cannot replace a file through a still-open
    // handle to it the way Unix can.
    let write_then_close: Result<()> = (|| {
        let mut f = create_private(&tmp)?;
        use std::io::Write as _;
        f.write_all(&c.after)
            .map_err(|e| Error::internal(format!("cannot write {}: {e}", tmp.display())))?;
        copy_permissions(&target, &tmp)?;
        if c.executable {
            make_executable(&tmp)?;
        }
        sync_handle(&f, &tmp)?;
        drop(f);
        Ok(())
    })();
    let result = write_then_close
        .and_then(|()| {
            std::fs::rename(&tmp, &target)
                .map_err(|e| Error::internal(format!("cannot replace {}: {e}", target.display())))
        })
        .and_then(|()| sync_dir(dir));
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
        assert_eq!(shell_quote_unix("it's/pitcrew"), r"'it'\''s/pitcrew'");
    }

    #[test]
    fn is_single_shell_word_rejects_a_multi_word_command() {
        assert!(is_single_shell_word("/usr/bin/pitcrew"));
        assert!(is_single_shell_word("'/home/sam/my apps/pitcrew'"));
        assert!(is_single_shell_word(
            "\"C:\\Program Files\\PitCrew\\pitcrew.exe\""
        ));
        // The injection this guards against: a foreign, multi-word command that merely ends by
        // mentioning a path.
        assert!(!is_single_shell_word("afplay ding.aiff; ~/bin/pitcrew"));
        assert!(!is_single_shell_word("echo hi && /bin/pitcrew"));
        assert!(!is_single_shell_word("/bin/pitcrew | tee log"));
        assert!(!is_single_shell_word("$(echo /bin/pitcrew)"));
        assert!(!is_single_shell_word("`echo /bin/pitcrew`"));
        assert!(!is_single_shell_word(""));
        // Starts and ends with a quote character, but is still two shell words: the first
        // quote's own closing `'`/`"` comes early, unescaped.
        assert!(!is_single_shell_word(
            "'/usr/bin/afplay' ding.aiff; '/home/u/bin/pitcrew'"
        ));
        assert!(!is_single_shell_word(
            "\"C:\\a.exe\" & \"C:\\x\\pitcrew.exe\""
        ));
        // But a genuinely single quoted word, including one with a properly escaped embedded
        // quote, is still accepted.
        assert!(is_single_shell_word("'it'\\''s/pitcrew'"));
        assert!(is_single_shell_word("\"C:\\a\"\"b.exe\""));
    }

    #[test]
    fn quoted_word_file_name_strips_either_quoting_style() {
        assert_eq!(quoted_word_file_name("/usr/bin/pitcrew"), "pitcrew");
        assert_eq!(
            quoted_word_file_name("'/home/sam/my apps/pitcrew'"),
            "pitcrew"
        );
        assert_eq!(quoted_word_file_name("'it'\\''s/pitcrew'"), "pitcrew");
        assert_eq!(
            quoted_word_file_name("\"C:\\Program Files\\PitCrew\\pitcrew.exe\""),
            "pitcrew.exe"
        );
    }

    #[test]
    fn only_pitcrew_by_name_counts_as_ours() {
        assert!(is_our_exe_name("pitcrew"));
        assert!(is_our_exe_name("pitcrew.exe"));
        assert!(is_our_exe_name("PITCREW.EXE"));
        assert!(!is_our_exe_name("not-pitcrew"));
        assert!(!is_our_exe_name("pitcrew-notify-wrapper.sh"));
    }

    #[test]
    fn target_names_parse_case_insensitively() {
        assert!(Target::parse("Claude").is_ok());
        assert!(Target::parse("CODEX").is_ok());
        assert!(Target::parse("opencode").is_ok());
        assert!(Target::parse("gemini").is_err());
    }

    #[test]
    fn apply_change_refuses_a_file_that_changed_since_it_was_read() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("settings.json");
        std::fs::write(&path, b"{\"a\": 1}").unwrap();
        let before = Some(std::fs::read(&path).unwrap());
        // Someone else edits the file after we planned, before we apply.
        std::fs::write(&path, b"{\"a\": 2}").unwrap();

        let change = Change {
            path: path.clone(),
            before,
            after: b"{\"a\": 3}".to_vec(),
            delete: false,
            executable: false,
        };
        let err = apply_change(&change).unwrap_err();
        assert!(err.message.contains("changed on disk"), "{}", err.message);
        assert_eq!(std::fs::read(&path).unwrap(), b"{\"a\": 2}");
    }

    #[test]
    fn apply_change_refuses_a_file_that_appeared_since_it_was_read() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("settings.json");
        // The plan was built when the file did not exist...
        let change = Change {
            path: path.clone(),
            before: None,
            after: b"{}".to_vec(),
            delete: false,
            executable: false,
        };
        // ...but something else created it in the meantime.
        std::fs::write(&path, b"{\"from\": \"someone else\"}").unwrap();

        let err = apply_change(&change).unwrap_err();
        assert!(err.message.contains("changed on disk"), "{}", err.message);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "{\"from\": \"someone else\"}"
        );
    }

    #[test]
    fn apply_change_backs_up_and_writes_atomically() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("settings.json");
        std::fs::write(&path, b"old").unwrap();
        let change = Change {
            path: path.clone(),
            before: Some(b"old".to_vec()),
            after: b"new".to_vec(),
            delete: false,
            executable: false,
        };
        apply_change(&change).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new");
        let backups = our_backups(&path);
        assert_eq!(backups.len(), 1);
        assert_eq!(std::fs::read_to_string(&backups[0]).unwrap(), "old");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&backups[0]).unwrap().permissions().mode() & 0o777;
            assert_eq!(
                mode, 0o600,
                "settings.json may hold secrets under env; the backup must too"
            );
        }
        // No leftover temp file.
        let leftover: Vec<_> = std::fs::read_dir(tmp.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains("pitcrew-tmp"))
            .collect();
        assert!(leftover.is_empty(), "{leftover:?}");
    }

    #[test]
    fn old_backups_beyond_the_cap_are_pruned() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("settings.json");
        std::fs::write(&path, b"v0").unwrap();
        for i in 1..=(MAX_BACKUPS + 3) {
            let change = Change {
                path: path.clone(),
                before: Some(format!("v{}", i - 1).into_bytes()),
                after: format!("v{i}").into_bytes(),
                delete: false,
                executable: false,
            };
            apply_change(&change).unwrap();
            // Backups are named by millisecond; without a tiny sleep, a fast loop could produce
            // duplicate names and undercount.
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert_eq!(our_backups(&path).len(), MAX_BACKUPS);
    }

    #[test]
    fn a_file_merely_starting_like_a_backup_is_never_counted_or_pruned() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("settings.json");
        std::fs::write(&path, b"v0").unwrap();
        // Not ours: the suffix is not our all-digits timestamp.
        let foreign = tmp
            .path()
            .join("settings.json.pitcrew-backup-from-someone-else");
        std::fs::write(&foreign, b"not ours").unwrap();
        for i in 1..=(MAX_BACKUPS + 3) {
            let change = Change {
                path: path.clone(),
                before: Some(format!("v{}", i - 1).into_bytes()),
                after: format!("v{i}").into_bytes(),
                delete: false,
                executable: false,
            };
            apply_change(&change).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert_eq!(our_backups(&path).len(), MAX_BACKUPS);
        assert!(
            foreign.exists(),
            "pruning must never touch a file that is not ours"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_config_is_written_through_keeping_the_link() {
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().join("real-settings.json");
        let link = tmp.path().join("settings.json");
        std::fs::write(&real, b"old").unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();

        let change = Change {
            path: link.clone(),
            before: Some(b"old".to_vec()),
            after: b"new".to_vec(),
            delete: false,
            executable: false,
        };
        apply_change(&change).unwrap();

        assert_eq!(std::fs::read_to_string(&real).unwrap(), "new");
        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink(),
            "the link itself must survive the write"
        );
        assert_eq!(std::fs::read_link(&link).unwrap(), real);
    }

    #[cfg(unix)]
    #[test]
    fn a_broken_symlink_is_refused_not_guessed_at() {
        let tmp = tempfile::tempdir().unwrap();
        let link = tmp.path().join("settings.json");
        std::os::unix::fs::symlink(tmp.path().join("nowhere"), &link).unwrap();

        let change = Change {
            path: link.clone(),
            before: None,
            after: b"new".to_vec(),
            delete: false,
            executable: false,
        };
        let err = apply_change(&change).unwrap_err();
        assert!(err.message.contains("symlink"), "{}", err.message);
        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink(),
            "must not have replaced the link"
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_temp_file_is_private_from_creation() {
        use std::os::unix::fs::PermissionsExt as _;
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("settings.json");
        let change = Change {
            path: path.clone(),
            before: None,
            after: b"{}".to_vec(),
            delete: false,
            executable: false,
        };
        apply_change(&change).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
}
