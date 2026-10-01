//! The SLURM launcher against a fake SLURM, using `deploy.rs`'s harness. `sbatch`, `squeue`,
//! `scancel`, `sacct`, `srun` and `sinfo` are this test binary too (`PITCREW_FAKE_SLURM=<tool>`),
//! keeping their state in files beside the machine. Running a job runs its script with the
//! machine's `sh` and the environment sbatch had, in the job's own process group, as slurmstepd
//! would on a compute node: here the compute node is this machine, named `node017`. scancel
//! sends SIGTERM to that group, and SIGKILL after a "KillWait".

use crate::unix::{
    Machine, RUN_ENV, Remote, alive, assert_private, block_on, comm, eventually, helper,
    helper_from, helper_script, me, mode, posix_shells, private_dir, quick, run_mark, script_len,
    shim, umask_of,
};
use pitcrew_protocol::model::Scheduler;
use pitcrew_remote::helper::slurm::{self, Cancelled, JobExit, LastHop, Site, SocketPlace};
use pitcrew_remote::{
    DirectLauncher, Endpoint, HelperError, HelperState, JobOptions, JobScript, JobSpec, JobState,
    LaunchOptions, Launcher, SlurmLauncher, SshError, Stopped, Target, deploy,
};
use serde::{Deserialize, Serialize};
use std::io::Write as _;
use std::os::unix::fs::{FileTypeExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::os::unix::process::{CommandExt as _, ExitStatusExt as _};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::{Duration, SystemTime};

/// Makes the binary play a SLURM tool, or the fake's own helpers (`run`, `killer`, `release`).
pub(crate) const SLURM_ENV: &str = "PITCREW_FAKE_SLURM";
/// The fake's state directory.
const DIR_ENV: &str = "PITCREW_FAKE_SLURM_DIR";
const VERSION: &str = "slurm 23.02.7";
const NODE: &str = "node017";
const TOOLS: [&str; 6] = ["sbatch", "squeue", "scancel", "sacct", "srun", "sinfo"];

// ─── The fake ──────────────────────────────────────────────────────────────────────────────

/// How the fake SLURM behaves: `config.json` in its directory.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
struct Config {
    /// Run jobs as soon as they are submitted; else they stay pending until released.
    start: bool,
    /// Why pending jobs wait.
    reason: String,
    /// The node jobs run on.
    node: String,
    /// The shell that runs job scripts (the machine's `sh`).
    interpreter: Option<String>,
    /// `$TMPDIR` in jobs.
    tmpdir: Option<PathBuf>,
    /// sbatch refuses every job with this message.
    sbatch_error: Option<String>,
    /// squeue fails with this message (the controller is down).
    squeue_error: Option<String>,
    /// Finished jobs stay listed by squeue, as within MinJobAge.
    keep_ended: bool,
    /// sacct fails: accounting is off.
    no_accounting: bool,
    /// Seconds between scancel's SIGTERM and SIGKILL.
    kill_wait: u64,
    /// `srun --help` lists `--overlap`.
    overlap: bool,
    /// What `sinfo -h -o %P` prints.
    partitions: Vec<String>,
    /// Jobs go to this cluster (sbatch answers `<id>;<cluster>`), and squeue, scancel and
    /// sacct see them only when asked with `-M <cluster>`.
    cluster: Option<String>,
    /// scancel fails with this message.
    scancel_error: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            start: true,
            reason: "Priority".to_owned(),
            node: NODE.to_owned(),
            interpreter: None,
            tmpdir: None,
            sbatch_error: None,
            squeue_error: None,
            keep_ended: false,
            no_accounting: false,
            kill_wait: 3,
            overlap: true,
            partitions: vec!["batch*".to_owned(), "gpu".to_owned()],
            cluster: None,
            scancel_error: None,
        }
    }
}

/// One job: `jobs/<id>.json`.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct FakeJob {
    id: u64,
    name: String,
    uid: u32,
    state: String,
    reason: String,
    node: String,
    /// The time limit, in seconds; `None` is unlimited.
    limit: Option<u64>,
    /// When it started to run (seconds since the epoch).
    started: Option<u64>,
    /// Exit code and signal.
    exit: Option<(i32, i32)>,
    /// The process group of its script.
    pgid: Option<u32>,
    /// sbatch's copy of the script, and its arguments.
    script: PathBuf,
    args: Vec<String>,
    chdir: PathBuf,
    output: String,
    /// sbatch's environment, which the job gets.
    env: Vec<(String, String)>,
    /// Its `#SBATCH` lines, as sbatch read them.
    directives: Vec<String>,
    /// The cluster it went to, if not the default one.
    #[serde(default)]
    cluster: Option<String>,
}

/// Whether a tool asked with `args` (`-M <cluster>` or not) sees `job`.
fn on_its_cluster(job: &FakeJob, args: &[String]) -> bool {
    value(args, "-M") == job.cluster.as_deref()
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

fn terminal(state: &str) -> bool {
    matches!(
        state,
        "COMPLETED" | "FAILED" | "CANCELLED" | "NODE_FAIL" | "TIMEOUT" | "OUT_OF_MEMORY"
    )
}

/// Writes `path` whole or not at all.
fn write_atomic(path: &Path, bytes: &[u8]) {
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    std::fs::write(&tmp, bytes).unwrap();
    std::fs::rename(&tmp, path).unwrap();
}

fn load_config(dir: &Path) -> Config {
    serde_json::from_slice(&std::fs::read(dir.join("config.json")).unwrap()).unwrap()
}

fn job_path(dir: &Path, id: u64) -> PathBuf {
    dir.join("jobs").join(format!("{id}.json"))
}

fn try_load(dir: &Path, id: u64) -> Option<FakeJob> {
    let bytes = std::fs::read(job_path(dir, id)).ok()?;
    Some(serde_json::from_slice(&bytes).unwrap())
}

fn load(dir: &Path, id: u64) -> FakeJob {
    try_load(dir, id).unwrap()
}

fn save(dir: &Path, job: &FakeJob) {
    write_atomic(&job_path(dir, job.id), &serde_json::to_vec(job).unwrap());
}

fn kill_group(pgid: u32, signal: rustix::process::Signal) {
    if let Some(pid) = i32::try_from(pgid)
        .ok()
        .and_then(rustix::process::Pid::from_raw)
    {
        let _ = rustix::process::kill_process_group(pid, signal);
    }
}

/// The value after `flag` in `args`.
fn value<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    let at = args.iter().position(|a| a == flag)?;
    args.get(at + 1).map(String::as_str)
}

