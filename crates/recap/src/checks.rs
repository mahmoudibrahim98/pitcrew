//! Rules that read tool runs and text: which commands are checks (tests, builds, lint), and which
//! texts report a diverged job. They are plain keyword rules over untrusted text, so they only
//! ever look at a bounded prefix, never allocate and never fail.
//!
//! [`Check`] is a wire type (facts carry it), so it lives in `pitcrew_protocol::recap`.

use crate::text::{TARGET_CHARS, basename, prefix};

pub use pitcrew_protocol::recap::Check;

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
/// Subcommands that hand over to the script or tool named by the next word, per program:
/// `npm run test`, `pnpm exec vitest`, `uv run pytest`, `deno task lint`. `cargo run` is not one:
/// it runs the crate's own binary, whatever its arguments are called.
const DISPATCHERS: &[(&str, &str)] = &[
    ("npm", "run"),
    ("npm", "run-script"),
    ("npm", "exec"),
    ("pnpm", "run"),
    ("pnpm", "exec"),
    ("pnpm", "dlx"),
    ("yarn", "run"),
    ("yarn", "exec"),
    ("yarn", "dlx"),
    ("bun", "run"),
    ("bun", "x"),
    ("deno", "task"),
    ("uv", "run"),
    ("poetry", "run"),
];
/// Runners whose words are targets, any of which may be the check: `make build test`.
const TARGET_RUNNERS: &[&str] = &["make", "just"];
const TEST_WORDS: &[&str] = &["test", "tests", "nextest", "unittest"];
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

/// What a subcommand, script or target name runs, e.g. `test`, `test:watch`, `lint-fix`,
/// `nextest` or `pytest`. Only the part before the first `:` or `-` counts, and it must match a
/// known word exactly: `testing-library` is not a test.
fn classify_word(word: &str) -> Option<Check> {
    let base = word.split([':', '-']).next().unwrap_or(word);
    if is_one_of(base, TEST_WORDS) || is_one_of(base, TEST_PROGRAMS) {
        Some(Check::Tests)
    } else if is_one_of(base, LINT_WORDS) || is_one_of(base, LINT_PROGRAMS) {
        Some(Check::Lint)
    } else if is_one_of(base, BUILD_WORDS) || is_one_of(base, BUILD_PROGRAMS) {
        Some(Check::Build)
    } else {
        None
    }
}

