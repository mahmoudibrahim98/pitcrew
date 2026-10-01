//! `pitcrew_remote::helper::slurm::Site::from_toml` on arbitrary recipe text. A site recipe
//! (`~/.pitcrew/sites/<name>.toml`) is trusted like a script the user runs, but it may be copied
//! from elsewhere: its checks keep its values from breaking the job script or changing which job
//! PitCrew acts on.
//!
//! Input: a flags byte, then the recipe text. Bits 0-2 pick the recipe's name (some invalid);
//! bits 3-4 pad the text with a leading comment to one byte under, exactly at, or one byte over
//! the 64 KiB cap.
//!
//! Checks, besides "no panic":
//! - an invalid name, or text over 64 KiB, is refused;
//! - **strict keys**: a recipe that loads has only the documented keys, each with its documented
//!   type (strings, a whole number for `cpus`, lists of strings for `sbatch` and `modules`);
//! - **what loads, renders**: a recipe that loads passes `Site::check` and `JobSpec::new`, and
//!   `JobSpec::render` makes a script for a plain target (R26: unless a value holds `hetjob` or
//!   `packjob`); the script's header is sound (see `pitcrew_fuzz::slurm::check_script`) and holds
//!   each of the recipe's `#SBATCH` options as its own line;
//! - an error is one line with no control characters (it reaches the UI and logs).
#![no_main]

use libfuzzer_sys::fuzz_target;
use pitcrew_fuzz::skip_known;
use pitcrew_fuzz::slurm::{check_script, target};
use pitcrew_remote::helper::slurm::{JobOptions, JobSpec, MAX_SITE_FILE, SITE_KEYS, Site};

const NAMES: [&str; 8] = [
    "generic",
    "example-cluster",
    "x",
    "a_b-9",
    "Bad Name",
    "",
    "-lead",
    "a.b",
];

/// The documented keys and the type each takes.
const KEYS: [(&str, Kind); 13] = [
    ("description", Kind::Text),
    ("partition", Kind::Text),
    ("account", Kind::Text),
    ("qos", Kind::Text),
    ("time", Kind::Text),
    ("cpus", Kind::Number),
    ("memory", Kind::Text),
    ("gres", Kind::Text),
    ("sbatch", Kind::List),
    ("modules_init", Kind::Text),
    ("modules", Kind::List),
    ("last_hop", Kind::Text),
    ("socket", Kind::Text),
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Text,
    Number,
    List,
}

fn name_is_valid(name: &str) -> bool {
    (1..=64).contains(&name.len())
        && name.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

fuzz_target!(|input: &[u8]| {
    let Some((&flags, rest)) = input.split_first() else {
        return;
    };
    assert_eq!(
        SITE_KEYS,
        KEYS.map(|(k, _)| k).as_slice(),
        "the documented keys changed"
    );
    let name = NAMES[usize::from(flags & 7)];
    let mut text = String::from_utf8_lossy(rest).into_owned();
    let cap = usize::try_from(MAX_SITE_FILE).expect("a small cap");
    let want = match (flags >> 3) & 3 {
        1 => Some(cap - 1),
        2 => Some(cap),
        3 => Some(cap + 1),
        _ => None,
    };
    if let Some(want) = want
        && text.len() + 3 <= want
    {
        let pad = "#".repeat(want - text.len() - 2);
        text = format!("{pad}\n\n{text}");
        assert_eq!(text.len(), want);
    }

    let result = Site::from_toml(name, &text);
    if !name_is_valid(name) || text.len() > cap {
        assert!(
            result.is_err(),
            "accepted: name {name:?}, {} bytes",
            text.len()
        );
    }
    let site = match result {
        Ok(site) => site,
        Err(e) => {
            let shown = e.to_string();
            assert!(
                !shown.chars().any(char::is_control),
                "an error with a control character: {shown:?}"
            );
            return;
        }
    };

    let doc: toml_edit::DocumentMut = text.parse().expect("a recipe that loads is TOML");
    for (key, item) in doc.iter() {
        let kind = KEYS
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, kind)| *kind)
            .unwrap_or_else(|| panic!("a recipe with an unknown key loads: {key:?}"));
        let ok = match kind {
            Kind::Text => item.as_str().is_some(),
            Kind::Number => item.as_integer().is_some(),
            Kind::List => item
                .as_array()
                .is_some_and(|a| a.iter().all(|v| v.as_str().is_some())),
        };
        assert!(ok, "a recipe whose {key:?} has the wrong type loads");
    }

    site.check()
        .unwrap_or_else(|e| panic!("a recipe that loads fails Site::check: {e}"));
    let spec = JobSpec::new(&site, &JobOptions::default())
        .unwrap_or_else(|e| panic!("a recipe that loads fails JobSpec::new: {e}"));
    let script = match spec.render(target()) {
        Ok(script) => script,
        Err(e) => {
            let words = format!("{:?}", site.defaults).to_ascii_lowercase();
            if skip_known() && (words.contains("hetjob") || words.contains("packjob")) {
                // Known finding R26: recipes with these words load but never render.
                return;
            }
            panic!("a recipe that loads does not render: {e}");
        }
    };
    let text = script.text();
    check_script(text);
    for option in &site.defaults.sbatch {
        assert!(
            text.contains(&format!("\n#SBATCH {option}\n")),
            "the recipe's {option:?} is not its own line:\n{text}"
        );
    }
});
