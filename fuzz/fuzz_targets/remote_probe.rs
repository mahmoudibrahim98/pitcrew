//! `pitcrew_remote::probe::parse` on arbitrary ssh output. The remote machine, and anything on its
//! login path (banners, a noisy `.bashrc`), controls every byte.
//!
//! Input: one byte that picks the call's tag, then the output.
//!
//! Checks, besides "no panic":
//! - a report counts only when this call's begin and end markers are both there, each on a line
//!   of its own, the end after the begin;
//! - the facts agree: `home_on_network_fs` is true exactly when `home_fs` is not on the local
//!   allowlist (an unknown filesystem counts as networked), SLURM is the scheduler only with both
//!   `sbatch` and `squeue`, a tmux version only with tmux found, no fact is an empty string;
//! - printing the facts as a report with another tag and parsing that gives the same facts.
#![no_main]

use libfuzzer_sys::fuzz_target;
use pitcrew_protocol::model::Scheduler;
use pitcrew_remote::Probe;
use pitcrew_remote::probe::{is_local_fs, parse};

const TAGS: [&str; 4] = ["0123456789abcdef", "fedcba9876543210", "00", ""];
const FALLBACK: &str = "hpc-login";

fuzz_target!(|input: &[u8]| {
    let Some((&pick, rest)) = input.split_first() else {
        return;
    };
    let tag = TAGS[usize::from(pick) % TAGS.len()];
    let stdout = String::from_utf8_lossy(rest);
    let Ok(probe) = parse(&stdout, tag, FALLBACK) else {
        return;
    };

    let begin = format!("@@pitcrew-probe-begin-{tag}");
    let end = format!("@@pitcrew-probe-end-{tag}");
    let mut lines = stdout.lines().map(|l| l.trim_end_matches('\r').trim());
    assert!(
        lines.any(|l| l == begin),
        "a report without this call's begin marker"
    );
    assert!(lines.any(|l| l == end), "a report without its end marker");

    check_facts(&probe);
    let again = parse(
        &render(&probe, "1111222233334444"),
        "1111222233334444",
        FALLBACK,
    )
    .expect("a printed report parses");
    assert_eq!(again, probe, "printing and parsing changed the facts");
});

fn check_facts(p: &Probe) {
    assert_eq!(
        p.info.home_on_network_fs,
        !p.home_fs.as_deref().is_some_and(is_local_fs),
        "the network-filesystem flag disagrees with the filesystem"
    );
    assert_eq!(
        p.info.scheduler == Some(Scheduler::Slurm),
        p.has_sbatch && p.has_squeue,
        "SLURM without both sbatch and squeue"
    );
    if p.tmux_version.is_some() {
        assert!(p.info.has_tmux, "a tmux version without tmux");
    }
    for value in [
        Some(p.info.hostname.as_str()),
        Some(p.info.os.as_str()),
        Some(p.info.arch.as_str()),
        p.home.as_deref(),
        p.home_fs.as_deref(),
        p.login_shell.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        assert!(!value.is_empty(), "an empty fact");
        assert!(!value.contains('\n'), "a fact spans lines");
    }
    assert!(
        !p.info.os.chars().any(|c| c.is_ascii_uppercase()),
        "the OS name is not normalised"
    );
}

/// The probe script's report for `p`, as the machine would print it.
fn render(p: &Probe, tag: &str) -> String {
    let mut out = format!("banner\n@@pitcrew-probe-begin-{tag}\n");
    let mut line = |key: &str, value: Option<&str>| {
        if let Some(value) = value {
            out.push_str(&format!("{key}={value}\n"));
        }
    };
    line("os", Some(&p.info.os));
    line("arch", Some(&p.info.arch));
    line("hostname", Some(&p.info.hostname));
    line("home", p.home.as_deref());
    line("shell", p.login_shell.as_deref());
    line("tmux_found", Some(if p.info.has_tmux { "1" } else { "0" }));
    let tmux = p.tmux_version.as_ref().map(|v| format!("tmux {v}"));
    line("tmux", tmux.as_deref());
    line("sbatch", Some(if p.has_sbatch { "1" } else { "0" }));
    line("squeue", Some(if p.has_squeue { "1" } else { "0" }));
    line("fs", p.home_fs.as_deref());
    out.push_str(&format!("@@pitcrew-probe-end-{tag}\n"));
    out
}
