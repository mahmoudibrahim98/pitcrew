//! Codex's `notify` setting, in `config.toml` (`CODEX_HOME`, else `~/.codex`).
//!
//! `notify` is a plain array of strings: the program and its fixed arguments; Codex appends the
//! JSON event as one more argument when it runs it. We want `notify = ["<exe>", "hook", "codex",
//! "notify"]`.
//!
//! If a *foreign* `notify` is already there, we never replace it: `install` reports the conflict
//! and makes no change, unless `--chain`, in which case `notify` becomes `["<exe>", "hook",
//! "codex", "notify", "--chain"]` — **no wrapper script, ever**. `pitcrew hook codex notify
//! --chain` (`crate::hook`) delivers our own hook and then runs the original program directly via
//! `std::process::Command` — argv exactly as recorded, the payload Codex passed appended exactly
//! as Codex passed it — with no shell and no `cmd.exe` involved at any point.
//!
//! The original `notify` value's **exact source text** (comments, layout and all — not a value
//! reconstructed from parsed strings, which would lose them) is recorded in a private sidecar
//! JSON file next to `config.toml`, together with the plain argv needed to run it.
//! `uninstall` restores that text byte-for-byte, writing the config change before deleting the
//! sidecar, so a failure partway through never leaves `notify` pointing at a chain whose record
//! is gone. An unreadable sidecar is reported as a conflict, never silently dropped. Installing a
//! new chain refuses to overwrite a sidecar that records a *different* original (stale from an
//! earlier, different chain), and refuses outright if the foreign `notify` already names
//! `pitcrew` itself — wrapping ourselves must stay impossible.

use super::{Change, Plan, Status, Target};
use crate::config::Env;
use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use toml_edit::{Array, DocumentMut, Item};

const FILE_NAME: &str = "config.toml";

pub(crate) fn path(env: Env<'_>) -> Result<PathBuf> {
    Ok(super::config_dir(env, "CODEX_HOME", ".codex")?.join(FILE_NAME))
}

/// `notify`'s last three elements once we have written it, direct or chained — present
/// regardless of the executable's own (possibly stale) path, so a moved binary is still
/// recognised.
const MARKER_TAIL: [&str; 3] = ["hook", "codex", "notify"];
const CHAIN_FLAG: &str = "--chain";

fn target_notify(exe: &str) -> Vec<String> {
    vec![
        exe.to_owned(),
        "hook".to_owned(),
        "codex".to_owned(),
        "notify".to_owned(),
    ]
}

fn target_notify_chained(exe: &str) -> Vec<String> {
    let mut values = target_notify(exe);
    values.push(CHAIN_FLAG.to_owned());
    values
}

/// Whether `values` ends with our marker (`hook codex notify`), with or without a trailing
/// `--chain` — so a moved executable is recognised whether or not it is chained — **and** its
/// first element actually names `pitcrew`/`pitcrew.exe`. Without that second check, a foreign
/// `notify = ["some-other-tool", "hook", "codex", "notify"]` that merely happens to share our
/// trailing three words would be claimed as ours too.
fn is_ours(values: &[String]) -> bool {
    let core = if values.last().map(String::as_str) == Some(CHAIN_FLAG) {
        &values[..values.len() - 1]
    } else {
        values
    };
    core.len() >= MARKER_TAIL.len()
        && core[core.len() - MARKER_TAIL.len()..] == MARKER_TAIL
        && core
            .first()
            .is_some_and(|first| super::is_our_exe_name(&super::quoted_word_file_name(first)))
}

/// Whether `values` is specifically our *chained* form.
fn is_chained(values: &[String]) -> bool {
    values.last().map(String::as_str) == Some(CHAIN_FLAG) && is_ours(values)
}

fn as_strings(item: &Item) -> Option<Vec<String>> {
    item.as_array()?
        .iter()
        .map(|v| v.as_str().map(str::to_owned))
        .collect()
}

fn set_notify(doc: &mut DocumentMut, values: &[String]) {
    let mut array = Array::new();
    for v in values {
        array.push(v.as_str());
    }
    doc["notify"] = toml_edit::value(array);
}

