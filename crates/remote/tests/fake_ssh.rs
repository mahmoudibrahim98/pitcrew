//! Drives [`Ssh`] against a fake `ssh`.
//!
//! The fake is this test binary itself: each case links it into a temporary directory as
//! `ssh`, next to a `scenario.json`. When the binary finds a scenario beside itself it acts as
//! ssh (logs its arguments, runs askpass prompts like ssh does, prints canned output) instead
//! of running the tests. Hence `harness = false` and a small runner below.

// Test code; clippy's allow-unwrap-in-tests only sees `#[test]` functions.
#![allow(clippy::unwrap_used)]

use pitcrew_protocol::model::Scheduler;
use pitcrew_remote::{
    Output, PromptHandler, PromptKind, PromptRequest, Reply, Secret, Ssh, SshError,
};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, Mutex};

const SCENARIO: &str = "scenario.json";
const LOG: &str = "log.json";
/// The askpass binary. `PITCREW_TEST_ASKPASS` overrides it for test binaries run on another
/// system than the one that built them (e.g. cross-built for Windows).
fn askpass() -> String {
    std::env::var("PITCREW_TEST_ASKPASS")
        .unwrap_or_else(|_| env!("CARGO_BIN_EXE_pitcrew-askpass").to_owned())
}

#[derive(Default, Serialize, Deserialize)]
struct Scenario {
    /// Prompts to put to askpass, in order, before anything else.
    #[serde(default)]
    prompts: Vec<ScriptedPrompt>,
    #[serde(default)]
    stdout: String,
    #[serde(default)]
    stderr: String,
    #[serde(default)]
    exit: i32,
    /// Run the remote command with the local `sh -c`, like a real login shell would.
    #[serde(default)]
    run_sh: bool,
}

#[derive(Serialize, Deserialize)]
struct ScriptedPrompt {
    text: String,
    hint: Option<String>,
    expect: String,
    /// What ssh prints when the answer is wrong or missing.
    fail_stderr: String,
}

#[derive(Serialize, Deserialize)]
struct Log {
    args: Vec<String>,
    askpass: Option<String>,
    askpass_require: Option<String>,
    has_bridge_env: bool,
}

fn main() -> ExitCode {
    if let Some(code) = act_as_ssh() {
        return ExitCode::from(code);
    }
    run_tests()
}

// ─── The fake ssh ───────────────────────────────────────────────────────────────────────────

fn act_as_ssh() -> Option<u8> {
    let dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
    let scenario: Scenario =
        serde_json::from_str(&std::fs::read_to_string(dir.join(SCENARIO)).ok()?).ok()?;
    let args: Vec<String> = std::env::args().skip(1).collect();
    let log = Log {
        args: args.clone(),
        askpass: std::env::var("SSH_ASKPASS").ok(),
        askpass_require: std::env::var("SSH_ASKPASS_REQUIRE").ok(),
        has_bridge_env: std::env::var("PITCREW_ASKPASS_ADDR").is_ok()
            && std::env::var("PITCREW_ASKPASS_KEY").is_ok(),
    };
    std::fs::write(dir.join(LOG), serde_json::to_vec(&log).ok()?).ok()?;

    for prompt in &scenario.prompts {
        let answered = std::env::var("SSH_ASKPASS").ok().and_then(|program| {
            let mut command = std::process::Command::new(program);
            command.arg(&prompt.text);
            if let Some(hint) = &prompt.hint {
                command.env("SSH_ASKPASS_PROMPT", hint);
            }
            let out = command.output().ok()?;
            out.status.success().then(|| {
                String::from_utf8_lossy(&out.stdout)
                    .trim_end_matches('\n')
                    .to_owned()
            })
        });
        if answered.as_deref() != Some(prompt.expect.as_str()) {
            eprintln!("{}", prompt.fail_stderr);
            return Some(255);
        }
    }

    if scenario.run_sh && cfg!(unix) {
        let command = args
            .iter()
            .position(|a| a == "--")
            .and_then(|i| args.get(i + 2))?;
        let status = std::process::Command::new("sh")
            .arg("-c")
            .arg(command)
            .status()
            .ok()?;
        return Some(
            status
                .code()
                .and_then(|c| u8::try_from(c).ok())
                .unwrap_or(1),
        );
    }
    print!("{}", scenario.stdout);
    eprint!("{}", scenario.stderr);
    Some(u8::try_from(scenario.exit).unwrap_or(1))
}