/// A time as squeue prints it: `M:SS`, `H:MM:SS` or `D-HH:MM:SS`.
fn slurm_time(secs: u64) -> String {
    let (d, h, m, s) = (
        secs / 86_400,
        secs % 86_400 / 3600,
        secs % 3600 / 60,
        secs % 60,
    );
    if d > 0 {
        format!("{d}-{h:02}:{m:02}:{s:02}")
    } else if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

pub(crate) fn act_as_slurm(tool: &str) -> ExitCode {
    let dir = PathBuf::from(std::env::var_os(DIR_ENV).unwrap());
    let args: Vec<String> = std::env::args().skip(1).collect();
    if TOOLS.contains(&tool) {
        let mut log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("calls.log"))
            .unwrap();
        writeln!(log, "{}", serde_json::to_string(&(tool, &args)).unwrap()).unwrap();
        if args.first().map(String::as_str) == Some("--version") {
            println!("{VERSION}");
            return ExitCode::SUCCESS;
        }
    }
    let config = load_config(&dir);
    let id = || -> u64 { args[0].parse().unwrap() };
    match tool {
        "sbatch" => sbatch(&dir, &config, &args),
        "squeue" => squeue(&dir, &config, &args),
        "scancel" => scancel(&dir, &config, &args),
        "sacct" => sacct(&dir, &config, &args),
        "srun" if args.first().map(String::as_str) == Some("--help") => {
            println!("Usage: srun [OPTIONS(0)... [executable(0) [args(0)...]]]");
            println!("  -A, --account=name          charge job to specified account");
            if config.overlap {
                println!("      --overlap               Allow other steps to overlap this step");
            }
            ExitCode::SUCCESS
        }
        "sinfo" if args == ["-h", "-o", "%P"] => {
            for partition in &config.partitions {
                println!("{partition}");
            }
            ExitCode::SUCCESS
        }
        "run" => run_job(&dir, &config, id()),
        "killer" => {
            // KillWait: SIGKILL if the job is still ending then; gone at once if it ended.
            let deadline = std::time::Instant::now() + Duration::from_secs(config.kill_wait);
            loop {
                let job = load(&dir, id());
                if job.state != "COMPLETING" {
                    break;
                }
                if std::time::Instant::now() >= deadline {
                    if let Some(pgid) = job.pgid {
                        kill_group(pgid, rustix::process::Signal::KILL);
                    }
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            ExitCode::SUCCESS
        }
        "release" => {
            start_job(&dir, &config, id());
            ExitCode::SUCCESS
        }
        _ => ExitCode::from(2),
    }
}

fn sbatch(dir: &Path, config: &Config, args: &[String]) -> ExitCode {
    let (mut parsable, mut name, mut chdir, mut output) = (false, None, None, None);
    let mut script = None;
    let mut script_args = Vec::new();
    for arg in args {
        if script.is_some() {
            script_args.push(arg.clone());
        } else if arg == "--parsable" {
            parsable = true;
        } else if let Some(v) = arg.strip_prefix("--job-name=") {
            name = Some(v.to_owned());
        } else if let Some(v) = arg.strip_prefix("--chdir=") {
            chdir = Some(PathBuf::from(v));
        } else if let Some(v) = arg.strip_prefix("--output=") {
            output = Some(v.to_owned());
        } else if arg.starts_with('-') {
            eprintln!("sbatch: unrecognized option '{arg}'");
            return ExitCode::from(1);
        } else {
            script = Some(PathBuf::from(arg));
        }
    }
    if let Some(error) = &config.sbatch_error {
        eprintln!("{error}");
        return ExitCode::from(1);
    }
    let text = std::fs::read_to_string(script.unwrap()).unwrap();
    // Directives, until the first line that is not a comment.
    let directives: Vec<String> = text
        .lines()
        .skip(1)
        .take_while(|l| l.starts_with('#') || l.trim().is_empty())
        .filter_map(|l| l.strip_prefix("#SBATCH "))
        .map(str::to_owned)
        .collect();
    let limit = directives
        .iter()
        .find_map(|d| d.strip_prefix("--time="))
        .and_then(slurm::parse_wall_time)
        .and_then(|t| match t {
            slurm::WallTime::Limited(d) => Some(d.as_secs()),
            slurm::WallTime::Unlimited => None,
        });
    let next = dir.join("next");
    let id = std::fs::read_to_string(&next)
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .unwrap_or(4099)
        + 1;
    write_atomic(&next, id.to_string().as_bytes());
    let spool = dir.join("spool").join(format!("{id}.sh"));
    std::fs::write(&spool, &text).unwrap();
    std::fs::set_permissions(&spool, std::fs::Permissions::from_mode(0o700)).unwrap();
    let job = FakeJob {
        id,
        name: name.unwrap_or_else(|| "sbatch".to_owned()),
        uid: rustix::process::getuid().as_raw(),
        state: "PENDING".to_owned(),
        reason: config.reason.clone(),
        node: String::new(),
        limit,
        started: None,
        exit: None,
        pgid: None,
        script: spool,
        args: script_args,
        chdir: chdir.unwrap_or_else(|| std::env::current_dir().unwrap()),
        output: output.unwrap_or_else(|| "slurm-%j.out".to_owned()),
        env: std::env::vars()
            .filter(|(k, _)| k != SLURM_ENV && k != DIR_ENV)
            .collect(),
        directives,
        cluster: config.cluster.clone(),
    };
    save(dir, &job);
    if config.start {
        start_job(dir, config, id);
    }
    // A warning first, as real sites often print, then the id; then a number on stderr that
    // only a reader of both streams would take for the id.
    eprintln!("sbatch: warning: this is a fake SLURM");
    match (parsable, &config.cluster) {
        (true, Some(cluster)) => println!("{id};{cluster}"),
        (true, None) => println!("{id}"),
        (false, _) => println!("Submitted batch job {id}"),
    }
    std::io::stdout().flush().unwrap();
    eprintln!("{}", id + 1000);
    ExitCode::SUCCESS
}

/// Puts a pending job on the node and runs it, in a process that outlives this one.
#[allow(clippy::zombie_processes)]
fn start_job(dir: &Path, config: &Config, id: u64) {
    let mut job = load(dir, id);
    if job.state != "PENDING" {
        return;
    }
    job.state = "RUNNING".to_owned();
    job.reason = "None".to_owned();
    job.node.clone_from(&config.node);
    job.started = Some(now());
    save(dir, &job);
    Command::new(me())
        .env(SLURM_ENV, "run")
        .env(DIR_ENV, dir)
        .arg(id.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
}

/// slurmstepd: the script in its own process group, its output in the job's file, then the
/// final state.
fn run_job(dir: &Path, config: &Config, id: u64) -> ExitCode {
    let job = load(dir, id);
    let output = job.output.replace("%j", &id.to_string());
    let output = job.chdir.join(output);
    let existed = output.exists();
    let Ok(file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o644)
        .open(&output)
    else {
        let mut job = load(dir, id);
        job.state = "FAILED".to_owned();
        job.exit = Some((1, 0));
        save(dir, &job);
        return ExitCode::SUCCESS;
    };
    if !existed {
        // As a slurmstepd with its own umask would make it: the job makes it private.
        std::fs::set_permissions(&output, std::fs::Permissions::from_mode(0o644)).unwrap();
    }
    let interpreter = config.interpreter.as_deref().unwrap_or("/bin/sh");
    let mut command = Command::new(interpreter);
    command
        .arg(&job.script)
        .args(&job.args)
        .current_dir(&job.chdir)
        .env_clear()
        .envs(job.env.iter().cloned())
        .env("SLURM_JOB_ID", id.to_string())
        .env("SLURM_JOB_NAME", &job.name)
        .env("SLURMD_NODENAME", &job.node)
        .stdin(Stdio::null())
        .stdout(file.try_clone().unwrap())
        .stderr(file)
        .process_group(0);
    match &config.tmpdir {
        Some(tmp) => command.env("TMPDIR", tmp),
        None => command.env_remove("TMPDIR"),
    };
    let mut child = command.spawn().unwrap();
    let mut job = load(dir, id);
    job.pgid = Some(child.id());
    save(dir, &job);
    if job.state == "COMPLETING" {
        kill_group(child.id(), rustix::process::Signal::TERM);
    }
    let status = child.wait().unwrap();
    let mut job = load(dir, id);
    let ended = (status.code().unwrap_or(0), status.signal().unwrap_or(0));
    match job.state.as_str() {
        "RUNNING" => {
            job.state = if ended == (0, 0) {
                "COMPLETED"
            } else {
                "FAILED"
            }
            .to_owned();
            job.exit = Some(ended);
        }
        "COMPLETING" => {
            job.state = "CANCELLED".to_owned();
            job.exit = Some((0, 15));
        }
        _ => {
            job.exit = job.exit.or(Some(ended));
        }
    }
    save(dir, &job);
    ExitCode::SUCCESS
}

fn squeue(dir: &Path, config: &Config, args: &[String]) -> ExitCode {
    if let Some(error) = &config.squeue_error {
        eprintln!("{error}");
        return ExitCode::from(1);
    }
    let id: u64 = value(args, "-j").unwrap().parse().unwrap();
    let format = value(args, "-o").unwrap();
    let Some(job) = try_load(dir, id)
        .filter(|j| config.keep_ended || !terminal(&j.state))
        .filter(|j| on_its_cluster(j, args))
    else {
        eprintln!("slurm_load_jobs error: Invalid job id specified");
        return ExitCode::from(1);
    };
    // As real squeue: SQUEUE_STATES filters even jobs asked for by id.
    if let Ok(states) = std::env::var("SQUEUE_STATES")
        && !states
            .split(',')
            .any(|s| s.eq_ignore_ascii_case(&job.state))
    {
        return ExitCode::SUCCESS;
    }
    let limit = job.limit.map_or_else(|| "UNLIMITED".to_owned(), slurm_time);
    let left = match (job.limit, job.started) {
        (None, _) => "UNLIMITED".to_owned(),
        (Some(limit), Some(started)) => slurm_time(limit.saturating_sub(now() - started)),
        (Some(limit), None) => slurm_time(limit),
    };
    let mut line = String::new();
    let mut chars = format.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            line.push(c);
            continue;
        }
        match chars.next() {
            Some('i') => line.push_str(&job.id.to_string()),
            Some('U') => line.push_str(&job.uid.to_string()),
            Some('T') => line.push_str(&job.state),
            Some('r') => line.push_str(&job.reason),
            Some('L') => line.push_str(&left),
            Some('l') => line.push_str(&limit),
            Some('N') => line.push_str(&job.node),
            Some('j') => line.push_str(&job.name),
            other => panic!("squeue format %{other:?}"),
        }
    }
    println!("{line}");
    ExitCode::SUCCESS
}

#[allow(clippy::zombie_processes)]
fn scancel(dir: &Path, config: &Config, args: &[String]) -> ExitCode {
    let id: u64 = args.last().unwrap().parse().unwrap();
    let Some(mut job) = try_load(dir, id).filter(|j| on_its_cluster(j, args)) else {
        eprintln!("scancel: error: Kill job error on job id {id}: Invalid job id specified");
        return ExitCode::from(1);
    };
    if let Some(error) = &config.scancel_error {
        eprintln!("{error}");
        return ExitCode::from(1);
    }
    // As real scancel: the filters given, and those in the environment, restrict what it
    // cancels; with SCANCEL_INTERACTIVE it asks first (and reads no answer here).
    let filter = |name: &str| {
        args.iter()
            .find_map(|a| a.strip_prefix(&format!("--{name}=")))
    };
    let skip = filter("user").is_some_and(|u| u != job.uid.to_string())
        || filter("name").is_some_and(|n| n != job.name)
        || std::env::var("SCANCEL_STATE").is_ok_and(|s| !s.eq_ignore_ascii_case(&job.state))
        || std::env::var_os("SCANCEL_INTERACTIVE").is_some();
    if skip {
        return ExitCode::SUCCESS;
    }
    match job.state.as_str() {
        "PENDING" => {
            job.state = "CANCELLED".to_owned();
            job.exit = Some((0, 0));
            save(dir, &job);
        }
        "RUNNING" => {
            job.state = "COMPLETING".to_owned();
            save(dir, &job);
            if let Some(pgid) = job.pgid {
                kill_group(pgid, rustix::process::Signal::TERM);
            }
            Command::new(me())
                .env(SLURM_ENV, "killer")
                .env(DIR_ENV, dir)
                .arg(id.to_string())
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap();
        }
        _ => {
            eprintln!(
                "scancel: error: Kill job error on job id {id}: Job/step already completing or \
                 completed"
            );
            return ExitCode::from(1);
        }
    }
    ExitCode::SUCCESS
}

fn sacct(dir: &Path, config: &Config, args: &[String]) -> ExitCode {
    if config.no_accounting {
        eprintln!("sacct: error: Slurm accounting storage is disabled");
        return ExitCode::from(1);
    }
    let id: u64 = value(args, "-j").unwrap().parse().unwrap();
    let fields = value(args, "-o").unwrap();
    let name = args.iter().find_map(|a| a.strip_prefix("--name="));
    let user = value(args, "-u");
    let job = try_load(dir, id)
        .filter(|j| on_its_cluster(j, args))
        .filter(|j| name.is_none_or(|n| n == j.name))
        .filter(|j| user.is_none_or(|u| u == j.uid.to_string()));
    if let Some(job) = job {
        let (code, signal) = job.exit.unwrap_or((0, 0));
        let line: Vec<String> = fields
            .split(',')
            .map(|field| match field {
                "JobID" => job.id.to_string(),
                "UID" => job.uid.to_string(),
                "State" if job.state == "CANCELLED" => format!("CANCELLED by {}", job.uid),
                "State" => job.state.clone(),
                "ExitCode" => format!("{code}:{signal}"),
                "JobName" => job.name.clone(),
                other => panic!("sacct field {other}"),
            })
            .collect();
        println!("{}", line.join("|"));
    }
    ExitCode::SUCCESS
}

// ─── Fixtures ──────────────────────────────────────────────────────────────────────────────

/// The fake SLURM of one machine.
struct Sim {
    dir: PathBuf,
}

impl Sim {
    fn set(&self, change: impl FnOnce(&mut Config)) {
        let mut config = load_config(&self.dir);
        change(&mut config);
        write_atomic(
            &self.dir.join("config.json"),
            &serde_json::to_vec(&config).unwrap(),
        );
    }

    fn job(&self, id: u64) -> FakeJob {
        load(&self.dir, id)
    }

    fn save(&self, job: &FakeJob) {
        save(&self.dir, job);
    }

    /// The scheduler starts a pending job.
    fn release(&self, id: u64) {
        let ok = Command::new(me())
            .env(SLURM_ENV, "release")
            .env(DIR_ENV, &self.dir)
            .env(RUN_ENV, run_mark())
            .arg(id.to_string())
            .status()
            .unwrap();
        assert!(ok.success());
    }

    /// The node under a running job fails: everything in it is killed, with no time to clean
    /// up.
    fn fail_node(&self, id: u64) {
        let mut job = self.job(id);
        job.state = "NODE_FAIL".to_owned();
        job.exit = Some((0, 9));
        self.save(&job);
        kill_group(job.pgid.unwrap(), rustix::process::Signal::KILL);
    }

    /// The arguments of each call to `tool`.
    fn calls(&self, tool: &str) -> Vec<Vec<String>> {
        std::fs::read_to_string(self.dir.join("calls.log"))
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str::<(String, Vec<String>)>(l).unwrap())
            .filter(|(t, _)| t == tool)
            .map(|(_, args)| args)
            .collect()
    }
}

