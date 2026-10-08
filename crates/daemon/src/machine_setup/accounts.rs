//! Each agent CLI's account (`GET /v1/machines/{id}/agents`), **as the CLI's own status command
//! reports it**. PitCrew never opens a CLI's token or credential files: it runs
//!
//! | CLI | Status command | What it reads of the output |
//! |---|---|---|
//! | Claude Code | `claude auth status` | `loggedIn` and `email` (JSON), or "Logged in" / "Not logged in" |
//! | Codex | `codex login status` | "Logged in using ChatGPT" / "using an API key" / "Not logged in" |
//! | OpenCode | `opencode auth list` | the "N credentials" line, and the providers listed |
//!
//! and keeps only what it recognises: signed in or not, and an account label that is an e-mail
//! address, a fixed word (`ChatGPT`, `API key`), or provider names. Nothing else of the output
//! (a masked key, a path, a token) leaves this module or reaches the log. Output it does not
//! recognise, a CLI too old to have the command, a timeout: the account is "could not tell".

use super::tools::{Ran, Tools, clean};
use pitcrew_protocol::machine_setup::AgentAccount;
use pitcrew_protocol::model::Engine;
use std::time::Duration;

/// How long one status command may take.
pub const STATUS_LIMIT: Duration = Duration::from_secs(20);

/// The longest account label kept.
const ACCOUNT_MAX: usize = 120;

/// The engines PitCrew runs, in the order they are reported.
pub const ENGINES: [Engine; 3] = [Engine::Claude, Engine::Codex, Engine::OpenCode];

/// The CLI's program name.
pub fn program(engine: Engine) -> Option<&'static str> {
    match engine {
        Engine::Claude => Some("claude"),
        Engine::Codex => Some("codex"),
        Engine::OpenCode => Some("opencode"),
        _ => None,
    }
}

/// The CLI's name for people.
pub fn label(engine: Engine) -> &'static str {
    match engine {
        Engine::Claude => "Claude Code",
        Engine::Codex => "Codex",
        Engine::OpenCode => "OpenCode",
        _ => "This CLI",
    }
}

/// The status command's arguments.
fn status_args(engine: Engine) -> Option<&'static [&'static str]> {
    match engine {
        Engine::Claude => Some(&["auth", "status"]),
        Engine::Codex => Some(&["login", "status"]),
        Engine::OpenCode => Some(&["auth", "list"]),
        _ => None,
    }
}

/// The status command, for people (`claude auth status`).
pub fn status_command(engine: Engine) -> Option<String> {
    Some(format!(
        "{} {}",
        program(engine)?,
        status_args(engine)?.join(" ")
    ))
}

/// Every engine's account, each from its own CLI, all at once.
pub async fn accounts(tools: &Tools) -> Vec<AgentAccount> {
    let mut tasks = tokio::task::JoinSet::new();
    for (index, engine) in ENGINES.into_iter().enumerate() {
        let tools = tools.clone();
        tasks.spawn(async move { (index, account(&tools, engine).await) });
    }
    let mut found = Vec::new();
    while let Some(joined) = tasks.join_next().await {
        match joined {
            Ok(entry) => found.push(entry),
            Err(e) => tracing::error!(error = %e, "an agent's status command failed"),
        }
    }
    found.sort_by_key(|(index, _)| *index);
    found.into_iter().map(|(_, account)| account).collect()
}

/// `engine`'s account.
pub async fn account(tools: &Tools, engine: Engine) -> AgentAccount {
    let unknown = |installed: bool, detail: String| AgentAccount {
        engine,
        installed,
        signed_in: None,
        account: None,
        detail: Some(detail),
    };
    let (Some(name), Some(args)) = (program(engine), status_args(engine)) else {
        return unknown(false, "PitCrew does not run this CLI.".to_owned());
    };
    let Some(path) = tools.find(name) else {
        return unknown(false, format!("{} ({name}) is not on PATH.", label(engine)));
    };
    let ran = tools.run(&path, args, STATUS_LIMIT).await;
    let command = status_command(engine).unwrap_or_default();
    let read = match engine {
        Engine::Claude => claude(&ran),
        Engine::Codex => codex(&ran),
        Engine::OpenCode => opencode(&ran),
        _ => None,
    };
    match read {
        Some(Status { signed_in, account }) => AgentAccount {
            engine,
            installed: true,
            signed_in: Some(signed_in),
            account: account.and_then(|a| plain_account(&a)),
            detail: None,
        },
        None => {
            tracing::debug!(
                engine = name,
                code = ?ran.code,
                timed_out = ran.timed_out,
                "an agent's status command said nothing PitCrew understands"
            );
            let why = if ran.timed_out || ran.failed.is_some() {
                ran.why()
            } else {
                "its answer was not one PitCrew understands (an older version may not have the \
                 command)"
                    .to_owned()
            };
            unknown(
                true,
                format!("`{command}` did not say whether it is signed in: {why}."),
            )
        }
    }
}