fn sidecar_path(config_path: &Path) -> PathBuf {
    config_path.with_file_name("pitcrew-notify-original.json")
}

/// The chain's recorded state: `values`, the original `notify`'s plain argv (what `pitcrew hook
/// codex notify --chain` actually runs), and `toml`, the original `notify` value's **exact
/// source text** — comments, multi-line layout and all — restored verbatim on `uninstall` rather
/// than ever being reconstructed from `values`.
#[derive(Serialize, Deserialize)]
struct OriginalRecord {
    values: Vec<String>,
    toml: String,
}

fn sidecar_json(values: &[String], toml_text: &str) -> String {
    let record = OriginalRecord {
        values: values.to_vec(),
        toml: toml_text.to_owned(),
    };
    serde_json::to_string_pretty(&record).unwrap_or_default()
}

fn parse_sidecar(bytes: &[u8]) -> Option<OriginalRecord> {
    serde_json::from_slice(bytes).ok()
}

/// The original `notify` command recorded for a chain, as plain argv, for `pitcrew hook codex
/// notify --chain` (`crate::hook`) to run after delivering our own hook. `None` if there is no
/// chain, or its sidecar cannot be read — silently: a hook must never fail loudly just because
/// what it forwards to is briefly unavailable.
#[must_use]
pub(crate) fn chained_original(env: Env<'_>) -> Option<Vec<String>> {
    let path = path(env).ok()?;
    let sidecar = sidecar_path(&path);
    let bytes = super::read_optional(&sidecar).ok().flatten()?;
    parse_sidecar(&bytes).map(|record| record.values)
}

fn parse(path: &Path, bytes: &[u8]) -> Result<DocumentMut> {
    let text = std::str::from_utf8(bytes)
        .map_err(|_| Error::invalid(format!("{} is not UTF-8 text", path.display())))?;
    text.parse::<DocumentMut>()
        .map_err(|e| Error::invalid(format!("{} is not valid TOML: {e}", path.display())))
}

/// Parses `value_text` (the exact source text of a `notify` value, as recorded) back into an
/// `Item` carrying that same formatting, by wrapping it in a one-line `notify = …` document and
/// pulling the key back out — `toml_edit` reproduces an unmodified parsed item's own formatting
/// exactly, so this is the verbatim original, not a reconstruction.
fn parse_notify_value(value_text: &str) -> Result<Item> {
    // No space between `=` and `{value_text}`: `value_text` is `Item::to_string()`, which already
    // includes that value's own leading decor (the space that followed the original `=`) — adding
    // another here would double it (`notify =  […]`) and break the byte-exact restoration this
    // exists for.
    let snippet = format!("notify ={value_text}\n");
    let mut doc: DocumentMut = snippet
        .parse()
        .map_err(|e| Error::internal(format!("cannot restore the original notify ({e})")))?;
    doc.remove("notify")
        .ok_or_else(|| Error::internal("cannot restore the original notify (internal error)"))
}

/// Whether `text` mixes CRLF and bare-LF line endings: if so, there is no single answer for what
/// a rewrite's *other* lines should use, so the safe choice is to refuse rather than silently
/// converting everything to one style.
fn has_mixed_line_endings(text: &str) -> bool {
    let bytes = text.as_bytes();
    let mut saw_crlf = false;
    let mut saw_bare_lf = false;
    for (i, &b) in bytes.iter().enumerate() {
        if b != b'\n' {
            continue;
        }
        if i > 0 && bytes[i - 1] == b'\r' {
            saw_crlf = true;
        } else {
            saw_bare_lf = true;
        }
    }
    saw_crlf && saw_bare_lf
}

/// `toml_edit`'s printer does not reliably keep a document's original line-ending style or a
/// missing final newline once anything in it is re-serialised (observed directly: CRLF comes
/// back as LF, and a file with no trailing newline gets one) — so both are restored here to match
/// `before`, after asking `toml_edit` to print the rest. Callers check `has_mixed_line_endings`
/// first and refuse rather than reach here with a file this cannot represent faithfully.
fn match_line_endings(generated: String, before: Option<&[u8]>) -> String {
    let Some(before) = before else {
        return generated;
    };
    let original = String::from_utf8_lossy(before);
    let had_crlf = original.contains("\r\n");
    let had_trailing_newline = original.ends_with('\n');

    let mut out = generated.replace("\r\n", "\n");
    if had_crlf {
        out = out.replace('\n', "\r\n");
    }
    if had_trailing_newline && !out.ends_with('\n') {
        out.push('\n');
    } else if !had_trailing_newline && out.ends_with('\n') {
        out.pop();
        if out.ends_with('\r') {
            out.pop();
        }
    }
    out
}

