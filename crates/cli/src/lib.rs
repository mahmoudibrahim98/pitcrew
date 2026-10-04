//! # pitcrew-cli
//!
//! `pitcrew`, the command agents run to see and report on their work, and `pitcrew hook`, which
//! runs on every agent turn.
//!
//! - [`config`]: where the daemon is and which token to send (`PITCREW_*`).
//! - [`transport`]: blocking connections, checked before any token is sent.
//! - [`http`]: just enough HTTP/1.1.
//! - [`client`]: API calls; [`hook`]: the hook; [`plan`]: plans for `task plan`.
//! - [`error`]: errors and exit codes.
//!
//! There is no async runtime: a verb is a few blocking requests, and the hook one.
//!
//! **Owned by stream I.** The work packages are in `docs/build/streams/I.md`.

pub mod client;
pub mod config;
pub mod display;
pub mod error;
pub mod hook;
pub mod http;
mod install;
pub mod plan;
pub mod transport;
mod verbs;

use clap::{ArgGroup, Args, Parser, Subcommand};
use config::Env;
use error::Result;
use std::ffi::OsString;
use std::io::{Read, Write};
use transport::Timeouts;

/// Largest text read from stdin (plans, `-` texts).
pub const MAX_STDIN: usize = 1 << 20;

/// The arguments after `hook` when this run is the hook: `hook` is the first argument after the
/// program name and any global `--json` flags. `main` then takes the fast, silent path.
#[must_use]
pub fn hook_args(args: &[OsString]) -> Option<&[OsString]> {
    let first = args
        .iter()
        .skip(1)
        .position(|a| a != "--json")
        .map(|i| i + 1)?;
    (args[first] == "hook").then(|| &args[first + 1..])
}

/// Whether `args` (after the program name and any global `--json` flags) start with `hooks`:
/// `main` skips the 60-second daemon watchdog for it, since `pitcrew hooks …` never talks to the
/// daemon (it only edits files on disk) and `install`/`uninstall` may be waiting on a person
/// answering a confirmation prompt instead.
#[must_use]
pub fn is_hooks_command(args: &[OsString]) -> bool {
    let first = args
        .iter()
        .skip(1)
        .position(|a| a != "--json")
        .map(|i| i + 1);
    first.is_some_and(|i| args[i] == "hooks")
}

/// Checks a task argument before anything is sent (see `verbs::task_ref`).
fn task_arg(value: &str) -> std::result::Result<String, String> {
    verbs::task_ref(value).map_err(|e| e.message)
}

/// Where a run reads and writes. `main` passes the process's own streams; tests pass buffers.
pub struct Io<'a> {
    /// Standard input.
    pub stdin: &'a mut dyn Read,
    /// Whether stdin is a terminal (then nothing is piped in).
    pub stdin_is_terminal: bool,
    /// Standard output.
    pub stdout: &'a mut dyn Write,
    /// Standard error.
    pub stderr: &'a mut dyn Write,
}

impl std::fmt::Debug for Io<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Io")
            .field("stdin_is_terminal", &self.stdin_is_terminal)
            .finish_non_exhaustive()
    }
}

const AFTER_HELP: &str = "\
Environment:
  PITCREW_SOCKET      the daemon's unix socket (or its directory)
  PITCREW_PIPE        the daemon's named pipe on Windows (default: \\\\.\\pipe\\pitcrewd-<your SID>)
  PITCREW_URL         loopback TCP for development only, e.g. http://127.0.0.1:47317
  PITCREW_TOKEN       the agent token, or
  PITCREW_TOKEN_FILE  a private file holding it (mode 0600 on Unix)

Exit codes: 0 ok, 1 other error, 2 invalid, 3 forbidden or unauthorized, 4 conflict,
5 daemon unavailable, 6 not found.";

