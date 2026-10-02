//! What the helper's job asks SLURM for ([`JobOptions`], [`JobSpec`]) and the exact script that
//! is submitted ([`JobScript`]).
//!
//! The script is the fixed text of `job.sh` behind a header: `#SBATCH` lines and shell
//! assignments made only of values checked here. `#SBATCH` values are limited to characters
//! that need no quoting (sbatch reads quotes in them its own way), and every shell value is
//! quoted besides.

use super::site::{LastHop, SiteRecipe, SocketPlace};
use crate::helper::launch::{MAX_SOCKET_PATH, tmux_name};
use crate::helper::script::clean;
use crate::helper::{HelperError, Layout, Target};
use crate::quote::sh_quote;
use std::time::Duration;

/// The fixed part of every job script.
pub(crate) const JOB_BODY: &str = include_str!("job.sh");

/// The job script's first two lines. `helper.sh` checks them, and the last line (`JOB_BODY`'s
/// `# pitcrew-job-script-end`), before submitting.
pub(crate) const JOB_BEGIN: &str = "#!/bin/sh\n# pitcrew-job-script-begin\n";

/// How long the job waits for its record and for the helper's socket, by default.
pub const DEFAULT_JOB_WAIT: Duration = Duration::from_secs(60);

/// The `#SBATCH` options a site recipe or the user may add, as `--name=value` (or `--name` alone
/// for those in [`SBATCH_FLAGS`]). Options PitCrew sets itself (`--job-name`, `--chdir`,
/// `--output`, `--error`, `--parsable`) are not on it, nor ones that would change which job or
/// which cluster status and stop look at (`--array`, `--clusters`, `--wrap`, `--wait`, `--uid`,
/// …), nor ones that change the helper's environment or where mail goes (`--export`,
/// `--get-user-env`, `--propagate`, `--mail-user`), nor the typed options of [`JobOptions`]
/// (`--partition`, `--account`, `--qos`, `--time`, `--cpus-per-task`, `--mem`, `--gres`).
/// Abbreviations, which sbatch would accept, are not either.
pub const ALLOWED_SBATCH: &[&str] = &[
    "acctg-freq",
    "begin",
    "comment",
    "constraint",
    "contiguous",
    "core-spec",
    "cores-per-socket",
    "cpu-freq",
    "cpus-per-gpu",
    "deadline",
    "delay-boot",
    "dependency",
    "distribution",
    "exclude",
    "exclusive",
    "gpu-bind",
    "gpu-freq",
    "gpus",
    "gpus-per-node",
    "gpus-per-socket",
    "gpus-per-task",
    "gres-flags",
    "hint",
    "kill-on-invalid-dep",
    "licenses",
    "mail-type",
    "mcs-label",
    "mem-bind",
    "mem-per-cpu",
    "mem-per-gpu",
    "mincpus",
    "network",
    "nice",
    "no-kill",
    "no-requeue",
    "nodelist",
    "nodes",
    "ntasks",
    "ntasks-per-core",
    "ntasks-per-gpu",
    "ntasks-per-node",
    "ntasks-per-socket",
    "oversubscribe",
    "prefer",
    "priority",
    "profile",
    "requeue",
    "reservation",
    "signal",
    "sockets-per-node",
    "spread-job",
    "switches",
    "thread-spec",
    "threads-per-core",
    "time-min",
    "tmp",
    "use-min-nodes",
    "wckey",
];

/// The options of [`ALLOWED_SBATCH`] that may stand alone, without `=value`. sbatch reads all
/// `#SBATCH` words as one command line, so any other option without its value would take the
/// next directive as its value.
pub const SBATCH_FLAGS: &[&str] = &[
    "contiguous",
    "exclusive",
    "nice",
    "no-kill",
    "no-requeue",
    "oversubscribe",
    "requeue",
    "spread-job",
    "use-min-nodes",
];

/// Words that SLURM up to 20.11 reads anywhere in an `#SBATCH` line (case-insensitively) as the
/// start of another component of a heterogeneous job. Status and stop could then lose the job.
const HETJOB_WORDS: [&str; 2] = ["hetjob", "packjob"];

/// A SLURM time limit, or time left.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WallTime {
    /// So long.
    Limited(Duration),
    /// No limit (`UNLIMITED`).
    Unlimited,
}

/// Reads a SLURM time as `--time` takes it and `squeue` prints it (`%L`, `%l`): `minutes`,
/// `minutes:seconds`, `hours:minutes:seconds`, `days-hours`, `days-hours:minutes`,
/// `days-hours:minutes:seconds`, or `UNLIMITED`/`INFINITE`. `None` for anything else, such as
/// `NOT_SET`, `INVALID` or an empty field.
#[must_use]
pub fn parse_wall_time(text: &str) -> Option<WallTime> {
    let text = text.trim();
    if matches!(text, "UNLIMITED" | "INFINITE") {
        return Some(WallTime::Unlimited);
    }
    let number = |s: &str| -> Option<u64> {
        if s.is_empty() || s.len() > 9 || !s.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        s.parse().ok()
    };
    let (days, rest) = match text.split_once('-') {
        Some((d, rest)) => (Some(number(d)?), rest),
        None => (None, text),
    };
    let parts: Vec<u64> = rest.split(':').map(number).collect::<Option<_>>()?;
    let secs = match (days, parts.as_slice()) {
        (None, [m]) => m * 60,
        (None, [m, s]) => m * 60 + s,
        (None, [h, m, s]) => h * 3600 + m * 60 + s,
        (Some(d), [h]) => d * 86_400 + h * 3600,
        (Some(d), [h, m]) => d * 86_400 + h * 3600 + m * 60,
        (Some(d), [h, m, s]) => d * 86_400 + h * 3600 + m * 60 + s,
        _ => return None,
    };
    Some(WallTime::Limited(Duration::from_secs(secs)))
}

