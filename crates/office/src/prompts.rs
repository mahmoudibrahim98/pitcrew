//! Prompts as files: the versioned templates under `crates/office/prompts/<name>/v<n>.md`, built
//! into the binary. A prompt is never edited in place: a change is a new version, and what was
//! sent names the version it came from (`draft-board/v1`).

/// A versioned prompt template. `{{name}}` marks a value [`Prompt::render`] puts in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Prompt {
    /// Its name, the folder under `prompts/`.
    pub name: &'static str,
    /// Its version, the file `v<version>.md`.
    pub version: u32,
    /// The template.
    pub template: &'static str,
}

/// Drafting a workstream's board from its history (`prompts/draft-board/v1.md`).
pub const DRAFT_BOARD: Prompt = Prompt {
    name: "draft-board",
    version: 1,
    template: include_str!("../prompts/draft-board/v1.md"),
};

impl Prompt {
    /// `name/v<version>`, as a draft records it.
    #[must_use]
    pub fn id(&self) -> String {
        format!("{}/v{}", self.name, self.version)
    }

    /// The template with each `{{name}}` replaced by its value in `values`, in one pass: a value
    /// is never searched for placeholders itself, so text in a value cannot reach another one. A
    /// placeholder with no value is left as it is.
    #[must_use]
    pub fn render(&self, values: &[(&str, &str)]) -> String {
        let mut out = String::with_capacity(
            self.template.len() + values.iter().map(|(_, v)| v.len()).sum::<usize>(),
        );
        let mut rest = self.template;
        while let Some(open) = rest.find("{{") {
            out.push_str(&rest[..open]);
            let after = &rest[open + 2..];
            let Some(close) = after.find("}}") else {
                out.push_str(&rest[open..]);
                return out;
            };
            let name = &after[..close];
            match values.iter().find(|(n, _)| *n == name) {
                Some((_, value)) => out.push_str(value),
                None => out.push_str(&rest[open..open + 2 + close + 2]),
            }
            rest = &after[close + 2..];
        }
        out.push_str(rest);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_draft_prompt_names_its_version_and_placeholders() {
        assert_eq!(DRAFT_BOARD.id(), "draft-board/v1");
        for name in ["workstream", "project", "draft", "summary", "max_tasks", "max_title"] {
            assert!(
                DRAFT_BOARD.template.contains(&format!("{{{{{name}}}}}")),
                "{name}"
            );
        }
    }

    #[test]
    fn values_are_put_in_once_and_never_searched() {
        let prompt = Prompt {
            name: "t",
            version: 1,
            template: "a {{x}} b {{y}} c {{z}}",
        };
        assert_eq!(
            prompt.render(&[("x", "{{y}}"), ("y", "Y")]),
            "a {{y}} b Y c {{z}}"
        );
        let open = Prompt {
            name: "t",
            version: 1,
            template: "a {{x",
        };
        assert_eq!(open.render(&[("x", "X")]), "a {{x");
    }
}
