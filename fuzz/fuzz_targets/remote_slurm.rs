//! The SLURM launcher's text checks in `pitcrew_remote::helper::slurm`: wall times as `squeue`
//! prints them and `--time` takes them, `sacct`'s exit codes, and the extra `#SBATCH` options a
//! site recipe or the user adds to the helper's job script. The scheduler's output comes from the
//! cluster (B11); the options come from recipes, which may be copied from elsewhere.
//!
//! Input: a mode byte, then text (or, for mode 1, a duration as 12 bytes).
//! - 0: `parse_wall_time` on the text;
//! - 1: `format_wall_time` on a duration;
//! - 2: `JobExit::parse` on the text;
//! - 3: `check_sbatch_option` on each line of the text, then a job script rendered with the
//!   accepted ones.
//!
//! Checks, besides "no panic":
//! - **wall times round-trip**: what `parse_wall_time` accepts is exactly the documented grammar
//!   (`m`, `m:s`, `h:m:s`, `d-h`, `d-h:m`, `d-h:m:s`, numbers of 1 to 9 digits, or `UNLIMITED`/
//!   `INFINITE`), with the seconds it says, and prints and reads back as the same time; and
//!   `format_wall_time` of any duration (`u64::MAX` seconds and a fraction too: R27, fixed) is
//!   `[D-]HH:MM:SS` with the seconds rounded up, and reads
//!   back as the same time. Both while the days fit in 9 digits: the fields may overflow into the
//!   next unit (`1:99`), so a time can be read that prints with more days than can be read back;
//! - `JobExit::parse` accepts exactly `code:signal` (two `u32`s, as Rust reads them) and prints
//!   back the same;
//! - **an accepted `#SBATCH` option** is one word, `--name` or `--name=value`, with no newline,
//!   `#`, quote or space, and no value that starts with `-` (R25, fixed); a name from
//!   `ALLOWED_SBATCH`, alone only for `SBATCH_FLAGS`; and `JobSpec::new` accepts what
//!   `check_sbatch_option` accepted (R34, fixed: a value holding `hetjob` or `packjob` was let
//!   through);
//! - **the job script**: when `JobSpec::render` accepts the options, every `#SBATCH` line is one
//!   directive word before the first command, none holds `hetjob` or `packjob` in any case, and the
//!   job name, working directory and output appear once each, from PitCrew.
#![no_main]

use libfuzzer_sys::fuzz_target;
use pitcrew_fuzz::slurm::{check_script, target};
use pitcrew_remote::helper::slurm::{
    ALLOWED_SBATCH, JobExit, JobOptions, JobSpec, SBATCH_FLAGS, WallTime, check_sbatch_option,
    format_wall_time, generic, parse_wall_time,
};
use std::time::Duration;

fuzz_target!(|input: &[u8]| {
    let Some((&mode, rest)) = input.split_first() else {
        return;
    };
    match mode % 4 {
        0 => wall_time(&String::from_utf8_lossy(rest)),
        1 => duration(rest),
        2 => exit(&String::from_utf8_lossy(rest)),
        _ => sbatch(&String::from_utf8_lossy(rest)),
    }
});

/// The documented grammar, written independently: `None` where `parse_wall_time` must refuse.
fn model_wall_time(text: &str) -> Option<WallTime> {
    let text = text.trim();
    if text == "UNLIMITED" || text == "INFINITE" {
        return Some(WallTime::Unlimited);
    }
    let number = |s: &str| -> Option<u64> {
        (!s.is_empty() && s.len() <= 9 && s.bytes().all(|b| b.is_ascii_digit()))
            .then(|| s.parse().ok())
            .flatten()
    };
    let (days, clock) = match text.find('-') {
        Some(i) => (Some(number(&text[..i])?), &text[i + 1..]),
        None => (None, text),
    };
    let parts = clock.split(':').map(number).collect::<Option<Vec<u64>>>()?;
    let secs = match (days, parts.len()) {
        (None, 1) => parts[0] * 60,
        (None, 2) => parts[0] * 60 + parts[1],
        (None, 3) => parts[0] * 3600 + parts[1] * 60 + parts[2],
        (Some(d), 1) => d * 86_400 + parts[0] * 3600,
        (Some(d), 2) => d * 86_400 + parts[0] * 3600 + parts[1] * 60,
        (Some(d), 3) => d * 86_400 + parts[0] * 3600 + parts[1] * 60 + parts[2],
        _ => return None,
    };
    Some(WallTime::Limited(Duration::from_secs(secs)))
}

