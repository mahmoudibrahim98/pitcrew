//! The command line.

use clap::{ArgAction, Args, Parser, Subcommand, ValueEnum};
use pitcrew_protocol::model::Engine;
use std::ffi::OsString;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::str::FromStr;

/// The PitCrew daemon of a solo workspace: the store, the work model, the runner that watches
/// this machine's agent sessions, and API v1 in one process. Remote machines join later.
#[derive(Debug, Parser)]
#[command(name = "pitcrewd", disable_version_flag = true)]
pub struct Cli {
    /// Where the store, the tokens and the socket live. Default: the platform's local (never
    /// roaming) data folder, e.g. `%LOCALAPPDATA%\PitCrew\data` or `~/.local/share/pitcrew`.
    #[arg(long, global = true, value_name = "DIR")]
    pub state_dir: Option<PathBuf>,

    /// Print the version and the protocol range, then exit.
    #[arg(short = 'V', long, action = ArgAction::SetTrue)]
    pub version: bool,

    /// What to do.
    #[command(subcommand)]
    pub command: Option<Command>,
}

/// The subcommands.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Serve API v1 until Ctrl+C or SIGTERM.
    Serve(ServeArgs),
    /// Set up a fresh workspace (its first run), through the daemon running on this state
    /// directory: for people without the desktop, whose onboarding does the same.
    Init(InitArgs),
    /// The stdio bridge to a daemon's socket, for a remote machine's helper reached over SSH:
    /// `pitcrewd connect --socket <path> [--framed] [--nonce <hex>]`. Uses no state directory.
    #[command(disable_help_flag = true)]
    Connect(ConnectArgs),
    /// The desktop's device token.
    #[command(subcommand)]
    Token(TokenCommand),
}

/// `pitcrewd init`: the fields of `POST /v1/setup` (api-v1.md, "The first run").
#[derive(Debug, Args)]
pub struct InitArgs {
    /// The workspace's name, 1–80 characters.
    #[arg(long, value_name = "NAME")]
    pub workspace: String,

    /// Your name, 1–80 characters.
    #[arg(long, value_name = "PERSON")]
    pub name: String,

    /// Your handle: `@` and 1–32 of a-z, 0-9, `_` and `-`. The `@` may be left out (PowerShell
    /// reads a bare `@sam` as something else).
    #[arg(long, value_name = "@HANDLE")]
    pub handle: String,

    /// This machine's name, 1–60 characters.
    #[arg(long, value_name = "NAME")]
    pub machine: String,

    /// Where the daemon of this state directory listens, as given to its `serve --listen`.
    #[arg(long, value_name = "WHERE", default_value = "private")]
    pub listen: ListenArg,
}

/// `pitcrewd connect`: everything after it goes to the bridge as it is, which reads it itself.
#[derive(Debug, Args)]
pub struct ConnectArgs {
    /// `--socket <path> [--framed] [--nonce <hex>]`.
    #[arg(
        num_args = 0..,
        trailing_var_arg = true,
        allow_hyphen_values = true,
        value_name = "ARGS"
    )]
    pub args: Vec<OsString>,
}

/// `pitcrewd serve`.
#[derive(Debug, Args)]
pub struct ServeArgs {
    /// `private` (the default): a unix socket in `<state dir>/run`, or on Windows the current
    /// user's named pipe. `unix:<dir>/pitcrewd.sock` (Unix): exactly that socket, in a private
    /// directory, e.g. for a launcher on a remote machine. `tcp:127.0.0.1:<port>` is for
    /// development only; tokens are then the only protection.
    #[arg(long, value_name = "WHERE", default_value = "private")]
    pub listen: ListenArg,

    /// Seed the demo workspace. Only into an empty store: a store with data is refused.
    #[arg(long)]
    pub demo: bool,

    /// Do not run the back office (`@office`): no rules act on the log while this daemon runs,
    /// and what is appended meanwhile is never acted on later either.
    #[arg(long)]
    pub no_office: bool,

