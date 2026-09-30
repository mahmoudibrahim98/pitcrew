//! Rules that read tool runs and text: which commands are checks (tests, builds, lint), and which
//! texts report a diverged job. They are plain keyword rules over untrusted text, so they only
//! ever look at a bounded prefix, never allocate and never fail.

use crate::text::{TARGET_CHARS, basename, prefix};
use serde::{Deserialize, Serialize};

/// A kind of check a command runs. The order is the strength used for command chains.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Check {
    /// A test suite, e.g. `cargo test` or `pytest`.
    Tests,
    /// A linter or type checker, e.g. `cargo clippy` or `ruff`.
    Lint,
    /// A build or compile, e.g. `cargo build` or `latexmk`.
    Build,
}

/// Programs that are test runners on their own.
const TEST_PROGRAMS: &[&str] = &[
    "pytest", "jest", "vitest", "mocha", "rspec", "ctest", "tox", "nox", "phpunit",
];
/// Programs that are linters or type checkers on their own.
const LINT_PROGRAMS: &[&str] = &[
    "eslint",
    "ruff",
    "flake8",
    "pylint",
    "mypy",
    "shellcheck",
    "chktex",
];
/// Programs that build on their own.
const BUILD_PROGRAMS: &[&str] = &[
    "tsc", "latexmk", "pdflatex", "xelatex", "lualatex", "cmake", "make", "ninja", "gcc", "g++",
    "clang", "rustc", "javac",
];
/// Programs whose subcommand says what they do (`cargo test`, `npm run lint`, `make build`).
const RUNNERS: &[&str] = &[
    "cargo", "npm", "pnpm", "yarn", "bun", "deno", "go", "dotnet", "mix", "gradle", "gradlew",
    "mvn", "make", "just", "python", "python3", "uv", "poetry", "npx",
];
const LINT_WORDS: &[&str] = &["clippy", "lint", "eslint", "ruff", "mypy", "vet"];
const BUILD_WORDS: &[&str] = &["build", "check", "compile"];
/// Words in a tool's name that mark it as running shell commands.
const SHELL_TOOLS: &[&str] = &[
    "bash",
    "shell",
    "exec",
    "command",
    "terminal",
    "powershell",
    "cmd",
];

/// Whether `hay` contains `needle`, ignoring ASCII case.
fn contains_ci(hay: &str, needle: &str) -> bool {
    let (h, n) = (hay.as_bytes(), needle.as_bytes());
    n.is_empty() || h.windows(n.len()).any(|w| w.eq_ignore_ascii_case(n))
}

fn is_one_of(word: &str, list: &[&str]) -> bool {
    list.iter().any(|w| word.eq_ignore_ascii_case(w))
}

fn strip_exe(program: &str) -> &str {
    let cut = program.len().saturating_sub(4);
    match program.get(cut..) {
        Some(ext) if ext.eq_ignore_ascii_case(".exe") => program.get(..cut).unwrap_or(program),
        _ => program,
    }
}

/// The stronger of two findings.
fn strongest(a: Option<Check>, b: Option<Check>) -> Option<Check> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

/// Classifies one shell command, e.g. `RUST_LOG=1 cargo test -p x`.
fn classify_command(cmd: &str) -> Option<Check> {
    let mut words = cmd
        .split_whitespace()
        .skip_while(|w| w.contains('=') && !w.starts_with('-'));
    let program = strip_exe(basename(words.next()?));
    if is_one_of(program, TEST_PROGRAMS) {
        return Some(Check::Tests);
    }
    if is_one_of(program, LINT_PROGRAMS) {
        return Some(Check::Lint);
    }
    if is_one_of(program, RUNNERS) {
        let found = words.take(4).fold(None, |found, w| {
            let this = if contains_ci(w, "test") {
                Some(Check::Tests)
            } else if is_one_of(w, LINT_WORDS) {
                Some(Check::Lint)
            } else if is_one_of(w, BUILD_WORDS) {
                Some(Check::Build)
            } else {
                None
            };
            strongest(found, this)
        });
        if found.is_some() {
            return found;
        }
    }
    is_one_of(program, BUILD_PROGRAMS).then_some(Check::Build)
}

/// Which check a tool run is, if any. Only shell-like tools count, and a chain such as
/// `cd x && cargo build && cargo test` counts as its strongest part (tests, then lint, then
/// build).
#[must_use]
pub fn classify(tool: &str, target: &str) -> Option<Check> {
    let tool = prefix(tool, 64);
    if !SHELL_TOOLS.iter().any(|k| contains_ci(tool, k)) {
        return None;
    }
    prefix(target, TARGET_CHARS)
        .split(['&', ';', '|', '\n'])
        .filter_map(classify_command)
        .min()
}

/// Whether a text reports a diverged run, e.g. "Seed 3 diverged at epoch 9" or "loss went to
/// NaN".
#[must_use]
pub fn mentions_divergence(text: &str) -> bool {
    prefix(text, TARGET_CHARS * 2)
        .split(|c: char| !c.is_alphanumeric())
        .any(|w| {
            w.eq_ignore_ascii_case("nan")
                || w.get(..6).is_some_and(|p| p.eq_ignore_ascii_case("diverg"))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_common_commands() {
        let c = |t| classify("Bash", t);
        assert_eq!(c("cargo test -p pitcrew-recap"), Some(Check::Tests));
        assert_eq!(c("RUST_LOG=debug cargo test"), Some(Check::Tests));
        assert_eq!(c("python -m pytest tests/"), Some(Check::Tests));
        assert_eq!(c("/usr/bin/pytest -x"), Some(Check::Tests));
        assert_eq!(c("C:\\tools\\PYTEST.EXE"), Some(Check::Tests));
        assert_eq!(c("npm test"), Some(Check::Tests));
        assert_eq!(c("go test ./..."), Some(Check::Tests));
        assert_eq!(c("cargo clippy --all-targets"), Some(Check::Lint));
        assert_eq!(c("npm run lint"), Some(Check::Lint));
        assert_eq!(c("ruff check ."), Some(Check::Lint));
        assert_eq!(c("cargo build --release"), Some(Check::Build));
        assert_eq!(c("latexmk -pdf main.tex"), Some(Check::Build));
        assert_eq!(c("make"), Some(Check::Build));
        assert_eq!(c("make test"), Some(Check::Tests));
        assert_eq!(c("cd paper && latexmk && cargo test"), Some(Check::Tests));
        assert_eq!(c("cargo build && cargo clippy"), Some(Check::Lint));
        assert_eq!(c("grep -r test src"), None);
        assert_eq!(c("git commit -m 'fix test'"), None);
        assert_eq!(c("git checkout main"), None);
        assert_eq!(c("squeue --me"), None);
        assert_eq!(c(""), None);
        assert_eq!(c(".exe"), None);
        assert_eq!(classify("Read", "tests/foo.rs"), None);
        assert_eq!(classify("exec_command", "pytest"), Some(Check::Tests));
        assert_eq!(classify("", "pytest"), None);
    }

    #[test]
    fn detects_divergence() {
        assert!(mentions_divergence(
            "Seed 3 diverged at epoch 9. Rerun or drop it?"
        ));
        assert!(mentions_divergence("Loss went to NaN at step 18,400."));
        assert!(mentions_divergence("DIVERGENCE detected"));
        assert!(!mentions_divergence("5 jobs: 4 running, 1 failed"));
        assert!(!mentions_divergence("nanoseconds and financial"));
        assert!(!mentions_divergence("\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}"));
        assert!(!mentions_divergence(""));
    }
}