fn file_change(path: PathBuf, before: Option<Vec<u8>>, doc: DocumentMut) -> Change {
    let after = match_line_endings(doc.to_string(), before.as_deref());
    Change {
        path,
        before,
        after: after.into_bytes(),
        delete: false,
        executable: false,
    }
}

fn conflicting(path: &Path, detail: impl std::fmt::Display) -> Plan {
    Plan {
        target: Target::Codex,
        status: Status::Conflicting,
        detail: format!("{}: {detail}", path.display()),
        changes: vec![],
    }
}

pub(crate) fn plan_install(env: Env<'_>, exe: &str, chain: bool) -> Result<Plan> {
    let path = path(env)?;
    let before = super::read_optional(&path)?;
    if let Some(bytes) = &before {
        let text = String::from_utf8_lossy(bytes);
        if has_mixed_line_endings(&text) {
            return Ok(conflicting(
                &path,
                "has mixed line endings (both CRLF and plain LF); fix it by hand first so a \
                 rewrite does not have to guess which one the rest of the file should use",
            ));
        }
    }
    let mut doc = match &before {
        Some(bytes) => parse(&path, bytes)?,
        None => DocumentMut::new(),
    };
    let existing_item = doc.get("notify").cloned();
    let existing = existing_item.as_ref().and_then(as_strings);

    match existing_item {
        None => {
            set_notify(&mut doc, &target_notify(exe));
            Ok(Plan {
                target: Target::Codex,
                status: Status::Missing,
                detail: format!("{} (notify is not set)", path.display()),
                changes: vec![file_change(path, before, doc)],
            })
        }
        Some(_) if existing.is_none() => Ok(conflicting(
            &path,
            "has a notify that is not a plain array of strings; fix it by hand first",
        )),
        Some(ref item) => {
            let existing = existing.unwrap_or_default();
            let direct_target = target_notify(exe);
            let chained_target = target_notify_chained(exe);

            if existing == direct_target {
                Ok(Plan {
                    target: Target::Codex,
                    status: Status::Installed,
                    detail: format!("{} (already set)", path.display()),
                    changes: vec![],
                })
            } else if existing == chained_target {
                Ok(Plan {
                    target: Target::Codex,
                    status: Status::Installed,
                    detail: format!("{} (already chained)", path.display()),
                    changes: vec![],
                })
            } else if is_chained(&existing) {
                set_notify(&mut doc, &chained_target);
                Ok(Plan {
                    target: Target::Codex,
                    status: Status::Installed,
                    detail: format!(
                        "{} (chained; updating the executable's path)",
                        path.display()
                    ),
                    changes: vec![file_change(path, before, doc)],
                })
            } else if is_ours(&existing) {
                set_notify(&mut doc, &direct_target);
                Ok(Plan {
                    target: Target::Codex,
                    status: Status::Installed,
                    detail: format!("{} (updating the executable's path)", path.display()),
                    changes: vec![file_change(path, before, doc)],
                })
            } else if !chain {
                Ok(conflicting(
                    &path,
                    format!(
                        "already has notify = {existing:?}; rerun `install --chain` to run \
                         both, or remove it by hand"
                    ),
                ))
            } else {
                // A new chain. `notify`'s own program must not already name us — wrapping
                // ourselves (running `pitcrew ... --chain` as "the original") must stay
                // impossible, not just unlikely.
                if existing.first().is_some_and(|first| {
                    super::is_our_exe_name(&super::quoted_word_file_name(first))
                }) {
                    return Ok(conflicting(
                        &path,
                        format!(
                            "notify's program ({:?}) already names pitcrew, but this is not a \
                             chain pitcrew recognises; fix it by hand first",
                            existing[0]
                        ),
                    ));
                }
                let sidecar_path = sidecar_path(&path);
                let sidecar_before = super::read_optional(&sidecar_path)?;
                if let Some(bytes) = &sidecar_before {
                    match parse_sidecar(bytes) {
                        Some(record) if record.values == existing => {
                            // Already recorded, identical: harmless to refresh.
                        }
                        Some(_) => {
                            return Ok(conflicting(
                                &path,
                                format!(
                                    "{} already records a different original notify; fix it by \
                                     hand first",
                                    sidecar_path.display()
                                ),
                            ));
                        }
                        None => {
                            return Ok(conflicting(
                                &path,
                                format!(
                                    "{} already exists and is not a chain record pitcrew wrote; \
                                     move it aside first",
                                    sidecar_path.display()
                                ),
                            ));
                        }
                    }
                }
                let original_text = item.to_string();
                let sidecar = sidecar_json(&existing, &original_text);
                set_notify(&mut doc, &chained_target);
                Ok(Plan {
                    target: Target::Codex,
                    status: Status::Installed,
                    detail: format!("{} chained after the existing notify", path.display()),
                    changes: vec![
                        Change {
                            path: sidecar_path,
                            before: sidecar_before,
                            after: sidecar.into_bytes(),
                            delete: false,
                            executable: false,
                        },
                        file_change(path, before, doc),
                    ],
                })
            }
        }
    }
}