    /// Watch these agent homes instead of this user's own: `<dir>` is a folder laid out like a
    /// home folder (`<dir>/.claude`, `<dir>/.codex`, `<dir>/.local/share/opencode`);
    /// `<engine>=<dir>` is one engine's home itself (`claude=`, `codex=` or `opencode=`, like a
    /// `CLAUDE_CONFIG_DIR` or `CODEX_HOME`). With `--demo`, no home is watched unless given here.
    #[arg(long, value_name = "DIR", num_args = 1.., conflicts_with = "no_runner")]
    pub homes: Vec<HomeArg>,

    /// Do not run the runner: no agent sessions are watched, hooks are only logged, and no
    /// session has a terminal here.
    #[arg(long)]
    pub no_runner: bool,

    /// For tests and development only: the runner's tmux server's socket, instead of this state
    /// directory's own. Its directory must be private (it is made 0700 if missing).
    #[arg(long, value_name = "PATH", hide = true)]
    pub tmux_socket: Option<PathBuf>,

    /// For tests and development only: the pitcrew-ptyd executable the runner's terminals use where
    /// tmux is not, instead of the one next to pitcrewd.
    #[arg(long, value_name = "PATH", hide = true)]
    pub ptyd: Option<PathBuf>,

    /// For tests and development only: where pitcrew-ptyd listens (a socket path on Unix, whose
    /// directory must be private; a pipe name on Windows), instead of this state directory's own.
    #[arg(long, value_name = "PATH", hide = true)]
    pub ptyd_endpoint: Option<PathBuf>,

    /// For tests and development only: how long a pitcrew-ptyd this daemon starts waits with no
    /// terminal and no client before it exits (its own default is 30 seconds).
    #[arg(long, value_name = "MS", hide = true)]
    pub ptyd_idle_exit_ms: Option<u64>,

    /// For tests and development only: each machine scan (`POST /v1/machines/{id}/scan`) waits
    /// this long once it is accepted, holding its machine's place, before it walks the agent
    /// homes, so a second scan meanwhile can be shown to get 409.
    #[arg(long, value_name = "MS", hide = true)]
    pub scan_hold_ms: Option<u64>,

    /// For tests and development only: `pty` runs the terminals in pitcrew-ptyd even where tmux
    /// is usable; `auto` (the default) prefers tmux.
    #[arg(long, value_name = "RUNTIME", hide = true, value_enum, default_value_t = TerminalRuntimeArg::Auto)]
    pub terminal_runtime: TerminalRuntimeArg,
}

/// `serve --terminal-runtime`: which runtime the runner's terminals use.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
pub enum TerminalRuntimeArg {
    /// tmux where it is usable, else pitcrew-ptyd.
    #[default]
    Auto,
    /// pitcrew-ptyd, even where tmux is usable (tests and development).
    Pty,
}

/// One `--homes` value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HomeArg {
    /// `<dir>`: a folder laid out like a person's home folder.
    Root(PathBuf),
    /// `<engine>=<dir>`: that engine's home itself.
    Engine(Engine, PathBuf),
}

impl HomeArg {
    /// The engine homes this value names.
    #[must_use]
    pub fn homes(&self) -> Vec<(Engine, PathBuf)> {
        match self {
            Self::Root(dir) => vec![
                (Engine::Claude, dir.join(".claude")),
                (Engine::Codex, dir.join(".codex")),
                (
                    Engine::OpenCode,
                    dir.join(".local").join("share").join("opencode"),
                ),
            ],
            Self::Engine(engine, dir) => vec![(*engine, dir.clone())],
        }
    }
}

impl FromStr for HomeArg {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        if let Some((name, dir)) = s.split_once('=')
            && let Ok(engine) =
                serde_json::from_value::<Engine>(serde_json::Value::String(name.to_owned()))
        {
            if dir.is_empty() {
                return Err(format!(
                    "{name}= needs the folder of its home after the `=`"
                ));
            }
            return Ok(Self::Engine(engine, PathBuf::from(dir)));
        }
        if s.is_empty() {
            return Err("expected a folder, or <engine>=<folder>".to_owned());
        }
        Ok(Self::Root(PathBuf::from(s)))
    }
}

