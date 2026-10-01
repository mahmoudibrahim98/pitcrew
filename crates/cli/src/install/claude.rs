//! Claude Code's `hooks` section, in `settings.json` (`CLAUDE_CONFIG_DIR`, else `~/.claude`).
//!
//! Each of our five events becomes one matcher object, `{"matcher": "", "hooks": [{"type":
//! "command", "command": "<exe> hook claude <Event>", "timeout": 5}]}`, appended to that event's
//! array (creating the array, and `"hooks"` itself, if they are missing). `matcher: ""` matches
//! every occurrence of the event, the same as Claude Code's own tool-matcher events use it to
//! mean "every tool"; Claude Code's hooks reference does not document a matcher convention for
//! these five specifically, so this is the conservative, documented-elsewhere default.
//!
//! **Ownership is decided per inner hook, by its `command`**, never by the raw text of a whole
//! matcher group: a hook is ours exactly when its `command` ends in the literal text
//! ` hook claude <Event>` *and* the program named before that (unquoted, and stripped to its file
//! name) is exactly `pitcrew` or `pitcrew.exe` — not merely "any command that happens to end the
//! same way", which a person's own hook could coincidentally do. `uninstall` deletes a whole
//! matcher group only when it is *exactly* our shape (`matcher: ""`, and that one hook is our
//! only hook); otherwise it removes just our one hook from inside the group, leaving every other
//! hook, and the group itself, untouched.
//!
//! **A container is only ever removed if we are the one who created it.** Whether `"hooks"`
//! itself, and each of the five event keys, existed before our most recent `install` is recorded
//! in a small sidecar (`pitcrew-claude-provenance.json`, next to `settings.json`): `install`
//! writes it whenever it creates a key from nothing, and `uninstall` consults it before cascading
//! a now-empty array or object away, so a container the file already had — even an empty one — is
//! never guessed at and never removed, while one we built from scratch is fully reversed, down to
//! the key itself. Without this, uninstalling our own content could not tell "this array is empty
//! because we just removed our only entry from it" apart from "this array was already empty
//! before we ever touched the file" — and the first is safe to clean up, the second is not ours
//! to remove.
//!
//! Re-running `install` after the executable moved updates just the stale hooks' `command` text
//! in place (`Status::Stale` until then); it never duplicates them.
//!
//! A duplicate `"hooks"` key, or a duplicate event key inside it, is refused rather than guessed
//! at, since different JSON readers disagree about which duplicate wins. A leading UTF-8 BOM, if
//! the file has one, is preserved.

use super::jsontext::{self, Entry, Member};
use super::{Change, Plan, Status, Target};
use crate::config::Env;
use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use std::fmt::Write as _;
use std::path::PathBuf;

pub(crate) const EVENTS: [&str; 5] = [
    "SessionStart",
    "UserPromptSubmit",
    "Stop",
    "SessionEnd",
    "Notification",
];

const FILE_NAME: &str = "settings.json";

pub(crate) fn path(env: Env<'_>) -> Result<PathBuf> {
    Ok(super::config_dir(env, "CLAUDE_CONFIG_DIR", ".claude")?.join(FILE_NAME))
}

fn provenance_path(settings_path: &std::path::Path) -> PathBuf {
    settings_path.with_file_name("pitcrew-claude-provenance.json")
}

/// Which of `"hooks"` and our five events did not exist in `settings.json` before our most
/// recent `install` — accumulated across repeated installs, so installing two events today and
/// three more next week still remembers all five as ours to clean up later.
#[derive(Serialize, Deserialize, Default)]
struct Provenance {
    hooks_created: bool,
    events_created: Vec<String>,
}

fn provenance_json(p: &Provenance) -> String {
    serde_json::to_string_pretty(p).unwrap_or_default()
}

fn parse_provenance(bytes: &[u8]) -> Option<Provenance> {
    serde_json::from_slice(bytes).ok()
}

/// Quotes the executable's path for Claude Code's `command` field: **always** wrapped in quotes
/// on Windows, never left bare, because Claude Code may run hooks through Git Bash, whose POSIX
/// `sh` treats an unquoted backslash as an escape character — `C:\Users\...` unquoted loses its
/// backslashes there, even though the identical bare path is perfectly fine when `cmd.exe` runs
/// it directly. Quoting unconditionally sidesteps having to know in advance which shell will run
/// it. On Unix, the usual "quote only if needed" (`shell_quote_unix`) is used.
fn claude_quote_exe_path(path: &str) -> String {
    if cfg!(windows) {
        format!("\"{}\"", path.replace('"', "\"\""))
    } else {
        super::shell_quote_unix(path)
    }
}

fn command(exe: &str, event: &str) -> String {
    format!("{} hook claude {event}", claude_quote_exe_path(exe))
}

fn matcher_object(exe: &str, event: &str) -> String {
    format!(
        r#"{{"matcher": "", "hooks": [{{"type": "command", "command": {}, "timeout": 5}}]}}"#,
        jsontext::escape(&command(exe, event))
    )
}

/// Whether one inner hook's already-unescaped `command` text is ours: it ends in the literal
/// marker ` hook claude <Event>`, and the program named just before that — unquoted, and reduced
/// to its file name — is exactly `pitcrew` or `pitcrew.exe`. The program-name check is what keeps
/// a person's own hook, that happens to end with the same words, from being claimed as ours.
fn command_is_ours(command: &str, event: &str) -> bool {
    let suffix = format!(" hook claude {event}");
    let Some(exe_part) = command.strip_suffix(&suffix) else {
        return false;
    };
    super::is_our_exe_name(&super::quoted_word_file_name(exe_part))
}

