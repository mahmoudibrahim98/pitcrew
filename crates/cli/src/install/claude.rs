//! Claude Code's `hooks` section, in `settings.json` (`CLAUDE_CONFIG_DIR`, else `~/.claude`).
//!
//! Each of our five events becomes one matcher object, `{"matcher": "", "hooks": [{"type":
//! "command", "command": "<exe> hook claude <Event>", "timeout": 5}]}`, appended to that event's
//! array (creating the array, and `"hooks"` itself, if they are missing). `matcher: ""` matches
//! every occurrence of the event, the same as Claude Code's own tool-matcher events use it to
//! mean "every tool"; Claude Code's hooks reference does not document a matcher convention for
//! these five specifically, so this is the conservative, documented-elsewhere default.
//!
//! An entry is ours exactly when its `command` ends in the literal text `hook claude <Event>`,
//! regardless of the quoted path in front of it, so it is still recognised after the executable
//! moves. We only ever add or remove our own matcher objects: existing keys, events, and
//! formatting are otherwise untouched, which is what makes an install/uninstall round trip
//! byte-identical.

use super::jsontext::{self, Entry};
use super::{Change, Plan, Status, Target};
use crate::config::Env;
use crate::error::{Error, Result};
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

fn command(exe: &str, event: &str) -> String {
    format!("{} hook claude {event}", super::quote_exe_path(exe))
}

fn matcher_object(exe: &str, event: &str) -> String {
    format!(
        r#"{{"matcher": "", "hooks": [{{"type": "command", "command": {}, "timeout": 5}}]}}"#,
        jsontext::escape(&command(exe, event))
    )
}

