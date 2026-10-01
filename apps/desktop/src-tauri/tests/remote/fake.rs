//! The fake machine `hpc-login`: this computer's own `/bin/sh` in a temporary home, behind a
//! fake `ssh` that plays OpenSSH's calls as `pitcrew-remote` makes them.
//!
//! The fake `ssh` is this test binary, started by a small script (`<machine>/ssh`) that names its
//! role and its machine; ssh's own environment is the minimal one `pitcrew-remote` gives it. It
//! plays:
//! - `ssh -G -- <host>`: what the host resolves to;
//! - `ssh -N … -- <host>`, a link: it signs in (see below), logs "Authenticated to …" to its
//!   `-E` log, and with a `ControlPath` is a ControlMaster listening there: `check`, `exit`,
//!   `forward` (it then listens on the local socket and relays each connection to the remote
//!   one, with its half-closes) and `session` requests;
//! - `ssh -O <op> -o ControlPath=<p> -- <host>`: asks the link;
//! - `ssh -o ControlMaster=no -o ControlPath=<p> … -- <host> <command>`: a session of the link;
//! - any other call: signs in, then runs the command as sshd would, with `/bin/sh -c` in the
//!   machine's home, with its `PATH` (the stand-ins of `<machine>/bin` first), and this
//!   process's stdin, stdout and stderr.
//!
//! **Signing in** (not in `BatchMode`): with a `hostkey` file and no `known` file, it asks
//! through `SSH_ASKPASS` whether to trust the host key (`yes` makes `known`); with a `password`
//! file, it asks for the password, three times at most, as ssh does. Each answer is logged to
//! `asked.log` as `yes`, `no`, `text` or `empty`, never itself. Each call is logged to
//! `calls.log` as its kind and host.
//!
//! The binary is also `pitcrew-askpass` (`<machine>/pitcrew-askpass`), as the real one: the
//! crate's own client.
//!
//! Everything the machine runs carries the run's mark (`PITCREW_DESKTOP_TEST_RUN`) in its
//! environment, so the cases can find and stop what outlived them.

use std::io::{BufRead as _, BufReader, Write as _};
use std::net::Shutdown;
use std::os::unix::fs::PermissionsExt as _;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// What this binary plays: `ssh`, `askpass`, or (unset) the test runner.
pub const ROLE: &str = "PITCREW_DESKTOP_TEST_ROLE";
/// The fake ssh's machine directory.
pub const MACHINE: &str = "PITCREW_DESKTOP_TEST_MACHINE";
/// The mark everything the machine runs carries.
pub const RUN: &str = "PITCREW_DESKTOP_TEST_RUN";
/// The only host the fake ssh knows.
pub const HOST: &str = "hpc-login";
/// The host key's fingerprint, as the fake ssh shows it.
pub const FINGERPRINT: &str = "SHA256:ZmFrZS1ob3N0LWtleS1mb3ItdGhlLWRlc2t0b3A";

/// Plays this binary's role, if it has one.
pub fn act() -> Option<ExitCode> {
    match std::env::var(ROLE).as_deref() {
        Ok("ssh") => Some(ExitCode::from(ssh())),
        Ok("askpass") => Some(askpass()),
        _ => None,
    }
}

/// `pitcrew-askpass`, as `crates/remote/src/bin/pitcrew-askpass.rs` is.
fn askpass() -> ExitCode {
    use pitcrew_remote::askpass::client;
    let parent = client::Parent::at_start();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = client::main_with(
        &args,
        |name| std::env::var(name).ok(),
        &mut std::io::stdout().lock(),
        &mut std::io::stderr().lock(),
    );
    if code == client::EXIT_NO_ANSWER {
        parent.stop_ssh();
    }
    ExitCode::from(u8::try_from(code).unwrap_or(1))
}

/// One call's arguments, as ssh reads them.
#[derive(Debug, Default)]
struct Call {
    options: Vec<(String, String)>,
    flags: Vec<String>,
    log: Option<PathBuf>,
    op: Option<String>,
    forward: Option<String>,
    host: String,
    command: Option<String>,
}