fn braces(indent: &str, inner: &str) -> String {
    format!("{{\n{indent}  {inner}\n{indent}}}")
}

fn brackets(indent: &str, inner: &str) -> String {
    format!("[\n{indent}  {inner}\n{indent}]")
}

/// A freshly formatted file, for when there is nothing to preserve.
fn fresh_document(exe: &str) -> String {
    let mut out = String::from("{\n  \"hooks\": {\n");
    for (i, event) in EVENTS.iter().enumerate() {
        let sep = if i + 1 == EVENTS.len() { "" } else { "," };
        let _ = write!(
            &mut out,
            "    \"{event}\": [\n      {}\n    ]{sep}\n",
            matcher_object(exe, event)
        );
    }
    out.push_str("  }\n}\n");
    out
}

/// One of our hooks found inside one event's array: which matcher-group element holds it, which
/// hook element inside that group's own `"hooks"` array it is, and the byte span of its
/// `"command"` string value (for a stale-path rewrite in place).
struct Found {
    group_index: usize,
    hook_index: usize,
    command_value: Entry,
    /// Whether the enclosing group is *exactly* our shape — `matcher: ""`, and this is its only
    /// hook — so removing it can safely remove the whole group rather than just this one hook.
    exactly_ours: bool,
}

/// Scans one event's array of matcher-group objects for one of our hooks. Only the first match
/// is ever returned; there should only be one, since `install` never adds a second once one is
/// found, and every caller here re-scans fresh after each edit.
fn find_ours(bytes: &[u8], arr: &jsontext::Arr, event: &str) -> Option<Found> {
    for (group_index, el) in arr.elements.iter().enumerate() {
        if bytes.get(el.value_start) != Some(&b'{') {
            continue; // not an object: not a matcher group we understand, so never ours
        }
        let group = jsontext::object(bytes, el.value_start);
        let Some(hooks_member) = group.members.iter().find(|m| m.key == "hooks") else {
            continue;
        };
        if bytes.get(hooks_member.entry.value_start) != Some(&b'[') {
            continue;
        }
        let inner = jsontext::array(bytes, hooks_member.entry.value_start);
        let matcher_is_empty_string = group
            .members
            .iter()
            .find(|m| m.key == "matcher")
            .is_some_and(|m| {
                bytes.get(m.entry.value_start) == Some(&b'"')
                    && jsontext::parse_string(bytes, m.entry.value_start)
                        .0
                        .is_empty()
            });
        for (hook_index, hook_el) in inner.elements.iter().enumerate() {
            if bytes.get(hook_el.value_start) != Some(&b'{') {
                continue;
            }
            let hook_obj = jsontext::object(bytes, hook_el.value_start);
            let Some(cmd_member) = hook_obj.members.iter().find(|m| m.key == "command") else {
                continue;
            };
            if bytes.get(cmd_member.entry.value_start) != Some(&b'"') {
                continue;
            }
            let (text, _) = jsontext::parse_string(bytes, cmd_member.entry.value_start);
            if command_is_ours(&text, event) {
                return Some(Found {
                    group_index,
                    hook_index,
                    command_value: cmd_member.entry,
                    exactly_ours: matcher_is_empty_string && inner.elements.len() == 1,
                });
            }
        }
    }
    None
}

fn command_matches_current(
    bytes: &[u8],
    command_value_start: usize,
    exe: &str,
    event: &str,
) -> bool {
    jsontext::parse_string(bytes, command_value_start).0 == command(exe, event)
}

/// Adds `event`'s matcher object to `doc` if it is not already there, re-scanning fresh (the
/// document is small; this keeps every case, including ones nested two or three levels deep,
/// correct without duplicating the splicing logic).
fn insert_event(doc: &str, exe: &str, event: &str) -> String {
    let bytes = doc.as_bytes();
    let root = jsontext::object(bytes, jsontext::skip_ws(bytes, 0));
    let Some(hooks_member) = root.members.iter().find(|m| m.key == "hooks") else {
        let child_indent = root
            .members
            .last()
            .map(|m| jsontext::indent_before(bytes, m.entry.start).to_owned())
            .unwrap_or_else(|| "  ".to_owned());
        let inner_indent = format!("{child_indent}  ");
        let new_text = format!(
            "\"hooks\": {}",
            braces(
                &child_indent,
                &format!(
                    "\"{event}\": {}",
                    brackets(&inner_indent, &matcher_object(exe, event))
                )
            )
        );
        let entries: Vec<Entry> = root.members.iter().map(|m| m.entry).collect();
        return jsontext::append(doc, &entries, root.close, &new_text, &child_indent, "");
    };

    let hooks_start = hooks_member.entry.value_start;
    if bytes.get(hooks_start) != Some(&b'{') {
        return doc.to_owned(); // not ours to touch; `inspect` already reported the conflict
    }
    let hooks_obj = jsontext::object(bytes, hooks_start);
    let hooks_indent = jsontext::indent_before(bytes, hooks_member.entry.start).to_owned();
    let event_member = hooks_obj.members.iter().find(|m| m.key == event);

    match event_member {
        None => {
            let child_indent = hooks_obj
                .members
                .last()
                .map(|m| jsontext::indent_before(bytes, m.entry.start).to_owned())
                .unwrap_or_else(|| format!("{hooks_indent}  "));
            let new_text = format!(
                "\"{event}\": {}",
                brackets(&child_indent, &matcher_object(exe, event))
            );
            let entries: Vec<Entry> = hooks_obj.members.iter().map(|m| m.entry).collect();
            jsontext::append(
                doc,
                &entries,
                hooks_obj.close,
                &new_text,
                &child_indent,
                &hooks_indent,
            )
        }
        Some(m) => {
            if bytes.get(m.entry.value_start) != Some(&b'[') {
                return doc.to_owned();
            }
            let arr = jsontext::array(bytes, m.entry.value_start);
            if find_ours(bytes, &arr, event).is_some() {
                return doc.to_owned(); // already there
            }
            let event_indent = jsontext::indent_before(bytes, m.entry.start).to_owned();
            let child_indent = arr
                .elements
                .last()
                .map(|el| jsontext::indent_before(bytes, el.value_start).to_owned())
                .unwrap_or_else(|| format!("{event_indent}  "));
            jsontext::append(
                doc,
                &arr.elements,
                arr.close,
                &matcher_object(exe, event),
                &child_indent,
                &event_indent,
            )
        }
    }
}