// ─── Fixtures ───────────────────────────────────────────────────────────────────────────────

/// A temporary directory with the fake linked in as `ssh`.
struct Fake {
    dir: tempfile::TempDir,
    ssh: PathBuf,
}

impl Fake {
    fn new(scenario: &Scenario) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let ssh = dir
            .path()
            .join(format!("ssh{}", std::env::consts::EXE_SUFFIX));
        let me = std::env::current_exe().unwrap();
        if std::fs::hard_link(&me, &ssh).is_err() {
            std::fs::copy(&me, &ssh).unwrap();
        }
        std::fs::write(
            dir.path().join(SCENARIO),
            serde_json::to_vec(scenario).unwrap(),
        )
        .unwrap();
        Self { dir, ssh }
    }

    fn ssh(&self) -> Ssh {
        Ssh::new(&self.ssh).with_runtime_dir(self.runtime_dir())
    }

    fn runtime_dir(&self) -> PathBuf {
        self.dir.path().join("rt")
    }

    fn log(&self) -> Log {
        let text = std::fs::read_to_string(self.dir.path().join(LOG)).expect("ssh was not run");
        serde_json::from_str(&text).unwrap()
    }

    fn ran(&self) -> bool {
        self.dir.path().join(LOG).exists()
    }
}

struct Handler {
    seen: Mutex<Vec<PromptRequest>>,
    answer: fn(PromptKind) -> Reply,
}

impl Handler {
    fn new(answer: fn(PromptKind) -> Reply) -> Arc<Self> {
        Arc::new(Self {
            seen: Mutex::new(Vec::new()),
            answer,
        })
    }

    fn kinds(&self) -> Vec<(String, PromptKind)> {
        let seen = self.seen.lock().unwrap();
        seen.iter().map(|r| (r.host.clone(), r.kind)).collect()
    }
}

impl PromptHandler for Handler {
    fn prompt(&self, request: &PromptRequest) -> Reply {
        self.seen.lock().unwrap().push(request.clone());
        (self.answer)(request.kind)
    }
}

fn block_on<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
        .block_on(future)
}

fn prompt(text: &str, expect: &str, fail_stderr: &str) -> ScriptedPrompt {
    ScriptedPrompt {
        text: text.to_owned(),
        hint: None,
        expect: expect.to_owned(),
        fail_stderr: fail_stderr.to_owned(),
    }
}

const HOST_KEY_PROMPT: &str = "The authenticity of host 'cluster (192.0.2.10)' can't be established.\n\
     ED25519 key fingerprint is SHA256:AAAAexampleexampleexampleexampleexample.\n\
     This key is not known by any other names.\n\
     Are you sure you want to continue connecting (yes/no/[fingerprint])? ";

// ─── Cases ──────────────────────────────────────────────────────────────────────────────────