/// What a status command said.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Status {
    signed_in: bool,
    account: Option<String>,
}

/// `claude auth status`: JSON (`{"loggedIn": true, "email": …}`), or a sentence.
fn claude(ran: &Ran) -> Option<Status> {
    if ran.timed_out || ran.failed.is_some() {
        return None;
    }
    if let Some(json) = json_object(&ran.stdout) {
        let signed_in = ["loggedIn", "logged_in", "signedIn"]
            .iter()
            .find_map(|key| json.get(*key).and_then(serde_json::Value::as_bool))?;
        // The exit code must agree, where it says anything: 0 signed in, 1 not.
        if (signed_in && ran.code == Some(1)) || (!signed_in && ran.code == Some(0)) {
            return None;
        }
        let account = if signed_in {
            ["email", "emailAddress", "account"]
                .iter()
                .find_map(|key| json.get(*key).and_then(serde_json::Value::as_str))
                .filter(|value| value.contains('@'))
                .map(str::to_owned)
                .or_else(|| {
                    json.get("authMethod")
                        .and_then(serde_json::Value::as_str)
                        .filter(|method| method.to_ascii_lowercase().contains("api"))
                        .map(|_| "API key".to_owned())
                })
        } else {
            None
        };
        return Some(Status { signed_in, account });
    }
    sentence(ran)
}

/// `codex login status`: "Logged in using ChatGPT", "Logged in using an API key - …" (whose
/// masked key is never kept), or "Not logged in", on stderr or stdout.
fn codex(ran: &Ran) -> Option<Status> {
    if ran.timed_out || ran.failed.is_some() {
        return None;
    }
    let text = clean(&ran.output()).to_ascii_lowercase();
    if text.contains("not logged in") {
        return (ran.code != Some(0)).then_some(Status {
            signed_in: false,
            account: None,
        });
    }
    if text.contains("logged in using chatgpt") {
        return (ran.code == Some(0)).then(|| Status {
            signed_in: true,
            account: Some("ChatGPT".to_owned()),
        });
    }
    if text.contains("logged in using an api key") || text.contains("logged in using api key") {
        return (ran.code == Some(0)).then(|| Status {
            signed_in: true,
            account: Some("API key".to_owned()),
        });
    }
    None
}

/// `opencode auth list`: a list of stored credentials ending "N credentials", then those from
/// the environment. Signed in when it stores at least one.
fn opencode(ran: &Ran) -> Option<Status> {
    if ran.timed_out || ran.failed.is_some() || ran.code != Some(0) {
        return None;
    }
    let lines: Vec<String> = ran.stdout.lines().map(clean).collect();
    let (at, count) = lines
        .iter()
        .enumerate()
        .find_map(|(at, line)| credentials_count(line).map(|count| (at, count)))?;
    if count == 0 {
        return Some(Status {
            signed_in: false,
            account: None,
        });
    }
    // The providers are the bulleted lines before the count: "● Anthropic oauth".
    let providers: Vec<String> = lines[..at]
        .iter()
        .filter_map(|line| {
            let rest = line.trim_start_matches(['│', '┌', '└', '|', ' ']);
            let rest = rest.strip_prefix('●').or_else(|| rest.strip_prefix('◆'))?;
            let mut words: Vec<&str> = rest.split_whitespace().collect();
            if words
                .last()
                .is_some_and(|w| matches!(*w, "oauth" | "api" | "wellknown"))
            {
                words.pop();
            }
            let name = words.join(" ");
            (!name.is_empty()).then_some(name)
        })
        .collect();
    let account = if providers.len() == usize::try_from(count).unwrap_or(usize::MAX) {
        providers.join(", ")
    } else if count == 1 {
        "1 provider".to_owned()
    } else {
        format!("{count} providers")
    };
    Some(Status {
        signed_in: true,
        account: Some(account),
    })
}