/// Rewrites one event's existing hook's `"command"` value text in place to the current
/// executable's path, re-scanning fresh (an earlier event's rewrite in this same call may have
/// shifted later offsets). A no-op if the hook can no longer be found (should not happen: this is
/// only ever called for an event `inspect` already classified as stale).
fn update_stale_event(doc: &str, exe: &str, event: &str) -> String {
    let bytes = doc.as_bytes();
    let root = jsontext::object(bytes, jsontext::skip_ws(bytes, 0));
    let Some(hooks_member) = root.members.iter().find(|m| m.key == "hooks") else {
        return doc.to_owned();
    };
    if bytes.get(hooks_member.entry.value_start) != Some(&b'{') {
        return doc.to_owned();
    }
    let hooks_obj = jsontext::object(bytes, hooks_member.entry.value_start);
    let Some(event_member) = hooks_obj.members.iter().find(|m| m.key == event) else {
        return doc.to_owned();
    };
    if bytes.get(event_member.entry.value_start) != Some(&b'[') {
        return doc.to_owned();
    }
    let arr = jsontext::array(bytes, event_member.entry.value_start);
    let Some(found) = find_ours(bytes, &arr, event) else {
        return doc.to_owned();
    };
    let new_value = jsontext::escape(&command(exe, event));
    format!(
        "{}{}{}",
        &doc[..found.command_value.value_start],
        new_value,
        &doc[found.command_value.value_end..]
    )
}

fn duplicate_key(members: &[Member]) -> Option<&str> {
    let mut seen = std::collections::HashSet::new();
    members
        .iter()
        .find(|m| !seen.insert(m.key.as_str()))
        .map(|m| m.key.as_str())
}

/// Checks the structure without needing the executable's path: the top level is an object,
/// `"hooks"` (if present at all) is not duplicated and is itself an object with no duplicated
/// key, and none of our five events is a non-array. Returns the parsed `hooks` object, if there
/// is one, so callers do not have to re-scan for it.
fn parse_checked(doc: &str) -> Result<Option<jsontext::Obj>> {
    let bytes = doc.as_bytes();
    let start = jsontext::skip_ws(bytes, 0);
    if bytes.get(start) != Some(&b'{') {
        return Err(Error::invalid(
            "the top level of settings.json is not an object",
        ));
    }
    let root = jsontext::object(bytes, start);
    if root.members.iter().filter(|m| m.key == "hooks").count() > 1 {
        return Err(Error::invalid(
            "settings.json has \"hooks\" more than once; fix it by hand first",
        ));
    }
    let Some(hooks_member) = root.members.iter().find(|m| m.key == "hooks") else {
        return Ok(None);
    };
    if bytes.get(hooks_member.entry.value_start) != Some(&b'{') {
        return Err(Error::invalid(
            "\"hooks\" in settings.json is not an object",
        ));
    }
    let hooks_obj = jsontext::object(bytes, hooks_member.entry.value_start);
    if let Some(dup) = duplicate_key(&hooks_obj.members) {
        return Err(Error::invalid(format!(
            "settings.json's \"hooks\" has \"{dup}\" more than once; fix it by hand first"
        )));
    }
    for event in EVENTS {
        if let Some(m) = hooks_obj.members.iter().find(|m| m.key == event)
            && bytes.get(m.entry.value_start) != Some(&b'[')
        {
            return Err(Error::invalid(format!(
                "\"hooks\".\"{event}\" in settings.json is not an array"
            )));
        }
    }
    Ok(Some(hooks_obj))
}

enum EventState {
    Missing,
    Fresh,
    Stale,
}

struct EventInfo {
    state: EventState,
    /// Whether this event's key already existed (with any array, even empty) before this call.
    key_existed: bool,
}