/// A machine with the fake SLURM on its `PATH`, whose `sh` is `sh` (by default `/bin/sh`).
fn machine_with(sh: Option<&Path>, config: Config) -> (Machine, Sim) {
    let mut dir = None;
    let mut m = Machine::build(sh.unwrap_or(Path::new("/bin/sh")), |bin| {
        let state = bin.parent().unwrap().join("slurm");
        for sub in ["jobs", "spool"] {
            std::fs::create_dir_all(state.join(sub)).unwrap();
        }
        for tool in TOOLS {
            shim(
                bin,
                tool,
                &format!(
                    "{SLURM_ENV}={tool}; {DIR_ENV}='{}'; export {SLURM_ENV} {DIR_ENV}\n\
                     exec '{}' \"$@\"",
                    state.display(),
                    me().display()
                ),
            );
        }
        dir = Some(state);
    });
    if sh.is_some() {
        m.interpreter = Some(m.bin.join("sh").to_str().unwrap().to_owned());
    }
    let sim = Sim { dir: dir.unwrap() };
    let config = Config {
        interpreter: m.interpreter.clone(),
        ..config
    };
    write_atomic(
        &sim.dir.join("config.json"),
        &serde_json::to_vec(&config).unwrap(),
    );
    (m, sim)
}

fn machine(config: Config) -> (Machine, Sim) {
    machine_with(None, config)
}

fn options() -> LaunchOptions {
    LaunchOptions {
        ready_timeout: Duration::from_secs(15),
        stop_timeout: Duration::from_secs(10),
        lock_wait: Duration::from_secs(5),
        stale_lock: Duration::from_secs(2 * 60),
        ..LaunchOptions::default()
    }
}

/// The script for `site` and `job`, with a short wait in the job.
fn render(target: &Target, site: &Site, job: &JobOptions) -> JobScript {
    JobSpec::new(site, job)
        .unwrap()
        .with_wait(Duration::from_secs(10))
        .unwrap()
        .render(target)
        .unwrap()
}

fn launcher(script: &JobScript) -> SlurmLauncher {
    SlurmLauncher::new(options()).with_script(script.clone())
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

/// Job scripts left in `run/`.
fn job_scripts_left(m: &Machine) -> Vec<String> {
    std::fs::read_dir(m.run_dir())
        .map(|entries| {
            entries
                .map(|e| e.unwrap().file_name().into_string().unwrap())
                .filter(|n| n.starts_with("job."))
                .collect()
        })
        .unwrap_or_default()
}

/// SIGUSR1 and SIGUSR2 as bits of `/proc/<pid>/status`'s signal masks (signal n is bit n - 1).
const USR_SIGNALS: u64 = (1 << 9) | (1 << 11);

/// The signals `pid` ignores, from `/proc/<pid>/status` (`SigIgn`).
fn ignored_signals(pid: u32) -> u64 {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).unwrap();
    let mask = status
        .lines()
        .find_map(|l| l.strip_prefix("SigIgn:"))
        .unwrap()
        .trim();
    u64::from_str_radix(mask, 16).unwrap()
}

/// Sends SIGUSR1 and SIGUSR2 to `pid`, as `sbatch --signal=B:…` would to the batch shell, and
/// gives it a moment to act on them.
fn send_usr_signals(pid: u32) {
    let pid = rustix::process::Pid::from_raw(i32::try_from(pid).unwrap()).unwrap();
    for signal in [rustix::process::Signal::USR1, rustix::process::Signal::USR2] {
        rustix::process::kill_process(pid, signal).unwrap();
    }
    std::thread::sleep(Duration::from_millis(500));
}

/// A site whose `module` command comes from a set-up script in the machine's directory, which
/// logs what it loads to `loaded.log` and fails for `broken/…`. The script also changes `IFS`
/// and the shell's traps, as a careless one might.
fn module_site(m: &Machine, socket: SocketPlace) -> Site {
    let init = m.dir.path().join("modules-init.sh");
    let log = m.dir.path().join("loaded.log");
    std::fs::write(
        &init,
        format!(
            "module() {{\n\
             \x20 case $2 in broken/*) echo \"module: $2 not found\" >&2; return 1 ;; esac\n\
             \x20 printf '%s\\n' \"$*\" >> '{}'\n\
             }}\n\
             IFS=:\n\
             trap - EXIT HUP INT TERM\n\
             trap '' USR1 USR2\n",
            log.display()
        ),
    )
    .unwrap();
    Site {
        name: "modules".to_owned(),
        modules_init: Some(init.to_str().unwrap().to_owned()),
        modules: vec!["example-toolchain/1.0".to_owned(), "nodejs/22".to_owned()],
        socket,
        ..Site::default()
    }
}

// ─── Cases ─────────────────────────────────────────────────────────────────────────────────