/// A wall time as `--time` takes it: `D-HH:MM:SS`, or `HH:MM:SS` under a day; a part of a
/// second counts as a whole one. Any duration prints (it may be shown in an error): one beyond
/// `u64::MAX` seconds prints as that many, which [`parse_wall_time`] refuses (more than 9 digits
/// of days). A job asks for at most a year ([`JobOptions::check`]).
#[must_use]
pub fn format_wall_time(time: Duration) -> String {
    let secs = time
        .as_secs()
        .saturating_add(u64::from(time.subsec_nanos() > 0));
    let (days, rest) = (secs / 86_400, secs % 86_400);
    let (h, m, s) = (rest / 3600, rest % 3600 / 60, rest % 60);
    if days > 0 {
        format!("{days}-{h:02}:{m:02}:{s:02}")
    } else {
        format!("{h:02}:{m:02}:{s:02}")
    }
}

/// The job name PitCrew gives the helper's job on `layout` when none is chosen:
/// `pitcrew-helper-` and 8 hex digits of the root's sha256 (as [`tmux_name`]).
#[must_use]
pub fn default_job_name(layout: &Layout) -> String {
    tmux_name(layout)
}

fn invalid(what: &str, value: &str, rule: &str) -> HelperError {
    HelperError::InvalidArgument(format!("{what} {:?}: {rule}", clean(value)))
}

/// `value` is 1 to `max` characters, each alphanumeric or in `extra`, not starting with `-`.
fn plain(what: &str, value: &str, extra: &str, max: usize) -> Result<(), HelperError> {
    let ok = (1..=max).contains(&value.len())
        && !value.starts_with('-')
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || extra.contains(c));
    if ok {
        Ok(())
    } else {
        Err(invalid(
            what,
            value,
            &format!("1 to {max} characters of A-Z a-z 0-9 {extra}, not starting with -"),
        ))
    }
}

/// Checks a site recipe's name: 1 to 64 characters of `a-z 0-9 _ -`, starting with a letter or
/// digit (it is also a file name).
///
/// # Errors
/// [`HelperError::InvalidArgument`].
pub fn check_site_name(name: &str) -> Result<(), HelperError> {
    let ok = (1..=64).contains(&name.len())
        && name.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '-'));
    if ok {
        Ok(())
    } else {
        Err(invalid(
            "site recipe name",
            name,
            "1 to 64 characters of a-z 0-9 _ -, starting with a letter or digit",
        ))
    }
}

/// Checks a module name for `module load`, e.g. `python/3.12`.
///
/// # Errors
/// [`HelperError::InvalidArgument`].
pub fn check_module(name: &str) -> Result<(), HelperError> {
    plain("module", name, "_.+/:@-", 128)
}

/// Checks the script a site sources to get the `module` command, e.g.
/// `/etc/profile.d/modules.sh`: an absolute path of plain characters.
///
/// # Errors
/// [`HelperError::InvalidArgument`].
pub fn check_modules_init(path: &str) -> Result<(), HelperError> {
    check_plain_path("modules_init", path, 256)
}

fn check_plain_path(what: &str, path: &str, max: usize) -> Result<(), HelperError> {
    let ok = (2..=max).contains(&path.len())
        && path.starts_with('/')
        && path
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_./+-".contains(c))
        && !path.split('/').any(|part| part == "." || part == "..");
    if ok {
        Ok(())
    } else {
        Err(invalid(
            what,
            path,
            &format!(
                "an absolute path of at most {max} characters of A-Z a-z 0-9 _ . / + -, \
                 without . or .. components"
            ),
        ))
    }
}

/// Checks one extra `#SBATCH` option: `--name=value`, or `--name` alone for [`SBATCH_FLAGS`];
/// the name on [`ALLOWED_SBATCH`], the value 1 to 256 characters of `A-Z a-z 0-9 _ . , : = + / @
/// % & | [ ] ( ) * -`, not starting with `-` (`--comment=--uid=0` could read as an option), and
/// no `hetjob` or `packjob` in any case (the rule [`JobSpec::new`] and [`JobSpec::render`] apply
/// to every line, so an option accepted here is never refused there).
///
/// # Errors
/// [`HelperError::InvalidArgument`] naming the problem.
pub fn check_sbatch_option(option: &str) -> Result<(), HelperError> {
    let Some(body) = option.strip_prefix("--") else {
        return Err(invalid(
            "#SBATCH option",
            option,
            "must be a long option, --name or --name=value",
        ));
    };
    let (name, value) = match body.split_once('=') {
        Some((name, value)) => (name, Some(value)),
        None => (body, None),
    };
    if !ALLOWED_SBATCH.contains(&name) {
        return Err(invalid(
            "#SBATCH option",
            option,
            "is not one PitCrew allows in the helper's job (see ALLOWED_SBATCH; the partition, \
             account, QOS, time, CPUs, memory and GPUs have their own settings)",
        ));
    }
    match value {
        Some(value) => {
            let ok = (1..=256).contains(&value.len())
                && !value.starts_with('-')
                && value
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "_.,:=+/@%&|[]()*-".contains(c));
            if !ok {
                return Err(invalid(
                    "#SBATCH option",
                    option,
                    "its value must be 1 to 256 characters of A-Z a-z 0-9 _ . , : = + / @ % & | \
                     [ ] ( ) * -, not starting with -",
                ));
            }
        }
        None if !SBATCH_FLAGS.contains(&name) => {
            return Err(invalid(
                "#SBATCH option",
                option,
                "needs =value (sbatch would take the next directive as its value)",
            ));
        }
        None => {}
    }
    check_no_hetjob(option)
}