/// `pitcrewd token`.
#[derive(Debug, Subcommand)]
pub enum TokenCommand {
    /// Print where the device token is kept. Never prints the token itself.
    ShowPath,
}

/// The file name `unix:<path>` must end in: `pitcrew-api` binds this name in a directory it
/// makes (or checks is) private.
pub const SOCKET_FILE: &str = "pitcrewd.sock";

/// Where to listen, as given on the command line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ListenArg {
    /// The platform's private transport.
    Private,
    /// Exactly this unix socket, whose file name is [`SOCKET_FILE`]. Unix only.
    Unix(PathBuf),
    /// Loopback TCP, for development.
    Tcp(SocketAddr),
}

impl FromStr for ListenArg {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        if s == "private" {
            return Ok(Self::Private);
        }
        if let Some(path) = s.strip_prefix("unix:") {
            let path = PathBuf::from(path);
            if path.file_name().and_then(|n| n.to_str()) != Some(SOCKET_FILE)
                || path.parent().is_none_or(|dir| dir.as_os_str().is_empty())
            {
                return Err(format!(
                    "unix:<path> must be <dir>/{SOCKET_FILE}, not {path:?}; the directory is \
                     made private (0700), or must already be"
                ));
            }
            return Ok(Self::Unix(path));
        }
        let Some(addr) = s.strip_prefix("tcp:") else {
            return Err(format!(
                "expected `private`, `unix:<dir>/{SOCKET_FILE}` or `tcp:127.0.0.1:<port>`, not \
                 {s:?}"
            ));
        };
        let addr: SocketAddr = addr
            .parse()
            .map_err(|_| format!("{addr:?} is not an address like 127.0.0.1:47460"))?;
        if !addr.ip().is_loopback() {
            return Err(format!(
                "{addr} is not a loopback address; development TCP listens on loopback only"
            ));
        }
        Ok(Self::Tcp(addr))
    }
}

