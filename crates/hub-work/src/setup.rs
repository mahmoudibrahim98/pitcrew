//! `POST /v1/setup`: the first run of a fresh hub (`docs/build/contracts/api-v1.md`, "The first
//! run").
//!
//! A fresh hub has a device token but no person, no machine and no name. This runs once: the
//! person is the device token's own member (the token already acts as a member id that nothing
//! knows), and the machine is the hub's own, local one. Both are appended in one `member_added`
//! and `machine_added`, authored by the caller; the workspace's name is kept outside the event log
//! (see [`crate::WorkService::set_workspace_name`]).

use crate::commands::require_person;
use crate::error::{Result, WorkError};
use crate::office::OFFICE_HANDLE;
use crate::query;
use crate::service::WorkService;
use pitcrew_protocol::api::{Caller, Setup, SetupDone};
use pitcrew_protocol::events::EventBody;
use pitcrew_protocol::ids::MachineId;
use pitcrew_protocol::model::{
    Liveness, Machine, MachineInfo, MachineKind, Member, MemberKind, Workspace,
};

/// Longest `workspace_name` and `person.name`, in characters (Unicode code points).
const NAME_CHARS: usize = 80;
/// Longest `machine_name`, in characters.
const MACHINE_NAME_CHARS: usize = 60;
/// Longest handle, after its leading `@`, in characters.
const HANDLE_CHARS: usize = 32;

/// The daemon's seam for starting what needed a person once [`WorkService::set_up`] commits
/// (api-v1.md, "The first run": the back office, and the runner on this machine).
///
/// Called at most once per hub's lifetime (setup runs once), synchronously, right after the
/// append that creates the person and the machine commits, with the command lock still held (see
/// "One writer" on [`WorkService`]): nothing else can write to the work model between the commit
/// and the listener seeing it. Register one with [`WorkService::with_setup_listener`].
///
/// **Reads only, never a write, and never panic.** The lock still held is `WorkService`'s own
/// command lock (a plain, non-reentrant `std::sync::Mutex`): `set_up` is still on the stack
/// waiting for `set_up` to return. [`WorkService::read`] and the other read methods (`tasks`,
/// `members`, ...) take a separate connection and are safe to call from here; any command method
/// (`create_task`, `move_task`, a second `set_up`, ...) tries to take the same lock again and
/// **deadlocks** the thread running `set_up`. A panic here unwinds through `set_up` itself: its
/// caller (the route) sees it as a task failure (`500`), not as "setup did not happen" — the
/// append already committed, so a retry then answers `409`, not a second attempt. If the daemon's
/// own work (writing `workspace.json`, starting the back office and the runner) can fail or needs
/// to write to the work model, it must catch its own errors here and hand `done` to its own task
/// (a channel, `tokio::spawn`, ...) to do that work off this call stack, not do it inline.
pub trait SetupListener: Send + Sync {
    /// `done` is exactly what `set_up` is about to return to its own caller.
    fn set_up(&self, done: &SetupDone);
}

/// Whitespace as JavaScript's `String.prototype.trim` sees it, so that this hub and the mock hub
/// store the same name: Unicode's `White_Space` except U+0085 (a control character, so refused
/// rather than trimmed), plus U+FEFF. Rust's `str::trim` differs in exactly those two.
fn is_trimmed_space(c: char) -> bool {
    (c.is_whitespace() && c != '\u{85}') || c == '\u{feff}'
}

/// A name of a [`Setup`] body (api-v1.md, "The first run"): trimmed of whitespace, then 1 to
/// `max_chars` characters (Unicode code points) with no control character anywhere. Returned
/// trimmed, as it is stored.
fn checked_text(value: &str, field: &str, max_chars: usize) -> Result<String> {
    let value = value.trim_matches(is_trimmed_space);
    // The byte-length check first: a value of more bytes than 4 per character allowed is too long
    // anyway, so an oversized input costs no full character count.
    if value.is_empty() || value.len() > max_chars * 4 || value.chars().count() > max_chars {
        return Err(WorkError::invalid(format!(
            "{field} must be 1 to {max_chars} characters after trimming."
        )));
    }
    if value.chars().any(char::is_control) {
        return Err(WorkError::invalid(format!(
            "{field} must not contain control characters."
        )));
    }
    Ok(value.to_owned())
}