/// Refuses an `#SBATCH` line holding `hetjob` or `packjob` in any case: SLURM up to 20.11 would
/// split the job there.
fn check_no_hetjob(line: &str) -> Result<(), HelperError> {
    let lower = line.to_ascii_lowercase();
    match HETJOB_WORDS.iter().find(|word| lower.contains(*word)) {
        Some(word) => Err(invalid(
            "#SBATCH line",
            line,
            &format!(
                "holds \"{word}\", where SLURM up to 20.11 would start another part of the job"
            ),
        )),
        None => Ok(()),
    }
}

/// The option's name: `--constraint=a100` is `constraint`.
fn option_name(option: &str) -> &str {
    let body = option.trim_start_matches('-');
    body.split_once('=').map_or(body, |(name, _)| name)
}

/// What the helper's job asks for. `None` leaves the choice to the site recipe, then to SLURM.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct JobOptions {
    /// `--partition`, e.g. `gpu` (a comma-separated list is allowed).
    pub partition: Option<String>,
    /// `--account`, e.g. `proj0001`.
    pub account: Option<String>,
    /// `--qos`.
    pub qos: Option<String>,
    /// `--time`, the wall time: at least a minute, at most a year.
    pub time: Option<Duration>,
    /// `--cpus-per-task`.
    pub cpus: Option<u32>,
    /// `--mem`, e.g. `4G` or `4096M` (`0` asks for all of a node's memory).
    pub memory: Option<String>,
    /// `--gres`, e.g. `gpu:1` or `gpu:a100:2`.
    pub gres: Option<String>,
    /// `--job-name`; by default [`default_job_name`]. Status and stop act on a job only while
    /// it has the name it was submitted with.
    pub job_name: Option<String>,
    /// More `#SBATCH` options, as long options (`--constraint=a100`), each on
    /// [`ALLOWED_SBATCH`] (see [`check_sbatch_option`]).
    pub sbatch: Vec<String>,
}

impl JobOptions {
    /// These options, with `defaults` (a site recipe's) filling in what they leave unset. Extra
    /// options are the defaults' then these, one of these replacing a default of the same name.
    #[must_use]
    pub fn or(&self, defaults: &JobOptions) -> JobOptions {
        let pick = |mine: &Option<String>, theirs: &Option<String>| mine.clone().or(theirs.clone());
        let mut sbatch: Vec<String> = defaults
            .sbatch
            .iter()
            .filter(|line| {
                !self
                    .sbatch
                    .iter()
                    .any(|mine| option_name(mine) == option_name(line))
            })
            .cloned()
            .collect();
        sbatch.extend(self.sbatch.iter().cloned());
        JobOptions {
            partition: pick(&self.partition, &defaults.partition),
            account: pick(&self.account, &defaults.account),
            qos: pick(&self.qos, &defaults.qos),
            time: self.time.or(defaults.time),
            cpus: self.cpus.or(defaults.cpus),
            memory: pick(&self.memory, &defaults.memory),
            gres: pick(&self.gres, &defaults.gres),
            job_name: pick(&self.job_name, &defaults.job_name),
            sbatch,
        }
    }

    /// Checks every value that would end up in the script.
    ///
    /// # Errors
    /// [`HelperError::InvalidArgument`] naming the first bad one.
    pub fn check(&self) -> Result<(), HelperError> {
        if let Some(p) = &self.partition {
            plain("partition", p, "_.,-", 128)?;
        }
        if let Some(a) = &self.account {
            plain("account", a, "_.-", 64)?;
        }
        if let Some(q) = &self.qos {
            plain("QOS", q, "_.-", 64)?;
        }
        if let Some(time) = self.time
            && !(Duration::from_secs(60)..=Duration::from_secs(366 * 86_400)).contains(&time)
        {
            return Err(HelperError::InvalidArgument(format!(
                "the wall time {} must be at least a minute and at most a year",
                format_wall_time(time)
            )));
        }
        if let Some(cpus) = self.cpus
            && !(1..=65_536).contains(&cpus)
        {
            return Err(HelperError::InvalidArgument(format!(
                "{cpus} CPUs: it must be 1 to 65536"
            )));
        }
        if let Some(m) = &self.memory {
            let digits = m.trim_end_matches(['K', 'M', 'G', 'T', 'k', 'm', 'g', 't']);
            let ok = (1..=12).contains(&digits.len())
                && m.len() - digits.len() <= 1
                && digits.bytes().all(|b| b.is_ascii_digit());
            if !ok {
                return Err(invalid(
                    "memory",
                    m,
                    "a number of at most 12 digits, with K, M, G or T after it if not megabytes",
                ));
            }
        }
        if let Some(g) = &self.gres {
            plain("gres", g, "_.:,+-", 128)?;
        }
        if let Some(n) = &self.job_name {
            plain("job name", n, "_.+-", 64)?;
        }
        for line in &self.sbatch {
            check_sbatch_option(line)?;
        }
        // Each value is an `#SBATCH` line of its own: refused here already, so that what a
        // recipe loads with also renders (the script checks each line again).
        // `check_sbatch_option` has already applied the rule to the extra options.
        let values = [
            &self.partition,
            &self.account,
            &self.qos,
            &self.memory,
            &self.gres,
            &self.job_name,
        ];
        for value in values.into_iter().flatten() {
            check_no_hetjob(value)?;
        }
        Ok(())
    }
}

