//! Setting a machine up for agents, as the API serves it (`docs/build/contracts/api-v1.md`,
//! "Machine setup"): the machine check, the agents' accounts, and signing in to an agent CLI.
//!
//! - **The check** ([`MachineCheck`]) says what the machine has for running agents: each agent
//!   CLI and its version, tmux, git and gh, free disk, and SLURM where it is there. Each row is
//!   `ok`, `warn` or `missing` with a short reason, and says whether PitCrew can help
//!   ([`MachineCheckFix`]). A fix never installs a system package: it opens the tool's install
//!   page (from the client's own table of pages, never a URL the machine sends), or installs
//!   PitCrew's own helper.
//! - **Accounts** ([`AgentAccount`]) are what each CLI's own status command printed (`claude auth
//!   status`, `codex login status`, `opencode auth list`). PitCrew never reads a CLI's token or
//!   credential files.
//! - **Signing in** ([`SignIn`]) runs the CLI's own login command (`claude auth login`, `codex
//!   login`, `opencode auth login`) in a terminal on the machine, which the person drives; PitCrew
//!   never sees or keeps what the login stores.

use crate::ids::SessionId;
use crate::model::{Engine, TimestampMs};
use serde::{Deserialize, Serialize};

/// `GET /v1/machines/{id}/check`'s answer.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct MachineCheck {
    /// The rows, in a fixed order: the agent CLIs, tmux, git, gh, disk, then SLURM where the
    /// machine has it (and PitCrew's helper, in a check made before it is installed).
    pub rows: Vec<MachineCheckRow>,
}

/// One thing the check looked at.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct MachineCheckRow {
    /// What the row is about.
    pub id: MachineCheckItem,
    /// How it stands.
    pub status: MachineCheckStatus,
    /// A short reason for people: the version found, the free space, or what is wrong. Plain
    /// text, at most a line.
    pub detail: String,
    /// The tool's version as it printed it (its first line, cleaned), when it ran.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub version: Option<String>,
    /// What PitCrew can do about it, when anything. Absent on an `ok` row.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub fix: Option<MachineCheckFix>,
}

/// What a check row is about.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum MachineCheckItem {
    /// Claude Code (`claude`).
    CliClaude,
    /// The Codex CLI (`codex`).
    CliCodex,
    /// OpenCode (`opencode`).
    CliOpencode,
    /// tmux, where PitCrew's terminals run (3.2 or newer).
    Tmux,
    /// git.
    Git,
    /// GitHub's CLI, `gh`.
    Gh,
    /// Free space where PitCrew keeps its state.
    Disk,
    /// SLURM's tools (`sbatch`, `squeue`, `scancel`), where the machine has SLURM.
    Slurm,
    /// PitCrew's helper (`pitcrewd`), in a check made over SSH before it is installed.
    Helper,
}

impl MachineCheckItem {
    /// Every item, in the order the check reports them.
    pub const ALL: [Self; 9] = [
        Self::CliClaude,
        Self::CliCodex,
        Self::CliOpencode,
        Self::Tmux,
        Self::Git,
        Self::Gh,
        Self::Disk,
        Self::Slurm,
        Self::Helper,
    ];

    /// The wire name (`cli_claude`, `tmux`, …).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CliClaude => "cli_claude",
            Self::CliCodex => "cli_codex",
            Self::CliOpencode => "cli_opencode",
            Self::Tmux => "tmux",
            Self::Git => "git",
            Self::Gh => "gh",
            Self::Disk => "disk",
            Self::Slurm => "slurm",
            Self::Helper => "helper",
        }
    }

    /// The item named `name` on the wire, if any.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|item| item.as_str() == name)
    }

    /// The agent CLI's item for `engine`, if PitCrew checks it.
    #[must_use]
    pub const fn of_engine(engine: Engine) -> Option<Self> {
        match engine {
            Engine::Claude => Some(Self::CliClaude),
            Engine::Codex => Some(Self::CliCodex),
            Engine::OpenCode => Some(Self::CliOpencode),
        }
    }
}

/// How a check row stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum MachineCheckStatus {
    /// Ready.
    Ok,
    /// Usable, but worth a look: an old version, low disk, a tool that did not answer.
    Warn,
    /// Not there.
    Missing,
}