/// `@` followed by 1 to [`HANDLE_CHARS`] of `a-z 0-9 _ -`. The charset already excludes control
/// characters.
fn checked_handle(handle: &str) -> Result<String> {
    let invalid = || {
        WorkError::invalid(format!(
            "person.handle must be \"@\" followed by 1 to {HANDLE_CHARS} of a-z, 0-9, \"_\" or \
             \"-\"."
        ))
    };
    let rest = handle.strip_prefix('@').ok_or_else(invalid)?;
    let well_formed = !rest.is_empty()
        && rest.len() <= HANDLE_CHARS
        && rest
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-');
    if well_formed {
        Ok(handle.to_owned())
    } else {
        Err(invalid())
    }
}

/// A [`Setup`] body whose fields are all well formed.
struct CheckedSetup {
    workspace_name: String,
    person_name: String,
    handle: String,
    machine_name: String,
}

/// Every field of `setup`, checked exactly as api-v1.md's "The first run" says. Every `400` comes
/// before the command runs; `workspace_name` is checked first only because it reads first in the
/// contract's own list, not because it is somehow more authoritative.
fn checked_setup(setup: Setup) -> Result<CheckedSetup> {
    Ok(CheckedSetup {
        workspace_name: checked_text(&setup.workspace_name, "workspace_name", NAME_CHARS)?,
        person_name: checked_text(&setup.person.name, "person.name", NAME_CHARS)?,
        handle: checked_handle(&setup.person.handle)?,
        machine_name: checked_text(&setup.machine_name, "machine_name", MACHINE_NAME_CHARS)?,
    })
}