/// Whether a hook-matcher object's raw text is ours: it mentions `hook claude <Event>` right
/// before a closing quote. The marker is plain ASCII letters and spaces, so it appears
/// byte-for-byte the same inside a JSON string as outside one; no unescaping is needed to find
/// it.
fn is_ours_fragment(text: &str, event: &str) -> bool {
    text.contains(&format!("hook claude {event}\""))
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
            jsontext::append(doc, &entries, hooks_obj.close, &new_text, &child_indent, &hooks_indent)
        }
        Some(m) => {
            if bytes.get(m.entry.value_start) != Some(&b'[') {
                return doc.to_owned();
            }
            let arr = jsontext::array(bytes, m.entry.value_start);
            let already = arr
                .elements
                .iter()
                .any(|el| is_ours_fragment(&doc[el.value_start..el.value_end], event));
            if already {
                return doc.to_owned();
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

/// Checks the existing structure without changing anything: which events are already installed,
/// and whether `"hooks"` (or one of our events) holds something that is not an object/array,
/// which blocks us from adding anything safely.
fn inspect(doc: &str) -> Result<Vec<bool>> {
    let bytes = doc.as_bytes();
    let start = jsontext::skip_ws(bytes, 0);
    if bytes.get(start) != Some(&b'{') {
        return Err(Error::invalid("the top level of settings.json is not an object"));
    }
    let root = jsontext::object(bytes, start);
    let Some(hooks_member) = root.members.iter().find(|m| m.key == "hooks") else {
        return Ok(vec![false; EVENTS.len()]);
    };
    if bytes.get(hooks_member.entry.value_start) != Some(&b'{') {
        return Err(Error::invalid("\"hooks\" in settings.json is not an object"));
    }
    let hooks_obj = jsontext::object(bytes, hooks_member.entry.value_start);
    let mut installed = Vec::with_capacity(EVENTS.len());
    for event in EVENTS {
        let Some(m) = hooks_obj.members.iter().find(|m| m.key == event) else {
            installed.push(false);
            continue;
        };
        if bytes.get(m.entry.value_start) != Some(&b'[') {
            return Err(Error::invalid(format!(
                "\"hooks\".\"{event}\" in settings.json is not an array"
            )));
        }
        let arr = jsontext::array(bytes, m.entry.value_start);
        installed.push(
            arr.elements
                .iter()
                .any(|el| is_ours_fragment(&doc[el.value_start..el.value_end], event)),
        );
    }
    Ok(installed)
}

fn read_text(path: &std::path::Path) -> Result<(Option<Vec<u8>>, String)> {
    let before = super::read_optional(path)?;
    let text = match &before {
        Some(bytes) => String::from_utf8(bytes.clone())
            .map_err(|_| Error::invalid(format!("{} is not UTF-8 text", path.display())))?,
        None => String::new(),
    };
    Ok((before, text))
}

pub(crate) fn plan_install(env: Env<'_>, exe: &str) -> Result<Plan> {
    let path = path(env)?;
    let (before, original) = read_text(&path)?;

    if original.trim().is_empty() {
        return Ok(Plan {
            target: Target::Claude,
            status: Status::Missing,
            detail: format!("{} (none of the 5 events are set up)", path.display()),
            changes: vec![Change {
                path,
                before,
                after: fresh_document(exe).into_bytes(),
                delete: false,
                executable: false,
            }],
        });
    }

    match serde_json::from_str::<serde_json::Value>(&original) {
        Ok(serde_json::Value::Object(_)) => {}
        Ok(_) => return Err(Error::invalid(format!("{} is not a JSON object", path.display()))),
        Err(e) => {
            return Err(Error::invalid(format!(
                "{} is not valid JSON: {e}",
                path.display()
            )));
        }
    }

    let installed = match inspect(&original) {
        Ok(installed) => installed,
        Err(e) => {
            return Ok(Plan {
                target: Target::Claude,
                status: Status::Conflicting,
                detail: format!("{}: {}", path.display(), e.message),
                changes: vec![],
            });
        }
    };
    let missing: Vec<&str> = EVENTS
        .iter()
        .copied()
        .zip(installed.iter().copied())
        .filter(|(_, ok)| !*ok)
        .map(|(e, _)| e)
        .collect();
    if missing.is_empty() {
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
    let status = if missing.len() == EVENTS.len() {
        Status::Missing
    } else {
        Status::Partial
    };
    Ok(Plan {
        target: Target::Claude,
        status,
        detail: format!(
            "{} ({} of 5 events missing: {})",
            path.display(),
            missing.len(),
            missing.join(", ")
        ),
        changes: vec![Change {
            path,
            before,
            after: doc.into_bytes(),
            delete: false,
            executable: false,
        }],
    })
}

/// Removes `event`'s matcher object from `doc` if present, dropping the event's array (and, if
/// that was the only event, `"hooks"` itself) when it becomes empty. Returns whether anything
/// changed.
fn remove_event(doc: &str, event: &str) -> (String, bool) {
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
    let Some(el_idx) = arr
        .elements
        .iter()
        .position(|el| is_ours_fragment(&doc[el.value_start..el.value_end], event))
    else {
        return (doc.to_owned(), false);
    };
    let was_only_element = arr.elements.len() == 1;
    let mut doc = jsontext::remove(doc, &arr.elements, el_idx);
    if was_only_element {
        doc = remove_member(&doc, &["hooks"], event);
    }
    doc = drop_if_empty(&doc, &["hooks"]);
    (doc, true)
}

/// Removes the member `key` from the object reached by following `path` from the root,
/// re-scanning fresh (earlier edits in this call moved everything after them).
fn remove_member(doc: &str, path: &[&str], key: &str) -> String {
    let Some(obj) = navigate(doc, path) else {
        return doc.to_owned();
    };
    let Some(idx) = obj.members.iter().position(|m| m.key == key) else {
        return doc.to_owned();
    };
    let entries: Vec<Entry> = obj.members.iter().map(|m| m.entry).collect();
    jsontext::remove(doc, &entries, idx)
}

/// Removes the last segment of `path` from its own parent if it is now an empty object.
fn drop_if_empty(doc: &str, path: &[&str]) -> String {
    let Some((&key, parent_path)) = path.split_last() else {
        return doc.to_owned();
    };
    let Some(parent) = navigate(doc, parent_path) else {
        return doc.to_owned();
    };
    let Some(idx) = parent.members.iter().position(|m| m.key == key) else {
        return doc.to_owned();
    };
    let bytes = doc.as_bytes();
    let member = &parent.members[idx];
    if bytes.get(member.entry.value_start) != Some(&b'{') {
        return doc.to_owned();
    }
    if !jsontext::object(bytes, member.entry.value_start).members.is_empty() {
        return doc.to_owned();
    }
    let entries: Vec<Entry> = parent.members.iter().map(|m| m.entry).collect();
    jsontext::remove(doc, &entries, idx)
}

/// The object reached by following `path` (each segment an object member) from the root.
fn navigate(doc: &str, path: &[&str]) -> Option<jsontext::Obj> {
    let bytes = doc.as_bytes();
    let mut obj = jsontext::object(bytes, jsontext::skip_ws(bytes, 0));
    for part in path {
        let m = obj.members.iter().find(|m| &m.key == part)?;
        if bytes.get(m.entry.value_start) != Some(&b'{') {
            return None;
        }
        obj = jsontext::object(bytes, m.entry.value_start);
    }
    Some(obj)
}

pub(crate) fn plan_uninstall(env: Env<'_>) -> Result<Plan> {
    let path = path(env)?;
    let (before, original) = read_text(&path)?;
    if original.trim().is_empty() {
        return Ok(missing_plan(path));
    }
    if !matches!(
        serde_json::from_str::<serde_json::Value>(&original),
        Ok(serde_json::Value::Object(_))
    ) {
        return Err(Error::invalid(format!("{} is not a valid JSON object", path.display())));
    }

    let mut doc = original.clone();
    let mut removed = 0usize;
    for event in EVENTS {
        let (new_doc, did) = remove_event(&doc, event);
        doc = new_doc;
        if did {
            removed += 1;
        }
    }
    if removed == 0 {
        return Ok(missing_plan(path));
    }
    let status = if removed == EVENTS.len() {
        Status::Installed
    } else {
        Status::Partial
    };
    Ok(Plan {
        target: Target::Claude,
        status,
        detail: format!("{} (removing {removed} of 5 events)", path.display()),
        changes: vec![Change {
            path,
            before,
            after: doc.into_bytes(),
            delete: false,
            executable: false,
        }],
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

    #[test]
    fn installs_into_an_empty_directory_then_is_a_no_op() {
        let tmp = tempfile::tempdir().unwrap();
        let env = env_of(&[("CLAUDE_CONFIG_DIR", tmp.path().to_str().unwrap())]);
        let plan = plan_install(&env, EXE).unwrap();
        assert!(plan.status == Status::Missing);
        assert_eq!(plan.changes.len(), 1);
        let after = String::from_utf8(plan.changes[0].after.clone()).unwrap();
        let value: serde_json::Value = serde_json::from_str(&after).unwrap();
        assert_eq!(value["hooks"]["Stop"][0]["hooks"][0]["command"], format!("{EXE} hook claude Stop"));
        std::fs::write(&plan.changes[0].path, &after).unwrap();

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
        std::fs::write(&path, &after).unwrap();

        // Uninstalling restores the original file exactly.
        let plan = plan_uninstall(&env).unwrap();
        let restored = String::from_utf8(plan.changes[0].after.clone()).unwrap();
        assert_eq!(restored, original);
    }

    #[test]
    fn round_trip_is_byte_identical_from_scratch() {
        let tmp = tempfile::tempdir().unwrap();
        let env = env_of(&[("CLAUDE_CONFIG_DIR", tmp.path().to_str().unwrap())]);
        let path = path(&env).unwrap();

        let plan = plan_install(&env, EXE).unwrap();
        let installed = String::from_utf8(plan.changes[0].after.clone()).unwrap();
        std::fs::write(&path, &installed).unwrap();

        let plan = plan_uninstall(&env).unwrap();
        assert_eq!(plan.changes.len(), 1);
        let restored = String::from_utf8(plan.changes[0].after.clone()).unwrap();
        // Nothing existed before install, so a full uninstall leaves nothing meaningful: an
        // empty `hooks: {}`/root cascade all the way down to a bare, empty document.
        let value: serde_json::Value = serde_json::from_str(&restored).unwrap();
        assert!(value.as_object().unwrap().is_empty(), "{restored}");
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
        std::fs::write(&path, &after).unwrap();

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
}