/// Submit, pending with a reason, running on a node with the time left, then stop.
fn slurm_submit_pending_running_stop() {
    let (mut m, sim) = machine(Config {
        start: false,
        ..Config::default()
    });
    // Variables a user's profile may set: they would override the script's directives, hide
    // the pending job from squeue -j, or make scancel ask, or skip the running job.
    for (name, value) in [
        ("SBATCH_PARTITION", "debug"),
        ("SBATCH_JOB_NAME", "impostor"),
        ("SBATCH_EXPORT", "NONE"),
        ("SQUEUE_STATES", "RUNNING"),
        ("SCANCEL_STATE", "PENDING"),
        ("SCANCEL_INTERACTIVE", "1"),
        ("SACCT_FORMAT", "JobName"),
    ] {
        m.env.push((name.to_owned(), value.to_owned()));
    }
    let target = m.plain();
    block_on(deploy(&target, &helper("1.0.0"), &quick())).unwrap();
    let two_hours = Duration::from_secs(2 * 3600);
    let job = JobOptions {
        partition: Some("gpu".to_owned()),
        account: Some("proj0001".to_owned()),
        time: Some(two_hours),
        ..JobOptions::default()
    };
    let script = render(&target, &slurm::generic(), &job);
    let launcher = launcher(&script);

    // Submitted, and pending for a reason, with the whole limit left.
    let submitted = block_on(launcher.submit(&target)).unwrap();
    assert!(submitted.submitted_now);
    let id = submitted.job;
    let status = &submitted.status;
    assert_eq!(
        status.state,
        JobState::Pending {
            reason: "Priority".to_owned()
        }
    );
    assert_eq!(status.time_left, Some(slurm::WallTime::Limited(two_hours)));
    assert_eq!(status.time_limit, Some(slurm::WallTime::Limited(two_hours)));
    assert_eq!(status.job_name.as_deref(), Some(script.job_name()));
    assert_eq!(status.installed.as_deref(), Some("1.0.0"));
    assert_eq!(status.endpoint, None);

    // sbatch got the script shown, byte for byte, and the name, directory and output on its
    // command line too; no SBATCH_* variable could override them.
    let fake = sim.job(id);
    assert_eq!(read(&fake.script), script.text());
    assert_eq!(fake.name, script.job_name());
    assert_eq!(fake.chdir, m.root());
    assert_eq!(
        fake.output,
        format!("{}/run/slurm-%j.out", m.root().display())
    );
    for directive in ["--partition=gpu", "--account=proj0001", "--time=02:00:00"] {
        assert!(
            fake.directives.contains(&directive.to_owned()),
            "{directive}: {:?}",
            fake.directives
        );
    }
    for family in ["SBATCH_", "SQUEUE_", "SCANCEL_", "SACCT_"] {
        assert!(
            fake.env.iter().all(|(k, _)| !k.starts_with(family)),
            "{family}"
        );
    }
    // sim.job(id) found the job: the id came from sbatch's stdout, not the number it printed
    // on stderr after it. No cluster was named.
    assert_eq!(status.cluster, None);
    // Its record, private like everything else; no copy of the script is left.
    let record = read(&m.run_dir().join("slurm.json"));
    assert!(
        record.starts_with(&format!(
            "{{\"job\":{id},\"name\":\"{}\",\"submitted\":",
            script.job_name()
        )),
        "{record}"
    );
    assert_eq!(mode(&m.run_dir().join("slurm.json")), 0o600);
    assert!(job_scripts_left(&m).is_empty());
    assert_private(&m.root());

    // Submitting again finds the same job; status says it waits.
    let again = block_on(launcher.submit(&target)).unwrap();
    assert!(!again.submitted_now);
    assert_eq!(again.job, id);
    let status = block_on(launcher.status(&target)).unwrap();
    assert_eq!(status.state, HelperState::Pending);
    assert!(!status.socket_ready);
    assert!(status.slurm.is_some());
    // Starting waits for it, then says it is still queued; it stays queued.
    let impatient = SlurmLauncher::new(LaunchOptions {
        ready_timeout: Duration::from_secs(1),
        ..options()
    })
    .with_script(script.clone());
    let err = block_on(impatient.start(&target)).unwrap_err();
    assert!(
        matches!(&err, HelperError::Queued { job, state } if *job == id && state.contains("pending (Priority)")),
        "{err:?}"
    );
    assert_eq!(sim.calls("sbatch").len(), 1);

    // It runs: the helper listens on its node.
    sim.release(id);
    let started = block_on(launcher.start(&target)).unwrap();
    assert!(!started.started_now);
    let e = &started.endpoint;
    assert_eq!(e.host, NODE);
    assert_eq!(e.launcher, "slurm");
    assert_eq!(e.job, Some(id));
    assert_eq!(e.version, "1.0.0");
    assert_eq!(e.socket, m.layout().socket());
    assert!(alive(e.pid));
    assert_eq!(comm(e.pid), "pitcrewd");
    let status = block_on(launcher.job_status(&target)).unwrap();
    assert!(status.ready());
    assert_eq!(
        status.state,
        JobState::Running {
            node: NODE.to_owned()
        }
    );
    match status.time_left {
        Some(slurm::WallTime::Limited(left)) => {
            assert!(left <= two_hours && left > two_hours - Duration::from_secs(60));
        }
        other => panic!("{other:?}"),
    }
    let status = block_on(launcher.status(&target)).unwrap();
    assert_eq!(status.state, HelperState::Running);
    assert!(status.socket_ready);
    assert_eq!(status.endpoint.as_ref(), Some(e));
    // Its output, made open but private before anything is written to it; the helper has the
    // user's umask.
    let out = m.run_dir().join(format!("slurm-{id}.out"));
    eventually("the job's output", || {
        read(&out).contains("runs on node017")
    });
    assert_eq!(mode(&out), 0o600);
    let text = read(&out);
    assert!(
        text.contains(&format!("fake pitcrewd umask {}\n", umask_of("self"))),
        "{text}"
    );
    assert_private(&m.root());
    // SIGUSR1 and SIGUSR2 (sbatch --signal) to the batch shell do not end the job.
    send_usr_signals(sim.job(id).pgid.unwrap());
    assert!(alive(sim.job(id).pgid.unwrap()));
    assert!(alive(e.pid));
    assert_eq!(sim.job(id).state, "RUNNING");

    // Stop: scancel; the job leaves the queue; its records go.
    let cancelled = block_on(launcher.cancel(&target)).unwrap();
    assert_eq!(cancelled.job, Some(id));
    assert!(cancelled.cancelled);
    assert_eq!(cancelled.pid, Some(e.pid));
    assert_eq!(
        cancelled.state,
        JobState::Ended {
            state: Some("CANCELLED".to_owned()),
            exit: Some(JobExit {
                code: 0,
                signal: 15
            })
        }
    );
    assert_eq!(
        sim.calls("scancel"),
        [[
            format!("--user={}", fake.uid),
            format!("--name={}", script.job_name()),
            id.to_string()
        ]]
    );
    eventually("the helper to be gone", || !alive(e.pid));
    for gone in ["slurm.json", "endpoint.json", "pitcrewd.sock"] {
        assert!(!m.run_dir().join(gone).exists(), "{gone}");
    }
    // Stopping again does nothing.
    let again = block_on(launcher.cancel(&target)).unwrap();
    assert_eq!(
        again,
        Cancelled {
            job: None,
            cancelled: false,
            pid: None,
            state: JobState::NoJob
        }
    );
    assert_eq!(
        block_on(launcher.stop(&target)).unwrap(),
        Stopped {
            pid: None,
            forced: false
        }
    );
    let status = block_on(launcher.job_status(&target)).unwrap();
    assert_eq!(status.state, JobState::NoJob);
    assert_eq!(sim.calls("scancel").len(), 1);
}