impl WorkService {
    /// The first run of a fresh hub (`POST /v1/setup`). Device tokens only.
    ///
    /// The person is the caller's own member (kind `human`, no owner), and the machine is the
    /// hub's own (kind `local`, liveness `live`, a new id): both appended in one `member_added`
    /// and `machine_added`, authored by the caller. The workspace's name is then kept in memory
    /// (see [`WorkService::set_workspace_name`]; the daemon persists it in `workspace.json`), and
    /// any [`SetupListener`] is called once with the result.
    ///
    /// # Errors
    ///
    /// `forbidden` for an agent token; `invalid` for a field out of its length bound, a malformed
    /// handle, or a control character anywhere; `conflict` when the workspace already has a
    /// person, or when the handle is already taken (so a retried request never makes a second
    /// person: the first call to commit wins, every other sees the conflict). `@office`
    /// ([`OFFICE_HANDLE`]) is reserved for the back office, so it is always taken, even before
    /// the back office's member exists.
    pub fn set_up(&self, caller: &Caller, setup: Setup) -> Result<SetupDone> {
        require_person(caller, "Setting up the workspace")?;
        let checked = checked_setup(setup)?;
        let _guard = self.lock();
        self.read(|c| {
            if query::has_person(c)? {
                return Err(WorkError::conflict(
                    "This workspace already has a person; setup runs once.",
                ));
            }
            if checked.handle == OFFICE_HANDLE {
                return Err(WorkError::conflict(format!(
                    "{OFFICE_HANDLE} is reserved for the back office."
                )));
            }
            if query::member_with_handle(c, &checked.handle)?.is_some() {
                return Err(WorkError::conflict(format!(
                    "{} is already taken.",
                    checked.handle
                )));
            }
            Ok(())
        })?;
        let member = Member {
            id: caller.member,
            kind: MemberKind::Human,
            handle: checked.handle,
            name: checked.person_name,
            owner: None,
            persona: None,
        };
        let machine = Machine {
            id: MachineId::new(),
            name: checked.machine_name.clone(),
            kind: MachineKind::Local,
            info: Some(MachineInfo {
                hostname: checked.machine_name,
                os: std::env::consts::OS.to_owned(),
                arch: std::env::consts::ARCH.to_owned(),
                has_tmux: false,
                scheduler: None,
                home_on_network_fs: false,
            }),
            liveness: Liveness::Live,
        };
        self.append(&[
            self.by(
                caller,
                EventBody::MemberAdded {
                    member: member.clone(),
                },
            ),
            self.by(
                caller,
                EventBody::MachineAdded {
                    machine: machine.clone(),
                },
            ),
        ])?;
        self.set_hub_machine(machine.id);
        self.set_workspace_name(checked.workspace_name.clone());
        let done = SetupDone {
            workspace: Workspace {
                id: self.workspace(),
                name: checked.workspace_name,
            },
            me: member,
            machine,
        };
        if let Some(listener) = self.setup_listener() {
            listener.set_up(&done);
        }
        Ok(done)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup(workspace_name: &str, name: &str, handle: &str, machine_name: &str) -> Setup {
        Setup {
            workspace_name: workspace_name.to_owned(),
            person: pitcrew_protocol::api::SetupPerson {
                name: name.to_owned(),
                handle: handle.to_owned(),
            },
            machine_name: machine_name.to_owned(),
        }
    }

    #[test]
    fn well_formed_setup_passes() {
        let checked = checked_setup(setup("Demo Lab", "Sam Rivera", "@sam", "This laptop"));
        assert!(checked.is_ok());
    }

    #[test]
    fn every_field_is_bounded_and_control_free() {
        assert!(checked_setup(setup("", "Sam", "@sam", "PC")).is_err());
        assert!(checked_setup(setup(&"x".repeat(81), "Sam", "@sam", "PC")).is_err());
        assert!(checked_setup(setup(&"x".repeat(80), "Sam", "@sam", "PC")).is_ok());
        assert!(checked_setup(setup("Lab", "", "@sam", "PC")).is_err());
        assert!(checked_setup(setup("Lab", &"x".repeat(81), "@sam", "PC")).is_err());
        assert!(checked_setup(setup("Lab", "Sam", "@sam", "")).is_err());
        assert!(checked_setup(setup("Lab", "Sam", "@sam", &"x".repeat(61))).is_err());
        assert!(checked_setup(setup("Lab", "Sam", "@sam", &"x".repeat(60))).is_ok());
        assert!(checked_setup(setup("Lab\u{0007}", "Sam", "@sam", "PC")).is_err());
        assert!(checked_setup(setup("Lab", "Sam\u{0007}", "@sam", "PC")).is_err());
        assert!(checked_setup(setup("Lab", "Sam", "@sam", "PC\u{0007}")).is_err());
        // Code points, not bytes.
        assert!(checked_setup(setup(&"é".repeat(80), "Sam", "@sam", "PC")).is_ok());
        assert!(checked_setup(setup(&"é".repeat(81), "Sam", "@sam", "PC")).is_err());
    }

    #[test]
    fn names_are_trimmed_then_counted_and_stored_trimmed() {
        let checked = checked_setup(setup(
            "  Demo Lab\t",
            "\u{3000}Sam Rivera ",
            "@sam",
            "\nThis laptop\r\n",
        ))
        .unwrap();
        assert_eq!(checked.workspace_name, "Demo Lab");
        assert_eq!(checked.person_name, "Sam Rivera");
        assert_eq!(checked.machine_name, "This laptop");
        // Whitespace inside a name stays.
        let inside = checked_setup(setup("Demo  Lab", "Sam", "@sam", "PC")).unwrap();
        assert_eq!(inside.workspace_name, "Demo  Lab");
        // Nothing but whitespace is empty.
        for blank in [" ", "\t\n", "\u{a0}\u{2003}", "\u{feff}"] {
            assert!(
                checked_setup(setup(blank, "Sam", "@sam", "PC")).is_err(),
                "{blank:?}"
            );
            assert!(
                checked_setup(setup("Lab", blank, "@sam", "PC")).is_err(),
                "{blank:?}"
            );
            assert!(
                checked_setup(setup("Lab", "Sam", "@sam", blank)).is_err(),
                "{blank:?}"
            );
        }
        // Counted after trimming: 80 characters and padding fit.
        let padded = format!("  {}  ", "x".repeat(80));
        assert!(checked_setup(setup(&padded, "Sam", "@sam", "PC")).is_ok());
        let padded = format!(" {} ", "x".repeat(60));
        assert!(checked_setup(setup("Lab", "Sam", "@sam", &padded)).is_ok());
        // As JavaScript trims: U+FEFF goes, U+0085 (a control character) stays and is refused.
        assert_eq!(
            checked_setup(setup("\u{feff}Lab", "Sam", "@sam", "PC"))
                .unwrap()
                .workspace_name,
            "Lab"
        );
        assert!(checked_setup(setup("Lab\u{85}", "Sam", "@sam", "PC")).is_err());
        // A control character inside is still refused (a tab at an end is trimmed, above).
        assert!(checked_setup(setup("La\tb", "Sam", "@sam", "PC")).is_err());
        // The handle is not trimmed.
        assert!(checked_handle(" @sam").is_err());
        assert!(checked_handle("@sam ").is_err());
    }

    #[test]
    fn handles_are_an_at_then_bounded_lowercase_ascii() {
        assert!(checked_handle("@sam").is_ok());
        assert!(checked_handle("@a1_b-2").is_ok());
        assert!(checked_handle(&format!("@{}", "a".repeat(32))).is_ok());
        assert!(checked_handle(&format!("@{}", "a".repeat(33))).is_err());
        assert!(checked_handle("@").is_err());
        assert!(checked_handle("sam").is_err());
        assert!(checked_handle("@Sam").is_err());
        assert!(checked_handle("@sam rivera").is_err());
        assert!(checked_handle("@sam!").is_err());
    }
}