/// "2 credentials", "1 credential", "└  0 credentials".
fn credentials_count(line: &str) -> Option<u64> {
    let words: Vec<&str> = line
        .split(|c: char| c.is_whitespace() || matches!(c, '│' | '└' | '┌'))
        .filter(|w| !w.is_empty())
        .collect();
    words.windows(2).find_map(|pair| {
        matches!(pair[1], "credential" | "credentials")
            .then(|| pair[0].parse().ok())
            .flatten()
    })
}

/// A sentence that says it: "Not logged in" (with a failing exit code), or "Logged in" (with 0).
fn sentence(ran: &Ran) -> Option<Status> {
    let text = clean(&ran.output()).to_ascii_lowercase();
    if text.contains("not logged in") || text.contains("not signed in") {
        return (ran.code != Some(0)).then_some(Status {
            signed_in: false,
            account: None,
        });
    }
    if text.contains("logged in") || text.contains("signed in") {
        return (ran.code == Some(0)).then_some(Status {
            signed_in: true,
            account: None,
        });
    }
    None
}

/// The first JSON object in `text`, if it is one.
fn json_object(text: &str) -> Option<serde_json::Map<String, serde_json::Value>> {
    match serde_json::from_str(text.trim()) {
        Ok(serde_json::Value::Object(map)) => Some(map),
        _ => None,
    }
}

