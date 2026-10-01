//! `pitcrew_remote::list_hosts_in` on arbitrary ssh config files, `Include` included. The files
//! are the user's own, but they are often copied from elsewhere, generated, or shared, and the
//! host names end up on ssh's command line.
//!
//! Input: up to five files separated by NUL bytes: `~/.ssh/config`, `~/.ssh/a`,
//! `~/.ssh/conf.d/b.conf`, `~/.ssh/conf.d/c.conf` and `~/extra`, written to a scratch home.
//! Inputs that could reach outside that home (an absolute path, or `..`) are skipped, so a run
//! never reads the machine's own files.
//!
//! Checks, besides "no panic" and finishing in time (the include walk is bounded):
//! - every listed host is concrete (no wildcard, not negated) and passes `validate_host`, so it
//!   can go on ssh's command line;
//! - no host is listed twice;
//! - listing again gives the same result.
#![no_main]

use libfuzzer_sys::fuzz_target;
use pitcrew_fuzz::fresh_dir;
use pitcrew_remote::list_hosts_in;
use pitcrew_remote::quote::validate_host;
use std::collections::HashSet;
use std::fs;

const FILES: [&str; 5] = [
    ".ssh/config",
    ".ssh/a",
    ".ssh/conf.d/b.conf",
    ".ssh/conf.d/c.conf",
    "extra",
];

fuzz_target!(|input: &[u8]| {
    let text = String::from_utf8_lossy(input);
    if !stays_home(&text) {
        return;
    }
    let home = fresh_dir("ssh-home");
    for (name, content) in FILES.iter().zip(text.split('\0')) {
        let path = home.join(name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create the folder");
        }
        fs::write(&path, content).expect("write a config file");
    }
    let config = home.join(FILES[0]);
    let list = list_hosts_in(&config, &home);

    let mut seen = HashSet::new();
    for host in &list.hosts {
        assert!(seen.insert(host), "{host:?} is listed twice");
        assert!(
            !host.starts_with('!') && !host.contains(['*', '?']),
            "a pattern is listed as a host: {host:?}"
        );
        assert!(
            validate_host(host).is_ok(),
            "an invalid host is listed: {host:?}"
        );
    }
    assert_eq!(list_hosts_in(&config, &home), list, "listing again differs");
});

/// Whether every path the files can name stays inside the scratch home: no `..`, and no `/` that
/// could start an absolute path (after whitespace, `=`, a quote, a backslash, or at the start).
fn stays_home(text: &str) -> bool {
    if text.contains("..") {
        return false;
    }
    let mut previous: Option<char> = None;
    for c in text.chars() {
        if c == '/'
            && previous
                .is_none_or(|p| p.is_whitespace() || matches!(p, '=' | '"' | '\'' | '\\' | '\0'))
        {
            return false;
        }
        previous = Some(c);
    }
    true
}
