//! Reading a plan for `pitcrew task plan`, and turning it into the agent's own subtasks.
//!
//! Text: one step per line. List markers (`-`, `*`, `+`, `1.`, `1)`) and checkboxes (`[ ]`,
//! `[x]`; `[-]` and `[~]` mean started, not done) are understood; blank lines are skipped.
//!
//! JSON: an array of strings, or of `{"text": …, "done": …}`.

use crate::error::{Error, Result};
use pitcrew_protocol::ids::{MemberId, SubtaskId};
use pitcrew_protocol::model::{Subtask, SubtaskSource};
use serde::Deserialize;

/// One step.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Step {
    /// What to do.
    pub text: String,
    /// Whether it is done.
    pub done: bool,
}

/// Parses a plan.
///
/// # Errors
/// `invalid` for malformed JSON or an empty step.
pub fn parse(input: &str) -> Result<Vec<Step>> {
    if input.trim_start().starts_with('[') {
        parse_json(input)
    } else {
        Ok(input.lines().filter_map(parse_line).collect())
    }
}

fn parse_json(input: &str) -> Result<Vec<Step>> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Item {
        Text(String),
        Step {
            text: String,
            #[serde(default)]
            done: bool,
        },
    }
    let items: Vec<Item> = serde_json::from_str(input).map_err(|e| {
        Error::invalid(format!(
            "the plan is not a JSON array of steps (strings or {{\"text\", \"done\"}}): {e}"
        ))
    })?;
    items
        .into_iter()
        .enumerate()
        .map(|(i, item)| {
            let (text, done) = match item {
                Item::Text(text) => (text, false),
                Item::Step { text, done } => (text, done),
            };
            let text = text.trim().to_owned();
            if text.is_empty() {
                Err(Error::invalid(format!("step {} is empty", i + 1)))
            } else {
                Ok(Step { text, done })
            }
        })
        .collect()
}

/// `rest` without a leading `marker` that is followed by whitespace or nothing.
fn strip_marker<'a>(rest: &'a str, marker: &str) -> Option<&'a str> {
    rest.strip_prefix(marker)
        .filter(|r| r.is_empty() || r.starts_with(char::is_whitespace))
        .map(str::trim_start)
}

fn parse_line(line: &str) -> Option<Step> {
    let mut rest = line.trim();
    if let Some(r) = ["-", "*", "+"].iter().find_map(|m| strip_marker(rest, m)) {
        rest = r;
    }
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    if digits > 0 {
        let after = &rest[digits..];
        if let Some(r) = strip_marker(after, ".").or_else(|| strip_marker(after, ")")) {
            rest = r;
        }
    }
    let mut done = false;
    for (box_, is_done) in [
        ("[ ]", false),
        ("[x]", true),
        ("[X]", true),
        ("[-]", false),
        ("[~]", false),
    ] {
        if let Some(r) = rest.strip_prefix(box_) {
            rest = r.trim_start();
            done = is_done;
            break;
        }
    }
    let text = rest.trim();
    (!text.is_empty()).then(|| Step {
        text: text.to_owned(),
        done,
    })
}

/// The agent's new plan lines. A step with the same text as one of the agent's current lines
/// keeps that line's id, so a plan can be re-sent as it progresses without churning ids.
#[must_use]
pub fn to_subtasks(steps: Vec<Step>, agent: MemberId, current: &[Subtask]) -> Vec<Subtask> {
    let mut reusable: Vec<&Subtask> = current
        .iter()
        .filter(|s| matches!(s.source, SubtaskSource::AgentPlan { agent: a } if a == agent))
        .collect();
    steps
        .into_iter()
        .map(|step| {
            let id = reusable
                .iter()
                .position(|s| s.text == step.text)
                .map_or_else(SubtaskId::new, |i| reusable.remove(i).id);
            Subtask {
                id,
                text: step.text,
                done: step.done,
                source: SubtaskSource::AgentPlan { agent },
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(text: &str, done: bool) -> Step {
        Step {
            text: text.into(),
            done,
        }
    }

    #[test]
    fn text_plans() {
        let plan = "\n- [x] Read the brief\n* [ ] Write the draft\n  3. [~] Run the tests\n+ Ship it\n\n- \nPlain line\n";
        assert_eq!(
            parse(plan).unwrap(),
            vec![
                step("Read the brief", true),
                step("Write the draft", false),
                step("Run the tests", false),
                step("Ship it", false),
                step("Plain line", false),
            ]
        );
        assert!(parse("").unwrap().is_empty());
    }

    #[test]
    fn json_plans() {
        let plan = r#"["One", {"text": "Two", "done": true}, {"text": " Three "}]"#;
        assert_eq!(
            parse(plan).unwrap(),
            vec![step("One", false), step("Two", true), step("Three", false)]
        );
        assert!(parse("[1, 2]").is_err());
        assert!(parse(r#"["ok", ""]"#).is_err());
    }

    #[test]
    fn ids_are_kept_for_unchanged_steps_of_our_own_plan() {
        let me = MemberId::new();
        let other = MemberId::new();
        let line = |text: &str, source| Subtask {
            id: SubtaskId::new(),
            text: text.into(),
            done: false,
            source,
        };
        let mine = line("Write", SubtaskSource::AgentPlan { agent: me });
        let theirs = line("Read", SubtaskSource::AgentPlan { agent: other });
        let human = line("Read", SubtaskSource::Human);
        let current = vec![mine.clone(), theirs, human];

        let out = to_subtasks(
            vec![
                step("Read", false),
                step("Write", true),
                step("Write", false),
            ],
            me,
            &current,
        );
        assert_eq!(out.len(), 3);
        assert!(
            current.iter().all(|s| s.id != out[0].id),
            "not another's line"
        );
        assert_eq!(out[1].id, mine.id);
        assert!(out[1].done);
        assert_ne!(out[2].id, mine.id, "each id is used once");
        assert!(
            out.iter()
                .all(|s| s.source == SubtaskSource::AgentPlan { agent: me })
        );
    }
}
