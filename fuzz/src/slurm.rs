//! What `remote_slurm` and `remote_site` check in a rendered SLURM job script.

use pitcrew_remote::{Layout, Platform, Ssh, Target};
use std::sync::OnceLock;

/// A plain target to render job scripts for (a made-up cluster and home).
pub fn target() -> &'static Target {
    static TARGET: OnceLock<Target> = OnceLock::new();
    TARGET.get_or_init(|| {
        Target::with_layout(
            Ssh::new("ssh"),
            "example-cluster",
            Layout::at("/home/someone/.pitcrew").expect("a layout"),
            Platform::LinuxX86_64,
        )
        .expect("a target")
    })
}

/// The script's `#SBATCH` header: one long option per line, all before the first command, none
/// that would split the job (`hetjob`, `packjob`), and PitCrew's own job name, directory and
/// output exactly once each, the name first.
pub fn check_script(text: &str) {
    assert!(
        text.starts_with("#!/bin/sh\n# pitcrew-job-script-begin\n"),
        "the script's first lines:\n{text}"
    );
    assert!(
        text.ends_with("\n# pitcrew-job-script-end\n")
            && text.matches("# pitcrew-job-script-end").count() == 1,
        "the script's last line:\n{text}"
    );
    let mut directives = Vec::new();
    let mut commands = false;
    for line in text.lines().skip(1) {
        if let Some(directive) = line.strip_prefix("#SBATCH ") {
            assert!(
                !commands,
                "an #SBATCH line after the first command:\n{text}"
            );
            directives.push(directive);
        } else if !line.is_empty() && !line.starts_with('#') {
            commands = true;
        }
    }
    assert!(!directives.is_empty(), "no #SBATCH lines:\n{text}");
    for directive in &directives {
        assert!(
            directive.starts_with("--")
                && !directive.chars().any(|c| c.is_whitespace() || c == '#'),
            "an #SBATCH line that is not one long option: {directive:?}"
        );
        let lower = directive.to_ascii_lowercase();
        assert!(
            !lower.contains("hetjob") && !lower.contains("packjob"),
            "an #SBATCH line that would split the job: {directive:?}"
        );
    }
    for name in ["--job-name=", "--chdir=", "--output="] {
        let count = directives.iter().filter(|d| d.starts_with(name)).count();
        assert_eq!(count, 1, "{name} appears {count} times:\n{text}");
    }
    assert!(directives[0].starts_with("--job-name="), "{text}");
}