/// `pitcrew`: see and report on your work in PitCrew.
#[derive(Debug, Parser)]
#[command(name = "pitcrew", version, after_help = AFTER_HELP)]
struct Cli {
    /// Print JSON instead of text.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Who this token belongs to.
    Whoami,
    /// List, show, move and plan tasks.
    #[command(subcommand)]
    Task(TaskCommand),
    /// Start work on a task: move it to in_progress.
    Claim {
        /// Task key (PAP-4) or id.
        #[arg(value_parser = task_arg)]
        task: String,
    },
    /// Report progress: comment on a task, and move it to review when asked.
    #[command(group(ArgGroup::new("what").required(true).multiple(true).args(["note", "review"])))]
    Report {
        /// Task key or id.
        #[arg(value_parser = task_arg)]
        task: String,
        /// A note to post as a comment (`-` reads it from stdin).
        #[arg(long)]
        note: Option<String>,
        /// Move the task to review.
        #[arg(long)]
        review: bool,
    },
    /// Comment on a task.
    Comment {
        /// Task key or id.
        #[arg(value_parser = task_arg)]
        task: String,
        /// The comment (`-` reads it from stdin).
        #[arg(required = true)]
        text: Vec<String>,
        /// Mention a member, e.g. --mention @sam (repeatable).
        #[arg(long = "mention", value_name = "@MEMBER")]
        mentions: Vec<String>,
    },
    /// Ask a member something; it shows in their Inbox.
    Ask {
        /// Who should answer, e.g. @sam.
        #[arg(value_name = "@MEMBER")]
        to: String,
        /// The question, one line (`-` reads it from stdin).
        #[arg(required = true)]
        title: Vec<String>,
        /// An answer to offer (repeatable, in order).
        #[arg(long = "option", value_name = "TEXT")]
        options: Vec<String>,
        /// More context.
        #[arg(long)]
        body: Option<String>,
        /// The task it is about.
        #[arg(long, value_parser = task_arg)]
        task: Option<String>,
        /// question, decision, review, approval or mention.
        #[arg(long, default_value = "question")]
        kind: String,
    },
    /// Answer an ask addressed to you.
    #[command(group(ArgGroup::new("answer").required(true).multiple(true).args(["text", "option"])))]
    Reply {
        /// The ask's id (ask_…), as `check` shows it.
        ask: String,
        /// The answer (`-` reads it from stdin).
        text: Vec<String>,
        /// Choose an offered option, numbered from 1 as `check` shows them.
        #[arg(long)]
        option: Option<usize>,
    },
    /// What needs you: open asks for you, recent mentions, and your own asks.
    Check,
    /// Board drafts: answer the one you were started for.
    #[command(subcommand)]
    Board(BoardCommand),
    /// Send an agent CLI's hook event to the daemon. Always silent; always exits 0.
    Hook {
        /// claude, codex or opencode.
        engine: String,
        /// The CLI's event name, e.g. SessionStart or Stop.
        event: String,
        /// The event's JSON, when the CLI passes it as an argument instead of on stdin.
        payload: Option<String>,
    },
    /// Install, inspect or remove the `pitcrew hook` wiring in each agent CLI's own config.
    /// Never talks to the daemon.
    #[command(subcommand)]
    Hooks(HooksAction),
}

/// Restricts a `hooks` command to one agent CLI; the default is all three.
#[derive(Debug, Args)]
struct EngineFilter {
    /// Only this agent CLI: claude, codex or opencode. Default: all three.
    #[arg(long, value_parser = engine_arg)]
    engine: Option<String>,
}

fn engine_arg(s: &str) -> std::result::Result<String, String> {
    install::Target::parse(s)
        .map(|_| s.to_ascii_lowercase())
        .map_err(|e| e.message)
}