/// Checks the existing structure without changing anything: whether `"hooks"` already existed,
/// and for each event, whether its key already existed and whether it already has one of our
/// hooks — and if so, whether it names the current executable or an old path.
fn inspect(doc: &str, exe: &str) -> Result<(bool, Vec<EventInfo>)> {
    let bytes = doc.as_bytes();
    let hooks_obj = parse_checked(doc)?;
    let hooks_existed = hooks_obj.is_some();
    let Some(hooks_obj) = hooks_obj else {
        let infos = EVENTS
            .iter()
            .map(|_| EventInfo {
                state: EventState::Missing,
                key_existed: false,
            })
            .collect();
        return Ok((false, infos));
    };
    let mut infos = Vec::with_capacity(EVENTS.len());
    for event in EVENTS {
        let Some(m) = hooks_obj.members.iter().find(|m| m.key == event) else {
            infos.push(EventInfo {
                state: EventState::Missing,
                key_existed: false,
            });
            continue;
        };
        let arr = jsontext::array(bytes, m.entry.value_start);
        let state = match find_ours(bytes, &arr, event) {
            None => EventState::Missing,
            Some(found)
                if command_matches_current(bytes, found.command_value.value_start, exe, event) =>
            {
                EventState::Fresh
            }
            Some(_) => EventState::Stale,
        };
        infos.push(EventInfo {
            state,
            key_existed: true,
        });
    }
    Ok((hooks_existed, infos))
}

fn read_text(path: &std::path::Path) -> Result<(Option<Vec<u8>>, String, bool)> {
    let before = super::read_optional(path)?;
    let text = match &before {
        Some(bytes) => String::from_utf8(bytes.clone())
            .map_err(|_| Error::invalid(format!("{} is not UTF-8 text", path.display())))?,
        None => String::new(),
    };
    let (had_bom, rest) = super::split_bom(&text);
    Ok((before, rest.to_owned(), had_bom))
}

fn conflicting(detail: String) -> Plan {
    Plan {
        target: Target::Claude,
        status: Status::Conflicting,
        detail,
        changes: vec![],
    }
}

/// Records `hooks_created`/`newly_created` into whatever provenance was already on disk (so two
/// separate installs accumulate rather than overwrite each other), as a `Change` to add to the
/// plan — `None` when there is nothing new to record.
fn provenance_change(
    settings_path: &std::path::Path,
    hooks_created: bool,
    newly_created: &[&str],
) -> Result<Option<Change>> {
    if !hooks_created && newly_created.is_empty() {
        return Ok(None);
    }
    let provenance_path = provenance_path(settings_path);
    let before = super::read_optional(&provenance_path)?;
    let mut provenance = before
        .as_deref()
        .and_then(parse_provenance)
        .unwrap_or_default();
    provenance.hooks_created = provenance.hooks_created || hooks_created;
    for event in newly_created {
        if !provenance.events_created.iter().any(|e| e == event) {
            provenance.events_created.push((*event).to_owned());
        }
    }
    Ok(Some(Change {
        path: provenance_path,
        before,
        after: provenance_json(&provenance).into_bytes(),
        delete: false,
        executable: false,
    }))
}

pub(crate) fn plan_install(env: Env<'_>, exe: &str) -> Result<Plan> {
    let path = path(env)?;
    let (before, original, had_bom) = read_text(&path)?;

    if original.trim().is_empty() {
        let all: Vec<&str> = EVENTS.to_vec();
        let mut changes = vec![Change {
            path: path.clone(),
            before,
            after: super::with_bom(had_bom, fresh_document(exe)).into_bytes(),
            delete: false,
            executable: false,
        }];
        if let Some(c) = provenance_change(&path, true, &all)? {
            changes.push(c);
        }
        return Ok(Plan {
            target: Target::Claude,
            status: Status::Missing,
            detail: format!("{} (none of the 5 events are set up)", path.display()),
            changes,
        });
    }

    match serde_json::from_str::<serde_json::Value>(&original) {
        Ok(serde_json::Value::Object(_)) => {}
        Ok(_) => {
            return Ok(conflicting(format!(
                "{} is not a JSON object",
                path.display()
            )));
        }
        Err(e) => {
            return Ok(conflicting(format!(
                "{} is not valid JSON: {e}",
                path.display()
            )));
        }
    }

    let (hooks_existed, infos) = match inspect(&original, exe) {
        Ok(v) => v,
        Err(e) => return Ok(conflicting(format!("{}: {}", path.display(), e.message))),
    };

    let missing: Vec<&str> = EVENTS
        .iter()
        .copied()
        .zip(&infos)
        .filter(|(_, i)| matches!(i.state, EventState::Missing))
        .map(|(e, _)| e)
        .collect();
    let stale: Vec<&str> = EVENTS
        .iter()
        .copied()
        .zip(&infos)
        .filter(|(_, i)| matches!(i.state, EventState::Stale))
        .map(|(e, _)| e)
        .collect();
    let newly_created: Vec<&str> = EVENTS
        .iter()
        .copied()
        .zip(&infos)
        .filter(|(_, i)| matches!(i.state, EventState::Missing) && !i.key_existed)
        .map(|(e, _)| e)
        .collect();

    if missing.is_empty() && stale.is_empty() {
        return Ok(Plan {
            target: Target::Claude,
            status: Status::Installed,
            detail: format!("{} (all 5 events wired up)", path.display()),
            changes: vec![],
        });
    }

    let mut doc = original.clone();
    for event in missing.iter().copied() {
        doc = insert_event(&doc, exe, event);
    }
    for event in stale.iter().copied() {
        doc = update_stale_event(&doc, exe, event);
    }

    let status = if missing.len() == EVENTS.len() {
        Status::Missing
    } else if !missing.is_empty() {
        Status::Partial
    } else {
        Status::Stale
    };
    let mut parts = Vec::new();
    if !missing.is_empty() {
        parts.push(format!(
            "{} of 5 events missing: {}",
            missing.len(),
            missing.join(", ")
        ));
    }
    if !stale.is_empty() {
        parts.push(format!(
            "{} stale (an old executable path): {}",
            stale.len(),
            stale.join(", ")
        ));
    }

    let mut changes = vec![Change {
        path: path.clone(),
        before,
        after: super::with_bom(had_bom, doc).into_bytes(),
        delete: false,
        executable: false,
    }];
    if let Some(c) = provenance_change(&path, !hooks_existed, &newly_created)? {
        changes.push(c);
    }

    Ok(Plan {
        target: Target::Claude,
        status,
        detail: format!("{} ({})", path.display(), parts.join("; ")),
        changes,
    })
}