impl Call {
    fn parse(args: &[String]) -> Self {
        let mut call = Self::default();
        let mut args = args.iter();
        while let Some(arg) = args.next() {
            let mut value = || args.next().cloned().unwrap_or_default();
            match arg.as_str() {
                "-o" => {
                    let option = value();
                    let (key, val) = option.split_once('=').unwrap_or((&option, ""));
                    call.options.push((key.to_owned(), val.to_owned()));
                }
                "-E" => call.log = Some(PathBuf::from(value())),
                "-O" => call.op = Some(value()),
                "-L" => call.forward = Some(value()),
                "-F" | "-W" | "-J" => {
                    let _ = value();
                }
                "--" => {
                    call.host = value();
                    call.command = args.next().cloned();
                    break;
                }
                flag => call.flags.push(flag.to_owned()),
            }
        }
        call
    }

    /// An option's value, the first given winning, as ssh's.
    fn option(&self, name: &str) -> Option<&str> {
        self.options
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    fn kind(&self) -> &'static str {
        if self.flags.iter().any(|f| f == "-G") {
            "resolve"
        } else if self.op.is_some() {
            "control"
        } else if self.flags.iter().any(|f| f == "-N") {
            "link"
        } else if self.option("ControlMaster") == Some("no") {
            "session"
        } else {
            "run"
        }
    }

    /// What ssh logs (`-E`).
    fn say(&self, line: &str) {
        say(self.log.as_deref(), line);
    }
}

fn say(log: Option<&Path>, line: &str) {
    match log {
        Some(path) => append(path, line),
        None => eprintln!("{line}"),
    }
}

fn append(path: &Path, line: &str) {
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(file, "{line}");
    }
}

fn ssh() -> u8 {
    let Some(machine) = std::env::var_os(MACHINE).map(PathBuf::from) else {
        eprintln!("fake ssh: no machine");
        return 255;
    };
    let args: Vec<String> = std::env::args().skip(1).collect();
    let call = Call::parse(&args);
    let kind = call.kind();
    append(&machine.join("calls.log"), &format!("{kind} {}", call.host));
    // `-O` talks to the master at the ControlPath: ssh does not resolve the host for it (a sweep
    // of a crashed app's links names none).
    if kind != "control" && call.host != HOST {
        call.say(&format!(
            "ssh: Could not resolve hostname {}: Name or service not known",
            call.host
        ));
        return 255;
    }
    match kind {
        "resolve" => {
            println!("hostname 192.0.2.10\nuser sam\nport 22");
            0
        }
        "control" => control_op(&call),
        "link" => link(&machine, &call),
        "session" => session(&machine, &call),
        _ => {
            if sign_in(&machine, &call) {
                run(&machine, &call)
            } else {
                255
            }
        }
    }
}

/// Signs in as the machine asks (see the module docs).
fn sign_in(machine: &Path, call: &Call) -> bool {
    let batch = call.option("BatchMode") == Some("yes");
    let asked = machine.join("asked.log");
    if machine.join("hostkey").exists() && !machine.join("known").exists() {
        let text = format!(
            "The authenticity of host '{HOST} (192.0.2.10)' can't be established.\n\
             ED25519 key fingerprint is {FINGERPRINT}.\n\
             This key is not known by any other names.\n\
             Are you sure you want to continue connecting (yes/no/[fingerprint])? "
        );
        let trusted = !batch && ask(&text).as_deref() == Some("yes");
        if !batch {
            append(&asked, if trusted { "yes" } else { "no" });
        }
        if !trusted {
            call.say("Host key verification failed.");
            return false;
        }
        std::fs::write(machine.join("known"), "").unwrap_or_default();
    }
    if let Ok(expected) = std::fs::read_to_string(machine.join("password")) {
        if !batch {
            for _ in 0..3 {
                let answer = ask(&format!("sam@{HOST}'s password: ")).unwrap_or_default();
                append(&asked, if answer.is_empty() { "empty" } else { "text" });
                if answer == expected.trim_end() {
                    return true;
                }
            }
        }
        call.say(&format!(
            "sam@{HOST}: Permission denied (publickey,password)."
        ));
        return false;
    }
    true
}