/// The helper's job for one site: checked options, modules, and where its socket goes. Make
/// the script with [`JobSpec::render`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JobSpec {
    site: String,
    options: JobOptions,
    modules: Vec<String>,
    modules_init: Option<String>,
    last_hop: LastHop,
    socket: SocketPlace,
    wait: Duration,
}

impl JobSpec {
    /// The job `options` ask for on `site`, whose defaults fill in what they leave unset. Every
    /// value is checked here, the recipe's included.
    ///
    /// # Errors
    /// [`HelperError::InvalidArgument`] for any value that cannot go into the script.
    pub fn new(site: &dyn SiteRecipe, options: &JobOptions) -> Result<Self, HelperError> {
        check_site_name(site.name())?;
        let options = options.or(&site.defaults());
        options.check()?;
        let modules = site.modules().to_vec();
        for module in &modules {
            check_module(module)?;
        }
        let modules_init = site.modules_init().map(str::to_owned);
        if let Some(init) = &modules_init {
            check_modules_init(init)?;
        }
        Ok(Self {
            site: site.name().to_owned(),
            options,
            modules,
            modules_init,
            last_hop: site.last_hop(),
            socket: site.socket(),
            wait: DEFAULT_JOB_WAIT,
        })
    }

    /// How long the job waits for PitCrew's record of it, and then for the helper's socket
    /// (whole seconds, 1 to 3600). Default [`DEFAULT_JOB_WAIT`].
    ///
    /// # Errors
    /// [`HelperError::InvalidArgument`] outside that range.
    pub fn with_wait(mut self, wait: Duration) -> Result<Self, HelperError> {
        if !(Duration::from_secs(1)..=Duration::from_secs(3600)).contains(&wait) {
            return Err(HelperError::InvalidArgument(
                "the job's wait must be 1 second to 1 hour".to_owned(),
            ));
        }
        self.wait = wait;
        Ok(self)
    }

    /// The options, with the site's defaults filled in.
    #[must_use]
    pub fn options(&self) -> &JobOptions {
        &self.options
    }

    /// The exact script to show the user and submit, for `target`.
    ///
    /// # Errors
    /// [`HelperError::InvalidArgument`] when the target's root has characters other than
    /// `A-Z a-z 0-9 _ . / + -` (it is written into `#SBATCH` lines, which do not quote), or
    /// when its socket path would be too long.
    pub fn render(&self, target: &Target) -> Result<JobScript, HelperError> {
        let layout = target.layout();
        let root = layout.root();
        check_plain_path("the SLURM launcher's root", root, 200)?;
        if self.socket == SocketPlace::Root && layout.socket().len() > MAX_SOCKET_PATH {
            return Err(HelperError::InvalidArgument(format!(
                "the socket path {:?} is longer than {MAX_SOCKET_PATH} bytes; use a shorter \
                 layout root or a node-local socket",
                layout.socket()
            )));
        }
        let job_name = self
            .options
            .job_name
            .clone()
            .unwrap_or_else(|| default_job_name(layout));
        let o = &self.options;
        let mut directives = vec![
            format!("--job-name={job_name}"),
            format!("--chdir={root}"),
            format!("--output={root}/run/slurm-%j.out"),
        ];
        let mut add = |name: &str, value: &Option<String>| {
            if let Some(value) = value {
                directives.push(format!("--{name}={value}"));
            }
        };
        add("partition", &o.partition);
        add("account", &o.account);
        add("qos", &o.qos);
        add("time", &o.time.map(format_wall_time));
        add("cpus-per-task", &o.cpus.map(|c| c.to_string()));
        add("mem", &o.memory);
        add("gres", &o.gres);
        directives.extend(o.sbatch.iter().cloned());
        for directive in &directives {
            check_no_hetjob(directive)?;
        }

        let mut text = String::from(JOB_BEGIN);
        text.push_str(&format!(
            "# PitCrew's helper (pitcrewd) as a SLURM batch job, from the site recipe \"{}\".\n\
             # PitCrew submits exactly this script. The job name, working directory and output\n\
             # are given on sbatch's command line too, where nothing overrides them.\n",
            self.site
        ));
        for directive in &directives {
            text.push_str(&format!("#SBATCH {directive}\n"));
        }
        text.push('\n');
        let assignments = [
            ("pc_root", root.to_owned()),
            ("pc_tool_path", target.tool_path().to_owned()),
            ("pc_socket", self.socket.as_str().to_owned()),
            ("pc_wait", self.wait.as_secs().to_string()),
            (
                "pc_modules_init",
                self.modules_init.clone().unwrap_or_default(),
            ),
            ("pc_modules", self.modules.join(" ")),
        ];
        for (name, value) in assignments {
            text.push_str(&format!("{name}={}\n", sh_quote_any(&value)));
        }
        text.push_str(JOB_BODY);
        Ok(JobScript {
            text,
            root: root.to_owned(),
            tool_path: target.tool_path().to_owned(),
            job_name,
            site: self.site.clone(),
            last_hop: self.last_hop,
            socket: self.socket,
        })
    }
}