pub(crate) fn plan_uninstall(env: Env<'_>) -> Result<Plan> {
    let path = path(env)?;
    let before = super::read_optional(&path)?;
    let Some(before_bytes) = &before else {
        return Ok(missing_plan(path));
    };
    if has_mixed_line_endings(&String::from_utf8_lossy(before_bytes)) {
        return Ok(conflicting(
            &path,
            "has mixed line endings (both CRLF and plain LF); fix it by hand first so a rewrite \
             does not have to guess which one the rest of the file should use",
        ));
    }
    let mut doc = parse(&path, before_bytes)?;
    let Some(existing) = doc.get("notify").and_then(as_strings) else {
        return Ok(missing_plan(path));
    };

    if is_chained(&existing) {
        let sidecar_path = sidecar_path(&path);
        let sidecar_before = super::read_optional(&sidecar_path)?;
        let Some(record) = sidecar_before.as_deref().and_then(parse_sidecar) else {
            return Ok(conflicting(
                &path,
                format!(
                    "the chain's recorded original notify ({}) is missing or unreadable; fix it \
                     by hand first",
                    sidecar_path.display()
                ),
            ));
        };
        let restored = match parse_notify_value(&record.toml) {
            Ok(item) => item,
            Err(e) => return Ok(conflicting(&path, e.message)),
        };
        doc["notify"] = restored;
        // The config is restored *first*: if anything fails before the sidecar is deleted,
        // `notify` already points at the real original again, never at a chain whose record is
        // about to be removed.
        let changes = vec![
            file_change(path.clone(), before, doc),
            Change {
                path: sidecar_path,
                before: sidecar_before,
                after: Vec::new(),
                delete: true,
                executable: false,
            },
        ];
        return Ok(Plan {
            target: Target::Codex,
            status: Status::Installed,
            detail: format!(
                "{} (removing the chain; restoring the original notify)",
                path.display()
            ),
            changes,
        });
    }

    if !is_ours(&existing) {
        return Ok(missing_plan(path));
    }
    doc.as_table_mut().remove("notify");
    Ok(Plan {
        target: Target::Codex,
        status: Status::Installed,
        detail: format!("{} (removing our notify hook)", path.display()),
        changes: vec![file_change(path, before, doc)],
    })
}