/// Asks through `SSH_ASKPASS`, as ssh does: the answer, or `None` when askpass failed.
fn ask(prompt: &str) -> Option<String> {
    let program = std::env::var_os("SSH_ASKPASS")?;
    let out = Command::new(program).arg(prompt).output().ok()?;
    out.status.success().then(|| {
        String::from_utf8_lossy(&out.stdout)
            .trim_end_matches('\n')
            .to_owned()
    })
}

/// Runs the call's command on the machine.
fn run(machine: &Path, call: &Call) -> u8 {
    let Some(command) = &call.command else {
        return 255;
    };
    let mark = std::fs::read_to_string(machine.join("mark")).unwrap_or_default();
    let user = std::env::var("USER").unwrap_or_else(|_| "sam".to_owned());
    let path = format!("{}:/usr/bin:/bin", machine.join("bin").display());
    let status = Command::new("/bin/sh")
        .arg("-c")
        .arg(command)
        .current_dir(machine.join("home"))
        .env_clear()
        .env("HOME", machine.join("home"))
        .env("PATH", path)
        .env("USER", &user)
        .env("LOGNAME", &user)
        .env("SHELL", "/bin/sh")
        .env(RUN, mark.trim())
        .status();
    match status {
        Ok(status) => status
            .code()
            .and_then(|c| u8::try_from(c).ok())
            .unwrap_or(255),
        Err(_) => 255,
    }
}

/// `ssh -N`: a link, as a ControlMaster or a heartbeat. The network never fails here.
fn link(machine: &Path, call: &Call) -> u8 {
    if !sign_in(machine, call) {
        return 255;
    }
    call.say(&format!(
        "Authenticated to {} ([192.0.2.10]:22) using \"publickey\".",
        call.host
    ));
    let exit = Arc::new(AtomicBool::new(false));
    let control = call.option("ControlPath").map(PathBuf::from);
    if let Some(control) = &control {
        let _ = std::fs::remove_file(control);
        let Ok(listener) = UnixListener::bind(control) else {
            call.say("fake ssh: cannot listen on the control socket");
            return 255;
        };
        let log = call.log.clone();
        let exit = Arc::clone(&exit);
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let log = log.clone();
                let exit = Arc::clone(&exit);
                std::thread::spawn(move || serve_mux(stream, log.as_deref(), &exit));
            }
        });
    }
    while !exit.load(Ordering::SeqCst) {
        std::thread::sleep(Duration::from_millis(100));
    }
    if let Some(control) = &control {
        let _ = std::fs::remove_file(control);
    }
    0
}

/// One client of a link: one request line, one answer line.
fn serve_mux(mut stream: UnixStream, log: Option<&Path>, exit: &AtomicBool) {
    let mut line = String::new();
    let Ok(reader) = stream.try_clone() else {
        return;
    };
    if BufReader::new(reader).read_line(&mut line).is_err() {
        return;
    }
    let words: Vec<&str> = line.split_whitespace().collect();
    match words.as_slice() {
        ["check"] => {
            let _ = writeln!(stream, "ok {}", std::process::id());
        }
        ["exit"] => {
            exit.store(true, Ordering::SeqCst);
            let _ = writeln!(stream, "ok");
        }
        ["forward", local, remote] => match listen_forward(local, remote, log) {
            Ok(()) => {
                let _ = writeln!(stream, "ok");
            }
            Err(e) => {
                let _ = writeln!(stream, "fail {e}");
            }
        },
        ["session"] => {
            let _ = writeln!(stream, "ok");
            // Held until the client goes.
            let _ = std::io::copy(&mut stream, &mut std::io::sink());
        }
        _ => {
            let _ = writeln!(stream, "fail unknown request");
        }
    }
}