/// Removes `event`'s hook, exactly: the whole matcher group is deleted only when it is exactly
/// our shape (`matcher: ""`, our one hook and nothing else); otherwise only our one hook is
/// removed from inside the group's `"hooks"` array, and the group itself is left in place. If the
/// group was removed entirely *and* `created_by_us` says this event's key did not exist before
/// our first install, the now-empty key is removed too — fully reversing our own creation, never
/// a container the file already had. Returns whether anything changed.
fn remove_event(doc: &str, event: &str, created_by_us: bool) -> (String, bool) {
    let bytes = doc.as_bytes();
    let root = jsontext::object(bytes, jsontext::skip_ws(bytes, 0));
    let Some(hooks_member) = root.members.iter().find(|m| m.key == "hooks") else {
        return (doc.to_owned(), false);
    };
    if bytes.get(hooks_member.entry.value_start) != Some(&b'{') {
        return (doc.to_owned(), false);
    }
    let hooks_obj = jsontext::object(bytes, hooks_member.entry.value_start);
    let Some(event_member) = hooks_obj.members.iter().find(|m| m.key == event) else {
        return (doc.to_owned(), false);
    };
    if bytes.get(event_member.entry.value_start) != Some(&b'[') {
        return (doc.to_owned(), false);
    }
    let arr = jsontext::array(bytes, event_member.entry.value_start);
    let Some(found) = find_ours(bytes, &arr, event) else {
        return (doc.to_owned(), false);
    };

    let new_doc = if found.exactly_ours {
        jsontext::remove(doc, &arr.elements, found.group_index)
    } else {
        let group_el = arr.elements[found.group_index];
        let group = jsontext::object(bytes, group_el.value_start);
        let hooks_inner_member = group
            .members
            .iter()
            .find(|m| m.key == "hooks")
            .expect("find_ours only matches inside a group that has a \"hooks\" array");
        let inner = jsontext::array(bytes, hooks_inner_member.entry.value_start);
        jsontext::remove(doc, &inner.elements, found.hook_index)
    };

    let new_doc = if created_by_us && found.exactly_ours {
        remove_whole_event_key(&new_doc, event)
    } else {
        new_doc
    };
    (new_doc, true)
}

/// Removes the whole `event` member (key and array) from `"hooks"`, re-scanning fresh.
fn remove_whole_event_key(doc: &str, event: &str) -> String {
    let bytes = doc.as_bytes();
    let root = jsontext::object(bytes, jsontext::skip_ws(bytes, 0));
    let Some(hooks_member) = root.members.iter().find(|m| m.key == "hooks") else {
        return doc.to_owned();
    };
    if bytes.get(hooks_member.entry.value_start) != Some(&b'{') {
        return doc.to_owned();
    }
    let hooks_obj = jsontext::object(bytes, hooks_member.entry.value_start);
    let Some(idx) = hooks_obj.members.iter().position(|m| m.key == event) else {
        return doc.to_owned();
    };
    let entries: Vec<Entry> = hooks_obj.members.iter().map(|m| m.entry).collect();
    jsontext::remove(doc, &entries, idx)
}

/// Removes `"hooks"` itself from the root, but only if it is now empty, re-scanning fresh.
fn remove_hooks_key_if_empty(doc: &str) -> String {
    let bytes = doc.as_bytes();
    let root = jsontext::object(bytes, jsontext::skip_ws(bytes, 0));
    let Some(hooks_member) = root.members.iter().find(|m| m.key == "hooks") else {
        return doc.to_owned();
    };
    if bytes.get(hooks_member.entry.value_start) != Some(&b'{') {
        return doc.to_owned();
    }
    if !jsontext::object(bytes, hooks_member.entry.value_start)
        .members
        .is_empty()
    {
        return doc.to_owned();
    }
    let entries: Vec<Entry> = root.members.iter().map(|m| m.entry).collect();
    let idx = root
        .members
        .iter()
        .position(|m| m.key == "hooks")
        .expect("just found it above");
    jsontext::remove(doc, &entries, idx)
}

