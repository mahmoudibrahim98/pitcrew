//! The command line.

use clap::{ArgAction, Args, Parser, Subcommand};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::str::FromStr;

/// The PitCrew daemon. For now it is the hub of a solo workspace: the store, the work model and
/// API v1 in one process. The runner, terminals and remote machines join later.
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
    /// The desktop's device token.
    #[command(subcommand)]
    Token(TokenCommand),
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

    #[test]
    fn the_version_line_names_the_protocol_range() {
        let line = version_line();
        assert!(line.starts_with(&format!("pitcrewd {} ", env!("CARGO_PKG_VERSION"))));
        assert!(line.contains(&format!("protocol {}", pitcrew_protocol::PROTOCOL_VERSION)));
    }
}
