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
//! matcher group, and never by a single trailing path segment of a multi-word command: a hook is
//! ours exactly when its `command` ends in the literal text ` hook claude <Event>`, the text
//! before that names a single program — fully quoted, or free of whitespace and of the characters
//! a shell gives a second meaning to (so `afplay ding.aiff; ~/bin/pitcrew hook claude Stop`
//! cannot be read as naming `pitcrew`) — and that program's file name is exactly `pitcrew` or
//! `pitcrew.exe`. `uninstall` deletes a whole matcher group only when it is *exactly* our shape
//! (`matcher: ""`, and that one hook is our only hook); otherwise it removes just our one hook
//! from inside the group, leaving every other hook, and the group itself, untouched.
//!
//! `uninstall` also removes an event's array, or `"hooks"` itself, once removing our content
//! leaves it empty — the same way an empty array/object is treated everywhere else in this
//! module. **The one known case this is not byte-identical for**: if an event's array, or
//! `"hooks"`, already existed and was already empty before we ever touched the file, it is
//! removed along with our content rather than left behind empty. There is no way to tell that
//! case apart from one where removing our own last entry is what emptied it, since both look
//! identical by the time `uninstall` runs; an empty container and an absent key mean exactly the
//! same thing to Claude Code either way, so nothing is actually lost, only the file's own
//! formatting there.
//!
//! Exec hooks (Claude Code >=2.1.139) are ours only when `args` is exactly
//! `["hook", "claude", "<Event>"]` and the unquoted command's file name is pitcrew(.exe).
//! Installing converts between forms, keeps one owned hook, and preserves foreign hooks.
//!
//! Re-running `install` after the executable moved updates just the stale hooks' `command` text
//! in place (`Status::Stale` until then); it never duplicates them.
//!
//! A duplicate `"hooks"` key, or a duplicate event key inside it, is refused rather than guessed
//! at, since different JSON readers disagree about which duplicate wins. A leading UTF-8 BOM, if
//! the file has one, is preserved.

use super::jsontext::{self, Entry, Member};
use super::{Change, HookForm, Plan, Status, Target};
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

/// Quotes the executable's path for Claude Code's `command` field, which Claude Code runs with a
/// shell: `bash` everywhere, which on Windows is Git Bash, and PowerShell on a Windows machine
/// without Git Bash (Claude Code's hooks reference, the `shell` field). So it is quoted for POSIX
/// `sh`, only if needed (`shell_quote_unix`): single quotes keep every character literal, where
/// double quotes would still expand `$` and a backtick and fold `\\`.
///
/// On Windows the path's `\` become `/` first, which Windows, Git Bash and PowerShell all read as
/// separators: in bash an unquoted `\` is an escape, so `C:\Users\…` would lose its backslashes,
/// and a plain path then needs no quotes at all. Unquoted, it runs in PowerShell too, which treats
/// a leading quoted word as a string rather than a program; a path with a blank or another
/// character `sh` gives a meaning to is single-quoted, which suits Git Bash, the default.
fn claude_quote_exe_path(path: &str) -> String {
    if cfg!(windows) {
        super::shell_quote_unix(&path.replace('\\', "/"))
    } else {
        super::shell_quote_unix(path)
    }
}

fn command(exe: &str, event: &str) -> String {
    format!("{} hook claude {event}", claude_quote_exe_path(exe))
}

fn selected_command(exe: &str, event: &str, form: HookForm) -> String {
    if form == HookForm::Exec {
        exe.to_owned()
    } else {
        command(exe, event)
    }
}

fn matcher_object(exe: &str, event: &str, form: HookForm) -> String {
    let args = if form == HookForm::Exec {
        format!(
            r#", "args": ["hook", "claude", {}]"#,
            jsontext::escape(event)
        )
    } else {
        String::new()
    };
    format!(
        r#"{{"matcher": "", "hooks": [{{"type": "command", "command": {}{}, "timeout": 5}}]}}"#,
        jsontext::escape(&selected_command(exe, event, form)),
        args
    )
}

/// Whether one inner hook's already-unescaped `command` text is ours: it ends in the literal
/// marker ` hook claude <Event>`, the text before that is a single program reference (not a
/// multi-word shell command that merely ends by mentioning one), and that program's file name —
/// unquoted, reduced to its last path segment — is exactly `pitcrew` or `pitcrew.exe`.
fn command_is_ours(command: &str, event: &str) -> bool {
    let suffix = format!(" hook claude {event}");
    let Some(exe_part) = command.strip_suffix(&suffix) else {
        return false;
    };
    super::is_single_shell_word(exe_part)
        && super::is_our_exe_name(&super::quoted_word_file_name(exe_part))
}