pub(crate) fn plan_uninstall(env: Env<'_>) -> Result<Plan> {
    let path = path(env)?;
    let (before, original, had_bom) = read_text(&path)?;
    if original.trim().is_empty() {
        return Ok(missing_plan(path));
    }
    if !matches!(
        serde_json::from_str::<serde_json::Value>(&original),
        Ok(serde_json::Value::Object(_))
    ) {
        return Ok(conflicting(format!(
            "{} is not a valid JSON object",
            path.display()
        )));
    }
    if let Err(e) = parse_checked(&original) {
        return Ok(conflicting(format!("{}: {}", path.display(), e.message)));
    }

    let provenance_path = provenance_path(&path);
    let provenance_before = super::read_optional(&provenance_path)?;
    let provenance = provenance_before
        .as_deref()
        .and_then(parse_provenance)
        .unwrap_or_default();

    let mut doc = original.clone();
    let mut removed = 0usize;
    for event in EVENTS {
        let created_by_us = provenance.events_created.iter().any(|e| e == event);
        let (new_doc, did) = remove_event(&doc, event, created_by_us);
        doc = new_doc;
        if did {
            removed += 1;
        }
    }
    if provenance.hooks_created {
        doc = remove_hooks_key_if_empty(&doc);
    }

    if removed == 0 {
        return Ok(missing_plan(path));
    }
    let status = if removed == EVENTS.len() {
        Status::Installed
    } else {
        Status::Partial
    };
    let mut changes = vec![Change {
        path: path.clone(),
        before,
        after: super::with_bom(had_bom, doc).into_bytes(),
        delete: false,
        executable: false,
    }];
    if provenance_before.is_some() {
        changes.push(Change {
            path: provenance_path,
            before: provenance_before,
            after: Vec::new(),
            delete: true,
            executable: false,
        });
    }
    Ok(Plan {
        target: Target::Claude,
        status,
        detail: format!("{} (removing {removed} of 5 events)", path.display()),
        changes,
    })
}