/// The master's listener for a forward: each connection is a channel to `remote`.
fn listen_forward(local: &str, remote: &str, log: Option<&Path>) -> std::io::Result<()> {
    let _ = std::fs::remove_file(local);
    let listener = UnixListener::bind(local)?;
    std::fs::set_permissions(local, std::fs::Permissions::from_mode(0o600))?;
    let remote = remote.to_owned();
    let log = log.map(Path::to_path_buf);
    std::thread::spawn(move || {
        for client in listener.incoming().flatten() {
            let remote = remote.clone();
            let log = log.clone();
            std::thread::spawn(move || match UnixStream::connect(&remote) {
                Ok(daemon) => relay(client, daemon),
                Err(_) => {
                    say(
                        log.as_deref(),
                        "channel 3: open failed: connect failed: No such file or directory",
                    );
                    drop(client);
                }
            });
        }
    });
    Ok(())
}

/// Copies both ways, passing each end of file on as a half-close.
fn relay(a: UnixStream, b: UnixStream) {
    let (Ok(mut a_in), Ok(mut b_out)) = (a.try_clone(), b.try_clone()) else {
        return;
    };
    let up = std::thread::spawn(move || {
        let _ = std::io::copy(&mut a_in, &mut b_out);
        let _ = b_out.shutdown(Shutdown::Write);
    });
    let (mut b_in, mut a_out) = (b, a);
    let _ = std::io::copy(&mut b_in, &mut a_out);
    let _ = a_out.shutdown(Shutdown::Write);
    let _ = up.join();
}

/// `ssh -O <op>`.
fn control_op(call: &Call) -> u8 {
    let path = call.option("ControlPath").unwrap_or("");
    let Ok(mut master) = UnixStream::connect(path) else {
        eprintln!("Control socket connect({path}): No such file or directory");
        return 255;
    };
    let request = match (call.op.as_deref(), &call.forward) {
        (Some("forward"), Some(spec)) => {
            let (local, remote) = spec.split_once(':').unwrap_or((spec, ""));
            format!("forward {local} {remote}")
        }
        (Some(op), _) => op.to_owned(),
        (None, _) => return 255,
    };
    match ask_master(&mut master, &request) {
        Some(answer) if answer.starts_with("ok") => {
            if call.op.as_deref() == Some("check") {
                eprintln!("Master running (pid={})", answer.trim_start_matches("ok "));
            }
            0
        }
        Some(answer) => {
            eprintln!("{answer}");
            255
        }
        None => 255,
    }
}

/// Sends one request to a link and reads its answer line.
fn ask_master(stream: &mut UnixStream, request: &str) -> Option<String> {
    stream.write_all(format!("{request}\n").as_bytes()).ok()?;
    let mut line = String::new();
    let mut reader = BufReader::new(stream.try_clone().ok()?);
    match reader.read_line(&mut line) {
        Ok(n) if n > 0 => Some(line.trim_end().to_owned()),
        _ => None,
    }
}

/// A session of a link, running a command on the machine.
fn session(machine: &Path, call: &Call) -> u8 {
    let path = call.option("ControlPath").unwrap_or("");
    let Ok(mut master) = UnixStream::connect(path) else {
        call.say("kex_exchange_identification: Connection closed by remote host");
        return 255;
    };
    if ask_master(&mut master, "session").as_deref() != Some("ok") {
        call.say(
            "mux_client_request_session: session request failed: Session open refused by peer",
        );
        return 255;
    }
    let code = run(machine, call);
    drop(master);
    code
}

// ─── The machine ──────────────────────────────────────────────────────────────────────────────

/// A fake machine, with what the app needs to reach it.
pub struct Machine {
    /// Its directory.
    pub dir: PathBuf,
    /// Its home.
    pub home: PathBuf,
    /// The `ssh` to run.
    pub ssh: PathBuf,
    /// The `pitcrew-askpass` to run.
    pub askpass: PathBuf,
    /// The person's ssh config on the laptop (for the host list), and their laptop home.
    pub ssh_config: PathBuf,
    pub laptop_home: PathBuf,
    /// Where ssh keeps its sockets and logs.
    pub runtime: PathBuf,
    /// The helpers' folder.
    pub helpers: PathBuf,
    /// The SLURM stand-ins' state.
    pub slurm: PathBuf,
    mark: String,
    _tmp: tempfile::TempDir,
}

