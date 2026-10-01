//! Running `helper.sh` on a machine and reading its report.

use super::{HelperError, Target};
use crate::{Input, Limits};
use std::collections::HashMap;
use std::time::Duration;

/// The script, sent on stdin for every call.
pub(crate) const SCRIPT: &str = include_str!("helper.sh");

/// The interpreter: `/bin/sh` by its path, whatever `sh` the user's `PATH` finds first.
pub(crate) const SHELL: &str = "/bin/sh";

/// The command line, run as `/bin/sh -c BOOTSTRAP sh <bytes> <length> <tool path> …`:
/// - drops a `dd` or `echo` function imported from the environment, and the variables a shell
///   reads code or directories from (`ENV`, `BASH_ENV`, `CDPATH`);
/// - puts the tool path in front of `PATH`;
/// - reads exactly `<bytes>` of stdin (one byte per read, so nothing behind the script is
///   consumed);
/// - runs them only when they start with the script's first line, end with its last, and are
///   `<length>` long (`$(…)` drops the final newline). A shell start-up file that ate the start
///   of stdin must not make the rest run.
pub(crate) const BOOTSTRAP: &str = concat!(
    r#"unset -f dd echo 2>/dev/null; unset ENV BASH_ENV CDPATH; "#,
    r#"PATH=$3${PATH:+:$PATH}; export PATH; "#,
    r#"s=$(dd bs=1 count="$1" 2>/dev/null) || { echo 'pitcrew: dd failed' >&2; exit 97; }; "#,
    r#"case $s in '# pitcrew-helper-script-begin'*'# pitcrew-helper-script-end') ;; "#,
    r#"*) echo 'pitcrew: the script did not arrive whole' >&2; exit 97 ;; esac; "#,
    r#"[ "${#s}" = "$2" ] || { echo 'pitcrew: the script did not arrive whole' >&2; exit 97; }; "#,
    r#"shift 3; eval "$s""#,
);

/// Reports are small; this bounds a misbehaving machine.
const MAX_REPORT: usize = 1024 * 1024;

/// The longest detail kept from a report.
const MAX_DETAIL: usize = 400;

/// One run of the script.
pub(crate) struct Call<'a> {
    /// `check`, `install`, `start`, `status`, `stop`, or `slurm-submit`, `slurm-status`,
    /// `slurm-stop`.
    pub command: &'static str,
    pub args: Vec<String>,
    /// Bytes sent after the script (the helper, for `install`).
    pub payload: Option<&'a [u8]>,
    /// Called with the bytes sent so far, the script's included.
    pub progress: Option<&'a (dyn Fn(u64) + Send + Sync)>,
    /// The whole call, not counting time spent on prompts.
    pub timeout: Duration,
}

/// What the script reported. Values are cleaned of control characters.
#[derive(Debug)]
pub(crate) struct Report(HashMap<String, String>);

impl Report {
    /// A value, when present and not empty.
    pub(crate) fn get(&self, key: &str) -> Option<&str> {
        self.0
            .get(key)
            .map(String::as_str)
            .filter(|v| !v.is_empty())
    }

    pub(crate) fn number(&self, key: &str) -> Option<u64> {
        self.get(key).and_then(|v| v.parse().ok())
    }

    /// The error for `error=<code>`, from the report's other keys.
    fn error(&self, code: &str) -> HelperError {
        let detail = self.get("detail").unwrap_or("").to_owned();
        let text = |key: &str| self.get(key).unwrap_or("").to_owned();
        match code {
            "unsafe_dir" => HelperError::UnsafeDirectory(detail),
            "busy" => HelperError::Busy(detail),
            "lock_lost" => HelperError::LockLost(detail),
            "no_hash_tool" => HelperError::NoHashTool,
            "incomplete" => HelperError::Incomplete {
                expected: self.number("expected").unwrap_or(0),
                received: self.number("received").unwrap_or(0),
            },
            "hash_mismatch" => HelperError::HashMismatch {
                expected: text("expected"),
                actual: text("sha256"),
            },
            "not_runnable" => HelperError::NotRunnable {
                code: self.get("code").and_then(|c| c.parse().ok()),
                output: detail,
            },
            "version_mismatch" => HelperError::VersionMismatch {
                expected: text("expected"),
                reported: text("version_line"),
            },
            "switch_failed" => HelperError::SwitchFailed(detail),
            "not_deployed" => HelperError::NotDeployed(detail),
            "other_host" => HelperError::OtherHost(text("host")),
            "start_failed" => HelperError::StartFailed(detail),
            "stop_failed" => HelperError::StopFailed(detail),
            "slurm" | "no_slurm" => HelperError::Slurm(detail),
            "in_use" => HelperError::InUse(detail),
            "submit_failed" => HelperError::SubmitFailed(detail),
            "job_script" => HelperError::UnexpectedOutput(detail),
            other => HelperError::Remote(format!("{other}: {detail}")),
        }
    }

    /// A report from `key`, `value` pairs, for tests.
    #[cfg(test)]
    pub(crate) fn from_pairs(pairs: &[(&str, &str)]) -> Self {
        Self(
            pairs
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect(),
        )
    }
}

/// Replaces control characters (a terminal escape from the machine, say) and bounds the length,
/// since these strings end up on screens and in logs.
pub(crate) fn clean(text: &str) -> String {
    text.chars()
        .take(MAX_DETAIL)
        .map(|c| if c.is_control() { '?' } else { c })
        .collect()
}