/// The `--version` line: the daemon's version and the protocol range it speaks.
#[must_use]
pub fn version_line() -> String {
    format!(
        "pitcrewd {} (protocol {}, oldest accepted {})",
        env!("CARGO_PKG_VERSION"),
        pitcrew_protocol::PROTOCOL_VERSION,
        pitcrew_protocol::PROTOCOL_MIN,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory as _;

    #[test]
    fn the_command_line_is_well_formed() {
        Cli::command().debug_assert();
    }

    #[test]
    fn listen_takes_private_or_loopback_tcp() {
        assert_eq!("private".parse(), Ok(ListenArg::Private));
        assert_eq!(
            "tcp:127.0.0.1:47460".parse(),
            Ok(ListenArg::Tcp("127.0.0.1:47460".parse().unwrap()))
        );
        assert_eq!(
            "tcp:[::1]:0".parse(),
            Ok(ListenArg::Tcp("[::1]:0".parse().unwrap()))
        );
        assert_eq!(
            "unix:/home/me/.pitcrew/run/pitcrewd.sock".parse(),
            Ok(ListenArg::Unix(PathBuf::from(
                "/home/me/.pitcrew/run/pitcrewd.sock"
            )))
        );
        for bad in [
            "",
            "public",
            "tcp:",
            "tcp:localhost:1",
            "tcp:0.0.0.0:47460",
            "tcp:192.0.2.1:80",
            "127.0.0.1:47460",
            "unix:",
            "unix:pitcrewd.sock",
            "unix:/run/other.sock",
            "unix:/run/",
        ] {
            assert!(bad.parse::<ListenArg>().is_err(), "{bad}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn the_socket_file_is_the_one_pitcrew_api_binds() {
        assert_eq!(SOCKET_FILE, pitcrew_api::SOCKET_NAME);
    }

    #[test]
    fn state_dir_goes_before_or_after_the_command() {
        let before = Cli::try_parse_from(["pitcrewd", "--state-dir", "x", "serve"]).unwrap();
        let after = Cli::try_parse_from(["pitcrewd", "serve", "--state-dir", "x"]).unwrap();
        assert_eq!(before.state_dir, Some(PathBuf::from("x")));
        assert_eq!(after.state_dir, Some(PathBuf::from("x")));
        let Some(Command::Serve(args)) = after.command else {
            panic!("not serve");
        };
        assert_eq!(args.listen, ListenArg::Private);
        assert!(!args.demo);
        assert!(!args.no_office, "the back office is on by default");
        assert!(!args.no_runner, "the runner is on by default");
        assert!(args.homes.is_empty());
        assert!(
            args.tmux_socket.is_none(),
            "the state directory's own socket"
        );
        assert!(args.ptyd.is_none(), "the pitcrew-ptyd next to pitcrewd");
        assert!(
            args.ptyd_endpoint.is_none(),
            "the state directory's own endpoint"
        );
        assert!(args.ptyd_idle_exit_ms.is_none());
        assert_eq!(args.terminal_runtime, TerminalRuntimeArg::Auto);
    }

    /// `--tmux-socket`, `--ptyd`, `--ptyd-endpoint`, `--ptyd-idle-exit-ms` and
    /// `--terminal-runtime` are for tests and development: they parse, and help does not show
    /// them.
    #[test]
    fn the_runtime_options_are_hidden() {
        let cli = Cli::try_parse_from([
            "pitcrewd",
            "serve",
            "--tmux-socket",
            "/tmp/x/s",
            "--ptyd",
            "/opt/p/pitcrew-ptyd",
            "--ptyd-endpoint",
            "/tmp/y/ptyd",
            "--ptyd-idle-exit-ms",
            "500",
            "--terminal-runtime",
            "pty",
        ])
        .unwrap();
        let Some(Command::Serve(args)) = cli.command else {
            panic!("not serve");
        };
        assert_eq!(args.tmux_socket, Some(PathBuf::from("/tmp/x/s")));
        assert_eq!(args.ptyd, Some(PathBuf::from("/opt/p/pitcrew-ptyd")));
        assert_eq!(args.ptyd_endpoint, Some(PathBuf::from("/tmp/y/ptyd")));
        assert_eq!(args.ptyd_idle_exit_ms, Some(500));
        assert_eq!(args.terminal_runtime, TerminalRuntimeArg::Pty);
        for bad in [
            &["--terminal-runtime", "screen"][..],
            &["--ptyd-idle-exit-ms", "soon"],
        ] {
            let mut all = vec!["pitcrewd", "serve"];
            all.extend_from_slice(bad);
            assert!(Cli::try_parse_from(all).is_err(), "{bad:?}");
        }
        let mut command = Cli::command();
        let serve = command.find_subcommand_mut("serve").unwrap();
        let help = serve.render_long_help().to_string();
        for hidden in [
            "tmux-socket",
            "--ptyd",
            "ptyd-endpoint",
            "idle-exit",
            "terminal-runtime",
        ] {
            assert!(!help.contains(hidden), "{hidden}: {help}");
        }
    }

    #[test]
    fn homes_are_folders_or_one_engines_home() {
        let cli = Cli::try_parse_from([
            "pitcrewd",
            "serve",
            "--homes",
            "/tmp/home",
            "claude=/data/claude",
            "codex=C:\\Users\\x\\.codex",
            "--demo",
        ])
        .unwrap();
        let Some(Command::Serve(args)) = cli.command else {
            panic!("not serve");
        };
        assert!(args.demo);
        assert_eq!(
            args.homes,
            [
                HomeArg::Root(PathBuf::from("/tmp/home")),
                HomeArg::Engine(Engine::Claude, PathBuf::from("/data/claude")),
                HomeArg::Engine(Engine::Codex, PathBuf::from("C:\\Users\\x\\.codex")),
            ]
        );
        let root = PathBuf::from("/tmp/home");
        assert_eq!(
            args.homes[0].homes(),
            [
                (Engine::Claude, root.join(".claude")),
                (Engine::Codex, root.join(".codex")),
                (Engine::OpenCode, root.join(".local/share/opencode")),
            ]
        );
        assert_eq!(
            "opencode=/x".parse(),
            Ok(HomeArg::Engine(Engine::OpenCode, PathBuf::from("/x")))
        );
        // Only an engine's name before `=` makes it an engine's home.
        assert_eq!("a=b".parse(), Ok(HomeArg::Root(PathBuf::from("a=b"))));
        for bad in ["", "claude="] {
            assert!(bad.parse::<HomeArg>().is_err(), "{bad}");
        }
        // Without a runner there is nothing to watch.
        let both = Cli::try_parse_from(["pitcrewd", "serve", "--no-runner", "--homes", "/x"]);
        assert!(both.is_err());
        let off = Cli::try_parse_from(["pitcrewd", "serve", "--no-runner"]).unwrap();
        let Some(Command::Serve(args)) = off.command else {
            panic!("not serve");
        };
        assert!(args.no_runner);
    }

    #[test]
    fn no_office_turns_the_back_office_off() {
        let cli = Cli::try_parse_from(["pitcrewd", "serve", "--demo", "--no-office"]).unwrap();
        let Some(Command::Serve(args)) = cli.command else {
            panic!("not serve");
        };
        assert!(args.demo);
        assert!(args.no_office);
    }

    /// Everything after `connect` reaches the bridge as it is, flags included; the bridge reads
    /// it (and says what is wrong, with its own exit code).
    #[test]
    fn connect_hands_everything_after_it_to_the_bridge() {
        let args = |list: &[&str]| -> Vec<OsString> {
            let mut all = vec!["pitcrewd", "connect"];
            all.extend_from_slice(list);
            let cli = Cli::try_parse_from(all).unwrap();
            let Some(Command::Connect(ConnectArgs { args })) = cli.command else {
                panic!("not connect");
            };
            args
        };
        let given = [
            "--socket",
            "/home/me/.pitcrew/run/pitcrewd.sock",
            "--framed",
            "--nonce",
            "0a1b",
        ];
        assert_eq!(args(&given), given.map(OsString::from));
        assert!(args(&[]).is_empty());
        for odd in [&["--help"][..], &["-x", "--socket"], &["--verbose", "y"]] {
            assert_eq!(
                args(odd),
                odd.iter().map(OsString::from).collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn init_takes_the_four_names_and_where_the_daemon_listens() {
        let cli = Cli::try_parse_from([
            "pitcrewd",
            "--state-dir",
            "x",
            "init",
            "--workspace",
            "Demo Lab",
            "--name",
            "Sam Rivera",
            "--handle",
            "@sam",
            "--machine",
            "This laptop",
        ])
        .unwrap();
        let Some(Command::Init(args)) = cli.command else {
            panic!("not init");
        };
        assert_eq!(args.workspace, "Demo Lab");
        assert_eq!(args.name, "Sam Rivera");
        assert_eq!(args.handle, "@sam");
        assert_eq!(args.machine, "This laptop");
        assert_eq!(args.listen, ListenArg::Private);
        let tcp = Cli::try_parse_from([
            "pitcrewd",
            "init",
            "--workspace",
            "L",
            "--name",
            "S",
            "--handle",
            "s",
            "--machine",
            "M",
            "--listen",
            "tcp:127.0.0.1:47460",
        ])
        .unwrap();
        let Some(Command::Init(args)) = tcp.command else {
            panic!("not init");
        };
        assert_eq!(args.listen, "tcp:127.0.0.1:47460".parse().unwrap());
        // Each is needed.
        assert!(
            Cli::try_parse_from(["pitcrewd", "init", "--workspace", "L", "--name", "S"]).is_err()
        );
    }

    #[test]
    fn the_version_line_names_the_protocol_range() {
        let line = version_line();
        assert!(line.starts_with(&format!("pitcrewd {} ", env!("CARGO_PKG_VERSION"))));
        assert!(line.contains(&format!("protocol {}", pitcrew_protocol::PROTOCOL_VERSION)));
    }
}