#[derive(Debug, Subcommand)]
enum HooksAction {
    /// Show whether each agent CLI's hook is installed, missing, partial or conflicting.
    Status {
        #[command(flatten)]
        engine: EngineFilter,
    },
    /// Show exactly what `install` would change, without changing anything.
    Diff {
        #[command(flatten)]
        engine: EngineFilter,
        /// Preview chaining a foreign Codex `notify` instead of reporting a conflict.
        #[arg(long)]
        chain: bool,
        /// Claude Code hook form; auto requires Claude Code 2.1.139 or newer for exec.
        #[arg(long, value_enum, default_value_t = install::HookForm::Auto)]
        hook_form: install::HookForm,
    },
    /// Wire `pitcrew hook` into each agent CLI, after showing the diff and asking to confirm.
    Install {
        #[command(flatten)]
        engine: EngineFilter,
        /// Don't ask for confirmation.
        #[arg(long)]
        yes: bool,
        /// If Codex already has a `notify`, run it and ours both, instead of reporting a
        /// conflict.
        #[arg(long)]
        chain: bool,
        /// Claude Code hook form; auto falls back to shell if version detection fails.
        #[arg(long, value_enum, default_value_t = install::HookForm::Auto)]
        hook_form: install::HookForm,
    },
    /// Remove exactly what `install` wrote, after showing the diff and asking to confirm.
    Uninstall {
        #[command(flatten)]
        engine: EngineFilter,
        /// Don't ask for confirmation.
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Debug, Subcommand)]
enum BoardCommand {
    /// Propose the board a draft asks for: JSON on stdin, `{"tasks": [{"title", "status",
    /// "description", "evidence"}], "note"}`. Nothing is created until a person reviews it.
    Submit {
        /// The draft's id (drf_…), as the prompt names it.
        #[arg(value_parser = draft_arg)]
        draft: String,
    },
}

/// Checks a draft argument before anything is sent (see `verbs::draft_ref`).
fn draft_arg(value: &str) -> std::result::Result<String, String> {
    verbs::draft_ref(value).map_err(|e| e.message)
}

#[derive(Debug, Subcommand)]
enum TaskCommand {
    /// List tasks.
    List {
        /// Only tasks assigned to you.
        #[arg(long)]
        mine: bool,
        /// Only these statuses (repeatable or comma-separated).
        #[arg(long, value_delimiter = ',')]
        status: Vec<String>,
    },
    /// Show a task: its brief, status and subtasks.
    Show {
        /// Task key or id.
        #[arg(value_parser = task_arg)]
        task: String,
    },
    /// Move a task to another status.
    Move {
        /// Task key or id.
        #[arg(value_parser = task_arg)]
        task: String,
        /// backlog, todo, in_progress, review, done or canceled.
        status: String,
    },
    /// Replace your own plan on a task with the one on stdin (one step per line; `[x]` done).
    Plan {
        /// Task key or id.
        #[arg(value_parser = task_arg)]
        task: String,
    },
}

/// The variable Claude Code sets for the commands its hooks run.
const CLAUDE_HOOK_VAR: &str = "CLAUDE_PROJECT_DIR";

/// Whether a parse of `args` that failed with `kind` is a bare `pitcrew` run as a Claude Code
/// hook: nothing after the program name but global `--json` flags, no subcommand, stdin not a
/// terminal, and Claude Code's hook environment present. A Claude Code older than 2.1.139 ignores
/// a hook's `args` and runs its bare `command`; exit 2 with the usage would then block the prompt
/// or the stop (2 is Claude Code's blocking code), so such a run exits 0, silently, as the hook
/// does. A person at a terminal still gets the usage, and so does a command group without its
/// subcommand (`pitcrew task`), which no hook runs.
fn bare_hook_run(
    args: &[OsString],
    kind: clap::error::ErrorKind,
    env: Env<'_>,
    stdin_is_terminal: bool,
) -> bool {
    use clap::error::ErrorKind;
    args.iter().skip(1).all(|a| a == "--json")
        && matches!(
            kind,
            ErrorKind::MissingSubcommand | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
        )
        && !stdin_is_terminal
        && env(CLAUDE_HOOK_VAR).is_some()
}

