//! This computer's name, for onboarding's machine-name default (`gateway_local_host`).
//!
//! The host name: `uname`'s node name on Unix, `COMPUTERNAME` on Windows. Only its first label
//! is kept (no domain, no `.local`), cleaned as a notification's text is (no control, bidi or
//! invisible characters, whitespace collapsed) and cut to [`MAX`] characters, what
//! `POST /v1/setup` takes as a machine name. [`FALLBACK`] when nothing is left.

use serde::Serialize;

/// The longest name given: `POST /v1/setup`'s `machine_name` is 1 to 60 characters.
pub const MAX: usize = 60;

/// The name when the host has none worth showing.
pub const FALLBACK: &str = "This computer";

/// `gateway_local_host`'s answer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct LocalHost {
    /// This computer's host name, cleaned.
    pub name: String,
}

/// This computer's host name, cleaned ([`clean_host`]).
#[must_use]
pub fn local_host() -> LocalHost {
    LocalHost {
        name: clean_host(raw_host_name().as_deref().unwrap_or("")),
    }
}

#[cfg(unix)]
fn raw_host_name() -> Option<String> {
    let uname = rustix::system::uname();
    uname.nodename().to_str().ok().map(str::to_owned)
}

#[cfg(windows)]
fn raw_host_name() -> Option<String> {
    std::env::var("COMPUTERNAME").ok()
}

#[cfg(not(any(unix, windows)))]
fn raw_host_name() -> Option<String> {
    None
}

/// A host name as onboarding may show it: its first label, cleaned, at most [`MAX`] characters;
/// [`FALLBACK`] when nothing is left.
#[must_use]
pub fn clean_host(raw: &str) -> String {
    let label = raw.split('.').next().unwrap_or("");
    let name = crate::notify::clean(label, MAX);
    if name.is_empty() {
        FALLBACK.to_owned()
    } else {
        name
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_names_are_cleaned() {
        for (raw, name) in [
            ("sam-laptop", "sam-laptop"),
            ("sam-laptop.lab.example.org", "sam-laptop"),
            ("Sams-MacBook-Pro.local", "Sams-MacBook-Pro"),
            ("DESKTOP-4F2K9QJ", "DESKTOP-4F2K9QJ"),
            ("evil\u{202e}\u{200b}name\n\u{1b}[31m", "evilname [31m"),
            ("", FALLBACK),
            (".local", FALLBACK),
            ("\u{200b}\t", FALLBACK),
        ] {
            assert_eq!(clean_host(raw), name, "{raw:?}");
        }
        let long = clean_host(&"x".repeat(300));
        assert_eq!(long.chars().count(), MAX);
    }

    #[test]
    fn this_computer_has_a_name() {
        let name = local_host().name;
        assert!(!name.is_empty());
        assert!(name.chars().count() <= MAX, "{name}");
        assert!(!name.contains('.') || name == FALLBACK, "{name}");
        assert!(!name.chars().any(char::is_control), "{name:?}");
    }
}