fn missing_plan(path: PathBuf) -> Plan {
    Plan {
        target: Target::Codex,
        status: Status::Missing,
        detail: format!("{} (nothing of ours installed)", path.display()),
        changes: vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<std::ffi::OsString> + use<> {
        let map: std::collections::HashMap<String, std::ffi::OsString> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), std::ffi::OsString::from(v)))
            .collect();
        move |name| map.get(name).cloned()
    }

    const EXE: &str = "/home/sam/.local/bin/pitcrew";

    /// Applies every change in `plan` (test helper, mirroring `install::apply_change` closely
    /// enough for these tests, without pulling in the re-check-before-write machinery).
    fn apply(plan: &Plan) {
        for c in &plan.changes {
            if c.delete {
                let _ = std::fs::remove_file(&c.path);
            } else {
                std::fs::write(&c.path, &c.after).unwrap();
            }
        }
    }

    #[test]
    fn installs_then_is_a_no_op_then_uninstalls_cleanly() {
        let tmp = tempfile::tempdir().unwrap();
        let env = env_of(&[("CODEX_HOME", tmp.path().to_str().unwrap())]);
        let config_path = path(&env).unwrap();

        let plan = plan_install(&env, EXE, false).unwrap();
        assert!(plan.status == Status::Missing);
        let after = String::from_utf8(plan.changes[0].after.clone()).unwrap();
        std::fs::write(&config_path, &after).unwrap();

        let plan = plan_install(&env, EXE, false).unwrap();
        assert!(plan.status == Status::Installed);
        assert!(plan.changes.is_empty());

        let plan = plan_uninstall(&env).unwrap();
        let restored = String::from_utf8(plan.changes[0].after.clone()).unwrap();
        assert_eq!(restored.trim(), ""); // the file had nothing else in it
    }

    #[test]
    fn keeps_comments_and_unrelated_keys() {
        let original = "# a comment\napproval_policy = \"never\"\n";
        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("config.toml");
        std::fs::write(&config_path, original).unwrap();
        let env = env_of(&[("CODEX_HOME", tmp.path().to_str().unwrap())]);

        let plan = plan_install(&env, EXE, false).unwrap();
        let after = String::from_utf8(plan.changes[0].after.clone()).unwrap();
        assert!(after.contains("# a comment"));
        assert!(after.contains("approval_policy = \"never\""));
        std::fs::write(&config_path, &after).unwrap();

        let plan = plan_uninstall(&env).unwrap();
        let restored = String::from_utf8(plan.changes[0].after.clone()).unwrap();
        assert_eq!(restored, original);
    }

    #[test]
    fn a_foreign_notify_is_reported_not_overwritten() {
        let original = r#"notify = ["terminal-notifier", "-title", "Codex"]"#;
        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("config.toml");
        std::fs::write(&config_path, original).unwrap();
        let env = env_of(&[("CODEX_HOME", tmp.path().to_str().unwrap())]);

        let plan = plan_install(&env, EXE, false).unwrap();
        assert!(plan.status == Status::Conflicting);
        assert!(plan.changes.is_empty());
        // Untouched.
        assert_eq!(std::fs::read_to_string(&config_path).unwrap(), original);
    }

    #[test]
    fn chain_sets_notify_directly_with_no_wrapper_file_and_uninstall_restores_it_byte_exact() {
        let original = "notify = [\"terminal-notifier\", \"-title\", \"Codex\"] # a comment\n";
        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("config.toml");
        std::fs::write(&config_path, original).unwrap();
        let env = env_of(&[("CODEX_HOME", tmp.path().to_str().unwrap())]);

        let plan = plan_install(&env, EXE, true).unwrap();
        assert!(plan.status == Status::Installed);
        assert_eq!(plan.changes.len(), 2, "sidecar + config, no wrapper file");
        apply(&plan);

        let now = std::fs::read(&config_path).unwrap();
        let parsed = parse(&config_path, &now).unwrap();
        let notify = as_strings(parsed.get("notify").unwrap()).unwrap();
        assert_eq!(notify, vec![EXE, "hook", "codex", "notify", "--chain"]);
        // No wrapper script of any kind is ever written.
        let entries: Vec<String> = std::fs::read_dir(tmp.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert!(
            entries.iter().all(|n| !n.contains("wrapper")),
            "{entries:?}"
        );

        let plan = plan_uninstall(&env).unwrap();
        assert_eq!(plan.changes.len(), 2);
        // The config change comes first: restoring it never depends on the sidecar still
        // existing.
        assert_eq!(plan.changes[0].path, config_path);
        assert!(!plan.changes[0].delete);
        let restored = String::from_utf8(plan.changes[0].after.clone()).unwrap();
        assert_eq!(
            restored, original,
            "must be byte-exact, including the trailing comment"
        );
        assert!(plan.changes[1].delete);
    }

    #[test]
    fn chain_twice_is_idempotent() {
        let original = r#"notify = ["terminal-notifier", "-title", "Codex"]"#;
        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("config.toml");
        std::fs::write(&config_path, original).unwrap();
        let env = env_of(&[("CODEX_HOME", tmp.path().to_str().unwrap())]);

        let plan = plan_install(&env, EXE, true).unwrap();
        apply(&plan);
        let after_first = std::fs::read_to_string(&config_path).unwrap();

        // Running `install --chain` again, unchanged, is a complete no-op.
        let plan = plan_install(&env, EXE, true).unwrap();
        assert!(plan.status == Status::Installed, "{}", plan.detail);
        assert!(
            plan.changes.is_empty(),
            "must not re-chain an already-chained notify"
        );
        assert_eq!(std::fs::read_to_string(&config_path).unwrap(), after_first);

        // Running plain `install` (no --chain) while already chained is also a no-op, not a
        // conflict demanding `--chain` again.
        let plan = plan_install(&env, EXE, false).unwrap();
        assert!(plan.status == Status::Installed, "{}", plan.detail);
        assert!(plan.changes.is_empty());

        // And uninstall still restores the one true original, not a chain link.
        let plan = plan_uninstall(&env).unwrap();
        let restored = String::from_utf8(plan.changes[0].after.clone()).unwrap();
        assert_eq!(restored.trim(), original);
    }

    #[test]
    fn chain_then_moved_executable_updates_notify_in_place() {
        let original = r#"notify = ["terminal-notifier"]"#;
        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("config.toml");
        std::fs::write(&config_path, original).unwrap();
        let env = env_of(&[("CODEX_HOME", tmp.path().to_str().unwrap())]);

        let plan = plan_install(&env, "/old/place/pitcrew", true).unwrap();
        apply(&plan);

        let plan = plan_install(&env, "/new/place/pitcrew", true).unwrap();
        assert!(plan.status == Status::Installed);
        assert_eq!(
            plan.changes.len(),
            1,
            "only the config's notify needs to change"
        );
        let after = String::from_utf8(plan.changes[0].after.clone()).unwrap();
        assert!(after.contains("/new/place/pitcrew"));
        assert!(!after.contains("/old/place/pitcrew"));
        apply(&plan);

        let plan = plan_uninstall(&env).unwrap();
        let restored = String::from_utf8(plan.changes[0].after.clone()).unwrap();
        assert_eq!(restored.trim(), original);
    }

    #[test]
    fn install_refuses_to_chain_when_notify_already_names_pitcrew() {
        let original = r#"notify = ["pitcrew", "something", "unrecognised"]"#;
        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("config.toml");
        std::fs::write(&config_path, original).unwrap();
        let env = env_of(&[("CODEX_HOME", tmp.path().to_str().unwrap())]);

        let plan = plan_install(&env, EXE, true).unwrap();
        assert!(plan.status == Status::Conflicting, "{}", plan.detail);
        assert!(plan.changes.is_empty());
        assert_eq!(std::fs::read_to_string(&config_path).unwrap(), original);
    }

    #[test]
    fn install_refuses_to_overwrite_a_sidecar_recording_a_different_original() {
        let original = r#"notify = ["terminal-notifier"]"#;
        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("config.toml");
        std::fs::write(&config_path, original).unwrap();
        // A sidecar left behind recording a *different* original (as if a previous, different
        // chain's uninstall never finished, or someone hand-edited things).
        let sidecar = tmp.path().join("pitcrew-notify-original.json");
        std::fs::write(
            &sidecar,
            sidecar_json(&["something-else".to_owned()], "[\"something-else\"]"),
        )
        .unwrap();
        let env = env_of(&[("CODEX_HOME", tmp.path().to_str().unwrap())]);

        let plan = plan_install(&env, EXE, true).unwrap();
        assert!(plan.status == Status::Conflicting, "{}", plan.detail);
        assert!(plan.changes.is_empty());
        assert_eq!(std::fs::read_to_string(&config_path).unwrap(), original);
    }

    #[test]
    fn uninstall_with_a_missing_sidecar_is_a_conflict_not_a_removal() {
        let original = r#"notify = ["terminal-notifier"]"#;
        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("config.toml");
        std::fs::write(&config_path, original).unwrap();
        let env = env_of(&[("CODEX_HOME", tmp.path().to_str().unwrap())]);

        let plan = plan_install(&env, EXE, true).unwrap();
        apply(&plan);
        // Someone deletes the sidecar by hand, but leaves the chained notify in place.
        let sidecar = tmp.path().join("pitcrew-notify-original.json");
        std::fs::remove_file(&sidecar).unwrap();

        let plan = plan_uninstall(&env).unwrap();
        assert!(plan.status == Status::Conflicting, "{}", plan.detail);
        assert!(plan.changes.is_empty());
        // notify is left exactly as it was — still chained, not silently cleared.
        assert!(
            std::fs::read_to_string(&config_path)
                .unwrap()
                .contains("--chain")
        );
    }

    #[test]
    fn reinstalling_after_the_executable_moved_updates_the_path() {
        let tmp = tempfile::tempdir().unwrap();
        let env = env_of(&[("CODEX_HOME", tmp.path().to_str().unwrap())]);
        let config_path = path(&env).unwrap();
        let plan = plan_install(&env, "/old/pitcrew", false).unwrap();
        std::fs::write(&config_path, &plan.changes[0].after).unwrap();

        let plan = plan_install(&env, "/new/place/pitcrew", false).unwrap();
        assert!(plan.status == Status::Installed);
        let after = String::from_utf8(plan.changes[0].after.clone()).unwrap();
        assert!(after.contains("/new/place/pitcrew"));
        assert!(!after.contains("/old/pitcrew"));
    }

    #[test]
    fn crlf_and_a_missing_final_newline_round_trip() {
        let original = "notify = [\"terminal-notifier\"]\r\napproval_policy = \"never\"";
        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("config.toml");
        std::fs::write(&config_path, original).unwrap();
        let env = env_of(&[("CODEX_HOME", tmp.path().to_str().unwrap())]);

        let plan = plan_install(&env, EXE, true).unwrap();
        apply(&plan);
        let plan = plan_uninstall(&env).unwrap();
        let restored = String::from_utf8(plan.changes[0].after.clone()).unwrap();
        assert_eq!(
            restored, original,
            "CRLF and the missing final newline must survive"
        );
    }

    #[test]
    fn mixed_line_endings_are_refused_not_silently_converted() {
        let original = "notify = [\"terminal-notifier\"]\r\napproval_policy = \"never\"\n";
        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("config.toml");
        std::fs::write(&config_path, original).unwrap();
        let env = env_of(&[("CODEX_HOME", tmp.path().to_str().unwrap())]);

        let plan = plan_install(&env, EXE, false).unwrap();
        assert!(plan.status == Status::Conflicting, "{}", plan.detail);
        assert!(
            plan.detail.contains("mixed line endings"),
            "{}",
            plan.detail
        );
        assert!(plan.changes.is_empty());
    }

    #[test]
    fn is_ours_and_is_chained_recognise_both_forms() {
        let direct = target_notify(EXE);
        let chained = target_notify_chained(EXE);
        assert!(is_ours(&direct) && !is_chained(&direct));
        assert!(is_ours(&chained) && is_chained(&chained));
        assert!(!is_ours(&["terminal-notifier".to_owned()]));
    }

    #[test]
    fn is_ours_also_checks_the_program_name_not_just_the_trailing_words() {
        // Shares our exact trailing three words, but the program itself is not us.
        let foreign = vec![
            "some-other-tool".to_owned(),
            "hook".to_owned(),
            "codex".to_owned(),
            "notify".to_owned(),
        ];
        assert!(!is_ours(&foreign));
        assert!(!is_chained(&{
            let mut v = foreign;
            v.push(CHAIN_FLAG.to_owned());
            v
        }));
    }
}