/// Classifies one shell command, e.g. `RUST_LOG=1 cargo test -p x`. For a runner, only its
/// subcommand (the first word that is not a flag) is read, plus the next word after a dispatcher
/// (`npm run test:watch`) or a second target (`make build test`). Arguments are never read, so
/// `npm install testing-library` and `cargo install cargo-nextest` are not tests.
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
        // `+nightly` (a toolchain) and `-m` (python's module flag) are flags too.
        let mut args = words.filter(|w| !w.starts_with(['-', '+']));
        let found = args.next().and_then(|verb| {
            let dispatches = DISPATCHERS
                .iter()
                .any(|(p, v)| program.eq_ignore_ascii_case(p) && verb.eq_ignore_ascii_case(v));
            if dispatches {
                args.next().and_then(classify_word)
            } else if is_one_of(program, TARGET_RUNNERS) {
                strongest(classify_word(verb), args.next().and_then(classify_word))
            } else {
                classify_word(verb)
            }
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

/// Words next to which a lowercase `nan` is a value: "loss nan", "grad norm nan".
const METRIC_WORDS: &[&str] = &[
    "loss",
    "losses",
    "grad",
    "grads",
    "gradient",
    "gradients",
    "norm",
    "value",
    "values",
    "val",
    "weight",
    "weights",
    "output",
    "outputs",
    "logits",
    "metric",
    "score",
    "reward",
    "acc",
    "accuracy",
    "lr",
    "ppl",
    "perplexity",
];

/// The words of a text (runs of letters and digits) with their byte offsets.
fn words(text: &str) -> impl Iterator<Item = (usize, &str)> {
    let mut pos = 0;
    text.split_inclusive(|c: char| !c.is_alphanumeric())
        .filter_map(move |piece| {
            let start = pos;
            pos += piece.len();
            let word = piece.trim_end_matches(|c: char| !c.is_alphanumeric());
            (!word.is_empty()).then_some((start, word))
        })
}

fn is_number(word: &str) -> bool {
    word.bytes().all(|b| b.is_ascii_digit())
}

/// Whether a text reports a diverged run, e.g. "Seed 3 diverged at epoch 9" or "loss went to
/// NaN". `NaN` and `NAN` count as written; a lowercase `nan` only as a value: after `=` or `:`
/// ("loss=nan"), or next to a number or a metric word ("grad norm nan", "step 1200 nan"). So a
/// colleague called Nan, or a French "nan", is not a divergence.
#[must_use]
pub fn mentions_divergence(text: &str) -> bool {
    let text = prefix(text, TARGET_CHARS * 2);
    let mut prev: Option<&str> = None;
    let mut all = words(text).peekable();
    while let Some((start, w)) = all.next() {
        let next = all.peek().map(|(_, n)| *n);
        let found = if w.get(..6).is_some_and(|p| p.eq_ignore_ascii_case("diverg"))
            || w == "NaN"
            || w == "NAN"
        {
            true
        } else if w == "nan" {
            let before = text.get(..start).unwrap_or_default().trim_end();
            let assigned = before.ends_with(['=', ':']);
            let beside =
                |x: Option<&str>| x.is_some_and(|x| is_number(x) || is_one_of(x, METRIC_WORDS));
            assigned || beside(prev) || beside(next)
        } else {
            false
        };
        if found {
            return true;
        }
        prev = Some(w);
    }
    false
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
    fn reads_only_the_subcommand() {
        let c = |t| classify("Bash", t);
        // Arguments that merely contain "test" are not tests.
        assert_eq!(c("npm install testing-library"), None);
        assert_eq!(c("cargo install cargo-nextest"), None);
        assert_eq!(c("cargo run --bin test-harness"), None);
        assert_eq!(c("cargo add --dev test-case"), None);
        assert_eq!(c("uv pip install pytest"), None);
        assert_eq!(c("pip install pytest"), None);
        assert_eq!(c("npm ci"), None);
        assert_eq!(c("go get example.com/testify"), None);
        assert_eq!(c("python train.py --test-every 100"), None);
        // The subcommand, a dispatched script or tool, or a make target.
        assert_eq!(c("npm run test:watch"), Some(Check::Tests));
        assert_eq!(c("npm run test-ci"), Some(Check::Tests));
        assert_eq!(c("yarn test"), Some(Check::Tests));
        assert_eq!(c("yarn run test:unit"), Some(Check::Tests));
        assert_eq!(c("pnpm exec vitest run"), Some(Check::Tests));
        assert_eq!(c("pnpm vitest"), Some(Check::Tests));
        assert_eq!(c("npx --yes jest --ci"), Some(Check::Tests));
        assert_eq!(c("uv run pytest -x"), Some(Check::Tests));
        assert_eq!(c("deno task test"), Some(Check::Tests));
        assert_eq!(c("cargo nextest run"), Some(Check::Tests));
        assert_eq!(c("cargo +nightly test"), Some(Check::Tests));
        assert_eq!(c("python3 -m unittest discover"), Some(Check::Tests));
        assert_eq!(c("pytest"), Some(Check::Tests));
        assert_eq!(c("make -j4 test"), Some(Check::Tests));
        assert_eq!(c("make build test"), Some(Check::Tests));
        assert_eq!(c("npm run lint:fix"), Some(Check::Lint));
        assert_eq!(c("go vet ./..."), Some(Check::Lint));
        assert_eq!(c("npx eslint ."), Some(Check::Lint));
        assert_eq!(c("yarn build"), Some(Check::Build));
        assert_eq!(c("npx tsc --noEmit"), Some(Check::Build));
        assert_eq!(c("make install"), Some(Check::Build));
    }

    #[test]
    fn detects_divergence() {
        assert!(mentions_divergence(
            "Seed 3 diverged at epoch 9. Rerun or drop it?"
        ));
        assert!(mentions_divergence("Loss went to NaN at step 18,400."));
        assert!(mentions_divergence("DIVERGENCE detected"));
        assert!(mentions_divergence("NAN in layer 3"));
        assert!(mentions_divergence("loss=nan"));
        assert!(mentions_divergence("epoch 9 | loss: nan | lr 1e-4"));
        assert!(mentions_divergence("grad norm nan at step 12"));
        assert!(mentions_divergence("val_loss nan"));
        assert!(mentions_divergence("step 1200 nan"));
        assert!(!mentions_divergence("5 jobs: 4 running, 1 failed"));
        assert!(!mentions_divergence("nanoseconds and financial"));
        assert!(!mentions_divergence("\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}"));
        assert!(!mentions_divergence(""));
        assert!(!mentions_divergence("loss is finite"));
    }

    #[test]
    fn a_name_or_a_french_nan_is_not_a_divergence() {
        assert!(!mentions_divergence("Nan said she would rerun seed 3."));
        assert!(!mentions_divergence("Ask Nan: rerun or drop?"));
        assert!(!mentions_divergence("Thanks, Nan!"));
        assert!(!mentions_divergence("nan, je ne pense pas"));
        assert!(!mentions_divergence("Nan, c'est bon."));
        assert!(!mentions_divergence("mais nan, ça marche"));
        assert!(!mentions_divergence("nAn"));
        assert!(!mentions_divergence("déjà vu, nan"));
        // Offsets stay right after multi-byte characters.
        assert!(mentions_divergence("é é loss=nan"));
        assert!(mentions_divergence("déjà: loss nan"));
    }
}
