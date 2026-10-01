//! `pitcrewd init`: sets a fresh workspace up (api-v1.md, "The first run") for people without the
//! desktop, whose onboarding sends the same `POST /v1/setup`.
//!
//! It asks the daemon running on this state directory, over its private transport (or the
//! `--listen` it was started with), with the device token in `device.token`. It reuses the
//! `pitcrew` CLI's client (`pitcrew_cli::client`): its checks of the daemon before the token is
//! sent, its HTTP, and its errors and exit codes. It never opens the store, and never prints the
//! token.
//!
//! - No running daemon: a clear error saying to start `pitcrewd serve` (exit 5).
//! - The API's `400` and `409`: their message (exits 2 and 4); any other failure as the CLI says
//!   it (`pitcrew --help` lists the exit codes).
//! - Done: one line on stdout naming the workspace, the person and the machine.
//!
//! On Windows the private transport is the user's own named pipe, which does not depend on the
//! state directory: `init` reaches whichever daemon holds it, and a daemon of another state
//! directory refuses this one's token (`401`).

use crate::cli::{InitArgs, ListenArg};
use crate::state::StateDir;
use pitcrew_cli::client::{Client, from_value};
use pitcrew_cli::display;
use pitcrew_cli::error::{Error, Kind};
use pitcrew_cli::transport::Timeouts;
use pitcrew_protocol::api::{Setup, SetupDone, SetupPerson};
use std::collections::HashMap;
use std::ffi::OsString;
use std::path::Path;
use std::process::ExitCode;

/// Runs `pitcrewd init`; errors are said on stderr and become the exit code.
pub fn init(state: &StateDir, args: &InitArgs) -> ExitCode {
    match set_up(state, args) {
        Ok(done) => {
            println!(
                "Set up the workspace \"{}\": you are {} ({}) on \"{}\".",
                display::line(&done.workspace.name),
                display::line(&done.me.handle),
                display::line(&done.me.name),
                display::line(&done.machine.name),
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("pitcrewd init: {}", display::line(&e.message));
            ExitCode::from(u8::try_from(e.exit_code()).unwrap_or(1))
        }
    }
}

/// `POST /v1/setup` on the daemon of `state`.
fn set_up(state: &StateDir, args: &InitArgs) -> Result<SetupDone, Error> {
    let not_running = || {
        Error::new(
            Kind::Unavailable,
            format!(
                "no pitcrewd is running on {}; start it with `pitcrewd --state-dir {} serve`, \
                 then run `pitcrewd init` again",
                state.root().display(),
                state.root().display()
            ),
        )
    };
    if cfg!(not(unix)) && matches!(args.listen, ListenArg::Unix(_)) {
        return Err(Error::invalid(
            "--listen unix:<path> is for Unix; here the daemon listens on `private` (the named \
             pipe) or `tcp:127.0.0.1:<port>`",
        ));
    }
    let token = state.device_token();
    if !token.is_file() {
        // `serve` writes it at its first start.
        return Err(not_running());
    }
    let env = environment(state, &args.listen, &token);
    let client = Client::from_env(&|name: &str| env.get(name).cloned(), Timeouts::VERB)?;
    client.check_version().map_err(|e| match e.kind {
        Kind::Unavailable => not_running(),
        _ => e,
    })?;
    let body = serde_json::to_value(setup(args))
        .map_err(|e| Error::internal(format!("cannot encode the setup: {e}")))?;
    from_value(client.post("/setup", &body)?)
}

/// The request's body. A handle given without its `@` gets one: in PowerShell a bare `@sam` is
/// not text.
fn setup(args: &InitArgs) -> Setup {
    let handle = if args.handle.starts_with('@') {
        args.handle.clone()
    } else {
        format!("@{}", args.handle)
    };
    Setup {
        workspace_name: args.workspace.clone(),
        person: SetupPerson {
            name: args.name.clone(),
            handle,
        },
        machine_name: args.machine.clone(),
    }
}

/// What the CLI's client reads from its environment: where the daemon listens, and the device
/// token's file. Nothing comes from this process's own environment.
fn environment(
    state: &StateDir,
    listen: &ListenArg,
    token: &Path,
) -> HashMap<&'static str, OsString> {
    let mut env = HashMap::new();
    env.insert("PITCREW_TOKEN_FILE", token.as_os_str().to_owned());
    match listen {
        // Without a variable, the client takes the user's own pipe on Windows.
        ListenArg::Private => {
            if cfg!(unix) {
                env.insert("PITCREW_SOCKET", state.run_dir().into_os_string());
            }
        }
        ListenArg::Unix(path) => {
            env.insert("PITCREW_SOCKET", path.as_os_str().to_owned());
        }
        ListenArg::Tcp(addr) => {
            env.insert("PITCREW_URL", OsString::from(format!("http://{addr}")));
        }
    }
    env
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn args(handle: &str) -> InitArgs {
        InitArgs {
            workspace: "Demo Lab".into(),
            name: "Sam Rivera".into(),
            handle: handle.into(),
            machine: "This laptop".into(),
            listen: ListenArg::Private,
        }
    }

    #[test]
    fn the_body_is_the_setup_and_a_bare_handle_gets_its_at() {
        let body = setup(&args("@sam"));
        assert_eq!(body.workspace_name, "Demo Lab");
        assert_eq!(body.person.name, "Sam Rivera");
        assert_eq!(body.person.handle, "@sam");
        assert_eq!(body.machine_name, "This laptop");
        assert_eq!(setup(&args("sam")).person.handle, "@sam");
        // Anything else is the API's to judge.
        assert_eq!(setup(&args("")).person.handle, "@");
    }

    #[test]
    fn the_client_is_pointed_at_this_state_directory() {
        let state = StateDir::resolve(Some(PathBuf::from("/tmp/x/state"))).unwrap();
        let token = state.device_token();
        let env = environment(&state, &ListenArg::Private, &token);
        assert_eq!(env["PITCREW_TOKEN_FILE"], token.into_os_string());
        if cfg!(unix) {
            assert_eq!(env["PITCREW_SOCKET"], state.run_dir().into_os_string());
        } else {
            assert!(!env.contains_key("PITCREW_SOCKET"));
        }
        let tcp = environment(
            &state,
            &"tcp:127.0.0.1:47460".parse().unwrap(),
            &state.device_token(),
        );
        assert_eq!(tcp["PITCREW_URL"], "http://127.0.0.1:47460");
        assert!(!tcp.contains_key("PITCREW_SOCKET"));
    }
}