/// Runs `pitcrew` with `args` (including the program name) and returns the exit code.
pub fn run(args: Vec<OsString>, env: Env<'_>, io: &mut Io<'_>) -> i32 {
    let cli = match Cli::try_parse_from(&args) {
        Ok(cli) => cli,
        Err(e) => {
            if bare_hook_run(&args, e.kind(), env, io.stdin_is_terminal) {
                return 0;
            }
            let text = e.render().to_string();
            let out: &mut dyn Write = if e.use_stderr() {
                &mut *io.stderr
            } else {
                &mut *io.stdout
            };
            let _ = out.write_all(text.as_bytes());
            return e.exit_code();
        }
    };
    if let Command::Hook {
        engine,
        event,
        payload,
    } = cli.command
    {
        let args: Vec<OsString> = [Some(engine), Some(event), payload]
            .into_iter()
            .flatten()
            .map(OsString::from)
            .collect();
        let _ = hook::run(&args, env, io.stdin, io.stdin_is_terminal);
        return 0;
    }
    if let Command::Hooks(action) = cli.command {
        return match install::dispatch(action, env, io, cli.json) {
            Ok(()) => 0,
            Err(e) => {
                let text = if cli.json {
                    format!("{}\n", e.to_json())
                } else {
                    format!("pitcrew: {}\n", display::line(&e.message))
                };
                let _ = io.stderr.write_all(text.as_bytes());
                e.exit_code()
            }
        };
    }
    match execute(cli.command, env, io, cli.json) {
        Ok(()) => 0,
        Err(e) => {
            // Messages can quote the daemon, so text mode makes them safe to print.
            let text = if cli.json {
                format!("{}\n", e.to_json())
            } else {
                format!("pitcrew: {}\n", display::line(&e.message))
            };
            let _ = io.stderr.write_all(text.as_bytes());
            e.exit_code()
        }
    }
}

