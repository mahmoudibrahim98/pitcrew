//! `pitcrew`: the agent-facing CLI and the hook entry point. See the library for the parts.

use std::ffi::OsString;
use std::io::IsTerminal as _;
use std::time::Duration;

/// A verb gives up after this long, whatever the transport (pipes have no I/O timeouts).
const VERB_DEADLINE: Duration = Duration::from_secs(60);

fn main() {
    let args: Vec<OsString> = std::env::args_os().collect();
    let env = |name: &str| std::env::var_os(name);
    let stdin = std::io::stdin();
    let stdin_is_terminal = stdin.is_terminal();
    let mut stdin = stdin.lock();

    // The hook skips argument parsing, prints nothing and always exits 0, even if the daemon
    // hangs.
    if args.get(1).is_some_and(|a| a == "hook") {
        exit_after(pitcrew_cli::hook::DEADLINE, 0, None);
        let result = pitcrew_cli::hook::run(&args[2..], &env, &mut stdin, stdin_is_terminal);
        if let Err(e) = result
            && env(pitcrew_cli::hook::DEBUG_VAR).is_some_and(|v| !v.is_empty())
        {
            eprintln!("pitcrew hook: {e}");
        }
        std::process::exit(0);
    }

    exit_after(
        VERB_DEADLINE,
        5,
        Some("pitcrew: gave up waiting for the daemon after 60 s"),
    );
    // Not locked for the whole run, so the watchdog can still print.
    let mut stdout = std::io::stdout();
    let mut stderr = std::io::stderr();
    let mut io = pitcrew_cli::Io {
        stdin: &mut stdin,
        stdin_is_terminal,
        stdout: &mut stdout,
        stderr: &mut stderr,
    };
    let code = pitcrew_cli::run(args, &env, &mut io);
    std::process::exit(code);
}

/// Ends the process with `code` after `deadline`, from a watchdog thread.
fn exit_after(deadline: Duration, code: i32, message: Option<&'static str>) {
    let _ = std::thread::Builder::new()
        .name("deadline".into())
        .spawn(move || {
            std::thread::sleep(deadline);
            if let Some(message) = message {
                eprintln!("{message}");
            }
            std::process::exit(code);
        });
}