/// Runs `call` on `target`: `sh -c BOOTSTRAP sh <length> <command> <tag> <root> <args…>`, with
/// the script and then the payload on stdin.
///
/// # Errors
/// The ssh call fails, the output is not this call's complete report, or the report carries an
/// error.
pub(crate) async fn run(target: &Target, call: Call<'_>) -> Result<Report, HelperError> {
    let tag = crate::askpass::random::<8>().map_err(crate::SshError::Setup)?;
    let tag = crate::askpass::to_hex(&tag);
    let bytes = SCRIPT.len().to_string();
    let length = SCRIPT.trim_end_matches('\n').len().to_string();
    let mut argv: Vec<&str> = vec![
        SHELL,
        "-c",
        BOOTSTRAP,
        "sh",
        &bytes,
        &length,
        target.tool_path(),
        call.command,
        &tag,
        target.layout().root(),
    ];
    argv.extend(call.args.iter().map(String::as_str));
    let mut input = Input::new(SCRIPT.as_bytes());
    if let Some(payload) = call.payload {
        input = input.then(payload);
    }
    if let Some(progress) = call.progress {
        input = input.with_progress(progress);
    }
    let limits = Limits {
        max_output: Some(MAX_REPORT),
        timeout: Some(call.timeout),
    };
    let output = target
        .ssh()
        .run_with_input(target.host(), &argv, input, limits)
        .await?;
    let stdout = output.stdout_text();
    let values = crate::report::parse(&stdout, "helper", &tag).map_err(|why| {
        HelperError::UnexpectedOutput(format!(
            "{why} (the {} script exited with {:?}: {})",
            call.command,
            output.code,
            crate::ssh::last_line(&String::from_utf8_lossy(&output.stderr))
        ))
    })?;
    if !output.success() {
        return Err(HelperError::UnexpectedOutput(format!(
            "the {} script exited with {:?}",
            call.command, output.code
        )));
    }
    let report = Report(
        values
            .into_iter()
            .map(|(k, v)| (k.to_owned(), clean(v)))
            .collect(),
    );
    if let Some(code) = report.get("error") {
        return Err(report.error(code));
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The script's first and last lines. The bootstrap runs nothing without both.
    const BEGIN: &str = "# pitcrew-helper-script-begin";
    const SENTINEL: &str = "# pitcrew-helper-script-end";

    #[test]
    fn the_script_is_whole_and_plain() {
        assert!(SCRIPT.starts_with(&format!("{BEGIN}\n")));
        assert_eq!(SCRIPT.matches(BEGIN).count(), 1);
        assert!(SCRIPT.ends_with(&format!("\n{SENTINEL}\n")));
        assert_eq!(SCRIPT.matches(SENTINEL).count(), 1);
        assert!(BOOTSTRAP.contains(&format!("'{BEGIN}'*'{SENTINEL}'")));
        // A checkout with CRLF line ends would break every line of it.
        assert!(!SCRIPT.contains('\r'));
        assert!(SCRIPT.is_ascii());
        // The command line stays small: about 3 KB once wrapped, well under the 30,000
        // characters the wrapper allows on Windows.
        let argv = [
            SHELL,
            "-c",
            BOOTSTRAP,
            "sh",
            "99999",
            "99998",
            super::super::DEFAULT_TOOL_PATH,
            "install",
            "0123456789abcdef",
            &format!("/{}", "h".repeat(200)),
        ];
        let wrapped = crate::quote::remote_command(&argv).unwrap();
        assert!(wrapped.len() < 4_000, "{}", wrapped.len());
    }

    #[test]
    fn report_errors_map_to_kinds() {
        let report = |pairs: &[(&str, &str)]| {
            Report(
                pairs
                    .iter()
                    .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                    .collect(),
            )
        };
        let r = report(&[("received", "10"), ("expected", "20")]);
        assert!(matches!(
            r.error("incomplete"),
            HelperError::Incomplete {
                expected: 20,
                received: 10
            }
        ));
        let r = report(&[("sha256", "ab"), ("expected", "cd")]);
        assert!(
            matches!(r.error("hash_mismatch"), HelperError::HashMismatch { expected, actual } if expected == "cd" && actual == "ab")
        );
        let r = report(&[("code", "126"), ("detail", "Exec format error")]);
        assert!(matches!(
            r.error("not_runnable"),
            HelperError::NotRunnable {
                code: Some(126),
                ..
            }
        ));
        let r = report(&[("host", "login02")]);
        assert!(matches!(r.error("other_host"), HelperError::OtherHost(h) if h == "login02"));
        let r = report(&[("detail", "x")]);
        assert!(matches!(r.error("busy"), HelperError::Busy(d) if d == "x"));
        assert!(matches!(r.error("slurm"), HelperError::Slurm(d) if d == "x"));
        assert!(matches!(r.error("no_slurm"), HelperError::Slurm(d) if d == "x"));
        assert!(matches!(r.error("in_use"), HelperError::InUse(d) if d == "x"));
        assert!(matches!(r.error("submit_failed"), HelperError::SubmitFailed(d) if d == "x"));
        assert!(matches!(r.error("job_script"), HelperError::UnexpectedOutput(d) if d == "x"));
        assert!(matches!(r.error("io"), HelperError::Remote(d) if d == "io: x"));
        assert!(matches!(r.error("no_hash_tool"), HelperError::NoHashTool));
    }

    #[test]
    fn details_are_cleaned() {
        assert_eq!(clean("a\x1b[31mb\x07"), "a?[31mb?");
        assert_eq!(clean(&"x".repeat(1000)).len(), MAX_DETAIL);
    }
}