/// `account` as a label: one plain line of at most [`ACCOUNT_MAX`] characters, and never
/// something that looks like a key or a token.
fn plain_account(account: &str) -> Option<String> {
    let line = clean(account);
    if line.is_empty() || line.chars().count() > ACCOUNT_MAX {
        return None;
    }
    let lower = line.to_ascii_lowercase();
    let secret = [
        "sk-",
        "sk_",
        "ghp_",
        "gho_",
        "github_pat_",
        "pcd_",
        "pca_",
        "bearer",
        "token",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
        || line
            .split(|c: char| !c.is_ascii_alphanumeric())
            .any(|word| word.len() >= 32);
    (!secret).then_some(line)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ran(code: i32, stdout: &str, stderr: &str) -> Ran {
        Ran {
            code: Some(code),
            stdout: stdout.into(),
            stderr: stderr.into(),
            ..Ran::default()
        }
    }

    fn status(signed_in: bool, account: Option<&str>) -> Option<Status> {
        Some(Status {
            signed_in,
            account: account.map(str::to_owned),
        })
    }

    #[test]
    fn claude_reports_json_or_a_sentence() {
        let json = r#"{"loggedIn":true,"authMethod":"claude.ai","email":"sam@example.com","orgName":"Lab"}"#;
        assert_eq!(
            claude(&ran(0, json, "")),
            status(true, Some("sam@example.com"))
        );
        assert_eq!(
            claude(&ran(0, r#"{"loggedIn":true,"authMethod":"api_key"}"#, "")),
            status(true, Some("API key"))
        );
        assert_eq!(
            claude(&ran(1, r#"{"loggedIn":false}"#, "")),
            status(false, None)
        );
        // Exit code and answer disagree: could not tell.
        assert_eq!(claude(&ran(1, r#"{"loggedIn":true}"#, "")), None);
        assert_eq!(claude(&ran(0, "Not logged in", "")), None);
        assert_eq!(
            claude(&ran(1, "Not logged in. Run claude auth login.", "")),
            status(false, None)
        );
        assert_eq!(
            claude(&ran(0, "Logged in as sam@example.com", "")),
            status(true, None)
        );
        // An older Claude Code, without the command.
        assert_eq!(
            claude(&ran(
                1,
                "",
                "Error: Raw mode is not supported on the current process.stdin"
            )),
            None
        );
        assert_eq!(
            claude(&Ran {
                timed_out: true,
                ..Ran::default()
            }),
            None
        );
    }

    #[test]
    fn codex_keeps_the_method_never_the_key() {
        assert_eq!(
            codex(&ran(0, "", "Logged in using ChatGPT\n")),
            status(true, Some("ChatGPT"))
        );
        let with_key = codex(&ran(
            0,
            "",
            "Logged in using an API key - sk-proj-***ABCD\n",
        ));
        assert_eq!(with_key, status(true, Some("API key")));
        assert_eq!(codex(&ran(1, "", "Not logged in\n")), status(false, None));
        assert_eq!(codex(&ran(0, "", "Not logged in\n")), None);
        assert_eq!(
            codex(&ran(2, "", "error: unrecognized subcommand 'login'\n")),
            None
        );
    }

    #[test]
    fn opencode_counts_stored_credentials() {
        let two = "\u{1b}[90m┌\u{1b}[0m  Credentials \u{1b}[90m~/.local/share/opencode/auth.json\n│\n\
                   ●  Anthropic \u{1b}[90moauth\n│\n●  OpenAI \u{1b}[90mapi\n│\n└  2 credentials\n\n\
                   ┌  Environment\n│\n●  Groq \u{1b}[90mGROQ_API_KEY\n│\n└  1 environment variable\n";
        assert_eq!(
            opencode(&ran(0, two, "")),
            status(true, Some("Anthropic, OpenAI"))
        );
        let none = "┌  Credentials ~/.local/share/opencode/auth.json\n│\n└  0 credentials\n";
        assert_eq!(opencode(&ran(0, none, "")), status(false, None));
        let unnamed = "Credentials\n3 credentials\n";
        assert_eq!(
            opencode(&ran(0, unnamed, "")),
            status(true, Some("3 providers"))
        );
        assert_eq!(opencode(&ran(0, "something else", "")), None);
        assert_eq!(opencode(&ran(1, none, "")), None);
    }

    #[test]
    fn account_labels_are_plain_and_never_secrets() {
        assert_eq!(
            plain_account(" sam@example.com ").as_deref(),
            Some("sam@example.com")
        );
        assert_eq!(plain_account("sk-proj-abc"), None);
        assert_eq!(plain_account("gho_abcdef"), None);
        assert_eq!(plain_account(&"a".repeat(40)), None);
        assert_eq!(
            plain_account(&format!("{}@example.com", "b".repeat(130))),
            None
        );
        assert_eq!(plain_account("\u{1b}[1m\u{1b}[0m"), None);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn every_engine_is_reported_by_its_own_cli() {
        let tmp = tempfile::tempdir().unwrap();
        let bin = tmp.path();
        let script = |name: &str, body: &str| {
            crate::test_scripts::write_script(
                &bin.join(name),
                &format!("#!/bin/sh\n{body}\n"),
                0o755,
            );
        };
        script(
            "claude",
            r#"[ "$1 $2" = "auth status" ] || exit 9; echo '{"loggedIn":true,"email":"sam@example.com"}'"#,
        );
        script(
            "codex",
            r#"[ "$1 $2" = "login status" ] || exit 9; echo 'Not logged in' >&2; exit 1"#,
        );
        let tools = Tools::with_path(bin.as_os_str().to_owned());
        let all = accounts(&tools).await;
        assert_eq!(
            all,
            vec![
                AgentAccount {
                    engine: Engine::Claude,
                    installed: true,
                    signed_in: Some(true),
                    account: Some("sam@example.com".into()),
                    detail: None,
                },
                AgentAccount {
                    engine: Engine::Codex,
                    installed: true,
                    signed_in: Some(false),
                    account: None,
                    detail: None,
                },
                AgentAccount {
                    engine: Engine::OpenCode,
                    installed: false,
                    signed_in: None,
                    account: None,
                    detail: Some("OpenCode (opencode) is not on PATH.".into()),
                },
            ]
        );
        script("opencode", "echo 'a new format'");
        let opencode = account(&tools, Engine::OpenCode).await;
        assert!(opencode.installed);
        assert_eq!(opencode.signed_in, None);
        assert_eq!(
            opencode.detail.as_deref(),
            Some(
                "`opencode auth list` did not say whether it is signed in: its answer was not \
                 one PitCrew understands (an older version may not have the command)."
            )
        );
    }
}