fn missing_plan(path: PathBuf) -> Plan {
    Plan {
        target: Target::Claude,
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

    /// Applies every change in `plan` (test helper, without the production re-check-before-write
    /// machinery that lives in `install::mod`).
    fn apply(plan: &Plan) {
        for c in &plan.changes {
            if c.delete {
                let _ = std::fs::remove_file(&c.path);
            } else {
                if let Some(dir) = c.path.parent() {
                    let _ = std::fs::create_dir_all(dir);
                }
                std::fs::write(&c.path, &c.after).unwrap();
            }
        }
    }

    #[test]
    fn installs_into_an_empty_directory_then_is_a_no_op() {
        let tmp = tempfile::tempdir().unwrap();
        let env = env_of(&[("CLAUDE_CONFIG_DIR", tmp.path().to_str().unwrap())]);
        let plan = plan_install(&env, EXE).unwrap();
        assert!(plan.status == Status::Missing);
        let after = String::from_utf8(plan.changes[0].after.clone()).unwrap();
        let value: serde_json::Value = serde_json::from_str(&after).unwrap();
        assert_eq!(
            value["hooks"]["Stop"][0]["hooks"][0]["command"],
            format!("{EXE} hook claude Stop")
        );
        apply(&plan);

        // Installing again against this file is a no-op.
        let plan2 = plan_install(&env, EXE).unwrap();
        assert!(plan2.status == Status::Installed);
        assert!(plan2.changes.is_empty());
    }

    #[test]
    fn preserves_unrelated_content_and_formatting() {
        let original = "{\n    \"foo\": \"bar\",\n    \"hooks\": {\n        \"PreToolUse\": [\n            {\n                \"matcher\": \"Bash\",\n                \"hooks\": [{\"type\": \"command\", \"command\": \"echo hi\"}]\n            }\n        ]\n    }\n}\n";
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("settings.json");
        std::fs::write(&path, original).unwrap();
        let env = env_of(&[("CLAUDE_CONFIG_DIR", tmp.path().to_str().unwrap())]);

        let plan = plan_install(&env, EXE).unwrap();
        let after = String::from_utf8(plan.changes[0].after.clone()).unwrap();
        assert!(after.contains("\"foo\": \"bar\""));
        assert!(after.contains("\"PreToolUse\""));
        assert!(after.contains("\"echo hi\""));
        let value: serde_json::Value = serde_json::from_str(&after).unwrap();
        assert_eq!(value["hooks"]["SessionStart"][0]["matcher"], "");
        apply(&plan);

        // Uninstalling restores the original file exactly: "hooks" and "PreToolUse" pre-existed,
        // so they stay; the five event keys we created from nothing are fully removed again.
        let plan = plan_uninstall(&env).unwrap();
        let restored = String::from_utf8(plan.changes[0].after.clone()).unwrap();
        assert_eq!(restored, original);
    }

    #[test]
    fn round_trip_removes_everything_it_added_from_scratch() {
        let tmp = tempfile::tempdir().unwrap();
        let env = env_of(&[("CLAUDE_CONFIG_DIR", tmp.path().to_str().unwrap())]);

        let plan = plan_install(&env, EXE).unwrap();
        assert_eq!(plan.changes.len(), 2, "config + provenance");
        apply(&plan);

        let plan = plan_uninstall(&env).unwrap();
        let restored = String::from_utf8(plan.changes[0].after.clone()).unwrap();
        // Nothing existed before install, so a full uninstall leaves nothing meaningful: "hooks"
        // was entirely our own creation, so it is fully removed again, down to the key.
        let value: serde_json::Value = serde_json::from_str(&restored).unwrap();
        assert!(value.as_object().unwrap().is_empty(), "{restored}");
        apply(&plan);
        assert!(
            !provenance_path(&path(&env).unwrap()).exists(),
            "the sidecar must be cleaned up too"
        );
    }

    #[test]
    fn a_non_object_hooks_is_a_conflict_not_a_crash() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("settings.json");
        std::fs::write(&path, r#"{"hooks": "nope"}"#).unwrap();
        let env = env_of(&[("CLAUDE_CONFIG_DIR", tmp.path().to_str().unwrap())]);
        let plan = plan_install(&env, EXE).unwrap();
        assert!(plan.status == Status::Conflicting);
        assert!(plan.changes.is_empty());
    }

    #[test]
    fn foreign_hooks_in_the_same_event_are_kept() {
        let original = r#"{"hooks": {"Stop": [{"matcher": "", "hooks": [{"type": "command", "command": "notify-send done"}]}]}}"#;
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("settings.json");
        std::fs::write(&path, original).unwrap();
        let env = env_of(&[("CLAUDE_CONFIG_DIR", tmp.path().to_str().unwrap())]);

        let plan = plan_install(&env, EXE).unwrap();
        let after = String::from_utf8(plan.changes[0].after.clone()).unwrap();
        assert!(after.contains("notify-send done"));
        let value: serde_json::Value = serde_json::from_str(&after).unwrap();
        assert_eq!(value["hooks"]["Stop"].as_array().unwrap().len(), 2);
        apply(&plan);

        let plan = plan_uninstall(&env).unwrap();
        let restored = String::from_utf8(plan.changes[0].after.clone()).unwrap();
        assert_eq!(restored, original);
    }

    #[test]
    fn a_foreign_hook_added_to_our_own_matcher_group_survives_uninstall() {
        // As if someone used Claude Code's own `/hooks` UI, or hand-edited, to add a second hook
        // into the exact matcher group we created.
        let tmp = tempfile::tempdir().unwrap();
        let env = env_of(&[("CLAUDE_CONFIG_DIR", tmp.path().to_str().unwrap())]);
        let path = path(&env).unwrap();

        let plan = plan_install(&env, EXE).unwrap();
        apply(&plan);
        let installed = std::fs::read_to_string(&path).unwrap();
        let mut value: serde_json::Value = serde_json::from_str(&installed).unwrap();
        value["hooks"]["Stop"][0]["hooks"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({"type": "command", "command": "echo also-run-this"}));
        std::fs::write(&path, serde_json::to_string_pretty(&value).unwrap()).unwrap();

        let plan = plan_uninstall(&env).unwrap();
        let restored = String::from_utf8(plan.changes[0].after.clone()).unwrap();
        let after: serde_json::Value = serde_json::from_str(&restored).unwrap();
        let stop_hooks = after["hooks"]["Stop"][0]["hooks"].as_array().unwrap();
        assert_eq!(stop_hooks.len(), 1, "{restored}");
        assert_eq!(stop_hooks[0]["command"], "echo also-run-this");
        // Every other event, untouched by hand, is removed entirely (we created all of them).
        assert!(after["hooks"]["SessionStart"].is_null(), "{restored}");
    }

    #[test]
    fn a_look_alike_program_is_never_claimed_as_ours() {
        let original = r#"{"hooks": {"Stop": [{"matcher": "", "hooks": [{"type": "command", "command": "/usr/local/bin/not-pitcrew hook claude Stop"}]}]}}"#;
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("settings.json");
        std::fs::write(&path, original).unwrap();
        let env = env_of(&[("CLAUDE_CONFIG_DIR", tmp.path().to_str().unwrap())]);

        // install treats Stop as still missing and adds our own, separate, group.
        let plan = plan_install(&env, EXE).unwrap();
        let after = String::from_utf8(plan.changes[0].after.clone()).unwrap();
        let value: serde_json::Value = serde_json::from_str(&after).unwrap();
        assert_eq!(value["hooks"]["Stop"].as_array().unwrap().len(), 2);
        apply(&plan);

        // uninstall never touches the look-alike's group.
        let plan = plan_uninstall(&env).unwrap();
        let restored = String::from_utf8(plan.changes[0].after.clone()).unwrap();
        let value: serde_json::Value = serde_json::from_str(&restored).unwrap();
        let stop = value["hooks"]["Stop"].as_array().unwrap();
        assert_eq!(stop.len(), 1, "{restored}");
        assert_eq!(
            stop[0]["hooks"][0]["command"],
            "/usr/local/bin/not-pitcrew hook claude Stop"
        );
    }

    #[test]
    fn a_pre_existing_empty_hooks_object_survives_uninstall() {
        let original = r#"{"hooks": {}}"#;
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("settings.json");
        std::fs::write(&path, original).unwrap();
        let env = env_of(&[("CLAUDE_CONFIG_DIR", tmp.path().to_str().unwrap())]);

        let plan = plan_install(&env, EXE).unwrap();
        apply(&plan);

        let plan = plan_uninstall(&env).unwrap();
        let restored = String::from_utf8(plan.changes[0].after.clone()).unwrap();
        let value: serde_json::Value = serde_json::from_str(&restored).unwrap();
        // "hooks" must still be present (it pre-existed us), just empty of our events again.
        assert!(value.get("hooks").is_some(), "{restored}");
        assert_eq!(value["hooks"].as_object().unwrap().len(), 0, "{restored}");
    }

    #[test]
    fn a_pre_existing_empty_event_array_survives_uninstall() {
        let original = r#"{"hooks": {"Stop": []}}"#;
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("settings.json");
        std::fs::write(&path, original).unwrap();
        let env = env_of(&[("CLAUDE_CONFIG_DIR", tmp.path().to_str().unwrap())]);

        let plan = plan_install(&env, EXE).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&plan.changes[0].after).unwrap();
        assert_eq!(value["hooks"]["Stop"].as_array().unwrap().len(), 1);
        apply(&plan);

        let plan = plan_uninstall(&env).unwrap();
        let restored = String::from_utf8(plan.changes[0].after.clone()).unwrap();
        let value: serde_json::Value = serde_json::from_str(&restored).unwrap();
        assert!(
            value["hooks"].as_object().unwrap().contains_key("Stop"),
            "{restored}"
        );
        assert_eq!(
            value["hooks"]["Stop"].as_array().unwrap().len(),
            0,
            "{restored}"
        );
        // The four events that really were created fresh are gone entirely.
        assert!(value["hooks"].get("SessionStart").is_none(), "{restored}");
    }

    #[test]
    fn a_moved_executable_is_reported_stale_and_install_updates_it() {
        let tmp = tempfile::tempdir().unwrap();
        let env = env_of(&[("CLAUDE_CONFIG_DIR", tmp.path().to_str().unwrap())]);
        let path = path(&env).unwrap();

        let plan = plan_install(&env, "/old/place/pitcrew").unwrap();
        apply(&plan);

        let status_plan = plan_install(&env, "/new/place/pitcrew").unwrap();
        assert!(
            status_plan.status == Status::Stale,
            "{}",
            status_plan.detail
        );
        assert!(status_plan.detail.contains("stale"));
        let updated = String::from_utf8(status_plan.changes[0].after.clone()).unwrap();
        assert!(updated.contains("/new/place/pitcrew"));
        assert!(!updated.contains("/old/place/pitcrew"));
        let value: serde_json::Value = serde_json::from_str(&updated).unwrap();
        // Everything else about the entry — including the array having exactly one hook — is
        // unchanged, only the command text moved.
        assert_eq!(value["hooks"]["Stop"].as_array().unwrap().len(), 1);
        let _ = path; // silence unused warning if the assertions above change
    }

    #[test]
    fn duplicate_hooks_key_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("settings.json");
        std::fs::write(&path, r#"{"hooks": {}, "other": 1, "hooks": {"Stop": []}}"#).unwrap();
        let env = env_of(&[("CLAUDE_CONFIG_DIR", tmp.path().to_str().unwrap())]);
        let plan = plan_install(&env, EXE).unwrap();
        assert!(plan.status == Status::Conflicting);
        assert!(plan.changes.is_empty());
        assert!(plan.detail.contains("more than once"), "{}", plan.detail);
    }

    #[test]
    fn duplicate_event_key_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("settings.json");
        std::fs::write(&path, r#"{"hooks": {"Stop": [], "Stop": []}}"#).unwrap();
        let env = env_of(&[("CLAUDE_CONFIG_DIR", tmp.path().to_str().unwrap())]);
        let plan = plan_install(&env, EXE).unwrap();
        assert!(plan.status == Status::Conflicting);
        assert!(plan.changes.is_empty());
        assert!(plan.detail.contains("more than once"), "{}", plan.detail);
    }

    #[test]
    fn crlf_and_tabs_and_a_bom_are_preserved() {
        let original = "\u{feff}{\r\n\t\"hooks\": {\r\n\t\t\"PreToolUse\": [\r\n\t\t\t{\"matcher\": \"Bash\", \"hooks\": [{\"type\": \"command\", \"command\": \"echo hi\"}]}\r\n\t\t]\r\n\t}\r\n}";
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("settings.json");
        std::fs::write(&path, original).unwrap();
        let env = env_of(&[("CLAUDE_CONFIG_DIR", tmp.path().to_str().unwrap())]);

        let plan = plan_install(&env, EXE).unwrap();
        let after = String::from_utf8(plan.changes[0].after.clone()).unwrap();
        assert!(after.starts_with('\u{feff}'), "BOM must survive");
        assert!(after.contains("\r\n"), "CRLF must survive");
        apply(&plan);

        let plan = plan_uninstall(&env).unwrap();
        let restored = String::from_utf8(plan.changes[0].after.clone()).unwrap();
        assert_eq!(restored, original);
    }

    #[test]
    fn a_minified_file_round_trips() {
        let original = r#"{"hooks":{"PreToolUse":[{"matcher":"Bash","hooks":[{"type":"command","command":"echo hi"}]}]}}"#;
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("settings.json");
        std::fs::write(&path, original).unwrap();
        let env = env_of(&[("CLAUDE_CONFIG_DIR", tmp.path().to_str().unwrap())]);

        let plan = plan_install(&env, EXE).unwrap();
        apply(&plan);

        let plan = plan_uninstall(&env).unwrap();
        let restored = String::from_utf8(plan.changes[0].after.clone()).unwrap();
        assert_eq!(restored, original);
    }

    #[test]
    fn the_quoted_command_matches_the_platform() {
        let cmd = command("/a/b c/pitcrew", "Stop");
        if cfg!(windows) {
            assert_eq!(cmd, "\"/a/b c/pitcrew\" hook claude Stop");
        } else {
            assert_eq!(cmd, "'/a/b c/pitcrew' hook claude Stop");
        }
    }

    #[test]
    fn windows_command_is_always_quoted_even_without_special_characters() {
        let cmd = command("/a/b/pitcrew", "Stop");
        if cfg!(windows) {
            assert_eq!(cmd, "\"/a/b/pitcrew\" hook claude Stop");
        } else {
            assert_eq!(cmd, "/a/b/pitcrew hook claude Stop");
        }
    }
}