fn execute(command: Command, env: Env<'_>, io: &mut Io<'_>, json: bool) -> Result<()> {
    let client = client::Client::from_env(env, Timeouts::VERB)?;
    client.check_version()?;
    let mut verb = verbs::Verb::connect(client, io, json)?;
    match command {
        Command::Whoami => verb.whoami(),
        Command::Task(TaskCommand::List { mine, status }) => verb.task_list(mine, &status),
        Command::Task(TaskCommand::Show { task }) => verb.task_show(&task),
        Command::Task(TaskCommand::Move { task, status }) => verb.task_move(&task, &status),
        Command::Task(TaskCommand::Plan { task }) => verb.task_plan(&task),
        Command::Claim { task } => verb.claim(&task),
        Command::Report { task, note, review } => verb.report(&task, note.as_deref(), review),
        Command::Comment {
            task,
            text,
            mentions,
        } => verb.comment(&task, &text, &mentions),
        Command::Ask {
            to,
            title,
            options,
            body,
            task,
            kind,
        } => verb.ask(&verbs::AskArgs {
            to,
            title,
            options,
            body,
            task,
            kind,
        }),
        Command::Reply { ask, text, option } => verb.reply(&ask, &text, option),
        Command::Check => verb.check(),
        Command::Board(BoardCommand::Submit { draft }) => verb.board_submit(&draft),
        // Handled in `run`, before the client ever connects.
        Command::Hook { .. } | Command::Hooks(_) => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<OsString> {
        list.iter().map(OsString::from).collect()
    }

    /// Runs `pitcrew` with `list`, only `vars` set, and stdin a terminal or not: the exit code,
    /// stdout and stderr.
    fn run_with(list: &[&str], vars: &[(&str, &str)], terminal: bool) -> (i32, String, String) {
        let env = |name: &str| {
            vars.iter()
                .find(|(n, _)| *n == name)
                .map(|(_, v)| OsString::from(v))
        };
        let mut stdin: &[u8] = b"";
        let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
        let mut io = Io {
            stdin: &mut stdin,
            stdin_is_terminal: terminal,
            stdout: &mut stdout,
            stderr: &mut stderr,
        };
        let code = run(args(list), &env, &mut io);
        (
            code,
            String::from_utf8(stdout).unwrap(),
            String::from_utf8(stderr).unwrap(),
        )
    }

    /// What Claude Code sets for its hooks' commands (synthetic).
    const HOOK_ENV: [(&str, &str); 1] = [("CLAUDE_PROJECT_DIR", "/home/sam/project")];

    /// A Claude Code older than 2.1.139 runs a hook's bare `command`: `pitcrew`, with the event
    /// on stdin. Exit 2 would block the prompt or the stop, so it exits 0 and says nothing.
    #[test]
    fn a_bare_run_by_an_old_claude_code_hook_is_silent_and_succeeds() {
        for list in [&["pitcrew"][..], &["pitcrew", "--json"]] {
            assert_eq!(
                run_with(list, &HOOK_ENV, false),
                (0, String::new(), String::new()),
                "{list:?}"
            );
        }
    }

    /// A person at a terminal, or a run outside a Claude Code hook, still gets the usage and 2;
    /// and in a hook's environment, anything but a bare run is parsed as before.
    #[test]
    fn a_bare_run_at_a_terminal_or_outside_a_hook_shows_the_usage() {
        for (vars, terminal) in [(&HOOK_ENV[..], true), (&[][..], false), (&[][..], true)] {
            let (code, stdout, stderr) = run_with(&["pitcrew"], vars, terminal);
            assert_eq!(code, 2, "{vars:?} {terminal}");
            assert!(stdout.is_empty(), "{stdout}");
            assert!(stderr.contains("Usage: pitcrew"), "{stderr}");
        }
        // A command group without its subcommand is not a bare run, even in a hook's environment
        // with stdin piped: it fails as it always has.
        for list in [
            &["pitcrew", "task"][..],
            &["pitcrew", "hooks"],
            &["pitcrew", "--json", "task"],
            &["pitcrew", "task", "--json"],
        ] {
            let (code, stdout, stderr) = run_with(list, &HOOK_ENV, false);
            assert_eq!(code, 2, "{list:?}");
            assert!(stdout.is_empty(), "{list:?}: {stdout}");
            assert!(stderr.contains("Usage: pitcrew"), "{list:?}: {stderr}");
        }
        let (code, stdout, stderr) = run_with(&["pitcrew", "nonsense"], &HOOK_ENV, false);
        assert_eq!(code, 2);
        assert!(stdout.is_empty(), "{stdout}");
        assert!(stderr.contains("nonsense"), "{stderr}");
        let (code, stdout, _) = run_with(&["pitcrew", "--help"], &HOOK_ENV, false);
        assert_eq!(code, 0);
        assert!(stdout.contains("Usage: pitcrew"), "{stdout}");
    }

    #[test]
    fn the_hook_is_found_after_global_flags() {
        let hook = |list: &[&str]| {
            let all = args(list);
            hook_args(&all).map(<[OsString]>::to_vec)
        };
        assert_eq!(
            hook(&["pitcrew", "hook", "claude", "Stop"]),
            Some(args(&["claude", "Stop"]))
        );
        assert_eq!(
            hook(&["pitcrew", "--json", "--json", "hook", "codex"]),
            Some(args(&["codex"]))
        );
        assert_eq!(hook(&["pitcrew", "hook"]), Some(Vec::new()));
        for other in [
            &["pitcrew"][..],
            &["pitcrew", "--json"],
            &["pitcrew", "task", "hook"],
            &["pitcrew", "--help", "hook"],
            &["pitcrew", "whoami"],
        ] {
            assert_eq!(hook(other), None, "{other:?}");
        }
    }
}