fn wall_time(text: &str) {
    let parsed = parse_wall_time(text);
    assert_eq!(parsed, model_wall_time(text), "parse_wall_time({text:?})");
    if let Some(WallTime::Limited(time)) = parsed {
        // Fields may overflow into the next unit (`1:99`, `9-30`), so a time read back after
        // printing needs its days to fit in 9 digits again.
        let printed = format_wall_time(time);
        let want = (time.as_secs() / 86_400 < 1_000_000_000).then_some(WallTime::Limited(time));
        assert_eq!(
            parse_wall_time(&printed),
            want,
            "{text:?} printed as {printed:?}"
        );
    }
}

fn duration(bytes: &[u8]) {
    let mut raw = [0u8; 12];
    let n = bytes.len().min(12);
    raw[..n].copy_from_slice(&bytes[..n]);
    let secs = u64::from_le_bytes(raw[..8].try_into().expect("8 bytes"));
    let nanos = u32::from_le_bytes(raw[8..].try_into().expect("4 bytes")) % 1_000_000_000;
    let time = Duration::new(secs, nanos);
    let printed = format_wall_time(time);
    let rounded = secs.saturating_add(u64::from(nanos > 0));
    let (days, clock) = match printed.split_once('-') {
        Some((d, clock)) => (Some(d), clock),
        None => (None, printed.as_str()),
    };
    let fields: Vec<&str> = clock.split(':').collect();
    assert!(
        fields.len() == 3
            && fields
                .iter()
                .all(|f| f.len() == 2 && f.bytes().all(|b| b.is_ascii_digit())),
        "format_wall_time({time:?}) = {printed:?}"
    );
    assert!(
        days.is_none_or(|d| !d.is_empty() && d != "0" && d.bytes().all(|b| b.is_ascii_digit())),
        "format_wall_time({time:?}) = {printed:?}"
    );
    let want = (rounded / 86_400 < 1_000_000_000)
        .then_some(WallTime::Limited(Duration::from_secs(rounded)));
    assert_eq!(
        parse_wall_time(&printed),
        want,
        "{time:?} printed as {printed:?}"
    );
}

fn exit(text: &str) {
    let model = text.trim().split_once(':').and_then(|(c, s)| {
        Some(JobExit {
            code: c.parse().ok()?,
            signal: s.parse().ok()?,
        })
    });
    let parsed = JobExit::parse(text);
    assert_eq!(parsed, model, "JobExit::parse({text:?})");
    if let Some(exit) = parsed {
        let printed = format!("{}:{}", exit.code, exit.signal);
        assert_eq!(JobExit::parse(&printed), Some(exit));
    }
}

fn sbatch(text: &str) {
    let mut accepted = Vec::new();
    for option in text.split('\n') {
        if check_sbatch_option(option).is_err() {
            continue;
        }
        check_accepted(option);
        accepted.push(option.to_owned());
    }
    let options = JobOptions {
        sbatch: accepted,
        ..JobOptions::default()
    };
    let Ok(spec) = JobSpec::new(&generic(), &options) else {
        panic!("options check_sbatch_option accepted are refused by JobSpec::new: {options:?}");
    };
    if let Ok(script) = spec.render(target()) {
        check_script(script.text());
    }
}

/// What an accepted option may be.
fn check_accepted(option: &str) {
    assert!(
        !option
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || matches!(c, '#' | '"' | '\'')),
        "an accepted #SBATCH option with a space, control, # or quote: {option:?}"
    );
    let body = option
        .strip_prefix("--")
        .unwrap_or_else(|| panic!("an accepted #SBATCH option that is not long: {option:?}"));
    let (name, value) = match body.split_once('=') {
        Some((name, value)) => (name, Some(value)),
        None => (body, None),
    };
    assert!(
        ALLOWED_SBATCH.contains(&name),
        "not on the allowlist: {option:?}"
    );
    match value {
        None => assert!(SBATCH_FLAGS.contains(&name), "{option:?} needs a value"),
        Some(value) => {
            assert!(!value.is_empty(), "an empty value: {option:?}");
            assert!(
                !value.starts_with('-'),
                "an accepted #SBATCH value that starts with '-': {option:?}"
            );
        }
    }
}