fn exact_argument_list() {
    let fake = Fake::new(&Scenario {
        stdout: "hi\n".into(),
        ..Scenario::default()
    });
    let ssh = fake.ssh();
    let out = block_on(ssh.run("cluster", &["echo", "hi there"])).unwrap();
    assert_eq!(out.stdout_text(), "hi\n");

    let mut want: Vec<String> = [
        "-T",
        "-o",
        "ForwardAgent=no",
        "-o",
        "ForwardX11=no",
        "-o",
        "PermitLocalCommand=no",
        "-o",
        "ClearAllForwardings=yes",
        "-o",
        "ServerAliveInterval=15",
        "-o",
        "ServerAliveCountMax=3",
        "-o",
        "StrictHostKeyChecking=ask",
        "-o",
        "ConnectTimeout=15",
        "-o",
        "BatchMode=yes",
    ]
    .map(str::to_owned)
    .to_vec();
    if cfg!(unix) {
        want.extend([
            "-o".to_owned(),
            "ControlMaster=auto".to_owned(),
            "-o".to_owned(),
            format!("ControlPath={}/%C", fake.runtime_dir().display()),
            "-o".to_owned(),
            "ControlPersist=10m".to_owned(),
        ]);
    }
    want.extend(["--", "cluster"].map(str::to_owned));
    want.push(pitcrew_remote::quote::remote_command(&["echo", "hi there"]).unwrap());
    let log = fake.log();
    assert_eq!(log.args, want);
    assert_eq!(log.askpass, None);
    assert!(!log.has_bridge_env);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(fake.runtime_dir())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o700);
    }

    // With prompts: no BatchMode, and the bridge is in the environment.
    let fake = Fake::new(&Scenario::default());
    let ssh = fake
        .ssh()
        .with_prompts(askpass(), Handler::new(|_| Reply::Cancel));
    block_on(ssh.run("user@cluster", &["true"])).unwrap();
    let log = fake.log();
    assert!(!log.args.iter().any(|a| a == "BatchMode=yes"));
    let dash = log.args.iter().position(|a| a == "--").unwrap();
    assert_eq!(&log.args[dash..dash + 2], ["--", "user@cluster"]);
    assert_eq!(
        log.args[dash + 2],
        pitcrew_remote::quote::remote_command(&["true"]).unwrap()
    );
    assert_eq!(log.askpass.as_deref(), Some(askpass().as_str()));
    assert_eq!(log.askpass_require.as_deref(), Some("force"));
    assert!(log.has_bridge_env);
}

fn hostile_argv_survives_the_remote_shell() {
    let hostile = [
        "it's",
        "a;b",
        "$(echo pwned)",
        "`id`",
        "line1\nline2",
        "-rf",
        "--",
        "日本語 ünïcödé",
        "",
        "a  b\tc",
        "*",
        "~",
        "\\",
        "\"",
        "A=b",
        "#",
        "!x",
    ];
    let mut argv = vec!["printf", "%s\\0"];
    argv.extend(hostile);
    let fake = Fake::new(&Scenario {
        run_sh: true,
        ..Scenario::default()
    });
    let out = block_on(fake.ssh().run("cluster", &argv)).unwrap();
    let log = fake.log();
    // The command crossed the process boundary as one argument, byte for byte.
    assert_eq!(
        log.args.last().unwrap(),
        &pitcrew_remote::quote::remote_command(&argv).unwrap()
    );
    if cfg!(unix) {
        assert!(out.success(), "{out:?}");
        let words: Vec<&str> = std::str::from_utf8(&out.stdout)
            .unwrap()
            .split_terminator('\0')
            .collect();
        assert_eq!(words, hostile);
    }
}

fn hostile_hosts_never_reach_ssh() {
    let fake = Fake::new(&Scenario::default());
    for host in ["-oProxyCommand=touch x", "a b", "a\nb", ""] {
        let err = block_on(fake.ssh().run(host, &["true"])).unwrap_err();
        assert!(matches!(err, SshError::InvalidHost(_)), "{host:?}: {err:?}");
        let err = block_on(fake.ssh().resolve(host)).unwrap_err();
        assert!(matches!(err, SshError::InvalidHost(_)), "{host:?}: {err:?}");
    }
    assert!(!fake.ran());
}