fn braces(indent: &str, inner: &str) -> String {
    format!("{{\n{indent}  {inner}\n{indent}}}")
}

fn brackets(indent: &str, inner: &str) -> String {
    format!("[\n{indent}  {inner}\n{indent}]")
}

/// A freshly formatted file, for when there is nothing to preserve.
fn fresh_document(exe: &str, form: HookForm) -> String {
    let mut out = String::from("{\n  \"hooks\": {\n");
    for (i, event) in EVENTS.iter().enumerate() {
        let sep = if i + 1 == EVENTS.len() { "" } else { "," };
        let _ = write!(
            &mut out,
            "    \"{event}\": [\n      {}\n    ]{sep}\n",
            matcher_object(exe, event, form)
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
    hook_value: Entry,
    /// Whether the enclosing group is *exactly* our shape — `matcher: ""`, and this is its only
    /// hook — so removing it can safely remove the whole group rather than just this one hook.
    exactly_ours: bool,
}

/// Scans one event's array of matcher-group objects for one of our hooks. Only the first match
/// is ever returned; there should only be one, since `install` never adds a second once one is
/// found, and every caller here re-scans fresh after each edit.
fn find_ours(bytes: &[u8], arr: &jsontext::Arr, event: &str) -> Option<Found> {
    find_all_ours(bytes, arr, event).into_iter().next()
}

fn find_all_ours(bytes: &[u8], arr: &jsontext::Arr, event: &str) -> Vec<Found> {
    let mut found = Vec::new();
    for (group_index, el) in arr.elements.iter().enumerate() {
        if bytes.get(el.value_start) != Some(&b'{') {
            continue; // not an object: not a matcher group we understand, so never ours
        }
        let group = jsontext::object(bytes, el.value_start);
        if duplicate_key(&group.members).is_some() {
            continue;
        }
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
            let hook: serde_json::Value =
                match serde_json::from_slice(&bytes[hook_el.value_start..hook_el.value_end]) {
                    Ok(value) => value,
                    Err(_) => continue,
                };
            if duplicate_key(&hook_obj.members).is_some() || hook["type"] != "command" {
                continue;
            }
            let ours = match hook.get("args") {
                Some(args) => {
                    args == &serde_json::json!(["hook", "claude", event])
                        && super::is_our_exe_name(text.rsplit(['/', '\\']).next().unwrap_or(""))
                }
                None => command_is_ours(&text, event),
            };
            if ours {
                found.push(Found {
                    group_index,
                    hook_index,
                    command_value: cmd_member.entry,
                    hook_value: *hook_el,
                    exactly_ours: matcher_is_empty_string
                        && inner.elements.len() == 1
                        && group.members.len() == 2,
                });
            }
        }
    }
    found
}

fn command_matches_current(
    bytes: &[u8],
    found: &Found,
    exe: &str,
    event: &str,
    form: HookForm,
) -> bool {
    if form == HookForm::Auto {
        return command_matches_current(bytes, found, exe, event, HookForm::Shell)
            || command_matches_current(bytes, found, exe, event, HookForm::Exec);
    }
    let hook: serde_json::Value = match serde_json::from_slice(
        &bytes[found.hook_value.value_start..found.hook_value.value_end],
    ) {
        Ok(value) => value,
        Err(_) => return false,
    };
    hook["command"] == selected_command(exe, event, form)
        && if form == HookForm::Exec {
            hook["args"] == serde_json::json!(["hook", "claude", event])
        } else {
            hook.get("args").is_none()
        }
}

/// Adds `event`'s matcher object to `doc` if it is not already there, re-scanning fresh (the
/// document is small; this keeps every case, including ones nested two or three levels deep,
/// correct without duplicating the splicing logic).
fn insert_event(doc: &str, exe: &str, event: &str, form: HookForm) -> String {
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
                    brackets(&inner_indent, &matcher_object(exe, event, form))
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
                brackets(&child_indent, &matcher_object(exe, event, form))
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
                &matcher_object(exe, event, form),
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
fn update_stale_event(doc: &str, exe: &str, event: &str, form: HookForm) -> String {
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
    // Change only our command and args fields, retaining timeout and any user options.
    let mut replacement = doc[found.hook_value.value_start..found.hook_value.value_end].to_owned();
    let command_start = found.command_value.value_start - found.hook_value.value_start;
    let command_end = found.command_value.value_end - found.hook_value.value_start;
    replacement.replace_range(
        command_start..command_end,
        &jsontext::escape(&selected_command(exe, event, form)),
    );
    let parsed = jsontext::object(replacement.as_bytes(), 0);
    let args = parsed.members.iter().find(|m| m.key == "args");
    if form == HookForm::Exec {
        let value = format!(r#"["hook", "claude", {}]"#, jsontext::escape(event));
        if let Some(args) = args {
            replacement.replace_range(args.entry.value_start..args.entry.value_end, &value);
        } else if let Some(last) = parsed.members.last() {
            let separator = if replacement.contains('\n') {
                let newline = if doc.contains("\r\n") { "\r\n" } else { "\n" };
                format!(
                    ",{newline}{}",
                    jsontext::indent_before(replacement.as_bytes(), last.entry.start)
                )
            } else {
                ", ".to_owned()
            };
            replacement.insert_str(
                last.entry.value_end,
                &format!("{separator}\"args\": {value}"),
            );
        }
    } else if let Some(index) = parsed.members.iter().position(|m| m.key == "args") {
        let entries: Vec<_> = parsed.members.iter().map(|m| m.entry).collect();
        replacement = jsontext::remove(&replacement, &entries, index);
    }
    let mut updated = format!(
        "{}{}{}",
        &doc[..found.hook_value.value_start],
        replacement,
        &doc[found.hook_value.value_end..]
    );
    // Keep one owned hook even if an old settings file contains both forms.
    loop {
        let (next, removed) = remove_nth_event(&updated, event, 1);
        if !removed {
            break;
        }
        updated = next;
    }
    updated
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

/// Checks the existing structure without changing anything: which events already have one of our
/// hooks, and whether it names the current executable or an old path.
fn inspect(doc: &str, exe: &str, form: HookForm) -> Result<Vec<EventState>> {
    let bytes = doc.as_bytes();
    let Some(hooks_obj) = parse_checked(doc)? else {
        return Ok(EVENTS.iter().map(|_| EventState::Missing).collect());
    };
    let mut states = Vec::with_capacity(EVENTS.len());
    for event in EVENTS {
        let Some(m) = hooks_obj.members.iter().find(|m| m.key == event) else {
            states.push(EventState::Missing);
            continue;
        };
        let arr = jsontext::array(bytes, m.entry.value_start);
        states.push(match find_ours(bytes, &arr, event) {
            None => EventState::Missing,
            Some(found)
                if command_matches_current(bytes, &found, exe, event, form)
                    && find_all_ours(bytes, &arr, event).len() == 1 =>
            {
                EventState::Fresh
            }
            Some(_) => EventState::Stale,
        });
    }
    Ok(states)
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

#[cfg(test)]
fn plan_install(env: Env<'_>, exe: &str) -> Result<Plan> {
    plan_install_with_form(env, exe, HookForm::Shell)
}

pub(crate) fn plan_install_with_form(env: Env<'_>, exe: &str, requested: HookForm) -> Result<Plan> {
    // Claude interpolates its own placeholders before execution, independently of a shell.
    // Without a documented literal escape, refusing is safer than installing a different path.
    if exe.contains("${") {
        return Err(Error::invalid(
            "pitcrew's path contains a Claude placeholder opener (${); install it at a literal path before wiring Claude hooks",
        ));
    }
    let (form, reason) = super::hook_form::select(requested, env)?;
    let mut plan = plan_for_form(env, exe, form)?;
    plan.detail = format!("{}; {reason}", plan.detail);
    Ok(plan)
}

pub(crate) fn plan_status(env: Env<'_>, exe: &str) -> Result<Plan> {
    let mut plan = plan_for_form(env, exe, HookForm::Auto)?;
    plan.changes.clear();
    if plan.status == Status::Conflicting || plan.status == Status::Missing {
        return Ok(plan);
    }
    let (_, original, _) = read_text(&path(env)?)?;
    let mut exec = 0;
    let mut shell = 0;
    if let Some(hooks) = parse_checked(&original)? {
        for event in EVENTS {
            if let Some(member) = hooks.members.iter().find(|m| m.key == event) {
                let arr = jsontext::array(original.as_bytes(), member.entry.value_start);
                for found in find_all_ours(original.as_bytes(), &arr, event) {
                    if command_matches_current(
                        original.as_bytes(),
                        &found,
                        exe,
                        event,
                        HookForm::Exec,
                    ) {
                        exec += 1;
                    } else if command_matches_current(
                        original.as_bytes(),
                        &found,
                        exe,
                        event,
                        HookForm::Shell,
                    ) {
                        shell += 1;
                    }
                }
            }
        }
    }
    plan.detail = format!(
        "{}; current path: {exec} exec form, {shell} shell form",
        plan.detail
    );
    Ok(plan)
}

fn plan_for_form(env: Env<'_>, exe: &str, form: HookForm) -> Result<Plan> {
    let path = path(env)?;
    let (before, original, had_bom) = read_text(&path)?;

    if original.trim().is_empty() {
        return Ok(Plan {
            target: Target::Claude,
            status: Status::Missing,
            detail: format!("{} (none of the 5 events are set up)", path.display()),
            changes: vec![Change {
                path,
                before,
                after: super::with_bom(had_bom, fresh_document(exe, form)).into_bytes(),
                delete: false,
                executable: false,
            }],
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

    let states = match inspect(&original, exe, form) {
        Ok(v) => v,
        Err(e) => return Ok(conflicting(format!("{}: {}", path.display(), e.message))),
    };

    let missing: Vec<&str> = EVENTS
        .iter()
        .copied()
        .zip(&states)
        .filter(|(_, s)| matches!(s, EventState::Missing))
        .map(|(e, _)| e)
        .collect();
    let stale: Vec<&str> = EVENTS
        .iter()
        .copied()
        .zip(&states)
        .filter(|(_, s)| matches!(s, EventState::Stale))
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
        doc = insert_event(&doc, exe, event, form);
    }
    for event in stale.iter().copied() {
        doc = update_stale_event(&doc, exe, event, form);
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
            "{} stale (an old executable path, hook form, or duplicate): {}",
            stale.len(),
            stale.join(", ")
        ));
    }

    Ok(Plan {
        target: Target::Claude,
        status,
        detail: format!("{} ({})", path.display(), parts.join("; ")),
        changes: vec![Change {
            path,
            before,
            after: super::with_bom(had_bom, doc).into_bytes(),
            delete: false,
            executable: false,
        }],
    })
}

/// Removes `event`'s hook, exactly: the whole matcher group is deleted only when it is exactly
/// our shape (`matcher: ""`, our one hook and nothing else); otherwise only our one hook is
/// removed from inside the group's `"hooks"` array, leaving every other hook, and the group
/// itself, untouched. The event's whole key is then removed too if that leaves its array empty
/// (`remove_event_key_if_empty`). Returns whether anything changed.
fn remove_event(doc: &str, event: &str) -> (String, bool) {
    remove_nth_event(doc, event, 0)
}

fn remove_nth_event(doc: &str, event: &str, which: usize) -> (String, bool) {
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
    let Some(found) = find_all_ours(bytes, &arr, event).into_iter().nth(which) else {
        return (doc.to_owned(), false);
    };

    let new_doc = if found.exactly_ours {
        jsontext::remove(doc, &arr.elements, found.group_index)
    } else {
        let group_el = arr.elements[found.group_index];
        let group = jsontext::object(bytes, group_el.value_start);
        let Some(hooks_inner_member) = group.members.iter().find(|m| m.key == "hooks") else {
            // Cannot happen: `find_ours` only ever matches inside a group it found a "hooks"
            // array in. Still handled, not panicked on — this edits a person's own file.
            return (doc.to_owned(), false);
        };
        let inner = jsontext::array(bytes, hooks_inner_member.entry.value_start);
        jsontext::remove(doc, &inner.elements, found.hook_index)
    };

    (remove_event_key_if_empty(&new_doc, event), true)
}

/// Removes the whole `event` member from `"hooks"`, but only if its array is now empty,
/// re-scanning fresh. See the module docs for the one case this cannot tell apart from a
/// container we created ourselves (an array that was already empty before we ever touched the
/// file).
fn remove_event_key_if_empty(doc: &str, event: &str) -> String {
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
    if !jsontext::array(bytes, event_member.entry.value_start)
        .elements
        .is_empty()
    {
        return doc.to_owned();
    }
    let entries: Vec<Entry> = hooks_obj.members.iter().map(|m| m.entry).collect();
    let Some(idx) = hooks_obj.members.iter().position(|m| m.key == event) else {
        return doc.to_owned();
    };
    jsontext::remove(doc, &entries, idx)
}

/// Removes `"hooks"` itself from the root, but only if it is now empty, re-scanning fresh. See
/// the module docs for the one case this cannot tell apart from a container we created ourselves.
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
    let Some(idx) = root.members.iter().position(|m| m.key == "hooks") else {
        return doc.to_owned();
    };
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

    let mut doc = original.clone();
    let mut removed = 0usize;
    for event in EVENTS {
        let mut removed_event = false;
        loop {
            let (new_doc, did) = remove_event(&doc, event);
            if !did {
                break;
            }
            doc = new_doc;
            removed_event = true;
        }
        if removed_event {
            removed += 1;
        }
    }
    doc = remove_hooks_key_if_empty(&doc);

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
            after: super::with_bom(had_bom, doc).into_bytes(),
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
    fn migration_preserves_inline_layout_and_multiline_indent_and_newlines() {
        for (original, expected) in [
            (
                r#"{"type":"command", "command":"pitcrew hook claude Stop", "timeout":17}"#,
                r#"{"type":"command", "command":"/home/sam/.local/bin/pitcrew", "timeout":17, "args": ["hook", "claude", "Stop"]}"#,
            ),
            (
                "{\r\n\t\"type\": \"command\",\r\n\t\"command\": \"pitcrew hook claude Stop\",\r\n\t\"timeout\": 17\r\n}",
                "{\r\n\t\"type\": \"command\",\r\n\t\"command\": \"/home/sam/.local/bin/pitcrew\",\r\n\t\"timeout\": 17,\r\n\t\"args\": [\"hook\", \"claude\", \"Stop\"]\r\n}",
            ),
            (
                "{\n    \"type\": \"command\",\n    \"command\": \"pitcrew hook claude Stop\"\n}",
                "{\n    \"type\": \"command\",\n    \"command\": \"/home/sam/.local/bin/pitcrew\",\n    \"args\": [\"hook\", \"claude\", \"Stop\"]\n}",
            ),
        ] {
            let wrap = |hook: &str| format!(r#"{{"hooks":{{"Stop":[{{"hooks":[{hook}]}}]}}}}"#);
            assert_eq!(
                update_stale_event(&wrap(original), EXE, "Stop", HookForm::Exec),
                wrap(expected)
            );
        }
    }

    #[test]
    fn status_accepts_mixed_forms_and_detects_stale_paths_without_a_probe() {
        let tmp = tempfile::tempdir().unwrap();
        let env = env_of(&[("CLAUDE_CONFIG_DIR", tmp.path().to_str().unwrap())]);
        assert!(plan_status(&env, EXE).unwrap().status == Status::Missing);
        let original = fresh_document(EXE, HookForm::Shell);
        let mixed = update_stale_event(&original, EXE, "Stop", HookForm::Exec);
        std::fs::write(path(&env).unwrap(), &mixed).unwrap();
        let plan = plan_status(&env, EXE).unwrap();
        assert!(plan.status == Status::Installed);
        assert!(plan.detail.contains("1 exec form, 4 shell form"));
        assert!(plan.changes.is_empty());
        let stale = plan_status(&env, "/home/sam/new/pitcrew").unwrap();
        assert!(stale.status == Status::Stale);
        assert!(stale.changes.is_empty());
    }

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
    fn exec_hooks_migrate_both_ways_without_losing_user_options() {
        let tmp = tempfile::tempdir().unwrap();
        let env = env_of(&[("CLAUDE_CONFIG_DIR", tmp.path().to_str().unwrap())]);
        let path = path(&env).unwrap();
        apply(&plan_install(&env, EXE).unwrap());
        let mut saved: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        saved["hooks"]["Stop"][0]["hooks"][0]["timeout"] = serde_json::json!(17);
        saved["hooks"]["Stop"][0]["hooks"][0]["async"] = serde_json::json!(true);
        std::fs::write(&path, saved.to_string()).unwrap();
        let new_exe = "/home/sam/Program Files/pitcrew";
        let exec = plan_for_form(&env, new_exe, HookForm::Exec).unwrap();
        apply(&exec);
        let saved: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        for event in EVENTS {
            let hook = &saved["hooks"][event][0]["hooks"][0];
            assert_eq!(hook["command"], new_exe);
            assert_eq!(hook["args"], serde_json::json!(["hook", "claude", event]));
        }
        assert_eq!(saved["hooks"]["Stop"][0]["hooks"][0]["timeout"], 17);
        assert_eq!(saved["hooks"]["Stop"][0]["hooks"][0]["async"], true);
        assert!(
            plan_for_form(&env, new_exe, HookForm::Exec)
                .unwrap()
                .changes
                .is_empty()
        );
        apply(&plan_for_form(&env, new_exe, HookForm::Shell).unwrap());
        let saved: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            saved["hooks"]["Stop"][0]["hooks"][0]["command"],
            command(new_exe, "Stop")
        );
        assert!(saved["hooks"]["Stop"][0]["hooks"][0].get("args").is_none());
        apply(&plan_uninstall(&env).unwrap());
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&std::fs::read(&path).unwrap()).unwrap(),
            serde_json::json!({})
        );
    }

    #[test]
    fn mixed_owned_forms_are_consolidated_and_foreign_exec_hooks_are_preserved() {
        let tmp = tempfile::tempdir().unwrap();
        let env = env_of(&[("CLAUDE_CONFIG_DIR", tmp.path().to_str().unwrap())]);
        let path = path(&env).unwrap();
        let foreign = serde_json::json!([
            {"type":"command", "command":"/home/sam/bin/pitcrew", "args":["hook", "claude", "Stop", "extra"]},
            {"type":"command", "command":"/home/sam/bin/not-pitcrew", "args":["hook", "claude", "Stop"]},
            {"type":"command", "command":"echo hi; pitcrew hook claude Stop"},
            {"type":"command", "command":"pitcrew hook claude Stop", "args":[]}
        ]);
        let mut hooks = vec![
            serde_json::json!({"type":"command", "command":command(EXE,"Stop")}),
            serde_json::json!({"type":"command", "command":EXE, "args":["hook","claude","Stop"]}),
        ];
        hooks.extend(foreign.as_array().unwrap().iter().cloned());
        std::fs::write(
            &path,
            serde_json::json!({"hooks":{"Stop":[{"matcher":"", "hooks":hooks}]}}).to_string(),
        )
        .unwrap();
        let original = std::fs::read(&path).unwrap();
        apply(&plan_for_form(&env, EXE, HookForm::Exec).unwrap());
        let saved: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            saved["hooks"]["Stop"][0]["hooks"].as_array().unwrap().len(),
            5
        );
        assert!(
            plan_for_form(&env, EXE, HookForm::Exec)
                .unwrap()
                .changes
                .is_empty()
        );
        apply(&plan_uninstall(&env).unwrap());
        let saved: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(saved["hooks"]["Stop"][0]["hooks"], foreign);
        std::fs::write(&path, original).unwrap();
        apply(&plan_uninstall(&env).unwrap());
        let saved: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(saved["hooks"]["Stop"][0]["hooks"], foreign);
    }

    #[test]
    fn a_path_that_could_be_interpolated_is_refused_without_changes() {
        let tmp = tempfile::tempdir().unwrap();
        let env = env_of(&[("CLAUDE_CONFIG_DIR", tmp.path().to_str().unwrap())]);
        for form in [HookForm::Exec, HookForm::Shell] {
            assert!(
                plan_install_with_form(&env, "/home/sam/${CLAUDE_PLUGIN_ROOT}/pitcrew", form)
                    .is_err()
            );
        }
        assert!(!path(&env).unwrap().exists());
    }

    #[test]
    fn installs_into_an_empty_directory_then_is_a_no_op() {
        let tmp = tempfile::tempdir().unwrap();
        let env = env_of(&[("CLAUDE_CONFIG_DIR", tmp.path().to_str().unwrap())]);
        let plan = plan_install(&env, EXE).unwrap();
        assert!(plan.status == Status::Missing);
        assert_eq!(plan.changes.len(), 1);
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

        // Uninstalling restores the original file exactly: "hooks" and "PreToolUse" survive
        // (they are not empty — PreToolUse is still there), so nothing about them is touched.
        let plan = plan_uninstall(&env).unwrap();
        let restored = String::from_utf8(plan.changes[0].after.clone()).unwrap();
        assert_eq!(restored, original);
    }

    #[test]
    fn round_trip_removes_everything_it_added_from_scratch() {
        let tmp = tempfile::tempdir().unwrap();
        let env = env_of(&[("CLAUDE_CONFIG_DIR", tmp.path().to_str().unwrap())]);
        let path = path(&env).unwrap();

        let plan = plan_install(&env, EXE).unwrap();
        assert_eq!(plan.changes.len(), 1, "no sidecar file, ever");
        apply(&plan);

        let plan = plan_uninstall(&env).unwrap();
        assert_eq!(plan.changes.len(), 1);
        let restored = String::from_utf8(plan.changes[0].after.clone()).unwrap();
        // Nothing existed before install, so a full uninstall leaves nothing meaningful.
        let value: serde_json::Value = serde_json::from_str(&restored).unwrap();
        assert!(value.as_object().unwrap().is_empty(), "{restored}");
        let _ = path;
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
        // Every other event, untouched by hand, is removed entirely (we created all of them, and
        // removing our only hook left them empty).
        assert!(after["hooks"]["SessionStart"].is_null(), "{restored}");
    }

    #[test]
    fn a_users_separate_matcher_group_in_the_same_event_survives_uninstall() {
        // A fresh install, then a *second*, separate matcher group added to the same event — not
        // merged into ours (the previous test), a whole extra group alongside it. This is exactly
        // the blocker the provenance sidecar introduced: deleting the user's own group because
        // the event key was "ours to clean up" according to a sidecar that did not know about the
        // group itself.
        let tmp = tempfile::tempdir().unwrap();
        let env = env_of(&[("CLAUDE_CONFIG_DIR", tmp.path().to_str().unwrap())]);
        let path = path(&env).unwrap();

        let plan = plan_install(&env, EXE).unwrap();
        apply(&plan);
        let installed = std::fs::read_to_string(&path).unwrap();
        let mut value: serde_json::Value = serde_json::from_str(&installed).unwrap();
        value["hooks"]["Stop"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({
                "matcher": "",
                "hooks": [{"type": "command", "command": "echo user-own-group"}]
            }));
        std::fs::write(&path, serde_json::to_string_pretty(&value).unwrap()).unwrap();

        let plan = plan_uninstall(&env).unwrap();
        let restored = String::from_utf8(plan.changes[0].after.clone()).unwrap();
        let after: serde_json::Value = serde_json::from_str(&restored).unwrap();
        let stop = after["hooks"]["Stop"].as_array().unwrap();
        assert_eq!(stop.len(), 1, "{restored}");
        assert_eq!(stop[0]["hooks"][0]["command"], "echo user-own-group");
    }

    #[test]
    fn user_edits_made_after_install_are_never_lost_by_uninstall() {
        // The file is installed, then a person edits *other* parts of it by hand before
        // uninstalling — a new unrelated top-level key, and a changed value in content that
        // pre-existed install. None of that may be lost.
        let original =
            "{\n  \"approvals\": \"never\",\n  \"hooks\": {\n    \"PreToolUse\": []\n  }\n}\n";
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("settings.json");
        std::fs::write(&path, original).unwrap();
        let env = env_of(&[("CLAUDE_CONFIG_DIR", tmp.path().to_str().unwrap())]);

        let plan = plan_install(&env, EXE).unwrap();
        apply(&plan);
        let installed = std::fs::read_to_string(&path).unwrap();
        let mut value: serde_json::Value = serde_json::from_str(&installed).unwrap();
        value["approvals"] = serde_json::json!("always");
        value["a_new_key_the_user_added"] = serde_json::json!(["x", "y"]);
        let edited = serde_json::to_string_pretty(&value).unwrap();
        std::fs::write(&path, &edited).unwrap();

        let plan = plan_uninstall(&env).unwrap();
        let restored = String::from_utf8(plan.changes[0].after.clone()).unwrap();
        let after: serde_json::Value = serde_json::from_str(&restored).unwrap();
        assert_eq!(after["approvals"], "always", "{restored}");
        assert_eq!(
            after["a_new_key_the_user_added"],
            serde_json::json!(["x", "y"]),
            "{restored}"
        );
        // "PreToolUse" (pre-existing, empty) is left exactly as it was too.
        assert_eq!(
            after["hooks"]["PreToolUse"],
            serde_json::json!([]),
            "{restored}"
        );
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
    fn a_multi_word_command_ending_in_our_path_is_never_claimed_as_ours() {
        // `afplay ding.aiff; ~/bin/pitcrew hook claude Stop` must not count as ours: the program
        // part is not a single word.
        assert!(!command_is_ours(
            "afplay ding.aiff; ~/bin/pitcrew hook claude Stop",
            "Stop"
        ));
        assert!(!command_is_ours(
            "echo pwned && /bin/pitcrew hook claude Stop",
            "Stop"
        ));
        // But a genuinely single, quoted path with the same ending is still recognised.
        assert!(command_is_ours("'/bin/pitcrew' hook claude Stop", "Stop"));
        assert!(command_is_ours("/bin/pitcrew hook claude Stop", "Stop"));
    }

    #[test]
    fn a_pre_existing_empty_hooks_object_is_removed_same_as_absent() {
        // Documented, accepted trade-off of dropping the provenance sidecar: an already-empty
        // "hooks" cannot be told apart from one install's own entries emptied out, so it is
        // removed too — an empty object and an absent key mean the same thing to Claude Code.
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
        assert!(value.as_object().unwrap().is_empty(), "{restored}");
    }

    #[test]
    fn a_pre_existing_empty_event_array_is_removed_same_as_absent() {
        // Same documented trade-off, one level down: a pre-existing empty event array is removed
        // too, when every other event we created from nothing is also gone.
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
        assert!(value.as_object().unwrap().is_empty(), "{restored}");
    }

    #[test]
    fn a_pre_existing_empty_event_array_among_real_content_is_also_removed() {
        // Unaffected by whether other events around it have real content: an event array that
        // was *already empty* is indistinguishable from one we emptied, so it goes too, even
        // though "hooks" itself survives (PreToolUse keeps it non-empty).
        let original = r#"{"hooks": {"PreToolUse": [{"matcher": "Bash", "hooks": [{"type": "command", "command": "echo hi"}]}], "Stop": []}}"#;
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("settings.json");
        std::fs::write(&path, original).unwrap();
        let env = env_of(&[("CLAUDE_CONFIG_DIR", tmp.path().to_str().unwrap())]);

        let plan = plan_install(&env, EXE).unwrap();
        apply(&plan);

        let plan = plan_uninstall(&env).unwrap();
        let restored = String::from_utf8(plan.changes[0].after.clone()).unwrap();
        let value: serde_json::Value = serde_json::from_str(&restored).unwrap();
        assert!(value["hooks"].get("Stop").is_none(), "{restored}");
        assert!(value["hooks"].get("PreToolUse").is_some(), "{restored}");
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

    /// Quoted for `sh` (Claude Code's `bash`, Git Bash on Windows) only where needed, and always
    /// recognised as ours.
    #[test]
    fn the_command_is_quoted_for_sh_only_where_needed() {
        for (exe, expected) in [
            ("/a/b/pitcrew", "/a/b/pitcrew hook claude Stop"),
            ("/a/b c/pitcrew", "'/a/b c/pitcrew' hook claude Stop"),
            ("/a/it's/pitcrew", r"'/a/it'\''s/pitcrew' hook claude Stop"),
            ("/a/$x/pitcrew", "'/a/$x/pitcrew' hook claude Stop"),
        ] {
            let cmd = command(exe, "Stop");
            assert_eq!(cmd, expected);
            assert!(command_is_ours(&cmd, "Stop"), "{cmd}");
        }
    }

    /// On Windows a path's `\` become `/`: unquoted, a plain path runs in Git Bash (where `\` is an
    /// escape) and in PowerShell alike. On Unix `\` is an ordinary character of a file name.
    #[test]
    fn a_windows_path_is_written_with_forward_slashes() {
        let plain = command(r"C:\Users\sam\.local\bin\pitcrew.exe", "Stop");
        let spaced = command(r"C:\Program Files\PitCrew\pitcrew.exe", "Stop");
        let unc = command(r"\\host\share\pitcrew.exe", "Stop");
        if cfg!(windows) {
            assert_eq!(
                plain,
                "C:/Users/sam/.local/bin/pitcrew.exe hook claude Stop"
            );
            assert_eq!(
                spaced,
                "'C:/Program Files/PitCrew/pitcrew.exe' hook claude Stop"
            );
            assert_eq!(unc, "//host/share/pitcrew.exe hook claude Stop");
        } else {
            assert_eq!(
                plain,
                r"'C:\Users\sam\.local\bin\pitcrew.exe' hook claude Stop"
            );
            assert_eq!(
                spaced,
                r"'C:\Program Files\PitCrew\pitcrew.exe' hook claude Stop"
            );
            assert_eq!(unc, r"'\\host\share\pitcrew.exe' hook claude Stop");
        }
        for cmd in [&plain, &spaced, &unc] {
            assert!(command_is_ours(cmd, "Stop"), "{cmd}");
        }
        // What earlier versions wrote on Windows is still ours (and stale, so install rewrites it).
        assert!(command_is_ours(
            r#""C:\Users\sam\.local\bin\pitcrew.exe" hook claude Stop"#,
            "Stop"
        ));
    }
}