impl Machine {
    /// A fresh machine. With `slurm`, its `PATH` has SLURM stand-ins (`sbatch`, `squeue`,
    /// `scancel`, `sinfo`): a submitted job stays pending, and its script is kept.
    pub fn new(slurm: bool, real_pitcrewd: &Path) -> Self {
        // Short paths: unix sockets live under it.
        let tmp = tempfile::Builder::new()
            .prefix("pck")
            .tempdir_in("/tmp")
            .unwrap();
        let dir = tmp.path().to_path_buf();
        for sub in ["home", "bin", "helpers", "rt", "laptop", "slurm"] {
            std::fs::create_dir(dir.join(sub)).unwrap();
            std::fs::set_permissions(dir.join(sub), std::fs::Permissions::from_mode(0o700))
                .unwrap();
        }
        let mark = format!("{}-{}", std::process::id(), dir.display());
        std::fs::write(dir.join("mark"), &mark).unwrap();
        let me = std::env::current_exe().unwrap();
        let ssh = dir.join("ssh");
        script(
            &ssh,
            &format!(
                "{ROLE}=ssh {MACHINE}='{}' exec '{}' \"$@\"",
                dir.display(),
                me.display()
            ),
        );
        let askpass = dir.join("pitcrew-askpass");
        script(
            &askpass,
            &format!("{ROLE}=askpass exec '{}' \"$@\"", me.display()),
        );
        let ssh_config = dir.join("laptop").join("ssh_config");
        std::fs::write(
            &ssh_config,
            format!(
                "Host {HOST}\n  HostName 192.0.2.10\n  User sam\n\nHost *\n  ServerAliveInterval 30\n\nHost node0*\n  User sam\n"
            ),
        )
        .unwrap();
        helpers(&dir.join("helpers"), real_pitcrewd);
        let slurm_dir = dir.join("slurm");
        if slurm {
            slurm_tools(&dir.join("bin"), &slurm_dir);
        }
        Self {
            home: dir.join("home"),
            ssh,
            askpass,
            ssh_config,
            laptop_home: dir.join("laptop"),
            runtime: dir.join("rt"),
            helpers: dir.join("helpers"),
            slurm: slurm_dir,
            dir,
            mark,
            _tmp: tmp,
        }
    }

    /// From now on logins ask for `password`.
    pub fn require_password(&self, password: &str) {
        std::fs::write(self.dir.join("password"), password).unwrap();
    }

    /// From now on the host key is unknown until accepted.
    pub fn unknown_host_key(&self) {
        std::fs::write(self.dir.join("hostkey"), "").unwrap();
        let _ = std::fs::remove_file(self.dir.join("known"));
    }

    /// The answers given at sign-in, in order.
    pub fn asked(&self) -> Vec<String> {
        std::fs::read_to_string(self.dir.join("asked.log"))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    /// The calls ssh got, as `<kind> <host>`.
    pub fn calls(&self) -> Vec<String> {
        std::fs::read_to_string(self.dir.join("calls.log"))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    /// The helper's record of itself, if it runs.
    pub fn endpoint(&self) -> Option<serde_json::Value> {
        let text = std::fs::read_to_string(self.home.join(".pitcrew/run/endpoint.json")).ok()?;
        serde_json::from_str(&text).ok()
    }

    /// Stops what the machine still runs: the helper its record names, and (Linux) anything
    /// carrying the run's mark. Returns how many processes it stopped.
    pub fn stop_everything(&self) -> usize {
        let mut stopped = 0;
        if let Some(pid) = self
            .endpoint()
            .and_then(|e| e["pid"].as_i64())
            .and_then(|p| i32::try_from(p).ok())
            .and_then(rustix::process::Pid::from_raw)
            && rustix::process::kill_process(pid, rustix::process::Signal::TERM).is_ok()
        {
            stopped += 1;
            std::thread::sleep(Duration::from_millis(500));
            let _ = rustix::process::kill_process(pid, rustix::process::Signal::KILL);
        }
        if let Ok(entries) = std::fs::read_dir("/proc") {
            let needle = format!("{RUN}={}", self.mark);
            for entry in entries.flatten() {
                let Some(pid) = entry
                    .file_name()
                    .to_str()
                    .and_then(|n| n.parse::<i32>().ok())
                    .and_then(rustix::process::Pid::from_raw)
                else {
                    continue;
                };
                let environ = std::fs::read(entry.path().join("environ")).unwrap_or_default();
                if environ
                    .split(|b| *b == 0)
                    .any(|var| var == needle.as_bytes())
                    && rustix::process::kill_process(pid, rustix::process::Signal::KILL).is_ok()
                {
                    stopped += 1;
                }
            }
        }
        stopped
    }
}

impl Drop for Machine {
    fn drop(&mut self) {
        self.stop_everything();
    }
}

fn script(path: &Path, body: &str) {
    std::fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
}

/// This computer's platform, as the probe will find the machine's.
pub fn platform() -> pitcrew_remote::Platform {
    use pitcrew_remote::Platform;
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "aarch64") => Platform::LinuxAarch64,
        ("macos", _) => Platform::MacOs,
        _ => Platform::LinuxX86_64,
    }
}