fn askpass_round_trip_and_cancel() {
    let scenario = Scenario {
        prompts: vec![
            prompt(
                "someone@cluster's password: ",
                "s3cr3t",
                "someone@cluster: Permission denied (keyboard-interactive).",
            ),
            prompt(
                "Verification code: ",
                "123456",
                "someone@cluster: Permission denied (keyboard-interactive).",
            ),
        ],
        stdout: "in\n".into(),
        ..Scenario::default()
    };
    let fake = Fake::new(&scenario);
    let handler = Handler::new(|kind| match kind {
        PromptKind::Password => Reply::Text(Secret::new("s3cr3t")),
        PromptKind::Otp => Reply::Text(Secret::new("123456")),
        _ => Reply::Cancel,
    });
    let ssh = fake.ssh().with_prompts(askpass(), handler.clone());
    let out = block_on(ssh.run("cluster", &["true"])).unwrap();
    assert_eq!(out.stdout_text(), "in\n");
    assert_eq!(
        handler.kinds(),
        [
            ("cluster".to_owned(), PromptKind::Password),
            ("cluster".to_owned(), PromptKind::Otp),
        ]
    );

    let fake = Fake::new(&scenario);
    let handler = Handler::new(|_| Reply::Cancel);
    let ssh = fake.ssh().with_prompts(askpass(), handler.clone());
    let err = block_on(ssh.run("cluster", &["true"])).unwrap_err();
    assert!(matches!(err, SshError::Cancelled), "{err:?}");
    assert_eq!(handler.kinds().len(), 1);

    // A wrong answer is an authentication failure, not a cancel.
    let fake = Fake::new(&scenario);
    let ssh = fake.ssh().with_prompts(
        askpass(),
        Handler::new(|_| Reply::Text(Secret::new("nope"))),
    );
    let err = block_on(ssh.run("cluster", &["true"])).unwrap_err();
    assert!(matches!(err, SshError::AuthFailed { .. }), "{err:?}");
}

fn host_key_prompt() {
    let scenario = Scenario {
        prompts: vec![prompt(
            HOST_KEY_PROMPT,
            "yes",
            "Host key verification failed.",
        )],
        ..Scenario::default()
    };
    let fake = Fake::new(&scenario);
    let handler = Handler::new(|kind| match kind {
        PromptKind::HostKey => Reply::Accept,
        _ => Reply::Cancel,
    });
    let ssh = fake.ssh().with_prompts(askpass(), handler.clone());
    block_on(ssh.run("cluster", &["true"])).unwrap();
    assert_eq!(
        handler.kinds(),
        [("cluster".to_owned(), PromptKind::HostKey)]
    );
    let seen = handler.seen.lock().unwrap();
    assert!(seen[0].prompt.contains("SHA256:AAAA"));
    drop(seen);

    let fake = Fake::new(&scenario);
    let ssh = fake
        .ssh()
        .with_prompts(askpass(), Handler::new(|_| Reply::Cancel));
    let err = block_on(ssh.run("cluster", &["true"])).unwrap_err();
    assert!(matches!(err, SshError::Cancelled), "{err:?}");

    // Without a handler ssh runs in batch mode and cannot ask.
    let fake = Fake::new(&Scenario {
        stderr: "Host key verification failed.\n".into(),
        exit: 255,
        ..Scenario::default()
    });
    let err = block_on(fake.ssh().run("cluster", &["true"])).unwrap_err();
    assert!(matches!(err, SshError::HostKeyRejected { .. }), "{err:?}");
}

fn timeouts_and_exit_codes() {
    let failing = |stderr: &str| {
        let fake = Fake::new(&Scenario {
            stderr: stderr.to_owned(),
            exit: 255,
            ..Scenario::default()
        });
        block_on(fake.ssh().run("cluster", &["true"])).unwrap_err()
    };
    let err = failing("ssh: connect to host cluster port 22: Connection timed out\n");
    assert!(matches!(err, SshError::ConnectTimeout { .. }), "{err:?}");
    let err = failing("ssh: Could not resolve hostname cluster: Name or service not known\n");
    assert!(matches!(err, SshError::Unreachable { .. }), "{err:?}");
    let err = failing("someone@cluster: Permission denied (publickey).\n");
    assert!(matches!(err, SshError::AuthFailed { .. }), "{err:?}");
    let err = failing("something odd\n");
    match err {
        SshError::Ssh { code, stderr } => {
            assert_eq!(code, 255);
            assert_eq!(stderr, "something odd\n");
        }
        other => panic!("{other:?}"),
    }

    // Any other exit code belongs to the remote command.
    let fake = Fake::new(&Scenario {
        stdout: "partial\n".into(),
        stderr: "grep: no match\n".into(),
        exit: 3,
        ..Scenario::default()
    });
    let out = block_on(fake.ssh().run("cluster", &["grep", "x"])).unwrap();
    assert_eq!(
        out,
        Output {
            code: Some(3),
            stdout: b"partial\n".to_vec(),
            stderr: b"grep: no match\n".to_vec(),
        }
    );
    assert!(!out.success());
}