/// Jobs that are refused, fail to start, or die: each status says so plainly.
fn slurm_failures_are_clear() {
    let (m, sim) = machine(Config::default());
    let target = m.plain();
    let script = render(&target, &slurm::generic(), &JobOptions::default());
    let launcher = launcher(&script);

    // Nothing deployed: refused before sbatch.
    let err = block_on(launcher.submit(&target)).unwrap_err();
    assert!(matches!(err, HelperError::NotDeployed(_)), "{err:?}");
    assert!(sim.calls("sbatch").is_empty());

    // sbatch refuses the job: its message; nothing recorded or left behind.
    block_on(deploy(&target, &helper("1.0.0"), &quick())).unwrap();
    sim.set(|c| {
        c.sbatch_error = Some(
            "sbatch: error: Batch job submission failed: Invalid account or account/partition \
             combination specified"
                .to_owned(),
        );
    });
    let err = block_on(launcher.submit(&target)).unwrap_err();
    assert!(
        matches!(&err, HelperError::SubmitFailed(d) if d.contains("Invalid account")),
        "{err:?}"
    );
    assert!(!m.run_dir().join("slurm.json").exists());
    assert!(job_scripts_left(&m).is_empty());
    sim.set(|c| c.sbatch_error = None);

    // The helper exits at once: the job fails, and its output says why.
    let failing = helper_from("2.0.0", helper_script("2.0.0", "exit", 0));
    block_on(deploy(&target, &failing, &quick())).unwrap();
    let err = block_on(launcher.start(&target)).unwrap_err();
    match &err {
        HelperError::StartFailed(d) => {
            assert!(d.contains("ended (FAILED, exit 1:0)"), "{d}");
            assert!(d.contains("the helper exited at once"), "{d}");
        }
        other => panic!("{other:?}"),
    }
    let status = block_on(launcher.job_status(&target)).unwrap();
    assert!(
        matches!(&status.state, JobState::Ended { state: Some(s), .. } if s == "FAILED"),
        "{status:?}"
    );
    assert!(
        status.output.as_deref().unwrap().contains("fake failure"),
        "{status:?}"
    );
    assert_eq!(
        block_on(launcher.status(&target)).unwrap().state,
        HelperState::NotRunning
    );

    // A working helper again: starting submits a new job (the failed one is forgotten).
    block_on(deploy(&target, &helper("3.0.0"), &quick())).unwrap();
    let started = block_on(launcher.start(&target)).unwrap();
    assert!(started.started_now);
    let id = started.endpoint.job.unwrap();
    assert_ne!(Some(id), status.job);

    // The node fails under it: nothing could clean up, but the job is plainly over.
    sim.fail_node(id);
    eventually("the helper to be gone", || !alive(started.endpoint.pid));
    let status = block_on(launcher.job_status(&target)).unwrap();
    assert_eq!(
        status.state,
        JobState::Ended {
            state: Some("NODE_FAIL".to_owned()),
            exit: Some(JobExit { code: 0, signal: 9 })
        }
    );
    assert!(!status.ready());
    assert_eq!(
        block_on(launcher.status(&target)).unwrap().state,
        HelperState::NotRunning
    );
    // Without accounting, how it ended is unknown; that it ended is not.
    sim.set(|c| c.no_accounting = true);
    let status = block_on(launcher.job_status(&target)).unwrap();
    assert_eq!(
        status.state,
        JobState::Ended {
            state: None,
            exit: None
        }
    );
    // While squeue still lists it, the state comes from there.
    sim.set(|c| c.keep_ended = true);
    let status = block_on(launcher.job_status(&target)).unwrap();
    assert_eq!(
        status.state,
        JobState::Ended {
            state: Some("NODE_FAIL".to_owned()),
            exit: None
        }
    );
    sim.set(|c| {
        c.keep_ended = false;
        c.no_accounting = false;
    });
    // Starting again submits a new job; the dead one's endpoint goes.
    let again = block_on(launcher.start(&target)).unwrap();
    assert!(again.started_now);
    assert_ne!(again.endpoint.job, Some(id));

    // squeue fails (the controller is down): nothing is concluded, and nothing changes.
    sim.set(|c| {
        c.squeue_error = Some(
            "slurm_load_jobs error: Unable to contact slurm controller (connect failure)"
                .to_owned(),
        );
    });
    let sbatch_calls = sim.calls("sbatch").len();
    for err in [
        block_on(launcher.job_status(&target)).unwrap_err(),
        block_on(launcher.submit(&target)).unwrap_err(),
        block_on(launcher.cancel(&target)).unwrap_err(),
    ] {
        assert!(
            matches!(&err, HelperError::Slurm(d) if d.contains("Unable to contact")),
            "{err:?}"
        );
    }
    assert_eq!(sim.calls("sbatch").len(), sbatch_calls);
    assert!(sim.calls("scancel").is_empty());
    assert!(m.run_dir().join("slurm.json").exists());
    assert!(alive(again.endpoint.pid));
    sim.set(|c| c.squeue_error = None);

    // scancel fails and the job stays: stop says why, and keeps the record.
    sim.set(|c| {
        c.scancel_error =
            Some("scancel: error: Kill job error on job id 1: Access/permission denied".to_owned());
    });
    let hasty = SlurmLauncher::new(LaunchOptions {
        stop_timeout: Duration::from_secs(1),
        ..options()
    });
    let err = block_on(hasty.cancel(&target)).unwrap_err();
    assert!(
        matches!(&err, HelperError::Slurm(d) if d.contains("Access/permission denied")),
        "{err:?}"
    );
    assert!(m.run_dir().join("slurm.json").exists());
    assert!(alive(again.endpoint.pid));
    sim.set(|c| c.scancel_error = None);
    assert!(block_on(launcher.cancel(&target)).unwrap().cancelled);

    // A job that does not leave the queue in time: stop says so, and keeps the record, so
    // stopping again finishes the job.
    let stubborn = helper_from("4.0.0", helper_script("4.0.0", "stubborn", 0));
    block_on(deploy(&target, &stubborn, &quick())).unwrap();
    sim.set(|c| c.kill_wait = 4);
    let started = block_on(launcher.start(&target)).unwrap();
    let err = block_on(hasty.cancel(&target)).unwrap_err();
    assert!(
        matches!(&err, HelperError::StopFailed(d) if d.contains("still COMPLETING")),
        "{err:?}"
    );
    assert!(m.run_dir().join("slurm.json").exists());
    let done = block_on(launcher.cancel(&target)).unwrap();
    assert!(matches!(&done.state, JobState::Ended { state: Some(s), .. } if s == "CANCELLED"));
    eventually("the stubborn helper to be gone", || {
        !alive(started.endpoint.pid)
    });
    assert!(!m.run_dir().join("slurm.json").exists());
}