/// [`sh_quote`], with `''` for the empty string.
fn sh_quote_any(value: &str) -> String {
    if value.is_empty() {
        "''".to_owned()
    } else {
        sh_quote(value)
    }
}

/// The exact job script PitCrew submits: show [`JobScript::text`] to the user before
/// [`super::SlurmLauncher::submit`]. It belongs to the target it was made for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JobScript {
    text: String,
    root: String,
    tool_path: String,
    job_name: String,
    site: String,
    last_hop: LastHop,
    socket: SocketPlace,
}

impl JobScript {
    /// The script, byte for byte as it is submitted.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The job's name, by which status and stop recognise it.
    #[must_use]
    pub fn job_name(&self) -> &str {
        &self.job_name
    }

    /// The site recipe it was made from.
    #[must_use]
    pub fn site(&self) -> &str {
        &self.site
    }

    /// How the compute node is reached from the login node (for the tunnel).
    #[must_use]
    pub fn last_hop(&self) -> LastHop {
        self.last_hop
    }

    /// Where the helper's socket goes.
    #[must_use]
    pub fn socket(&self) -> SocketPlace {
        self.socket
    }

    /// The text's sha256, lower-case hex: the machine checks the script it got against it.
    #[must_use]
    pub fn sha256(&self) -> String {
        use sha2::{Digest as _, Sha256};
        crate::askpass::to_hex(&Sha256::digest(self.text.as_bytes()))
    }