fn resolve_uses_ssh_g() {
    let fake = Fake::new(&Scenario {
        stdout: "user someone\nhostname login1.example.org\nport 2222\nproxyjump gateway\n".into(),
        ..Scenario::default()
    });
    let host = block_on(fake.ssh().resolve("cluster")).unwrap();
    assert_eq!(host.hostname, "login1.example.org");
    assert_eq!(host.user.as_deref(), Some("someone"));
    assert_eq!(host.port, 2222);
    assert_eq!(host.proxy_jump.as_deref(), Some("gateway"));
    assert_eq!(fake.log().args, ["-G", "--", "cluster"]);
}

fn probe_fixture(stdout: &str) -> pitcrew_remote::Probe {
    let fake = Fake::new(&Scenario {
        stdout: stdout.to_owned(),
        ..Scenario::default()
    });
    let probe = block_on(fake.ssh().probe("box")).unwrap();
    let log = fake.log();
    let dash = log.args.iter().position(|a| a == "--").unwrap();
    assert_eq!(
        log.args[dash + 2],
        pitcrew_remote::quote::remote_command(&["sh", "-c", pitcrew_remote::probe::SCRIPT])
            .unwrap()
    );
    probe
}

fn probe_linux() {
    let p = probe_fixture(
        "@@pitcrew-probe-begin\nos=Linux\narch=x86_64\nhostname=workstation\nhome=/home/someone\n\
         tmux_found=1\ntmux=tmux 3.3a\nsbatch=0\nsqueue=0\nfs=ext2/ext3\n@@pitcrew-probe-end\n",
    );
    assert_eq!(p.info.os, "linux");
    assert_eq!(p.info.arch, "x86_64");
    assert_eq!(p.info.hostname, "workstation");
    assert!(p.info.has_tmux);
    assert_eq!(p.tmux_version.as_deref(), Some("3.3a"));
    assert_eq!(p.info.scheduler, None);
    assert!(!p.info.home_on_network_fs);
    assert_eq!(p.home.as_deref(), Some("/home/someone"));
}

fn probe_macos() {
    let p = probe_fixture(
        "@@pitcrew-probe-begin\r\nos=Darwin\r\narch=arm64\r\nhostname=laptop.local\r\n\
         home=/Users/someone\r\ntmux_found=1\r\ntmux=tmux next-3.5\r\nsbatch=0\r\nsqueue=0\r\n\
         fs=apfs\r\n@@pitcrew-probe-end\r\n",
    );
    assert_eq!(p.info.os, "macos");
    assert_eq!(p.info.arch, "aarch64");
    assert_eq!(p.tmux_version.as_deref(), Some("next-3.5"));
    assert_eq!(p.home_fs.as_deref(), Some("apfs"));
    assert!(!p.info.home_on_network_fs);
}

fn probe_slurm_login_node() {
    let p = probe_fixture(
        "*** Welcome to the example cluster ***\nos=bogus-before-marker\n\
         @@pitcrew-probe-begin\nos=Linux\narch=x86_64\nhostname=login01.cluster.example.org\n\
         home=/shared/home/someone\ntmux_found=1\ntmux=tmux 2.7\nsbatch=1\nsqueue=1\nfs=nfs\n\
         @@pitcrew-probe-end\nlogout message\n",
    );
    assert_eq!(p.info.os, "linux");
    assert_eq!(p.info.scheduler, Some(Scheduler::Slurm));
    assert!(p.has_sbatch && p.has_squeue);
    assert!(p.info.home_on_network_fs);
    assert_eq!(p.home_fs.as_deref(), Some("nfs"));
}