/// The version `pitcrewd --version` says (its second word).
pub fn version_of(pitcrewd: &Path) -> String {
    let out = Command::new(pitcrewd).arg("--version").output().unwrap();
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.split_whitespace().nth(1).unwrap().to_owned()
}

/// The helpers' folder: for this platform, a stand-in for the release helper that is the real
/// `pitcrewd`, serving the demo workspace; and its manifest.
fn helpers(dir: &Path, real: &Path) {
    use sha2::{Digest as _, Sha256};
    let artefact = platform().artefact();
    let body = format!(
        "#!/bin/sh\n# The real pitcrewd, serving the demo workspace.\n\
         if [ \"$1\" = serve ]; then shift; exec '{real}' serve --demo \"$@\"; fi\n\
         exec '{real}' \"$@\"\n",
        real = real.display()
    );
    std::fs::write(dir.join(artefact), &body).unwrap();
    std::fs::set_permissions(dir.join(artefact), std::fs::Permissions::from_mode(0o700)).unwrap();
    let sha: String = Sha256::digest(body.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let manifest = serde_json::json!({ "version": version_of(real), "sha256": { artefact: sha } });
    std::fs::write(dir.join("manifest.json"), manifest.to_string()).unwrap();
}

/// SLURM stand-ins: `sbatch` keeps the script it got (`submitted.sh`) and its job stays
/// pending; `squeue` reports it while it is queued; `scancel` ends it (`scancel.log`); `sinfo`
/// names `batch` the default partition.
fn slurm_tools(bin: &Path, state: &Path) {
    let s = state.display();
    script(
        &bin.join("sbatch"),
        &format!(
            "case \"$1\" in --version) echo 'slurm 23.02.7'; exit 0;; esac\n\
             name= file=\n\
             for a in \"$@\"; do case \"$a\" in --job-name=*) name=${{a#--job-name=}};; -*) ;; *) [ -z \"$file\" ] && file=$a;; esac; done\n\
             cat \"$file\" > '{s}/submitted.sh' || exit 1\n\
             printf '%s' \"$name\" > '{s}/name'\n\
             echo PENDING > '{s}/state'\n\
             echo 4242"
        ),
    );
    script(
        &bin.join("squeue"),
        &format!(
            "case \"$1\" in --version) echo 'slurm 23.02.7'; exit 0;; esac\n\
             [ -f '{s}/state' ] || exit 0\n\
             printf '4242|%s|%s|Priority|1:00:00|1:00:00|(null)|%s\\n' \"$(id -u)\" \"$(cat '{s}/state')\" \"$(cat '{s}/name')\""
        ),
    );
    script(
        &bin.join("scancel"),
        &format!(
            "case \"$1\" in --version) echo 'slurm 23.02.7'; exit 0;; esac\n\
             echo \"$*\" >> '{s}/scancel.log'\n\
             rm -f '{s}/state'"
        ),
    );
    script(
        &bin.join("sinfo"),
        "case \"$1\" in --version) echo 'slurm 23.02.7'; exit 0;; esac\nprintf 'batch*\\ngpu\\n'",
    );
}