    /// Refuses a script made for another root or tool path than `target`'s.
    pub(crate) fn check_target(&self, target: &Target) -> Result<(), HelperError> {
        if self.root != target.layout().root() || self.tool_path != target.tool_path() {
            return Err(HelperError::InvalidArgument(format!(
                "the job script was made for {} (tool path {}), not for {} (tool path {})",
                self.root,
                self.tool_path,
                target.layout().root(),
                target.tool_path()
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::helper::Platform;
    use crate::helper::slurm::site::{Site, generic};

    const JOB_END: &str = "# pitcrew-job-script-end\n";

    fn target(root: &str) -> Target {
        Target::with_layout(
            crate::Ssh::new("ssh"),
            "example-cluster",
            Layout::at(root).unwrap(),
            Platform::LinuxX86_64,
        )
        .unwrap()
    }

    #[test]
    fn wall_times_read_and_write() {
        let limited = |s: u64| Some(WallTime::Limited(Duration::from_secs(s)));
        for (text, want) in [
            ("30", limited(30 * 60)),
            ("59:58", limited(59 * 60 + 58)),
            ("1:59:58", limited(3600 + 59 * 60 + 58)),
            ("08:00:00", limited(8 * 3600)),
            ("2-0", limited(2 * 86_400)),
            ("2-03:04", limited(2 * 86_400 + 3 * 3600 + 4 * 60)),
            ("1-02:03:04", limited(86_400 + 2 * 3600 + 3 * 60 + 4)),
            (" 12:00 ", limited(12 * 60)),
            ("UNLIMITED", Some(WallTime::Unlimited)),
            ("INFINITE", Some(WallTime::Unlimited)),
            ("NOT_SET", None),
            ("INVALID", None),
            ("", None),
            ("1:2:3:4", None),
            ("-1", None),
            ("1-", None),
            ("1-2:3:4:5", None),
            ("a:b", None),
            ("+5", None),
            ("1234567890", None),
        ] {
            assert_eq!(parse_wall_time(text), want, "{text:?}");
        }
        for (secs, text) in [
            (60, "00:01:00"),
            (8 * 3600, "08:00:00"),
            (86_400 + 3600 + 61, "1-01:01:01"),
            (3 * 86_400, "3-00:00:00"),
        ] {
            let time = Duration::from_secs(secs);
            assert_eq!(format_wall_time(time), text);
            assert_eq!(parse_wall_time(text), Some(WallTime::Limited(time)));
        }
        assert_eq!(format_wall_time(Duration::from_millis(60_500)), "00:01:01");
    }

    /// Fuzz finding R27 (`fuzz/regressions/remote_slurm`, the input's duration): rounding up
    /// `u64::MAX` seconds and a nanosecond overflowed.
    #[test]
    fn the_longest_wall_times_print() {
        let longest = Duration::new(u64::MAX, 1);
        let printed = format_wall_time(longest);
        assert_eq!(printed, "213503982334601-07:00:15");
        assert_eq!(printed, format_wall_time(Duration::from_secs(u64::MAX)));
        assert_eq!(parse_wall_time(&printed), None);
        assert_eq!(
            format_wall_time(Duration::new(u64::MAX - 1, 1)),
            format_wall_time(Duration::from_secs(u64::MAX))
        );
        // Too long for a job: refused, with the time in the message.
        let options = JobOptions {
            time: Some(longest),
            ..JobOptions::default()
        };
        let err = options.check().unwrap_err();
        assert!(err.to_string().contains(&printed), "{err}");
    }

    #[test]
    fn options_are_checked() {
        JobOptions::default().check().unwrap();
        let good = JobOptions {
            partition: Some("gpu,gpu_long".into()),
            account: Some("proj0001".into()),
            qos: Some("normal".into()),
            time: Some(Duration::from_secs(3600)),
            cpus: Some(4),
            memory: Some("16G".into()),
            gres: Some("gpu:a100:2".into()),
            job_name: Some("my-helper".into()),
            sbatch: vec![
                "--constraint=a100&ib".into(),
                "--exclusive".into(),
                "--mail-type=END".into(),
                "--signal=B:TERM@60".into(),
            ],
        };
        good.check().unwrap();
        let bad = |change: fn(&mut JobOptions)| {
            let mut options = good.clone();
            change(&mut options);
            let err = options.check().unwrap_err();
            assert!(matches!(err, HelperError::InvalidArgument(_)), "{err:?}");
            assert!(!err.to_string().contains('\n'), "{err}");
        };
        bad(|o| o.partition = Some("gpu\n#SBATCH --uid=0".into()));
        bad(|o| o.partition = Some("gpu; rm -rf ~".into()));
        bad(|o| o.partition = Some("-p".into()));
        bad(|o| o.partition = Some(String::new()));
        bad(|o| o.account = Some("$(id)".into()));
        bad(|o| o.account = Some("proj 0001".into()));
        bad(|o| o.qos = Some("a'b".into()));
        bad(|o| o.time = Some(Duration::from_secs(59)));
        bad(|o| o.time = Some(Duration::from_secs(400 * 86_400)));
        bad(|o| o.cpus = Some(0));
        bad(|o| o.memory = Some("4GB".into()));
        bad(|o| o.memory = Some("G".into()));
        bad(|o| o.memory = Some("-4G".into()));
        bad(|o| o.gres = Some("gpu:1 --wrap=x".into()));
        bad(|o| o.job_name = Some("a/b".into()));
        bad(|o| o.job_name = Some("x".repeat(65)));
        for line in [
            "--job-name=x",
            "--job=x",
            "--chdir=/tmp",
            "--output=/tmp/x",
            "--out=/tmp/x",
            "--error=/tmp/x",
            "--parsable",
            "--wrap=id",
            "--array=1-10",
            "--clusters=other",
            "--uid=0",
            "--wait",
            "--partition=gpu",
            "--time=1:00:00",
            "--mem=4G",
            "-p gpu",
            "-w node1",
            "constraint=a100",
            "--constraint=a b",
            "--constraint='a'",
            "--constraint=$(id)",
            "--comment=x;y",
            "--comment=",
            "--constraint=a\n#SBATCH --uid=0",
            "--Constraint=a",
            // They would change the helper's environment, or send mail elsewhere.
            "--export=ALL,LD_PRELOAD=/tmp/x.so",
            "--get-user-env",
            "--propagate=ALL",
            "--mail-user=someone@example.org",
            // Without its value, an option would take the next directive as one.
            "--constraint",
            "--comment",
            "--exclude",
        ] {
            let mut options = good.clone();
            options.sbatch.push(line.into());
            assert!(options.check().is_err(), "{line:?}");
        }
        // True flags may stand alone, or take a value.
        for flag in SBATCH_FLAGS {
            check_sbatch_option(&format!("--{flag}")).unwrap();
            assert!(ALLOWED_SBATCH.contains(flag), "{flag}");
        }
        check_sbatch_option("--exclusive=user").unwrap();
        check_sbatch_option("--nice=10").unwrap();
        check_sbatch_option("--comment=a-b").unwrap();
        for module in ["python/3.12", "tool/1.0@abc", "compiler:2", "cuda"] {
            check_module(module).unwrap();
        }
        for module in ["", "-f", "a b", "a;b", "a$b", "a\nb", "a'b"] {
            assert!(check_module(module).is_err(), "{module:?}");
        }
        check_modules_init("/etc/profile.d/modules.sh").unwrap();
        for init in ["", "/", "relative.sh", "/a/../b", "/a b", "/a;b", "/a/./b"] {
            assert!(check_modules_init(init).is_err(), "{init:?}");
        }
        check_site_name("example-cluster").unwrap();
        for name in ["", "Example", "-x", "a.b", "a/b", &"x".repeat(65)] {
            assert!(check_site_name(name).is_err(), "{name:?}");
        }
    }

    /// Fuzz finding R25 (`fuzz/regressions/remote_slurm`, the input's text): a value that starts
    /// with `-` was accepted.
    #[test]
    fn values_that_start_with_a_dash_are_refused() {
        let err = check_sbatch_option("--comment=--uid=0").unwrap_err();
        assert!(err.to_string().contains("not starting with -"), "{err}");
        for line in [
            "--comment=-x",
            "--constraint=-a100",
            "--nice=-5",
            "--exclusive=-",
        ] {
            assert!(check_sbatch_option(line).is_err(), "{line:?}");
        }
        let options = JobOptions {
            sbatch: vec!["--comment=--uid=0".into()],
            ..JobOptions::default()
        };
        assert!(options.check().is_err());
        assert!(JobSpec::new(&generic(), &options).is_err());
    }

    /// Fuzz finding R34 (`fuzz/regressions/remote_slurm/r34-hetjob-word-passes-the-option-check`,
    /// read from that file): `check_sbatch_option` accepted `--comment=a-hetjob-b`, which
    /// `JobSpec::new` refuses. The target's property: what the option check accepts, a job takes.
    #[test]
    fn the_option_check_refuses_what_a_job_refuses() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fuzz/regressions/remote_slurm/r34-hetjob-word-passes-the-option-check");
        let raw = std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let (mode, text) = raw.split_first().unwrap();
        assert_eq!(mode % 4, 3, "the input checks #SBATCH options");
        let found = std::str::from_utf8(text).unwrap();
        assert_eq!(found, "--comment=a-hetjob-b");

        let err = check_sbatch_option(found).unwrap_err();
        assert!(err.to_string().contains("\"hetjob\""), "{err}");
        let job = |option: &str| {
            let options = JobOptions {
                sbatch: vec![option.to_owned()],
                ..JobOptions::default()
            };
            JobSpec::new(&generic(), &options)
        };
        for option in [
            found,
            "--comment=PackJob",
            "--constraint=a100&HETJOB",
            "--wckey=packjobs",
            "--dependency=afterok:1,hetjob",
        ] {
            assert!(check_sbatch_option(option).is_err(), "{option}");
            assert!(job(option).is_err(), "{option}");
        }
        for option in [
            "--comment=a-het-job-b",
            "--comment=hetjo-b",
            "--comment=pack-job",
            "--constraint=a100",
            "--exclusive",
        ] {
            check_sbatch_option(option).unwrap();
            job(option).unwrap();
        }
    }

    #[test]
    fn options_fill_in_from_the_recipe() {
        let defaults = JobOptions {
            partition: Some("batch".into()),
            account: Some("proj0001".into()),
            time: Some(Duration::from_secs(3600)),
            sbatch: vec!["--constraint=cpu".into(), "--nodes=1".into()],
            ..JobOptions::default()
        };
        let mine = JobOptions {
            partition: Some("gpu".into()),
            sbatch: vec!["--constraint=a100".into()],
            ..JobOptions::default()
        };
        let both = mine.or(&defaults);
        assert_eq!(both.partition.as_deref(), Some("gpu"));
        assert_eq!(both.account.as_deref(), Some("proj0001"));
        assert_eq!(both.time, Some(Duration::from_secs(3600)));
        assert_eq!(both.sbatch, ["--nodes=1", "--constraint=a100"]);
    }

    /// The script shares its directory checks with helper.sh, word for word.
    #[test]
    fn the_job_checks_are_the_launchers() {
        let helper = crate::helper::script::SCRIPT;
        let function = |script: &'static str, name: &str| -> &'static str {
            let start = script
                .find(&format!("\n{name}() {{\n"))
                .unwrap_or_else(|| panic!("{name}"));
            let end = script[start..].find("\n}\n").unwrap() + start;
            &script[start..end]
        };
        for name in [
            "pc_where",
            "pc_alive",
            "pc_dir_ok",
            "pc_acl_ok",
            "pc_safe_way",
            "pc_private",
        ] {
            assert_eq!(function(helper, name), function(JOB_BODY, name), "{name}");
        }
        let nap = "\npc_nap() { sleep 0.2 2>/dev/null || sleep 1; }\n";
        assert!(helper.contains(nap) && JOB_BODY.contains(nap));
        // And the same ls: on macOS, /bin/ls by its path.
        let ls = "\npc_ls_cmd=ls\ncase $pc_os in Darwin) pc_ls_cmd=/bin/ls ;; esac\n";
        assert!(helper.contains(ls) && JOB_BODY.contains(ls));
    }

    #[test]
    fn the_body_is_plain_and_ends_the_script() {
        assert!(JOB_BODY.ends_with(&format!("\n{JOB_END}")));
        assert_eq!(JOB_BODY.matches(JOB_END.trim_end()).count(), 1);
        assert!(!JOB_BODY.contains("pitcrew-job-script-begin"));
        assert!(!JOB_BODY.contains('\r'));
        assert!(JOB_BODY.is_ascii());
        // No directive below the first command, where sbatch would not read it anyway.
        assert!(!JOB_BODY.contains("#SBATCH"));
    }

    /// The scripts for the generic recipe and the example one, compared with the files under
    /// `snapshots/`. With `PITCREW_UPDATE_SNAPSHOTS=1` the files are rewritten instead.
    #[test]
    fn scripts_match_their_snapshots() {
        let example =
            Site::from_toml("example-cluster", include_str!("example-site.toml")).unwrap();
        let options = JobOptions {
            time: Some(Duration::from_secs(4 * 3600)),
            ..JobOptions::default()
        };
        let target = target("/home/someone/.pitcrew");
        let cases = [
            (
                "generic.sh",
                JobSpec::new(&generic(), &JobOptions::default()).unwrap(),
                include_str!("snapshots/generic.sh"),
            ),
            (
                "example-cluster.sh",
                JobSpec::new(&example, &options).unwrap(),
                include_str!("snapshots/example-cluster.sh"),
            ),
        ];
        let update = std::env::var_os("PITCREW_UPDATE_SNAPSHOTS").is_some();
        for (file, spec, want) in cases {
            let script = spec.render(&target).unwrap();
            assert!(script.text().starts_with(JOB_BEGIN));
            assert!(script.text().ends_with(JOB_END));
            if update {
                let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("src/helper/slurm/snapshots")
                    .join(file);
                std::fs::write(path, script.text()).unwrap();
            } else {
                assert_eq!(
                    script.text(),
                    want,
                    "{file}: run with PITCREW_UPDATE_SNAPSHOTS=1"
                );
            }
        }
    }

    #[test]
    fn values_are_quoted_and_lines_kept() {
        let site = Site {
            name: "quoting".into(),
            modules: vec!["a/1".into(), "b@2".into()],
            modules_init: Some("/opt/site/init.sh".into()),
            socket: SocketPlace::NodeLocal,
            defaults: JobOptions {
                sbatch: vec!["--constraint=[a*2&b*4]".into()],
                ..JobOptions::default()
            },
            ..Site::default()
        };
        let spec = JobSpec::new(&site, &JobOptions::default()).unwrap();
        let script = spec
            .render(
                &target("/home/someone/.pitcrew")
                    .with_tool_path("/opt/a b:/usr/bin")
                    .unwrap(),
            )
            .unwrap();
        let text = script.text();
        assert!(
            text.contains("\n#SBATCH --constraint=[a*2&b*4]\n"),
            "{text}"
        );
        assert!(text.contains("\npc_modules='a/1 b@2'\n"), "{text}");
        assert!(
            text.contains("\npc_modules_init=/opt/site/init.sh\n"),
            "{text}"
        );
        assert!(
            text.contains("\npc_tool_path='/opt/a b:/usr/bin'\n"),
            "{text}"
        );
        assert!(text.contains("\npc_socket=node-local\n"), "{text}");
        // Every #SBATCH line comes before the first command.
        let first_command = text.find("\npc_root=").unwrap();
        assert!(text.rfind("#SBATCH").unwrap() < first_command);
        assert_eq!(script.socket(), SocketPlace::NodeLocal);
        assert_eq!(script.site(), "quoting");

        // A bad value from a recipe is refused like one from the user.
        let bad = Site {
            modules: vec!["x; rm -rf ~".into()],
            ..site.clone()
        };
        assert!(JobSpec::new(&bad, &JobOptions::default()).is_err());
        let bad = Site {
            name: "Bad Name".into(),
            ..site
        };
        assert!(JobSpec::new(&bad, &JobOptions::default()).is_err());
    }

    #[test]
    fn roots_must_be_plain_and_short() {
        let spec = JobSpec::new(&generic(), &JobOptions::default()).unwrap();
        for root in [
            "/home/some one/.pitcrew",
            "/home/a%j/.pitcrew",
            "/home/a'b/.pitcrew",
        ] {
            let err = spec.render(&target(root)).unwrap_err();
            assert!(matches!(err, HelperError::InvalidArgument(_)), "{root}");
        }
        let long = format!("/{}", "d".repeat(90));
        assert!(spec.render(&target(&long)).is_err());
        // A node-local socket does not live under the root.
        let node_local = Site {
            socket: SocketPlace::NodeLocal,
            ..generic()
        };
        let spec = JobSpec::new(&node_local, &JobOptions::default()).unwrap();
        spec.render(&target(&long)).unwrap();

        let script = spec.render(&target("/home/someone/.pitcrew")).unwrap();
        script
            .check_target(&target("/home/someone/.pitcrew"))
            .unwrap();
        assert!(
            script
                .check_target(&target("/home/other/.pitcrew"))
                .is_err()
        );
        assert_eq!(
            script.job_name(),
            default_job_name(&Layout::at("/home/someone/.pitcrew").unwrap())
        );
    }

    /// SLURM up to 20.11 splits a job at `hetjob` or `packjob` anywhere in an `#SBATCH` line:
    /// options holding one are refused when checked (so before any script), a root when the
    /// script is made.
    #[test]
    fn hetjob_words_are_refused_anywhere() {
        let refused = |options: JobOptions| {
            let err = options.check().unwrap_err();
            assert!(err.to_string().contains("job\""), "{err}");
            let err = JobSpec::new(&generic(), &options).unwrap_err();
            assert!(err.to_string().contains("job\""), "{err}");
        };
        refused(JobOptions {
            partition: Some("hetjobs".into()),
            ..JobOptions::default()
        });
        refused(JobOptions {
            account: Some("PackJob".into()),
            ..JobOptions::default()
        });
        refused(JobOptions {
            qos: Some("HetJob".into()),
            ..JobOptions::default()
        });
        refused(JobOptions {
            gres: Some("packjob:1".into()),
            ..JobOptions::default()
        });
        refused(JobOptions {
            job_name: Some("my-HETJOB".into()),
            ..JobOptions::default()
        });
        refused(JobOptions {
            sbatch: vec!["--comment=a-packjob-b".into()],
            ..JobOptions::default()
        });
        let fine = JobSpec::new(&generic(), &JobOptions::default()).unwrap();
        let err = fine.render(&target("/home/packjob/.pitcrew")).unwrap_err();
        assert!(err.to_string().contains("job\""), "{err}");
        fine.render(&target("/home/someone/.pitcrew")).unwrap();
    }

    #[test]
    fn waits_are_bounded() {
        let spec = JobSpec::new(&generic(), &JobOptions::default()).unwrap();
        assert!(spec.clone().with_wait(Duration::ZERO).is_err());
        assert!(spec.clone().with_wait(Duration::from_secs(3601)).is_err());
        let spec = spec.with_wait(Duration::from_secs(5)).unwrap();
        let text = spec.render(&target("/home/someone/.pitcrew")).unwrap();
        assert!(text.text().contains("\npc_wait=5\n"));
    }
}