/// What PitCrew can do about a row. Never installing a system package: the person does that.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum MachineCheckFix {
    /// Open the tool's install page in the person's browser. The client picks the page from its
    /// own table, by the row's `id`: no URL comes from the machine.
    InstallPage,
    /// Install PitCrew's helper on the machine (the connect wizard's next steps).
    InstallHelper,
}

/// One agent CLI's account on a machine, as the CLI's own status command reported it
/// (`GET /v1/machines/{id}/agents`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct AgentAccount {
    /// The CLI.
    pub engine: Engine,
    /// Whether the CLI is on the machine's `PATH`.
    pub installed: bool,
    /// Whether the CLI says it is signed in. Absent when it could not tell: it is not installed,
    /// it is too old to have a status command, it did not answer in time, or it printed
    /// something PitCrew does not understand.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub signed_in: Option<bool>,
    /// The account, as the CLI printed it (an e-mail address, `ChatGPT`, `API key`, a count of
    /// providers); never a key or a token. At most 120 characters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub account: Option<String>,
    /// Why `signed_in` is absent, for people.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub detail: Option<String>,
}

/// `POST /v1/machines/{id}/agents/{engine}/sign-in`'s body, which may be omitted.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct StartSignIn {
    /// How to sign in; the CLI's default when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub method: Option<SignInMethod>,
}

/// How a CLI's login proves who the person is.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum SignInMethod {
    /// The CLI's own default: it shows a link to open in a browser.
    #[default]
    Browser,
    /// A one-time code to enter on the provider's page, for a machine the browser cannot reach
    /// back to (Codex only: `codex login --device-auth`).
    DeviceCode,
}

/// A sign-in terminal: the CLI's own login, running on the machine.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct SignIn {
    /// The CLI.
    pub engine: Engine,
    /// The terminal's id. Its WebSocket is the terminals route,
    /// `GET /v1/sessions/{terminal}/terminal`; it is not a session, and no other session route
    /// knows it.
    pub terminal: SessionId,
    /// The command it runs, word by word (`["claude", "auth", "login"]`), for people.
    pub command: Vec<String>,
    /// Whether the login still runs. Once it has ended, the terminal's output stays readable for
    /// a few minutes, then the terminal is removed.
    pub running: bool,
    /// When it started.
    pub started: TimestampMs,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn items_round_trip_by_name() {
        for item in MachineCheckItem::ALL {
            let json = serde_json::to_value(item).unwrap();
            assert_eq!(json, serde_json::Value::String(item.as_str().to_owned()));
            assert_eq!(MachineCheckItem::parse(item.as_str()), Some(item));
        }
        assert_eq!(MachineCheckItem::parse("cli-claude"), None);
    }

    #[test]
    fn optional_fields_are_left_out() {
        let row = MachineCheckRow {
            id: MachineCheckItem::Git,
            status: MachineCheckStatus::Ok,
            detail: "git version 2.43.0".into(),
            version: None,
            fix: None,
        };
        assert_eq!(
            serde_json::to_string(&row).unwrap(),
            r#"{"id":"git","status":"ok","detail":"git version 2.43.0"}"#
        );
        let account = AgentAccount {
            engine: Engine::Codex,
            installed: false,
            signed_in: None,
            account: None,
            detail: None,
        };
        assert_eq!(
            serde_json::to_string(&account).unwrap(),
            r#"{"engine":"codex","installed":false}"#
        );
    }

    #[test]
    fn a_sign_in_body_may_be_empty_but_not_unknown() {
        let empty: StartSignIn = serde_json::from_str("{}").unwrap();
        assert_eq!(empty.method, None);
        let device: StartSignIn = serde_json::from_str(r#"{"method":"device_code"}"#).unwrap();
        assert_eq!(device.method, Some(SignInMethod::DeviceCode));
        assert!(serde_json::from_str::<StartSignIn>(r#"{"token":"x"}"#).is_err());
        assert!(serde_json::from_str::<StartSignIn>(r#"{"method":"password"}"#).is_err());
    }
}