/// The job checks where it runs: its record, and the root.
fn slurm_jobs_check_where_they_run() {
    let (m, sim) = machine(Config {
        start: false,
        ..Config::default()
    });
    let target = m.plain();
    block_on(deploy(&target, &helper("1.0.0"), &quick())).unwrap();
    let script = JobSpec::new(&slurm::generic(), &JobOptions::default())
        .unwrap()
        .with_wait(Duration::from_secs(3))
        .unwrap()
        .render(&target)
        .unwrap();
    let launcher = launcher(&script);
    let record = m.run_dir().join("slurm.json");
    let ended = |id: u64, why: &str| {
        eventually("the job to fail", || sim.job(id).state == "FAILED");
        let out = read(&m.run_dir().join(format!("slurm-{id}.out")));
        assert!(out.contains(why), "{out}");
        assert!(!m.run_dir().join("endpoint.json").exists());
    };

    // PitCrew's record names another job (it was submitted again meanwhile, say): this one
    // ends without starting the helper.
    let id = block_on(launcher.submit(&target)).unwrap().job;
    let text = read(&record).replace(&format!("{{\"job\":{id},"), "{\"job\":9999,");
    std::fs::write(&record, text).unwrap();
    sim.release(id);
    ended(id, "run/slurm.json records another job, not this one");

    // No record at all (the submission was cut off after sbatch): it ends on its own.
    let id = block_on(launcher.submit(&target)).unwrap().job;
    std::fs::remove_file(&record).unwrap();
    sim.release(id);
    ended(id, "does not record this job");

    // The root is open to the group when the job starts: refused, never repaired.
    let id = block_on(launcher.submit(&target)).unwrap().job;
    std::fs::set_permissions(m.root(), std::fs::Permissions::from_mode(0o750)).unwrap();
    sim.release(id);
    ended(id, "must be owned by uid");
    assert_eq!(mode(&m.root()), 0o750);
    std::fs::set_permissions(m.root(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let status = block_on(launcher.job_status(&target)).unwrap();
    assert!(
        matches!(&status.state, JobState::Ended { state: Some(s), .. } if s == "FAILED"),
        "{status:?}"
    );
    assert!(
        status.describe().contains("unsafe_dir"),
        "{}",
        status.describe()
    );
    block_on(launcher.cancel(&target)).unwrap();
}

/// A job id that names someone else's job is never cancelled.
fn slurm_never_touches_other_jobs() {
    let (m, sim) = machine(Config {
        start: false,
        ..Config::default()
    });
    let target = m.plain();
    block_on(deploy(&target, &helper("1.0.0"), &quick())).unwrap();
    let launcher = launcher(&render(&target, &slurm::generic(), &JobOptions::default()));

    // The recorded id now names another user's running job (the cluster lost its state and
    // numbered jobs again, say), with the same name: the name is easy to guess, the uid is
    // what tells them apart.
    let id = block_on(launcher.submit(&target)).unwrap().job;
    let name = sim.job(id).name;
    let mut theirs = sim.job(id);
    theirs.uid += 1;
    theirs.state = "RUNNING".to_owned();
    theirs.node = "node018".to_owned();
    sim.save(&theirs);
    let status = block_on(launcher.job_status(&target)).unwrap();
    let not_ours = JobState::NotOurs {
        uid: theirs.uid,
        name: name.clone(),
    };
    assert_eq!(status.state, not_ours);
    assert_eq!(
        block_on(launcher.status(&target)).unwrap().state,
        HelperState::NotRunning
    );
    // Stop leaves it alone, and only forgets PitCrew's own record.
    let cancelled = block_on(launcher.cancel(&target)).unwrap();
    assert_eq!(
        cancelled,
        Cancelled {
            job: Some(id),
            cancelled: false,
            pid: None,
            state: not_ours
        }
    );
    assert!(sim.calls("scancel").is_empty());
    assert_eq!(sim.job(id).state, "RUNNING");
    assert!(!m.run_dir().join("slurm.json").exists());

    // Gone from the queue, with sacct's only record of the id another user's: it does not say
    // how PitCrew's job ended.
    let id = block_on(launcher.submit(&target)).unwrap().job;
    let mut theirs = sim.job(id);
    theirs.uid += 1;
    theirs.state = "COMPLETED".to_owned();
    theirs.exit = Some((0, 0));
    sim.save(&theirs);
    let status = block_on(launcher.job_status(&target)).unwrap();
    assert_eq!(
        status.state,
        JobState::Ended {
            state: None,
            exit: None
        }
    );
    block_on(launcher.cancel(&target)).unwrap();

    // Another user's job, under another name too.
    let id = block_on(launcher.submit(&target)).unwrap().job;
    let mut theirs = sim.job(id);
    theirs.uid += 1;
    theirs.name = "someone-elses-job".to_owned();
    sim.save(&theirs);
    let cancelled = block_on(launcher.cancel(&target)).unwrap();
    assert!(
        matches!(&cancelled.state, JobState::NotOurs { name, .. } if name == "someone-elses-job")
    );
    assert!(!cancelled.cancelled);
    assert!(sim.calls("scancel").is_empty());

    // Another user's job whose name carries a line that looks like PitCrew's own job: squeue
    // then lists the id twice, and nothing is concluded or done.
    let id = block_on(launcher.submit(&target)).unwrap().job;
    let mut forged = sim.job(id);
    forged.uid += 1;
    forged.state = "RUNNING".to_owned();
    forged.name = format!(
        "x\n{id}|{}|RUNNING|None|1:00:00|1:00:00|node017|{name}",
        forged.uid - 1
    );
    sim.save(&forged);
    for err in [
        block_on(launcher.job_status(&target)).unwrap_err(),
        block_on(launcher.cancel(&target)).unwrap_err(),
    ] {
        assert!(
            matches!(&err, HelperError::Slurm(d) if d.contains("more than once")),
            "{err:?}"
        );
    }
    assert!(sim.calls("scancel").is_empty());
    assert!(m.run_dir().join("slurm.json").exists());
    std::fs::remove_file(m.run_dir().join("slurm.json")).unwrap();

    // The user's own job under that id, but with another name, is not ours either; starting
    // submits a new job and leaves it be.
    let id = block_on(launcher.submit(&target)).unwrap().job;
    let mut mine = sim.job(id);
    mine.name = "my-other-job".to_owned();
    sim.save(&mine);
    assert!(matches!(
        block_on(launcher.job_status(&target)).unwrap().state,
        JobState::NotOurs { uid, .. } if uid == mine.uid
    ));
    let fresh = block_on(launcher.submit(&target)).unwrap();
    assert!(fresh.submitted_now);
    assert_ne!(fresh.job, id);
    assert_eq!(sim.job(id).state, "PENDING");
    // Only the new job, PitCrew's own, is cancelled, scancel told its name and owner too.
    assert!(block_on(launcher.cancel(&target)).unwrap().cancelled);
    assert_eq!(
        sim.calls("scancel"),
        [[
            format!("--user={}", mine.uid),
            format!("--name={name}"),
            fresh.job.to_string()
        ]]
    );
    assert_eq!(sim.job(id).state, "PENDING");
}

/// A recipe from the user's sites directory: its extra lines reach sbatch, and its modules load
/// in the job, safely quoted. An unknown key in another file is refused.
fn slurm_recipes_add_lines_and_modules() {
    let (m, sim) = machine(Config::default());
    let target = m.plain();
    block_on(deploy(&target, &helper("1.0.0"), &quick())).unwrap();
    let base = module_site(&m, SocketPlace::Root);
    let sites = m.dir.path().join("sites");
    private_dir(&sites);
    std::fs::write(
        sites.join("example-cluster.toml"),
        format!(
            "partition = \"gpu\"\n\
             sbatch = [\"--constraint=a100&ib\", \"--nodes=1\"]\n\
             modules_init = \"{}\"\n\
             modules = [\"example-toolchain/1.0\", \"nodejs/22\"]\n",
            base.modules_init.as_deref().unwrap()
        ),
    )
    .unwrap();
    std::fs::write(sites.join("typo.toml"), "modulse = [\"x\"]\n").unwrap();
    let found = slurm::load_sites(&sites);
    assert_eq!(found.len(), 2);
    let err = found[1].as_ref().unwrap_err();
    assert!(err.why.contains("unknown key \"modulse\""), "{err}");
    let site = found[0].as_ref().unwrap();
    assert_eq!(site.name, "example-cluster");

    let script = render(&target, site, &JobOptions::default());
    let text = script.text();
    assert!(
        text.contains(
            "\n#SBATCH --partition=gpu\n#SBATCH --constraint=a100&ib\n#SBATCH --nodes=1\n"
        ),
        "{text}"
    );
    assert!(
        text.contains("\npc_modules='example-toolchain/1.0 nodejs/22'\n"),
        "{text}"
    );
    let started = block_on(launcher(&script).start(&target)).unwrap();
    let id = started.endpoint.job.unwrap();
    assert_eq!(
        read(&m.dir.path().join("loaded.log")),
        "load example-toolchain/1.0\nload nodejs/22\n"
    );
    let directives = sim.job(id).directives;
    assert!(
        directives.contains(&"--constraint=a100&ib".to_owned()),
        "{directives:?}"
    );
    assert!(
        directives.contains(&"--nodes=1".to_owned()),
        "{directives:?}"
    );
    block_on(launcher(&script).cancel(&target)).unwrap();

    // A module that does not load: the job ends, and says which.
    let broken = Site {
        modules: vec!["broken/1.0".to_owned()],
        ..site.clone()
    };
    let script = render(&target, &broken, &JobOptions::default());
    let err = block_on(launcher(&script).start(&target)).unwrap_err();
    assert!(
        matches!(&err, HelperError::StartFailed(d) if d.contains("module load broken/1.0 failed")),
        "{err:?}"
    );
    // No module command at all.
    let bare = Site {
        modules_init: None,
        ..site.clone()
    };
    let script = render(&target, &bare, &JobOptions::default());
    let err = block_on(launcher(&script).start(&target)).unwrap_err();
    assert!(
        matches!(&err, HelperError::StartFailed(d) if d.contains("there is no module command")),
        "{err:?}"
    );
    // A set-up script that moves elsewhere: the job's relative paths would no longer be the
    // root's, so it stops.
    let wandering = m.dir.path().join("wandering-init.sh");
    std::fs::write(
        &wandering,
        format!(". '{}'\ncd /tmp\n", site.modules_init.as_deref().unwrap()),
    )
    .unwrap();
    let moved = Site {
        modules_init: Some(wandering.to_str().unwrap().to_owned()),
        ..site.clone()
    };
    let script = render(&target, &moved, &JobOptions::default());
    let err = block_on(launcher(&script).start(&target)).unwrap_err();
    assert!(
        matches!(&err, HelperError::StartFailed(d) if d.contains("changed the working directory")),
        "{err:?}"
    );

    // The set-up script is checked as the way to the root is: one others can change is never
    // sourced, nor one in a directory they can write; a link of the user's is followed.
    let good = site.modules_init.clone().unwrap();
    let writable = m.dir.path().join("writable-init.sh");
    std::fs::copy(&good, &writable).unwrap();
    std::fs::set_permissions(&writable, std::fs::Permissions::from_mode(0o664)).unwrap();
    let open = m.dir.path().join("open");
    std::fs::create_dir(&open).unwrap();
    std::fs::set_permissions(&open, std::fs::Permissions::from_mode(0o777)).unwrap();
    let in_open = open.join("init.sh");
    std::fs::copy(&good, &in_open).unwrap();
    std::fs::set_permissions(&in_open, std::fs::Permissions::from_mode(0o644)).unwrap();
    for (init, why) in [
        (&writable, "is writable by others (-rw-rw-r--"),
        (&in_open, "on the way to"),
    ] {
        let unsafe_init = Site {
            modules_init: Some(init.to_str().unwrap().to_owned()),
            ..site.clone()
        };
        let script = render(&target, &unsafe_init, &JobOptions::default());
        let err = block_on(launcher(&script).start(&target)).unwrap_err();
        assert!(
            matches!(&err, HelperError::StartFailed(d) if d.contains("unsafe_") && d.contains(why)),
            "{}: {err:?}",
            init.display()
        );
    }
    std::fs::remove_file(m.dir.path().join("loaded.log")).unwrap();
    let link = m.dir.path().join("link-init.sh");
    std::os::unix::fs::symlink(&good, &link).unwrap();
    let linked = Site {
        modules_init: Some(link.to_str().unwrap().to_owned()),
        ..site.clone()
    };
    let script = render(&target, &linked, &JobOptions::default());
    block_on(launcher(&script).start(&target)).unwrap();
    assert_eq!(
        read(&m.dir.path().join("loaded.log")),
        "load example-toolchain/1.0\nload nodejs/22\n"
    );
    block_on(launcher(&script).cancel(&target)).unwrap();
}

/// The socket on the node's own disk: `$TMPDIR`, else `/tmp`.
fn slurm_socket_on_node_local_tmpdir() {
    let (m, sim) = machine(Config::default());
    let target = m.plain();
    block_on(deploy(&target, &helper("1.0.0"), &quick())).unwrap();
    let site = Site {
        name: "node-local".to_owned(),
        socket: SocketPlace::NodeLocal,
        ..Site::default()
    };
    let script = render(&target, &site, &JobOptions::default());
    assert_eq!(script.socket(), SocketPlace::NodeLocal);
    let launcher = launcher(&script);

    let tmp = m.dir.path().join("node-tmp");
    private_dir(&tmp);
    let odd = m.dir.path().join("odd tmp");
    private_dir(&odd);
    for (tmpdir, under) in [(&tmp, tmp.clone()), (&odd, PathBuf::from("/tmp"))] {
        sim.set(|c| c.tmpdir = Some(tmpdir.clone()));
        let started = block_on(launcher.start(&target)).unwrap();
        let id = started.endpoint.job.unwrap();
        let socket = PathBuf::from(&started.endpoint.socket);
        let dir = socket.parent().unwrap().to_path_buf();
        assert_eq!(dir.parent().unwrap(), under);
        assert!(
            dir.file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .starts_with(&format!("pitcrew-{id}.")),
            "{}",
            dir.display()
        );
        assert_eq!(socket.file_name().unwrap(), "pitcrewd.sock");
        assert!(
            std::fs::symlink_metadata(&socket)
                .unwrap()
                .file_type()
                .is_socket()
        );
        assert_eq!(mode(&dir), 0o700);
        assert!(!m.run_dir().join("pitcrewd.sock").exists());
        let status = block_on(launcher.job_status(&target)).unwrap();
        assert!(status.ready());
        block_on(launcher.cancel(&target)).unwrap();
        // The job removes what it made on the node.
        eventually("the socket's directory to go", || !dir.exists());
    }

    // A $TMPDIR others can write to (not sticky): refused, and said so about the right path;
    // nothing is made or removed there.
    let open = m.dir.path().join("open-tmp");
    std::fs::create_dir(&open).unwrap();
    std::fs::set_permissions(&open, std::fs::Permissions::from_mode(0o777)).unwrap();
    sim.set(|c| c.tmpdir = Some(open.clone()));
    let err = block_on(launcher.start(&target)).unwrap_err();
    let want = format!("on the way to {}/pitcrew-", open.display());
    assert!(
        matches!(&err, HelperError::StartFailed(d) if d.contains(&want) && d.contains("writable by others")),
        "{err:?}"
    );
    assert_eq!(std::fs::read_dir(&open).unwrap().count(), 0);
    block_on(launcher.cancel(&target)).unwrap();
}

/// A start-up file that eats stdin, or a connection that drops or garbles the job script: the
/// script is used whole or not at all.
fn slurm_scripts_arrive_whole_or_not_at_all() {
    let (m, sim) = machine(Config::default());
    block_on(deploy(&m.plain(), &helper("1.0.0"), &quick())).unwrap();
    let script = render(&m.plain(), &slurm::generic(), &JobOptions::default());
    let submit = |remote: Remote| {
        let target = m.target(&m.fake(remote));
        block_on(launcher(&script).submit(&target))
    };
    for eat in [1, 100] {
        let err = submit(Remote {
            eat: Some(eat),
            eat_on: Some("slurm-submit".to_owned()),
            ..Remote::default()
        })
        .unwrap_err();
        assert!(
            matches!(&err, HelperError::UnexpectedOutput(d) if d.contains("did not arrive whole")),
            "{eat}: {err:?}"
        );
    }
    // Cut off in the middle of the job script.
    let err = submit(Remote {
        cut_after: Some(script_len() + 200),
        ..Remote::default()
    })
    .unwrap_err();
    assert!(
        matches!(err, HelperError::Ssh(SshError::Ssh { code: 255, .. })),
        "{err:?}"
    );
    eventually("the remote script to clean up", || {
        job_scripts_left(&m).is_empty() && !m.run_dir().join(".lock").exists()
    });
    // The same, but the remote script is killed while it copies the job script (the fake waits
    // there first, so the remote side gets that far): its copy and its lock stay behind, until
    // the next submit breaks the dead run's lock and sweeps them.
    let err = submit(Remote {
        pause_after: Some(script_len() + 200),
        pause_ms: 1500,
        cut_after: Some(script_len() + 200),
        kill: true,
        ..Remote::default()
    })
    .unwrap_err();
    assert!(
        matches!(err, HelperError::Ssh(SshError::Ssh { code: 255, .. })),
        "{err:?}"
    );
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(job_scripts_left(&m).len(), 1);
    // A byte changed on the way, where neither the length nor the markers show it.
    let middle = script_len() + u64::try_from(script.text().len() / 2).unwrap();
    let err = submit(Remote {
        corrupt_at: Some(middle),
        ..Remote::default()
    })
    .unwrap_err();
    assert!(
        matches!(&err, HelperError::UnexpectedOutput(d) if d.contains("did not arrive as sent")),
        "{err:?}"
    );
    assert!(sim.calls("sbatch").is_empty());
    assert!(!m.run_dir().join("slurm.json").exists());
    assert!(job_scripts_left(&m).is_empty());
    // Whole, it goes.
    let submitted = submit(Remote::default()).unwrap();
    assert!(submitted.submitted_now);
    block_on(launcher(&script).cancel(&m.plain())).unwrap();
}

/// The probe finds the SLURM tools, their versions, `srun --overlap` and the default partition.
fn slurm_probe_finds_the_tools() {
    let (m, _sim) = machine(Config::default());
    let probe = block_on(m.fake(Remote::default()).ssh.probe("hpc-login")).unwrap();
    let tools = &probe.slurm;
    for tool in [
        &tools.sbatch,
        &tools.squeue,
        &tools.scancel,
        &tools.sacct,
        &tools.srun,
    ] {
        assert_eq!(tool.as_deref(), Some(VERSION));
    }
    assert!(tools.srun_overlap);
    assert_eq!(tools.default_partition.as_deref(), Some("batch"));
    assert_eq!(probe.info.scheduler, Some(Scheduler::Slurm));
    slurm::check_tools(tools, LastHop::SrunOverlap).unwrap();

    // An older SLURM without accounting tools or --overlap, and no default partition.
    let (m, _sim) = machine(Config {
        overlap: false,
        partitions: vec!["gpu".to_owned()],
        ..Config::default()
    });
    std::fs::remove_file(m.bin.join("sacct")).unwrap();
    let probe = block_on(m.fake(Remote::default()).ssh.probe("hpc-login")).unwrap();
    let tools = &probe.slurm;
    assert_eq!(tools.sacct, None);
    assert_eq!(tools.sbatch.as_deref(), Some(VERSION));
    assert!(!tools.srun_overlap);
    assert_eq!(tools.default_partition, None);
    slurm::check_tools(tools, LastHop::Ssh).unwrap();
    assert!(slurm::check_tools(tools, LastHop::SrunOverlap).is_err());
}

/// The other launchers share the root's `run/` and its socket: a SLURM job never starts over
/// one of their helpers, and never touches its socket or records.
fn slurm_never_overlaps_another_launcher() {
    let (m, sim) = machine(Config {
        start: false,
        ..Config::default()
    });
    let target = m.plain();
    block_on(deploy(&target, &helper("1.0.0"), &quick())).unwrap();
    let launcher = launcher(&render(&target, &slurm::generic(), &JobOptions::default()));
    let direct = DirectLauncher::new(crate::unix::launch_options());
    let socket = PathBuf::from(m.layout().socket());
    let endpoint = m.run_dir().join("endpoint.json");

    // The direct launcher's helper runs here: no job is submitted.
    let running = block_on(direct.start(&target)).unwrap().endpoint;
    let record = read(&endpoint);
    let err = block_on(launcher.submit(&target)).unwrap_err();
    assert!(
        matches!(&err, HelperError::InUse(d) if d.contains("direct launcher's helper runs here")),
        "{err:?}"
    );
    assert!(sim.calls("sbatch").is_empty());
    assert!(alive(running.pid));
    assert!(socket.exists());
    assert_eq!(read(&endpoint), record);
    block_on(direct.stop(&target)).unwrap();

    // Recorded on another host sharing the home: not either.
    let elsewhere = Endpoint {
        host: "hpc-login2".to_owned(),
        ..running.clone()
    };
    std::fs::write(
        &endpoint,
        format!("{}\n", serde_json::to_string(&elsewhere).unwrap()),
    )
    .unwrap();
    let err = block_on(launcher.submit(&target)).unwrap_err();
    assert!(
        matches!(&err, HelperError::OtherHost { host, launcher } if host == "hpc-login2" && launcher == "direct"),
        "{err:?}"
    );
    assert!(sim.calls("sbatch").is_empty());

    // A record of one that is gone is left over: the job is submitted, and the record goes.
    let gone = Endpoint {
        pid: crate::unix::dead_pid(),
        host: running.host.clone(),
        ..running.clone()
    };
    std::fs::write(
        &endpoint,
        format!("{}\n", serde_json::to_string(&gone).unwrap()),
    )
    .unwrap();
    let id = block_on(launcher.submit(&target)).unwrap().job;
    assert!(!endpoint.exists());
    assert_eq!(sim.job(id).state, "PENDING");

    // While the job waits in the queue, the direct launcher neither starts a helper nor
    // removes anything, with take-over or without; nor while squeue cannot say.
    let taking = DirectLauncher::new(LaunchOptions {
        take_over: true,
        ..crate::unix::launch_options()
    });
    let in_use = |err: HelperError, what: &str| {
        assert!(
            matches!(&err, HelperError::InUse(d) if d.contains(what)),
            "{what}: {err:?}"
        );
    };
    in_use(block_on(direct.start(&target)).unwrap_err(), "(PENDING)");
    in_use(block_on(taking.start(&target)).unwrap_err(), "(PENDING)");
    in_use(block_on(direct.stop(&target)).unwrap_err(), "(PENDING)");
    in_use(block_on(taking.stop(&target)).unwrap_err(), "(PENDING)");
    sim.set(|c| {
        c.squeue_error =
            Some("slurm_load_jobs error: Unable to contact slurm controller".to_owned());
    });
    in_use(
        block_on(direct.start(&target)).unwrap_err(),
        "Unable to contact",
    );
    sim.set(|c| c.squeue_error = None);
    let off = m.bin.join("squeue.off");
    std::fs::rename(m.bin.join("squeue"), &off).unwrap();
    in_use(
        block_on(taking.stop(&target)).unwrap_err(),
        "squeue is not on the PATH",
    );
    std::fs::rename(&off, m.bin.join("squeue")).unwrap();
    assert!(!endpoint.exists());
    assert!(!socket.exists());
    block_on(launcher.cancel(&target)).unwrap();

    // While a job starts (its modules take a few seconds to set up), before it records its
    // endpoint: the same.
    let init = m.dir.path().join("slow-init.sh");
    std::fs::write(&init, "sleep 3\nmodule() { :; }\n").unwrap();
    let slow = Site {
        name: "slow".to_owned(),
        modules_init: Some(init.to_str().unwrap().to_owned()),
        modules: vec!["example-toolchain/1.0".to_owned()],
        ..Site::default()
    };
    let slow =
        SlurmLauncher::new(options()).with_script(render(&target, &slow, &JobOptions::default()));
    let id = block_on(slow.submit(&target)).unwrap().job;
    sim.release(id);
    eventually("the job to run", || sim.job(id).state == "RUNNING");
    assert!(!endpoint.exists());
    in_use(block_on(direct.start(&target)).unwrap_err(), "(RUNNING)");
    in_use(block_on(taking.stop(&target)).unwrap_err(), "(RUNNING)");
    // Once it has started: its endpoint is on the node (another host), and take-over is
    // refused too. Its socket and record are left alone.
    let started = block_on(slow.start(&target)).unwrap().endpoint;
    let err = block_on(direct.start(&target)).unwrap_err();
    assert!(
        matches!(&err, HelperError::OtherHost { host, launcher } if host == NODE && launcher == "slurm"),
        "{err:?}"
    );
    in_use(block_on(taking.start(&target)).unwrap_err(), "(RUNNING)");
    in_use(block_on(taking.stop(&target)).unwrap_err(), "(RUNNING)");
    assert!(block_on(slow.job_status(&target)).unwrap().ready());
    assert!(socket.exists());
    assert!(alive(started.pid));
    block_on(slow.cancel(&target)).unwrap();

    // The job's own check, for when squeue was wrong: here the queue says the job has ended
    // while the direct helper starts, then it runs after all. It finds that helper recorded and
    // ends without touching it, its socket or its record.
    let id = block_on(launcher.submit(&target)).unwrap().job;
    let mut job = sim.job(id);
    job.state = "COMPLETED".to_owned();
    sim.save(&job);
    let running = block_on(direct.start(&target)).unwrap().endpoint;
    job.state = "PENDING".to_owned();
    sim.save(&job);
    let record = read(&endpoint);
    sim.release(id);
    eventually("the job to fail", || sim.job(id).state == "FAILED");
    let out = read(&m.run_dir().join(format!("slurm-{id}.out")));
    assert!(
        out.contains("records a helper another launcher started"),
        "{out}"
    );
    assert!(alive(running.pid));
    assert!(
        std::fs::symlink_metadata(&socket)
            .unwrap()
            .file_type()
            .is_socket()
    );
    assert_eq!(read(&endpoint), record);
    let stopped = block_on(direct.stop(&target)).unwrap();
    assert_eq!(stopped.pid, Some(running.pid));
    eventually("the direct helper to be gone", || !alive(running.pid));
    block_on(launcher.cancel(&target)).unwrap();

    // Should a direct helper get in while a job's helper runs all the same (squeue wrong
    // again), the job's way out leaves that helper's socket in run/ and its record alone: it
    // removes them only while the record still names the job.
    let id = block_on(launcher.submit(&target)).unwrap().job;
    sim.release(id);
    let started = block_on(launcher.start(&target)).unwrap().endpoint;
    assert_eq!(Path::new(&started.socket), socket);
    let mut theirs = Command::new("sleep").arg("60").spawn().unwrap();
    std::fs::remove_file(&socket).unwrap();
    let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    let direct_record = Endpoint {
        pid: theirs.id(),
        host: crate::unix::this_host(),
        launcher: "direct".to_owned(),
        job: None,
        ..started.clone()
    };
    std::fs::write(
        &endpoint,
        format!("{}\n", serde_json::to_string(&direct_record).unwrap()),
    )
    .unwrap();
    let record = read(&endpoint);
    block_on(launcher.cancel(&target)).unwrap();
    assert_eq!(sim.job(id).state, "CANCELLED");
    assert!(!alive(started.pid));
    assert!(
        std::fs::symlink_metadata(&socket)
            .unwrap()
            .file_type()
            .is_socket()
    );
    assert_eq!(read(&endpoint), record);
    drop(listener);
    theirs.kill().unwrap();
    theirs.wait().unwrap();
    std::fs::remove_file(&endpoint).unwrap();
}

/// A job sbatch puts on a named cluster (`<id>;<cluster>`) is asked about there (`-M`).
fn slurm_jobs_on_a_named_cluster() {
    let (m, sim) = machine(Config {
        cluster: Some("example-cluster".to_owned()),
        ..Config::default()
    });
    let target = m.plain();
    block_on(deploy(&target, &helper("1.0.0"), &quick())).unwrap();
    let launcher = launcher(&render(&target, &slurm::generic(), &JobOptions::default()));
    let started = block_on(launcher.start(&target)).unwrap();
    let id = started.endpoint.job.unwrap();
    let status = block_on(launcher.job_status(&target)).unwrap();
    assert!(status.ready(), "{status:?}");
    assert_eq!(status.cluster.as_deref(), Some("example-cluster"));
    assert!(read(&m.run_dir().join("slurm.json")).ends_with(",\"cluster\":\"example-cluster\"}\n"));
    // Submitting again finds the same job there, rather than submitting another.
    assert!(!block_on(launcher.submit(&target)).unwrap().submitted_now);
    let cancelled = block_on(launcher.cancel(&target)).unwrap();
    assert!(cancelled.cancelled);
    assert!(matches!(&cancelled.state, JobState::Ended { state: Some(s), .. } if s == "CANCELLED"));
    for call in sim.calls("squeue").iter().chain(&sim.calls("scancel")) {
        assert_eq!(&call[..2], ["-M", "example-cluster"], "{call:?}");
    }
    assert_eq!(sim.calls("sbatch").len(), 1);
    assert_eq!(sim.job(id).state, "CANCELLED");
}

/// A job with modules and a node-local socket, run by each POSIX shell as the machine's `sh`.
fn slurm_under_every_posix_sh() {
    let mut checked = Vec::new();
    for shell in posix_shells() {
        let (m, sim) = machine_with(Some(shell.as_path()), Config::default());
        let tmp = m.dir.path().join("node-tmp");
        private_dir(&tmp);
        sim.set(|c| c.tmpdir = Some(tmp.clone()));
        let target = m.plain();
        block_on(deploy(&target, &helper("1.0.0"), &quick())).unwrap();
        let site = module_site(&m, SocketPlace::NodeLocal);
        let script = render(&target, &site, &JobOptions::default());
        let launcher = launcher(&script);
        let started = block_on(launcher.start(&target))
            .unwrap_or_else(|e| panic!("{}: {e}", shell.display()));
        assert!(
            started.endpoint.socket.starts_with(tmp.to_str().unwrap()),
            "{}",
            shell.display()
        );
        assert_eq!(
            read(&m.dir.path().join("loaded.log")),
            "load example-toolchain/1.0\nload nodejs/22\n",
            "{}",
            shell.display()
        );
        let status = block_on(launcher.job_status(&target)).unwrap();
        assert!(status.ready(), "{}: {status:?}", shell.display());
        // The set-up script made the shell ignore SIGUSR1 and SIGUSR2: the job catches them
        // again, so they do not end it and the helper does not inherit them ignored.
        let id = started.endpoint.job.unwrap();
        let job_shell = sim.job(id).pgid.unwrap();
        assert_eq!(
            ignored_signals(started.endpoint.pid) & USR_SIGNALS,
            0,
            "{}",
            shell.display()
        );
        send_usr_signals(job_shell);
        assert!(alive(job_shell), "{}", shell.display());
        assert!(alive(started.endpoint.pid), "{}", shell.display());
        assert_eq!(sim.job(id).state, "RUNNING", "{}", shell.display());
        let cancelled = block_on(launcher.cancel(&target)).unwrap();
        assert!(cancelled.cancelled, "{}", shell.display());
        eventually("the helper to be gone", || !alive(started.endpoint.pid));
        let socket = PathBuf::from(&started.endpoint.socket);
        eventually("the socket's directory to go", || {
            !socket.parent().unwrap().exists()
        });
        checked.push(shell.display().to_string());
    }
    println!("SLURM jobs run with sh = {checked:?}");
}

pub(crate) const CASES: &[(&str, fn())] = &[
    (
        "slurm_submit_pending_running_stop",
        slurm_submit_pending_running_stop,
    ),
    ("slurm_failures_are_clear", slurm_failures_are_clear),
    (
        "slurm_jobs_check_where_they_run",
        slurm_jobs_check_where_they_run,
    ),
    (
        "slurm_never_touches_other_jobs",
        slurm_never_touches_other_jobs,
    ),
    (
        "slurm_recipes_add_lines_and_modules",
        slurm_recipes_add_lines_and_modules,
    ),
    (
        "slurm_socket_on_node_local_tmpdir",
        slurm_socket_on_node_local_tmpdir,
    ),
    (
        "slurm_scripts_arrive_whole_or_not_at_all",
        slurm_scripts_arrive_whole_or_not_at_all,
    ),
    ("slurm_probe_finds_the_tools", slurm_probe_finds_the_tools),
    (
        "slurm_never_overlaps_another_launcher",
        slurm_never_overlaps_another_launcher,
    ),
    (
        "slurm_jobs_on_a_named_cluster",
        slurm_jobs_on_a_named_cluster,
    ),
    ("slurm_under_every_posix_sh", slurm_under_every_posix_sh),
];