fn probe_box_without_tmux() {
    let p = probe_fixture(
        "@@pitcrew-probe-begin\nos=Linux\narch=aarch64\nhostname=\nhome=/root\ntmux_found=0\n\
         sbatch=1\nsqueue=0\nfs=\n",
    );
    assert!(!p.info.has_tmux);
    assert_eq!(p.tmux_version, None);
    // sbatch alone is not a usable SLURM.
    assert_eq!(p.info.scheduler, None);
    assert_eq!(p.info.hostname, "box");
    assert_eq!(p.home_fs, None);
    assert!(!p.info.home_on_network_fs);
}

fn probe_garbage_is_an_error() {
    let fake = Fake::new(&Scenario {
        stdout: "This account is disabled.\n".into(),
        exit: 1,
        ..Scenario::default()
    });
    let err = block_on(fake.ssh().probe("box")).unwrap_err();
    assert!(matches!(err, SshError::UnexpectedOutput(_)), "{err:?}");
}

/// The real script under the local `sh` (Unix only).
fn probe_script_runs_under_sh() {
    if !cfg!(unix) {
        return;
    }
    let fake = Fake::new(&Scenario {
        run_sh: true,
        ..Scenario::default()
    });
    let p = block_on(fake.ssh().probe("box")).unwrap();
    assert_eq!(p.info.os, std::env::consts::OS);
    assert_eq!(p.info.arch, std::env::consts::ARCH);
    assert_eq!(p.home, std::env::var("HOME").ok());
    assert!(p.home_fs.is_some(), "{p:?}");
    assert_eq!(p.info.has_tmux, on_path("tmux"));
}

fn on_path(program: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|path| {
        std::env::split_paths(&path).any(|dir| Path::new(&dir).join(program).is_file())
    })
}

// ─── Runner ─────────────────────────────────────────────────────────────────────────────────

fn run_tests() -> ExitCode {
    let cases: &[(&str, fn())] = &[
        ("exact_argument_list", exact_argument_list),
        (
            "hostile_argv_survives_the_remote_shell",
            hostile_argv_survives_the_remote_shell,
        ),
        (
            "hostile_hosts_never_reach_ssh",
            hostile_hosts_never_reach_ssh,
        ),
        (
            "askpass_round_trip_and_cancel",
            askpass_round_trip_and_cancel,
        ),
        ("host_key_prompt", host_key_prompt),
        ("timeouts_and_exit_codes", timeouts_and_exit_codes),
        ("resolve_uses_ssh_g", resolve_uses_ssh_g),
        ("probe_linux", probe_linux),
        ("probe_macos", probe_macos),
        ("probe_slurm_login_node", probe_slurm_login_node),
        ("probe_box_without_tmux", probe_box_without_tmux),
        ("probe_garbage_is_an_error", probe_garbage_is_an_error),
        ("probe_script_runs_under_sh", probe_script_runs_under_sh),
    ];
    let filters: Vec<String> = std::env::args()
        .skip(1)
        .filter(|a| !a.starts_with('-'))
        .collect();
    let list_only = std::env::args().any(|a| a == "--list");
    let mut failed = Vec::new();
    let mut ran = 0;
    for (name, case) in cases {
        if !filters.is_empty() && !filters.iter().any(|f| name.contains(f.as_str())) {
            continue;
        }
        if list_only {
            println!("{name}: test");
            continue;
        }
        ran += 1;
        let ok = std::panic::catch_unwind(case).is_ok();
        println!("test {name} ... {}", if ok { "ok" } else { "FAILED" });
        if !ok {
            failed.push(*name);
        }
    }
    if list_only {
        return ExitCode::SUCCESS;
    }
    println!(
        "\ntest result: {}. {} passed; {} failed",
        if failed.is_empty() { "ok" } else { "FAILED" },
        ran - failed.len(),
        failed.len()
    );
    if failed.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
