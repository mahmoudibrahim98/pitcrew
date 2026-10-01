//! Drives [`Ssh`] against a fake `ssh`.
//!
//! The fake is this test binary itself: each case links it into a temporary directory as
//! `ssh`, next to a `scenario.json`. When the binary finds a scenario beside itself it acts as
//! ssh instead of running the tests: it logs its arguments, runs askpass prompts the way ssh
//! does (re-asking after a failure, and "sending" whatever it got, an empty string when askpass
//! failed), writes its own failures to the `-E` log, and prints canned output. Hence
//! `harness = false` and a small runner below.
//!
//! With `PITCREW_FAKE_APP` set, the binary plays the PitCrew app instead: it runs one call and
//! quits abruptly as soon as it is asked for a password, the way a crash or a forced quit
//! would.

// Test code; clippy's allow-unwrap-in-tests only sees `#[test]` functions.
#![allow(clippy::unwrap_used)]

use pitcrew_protocol::model::Scheduler;
use pitcrew_remote::{
    Limits, Output, PromptCancel, PromptFuture, PromptHandler, PromptKind, PromptRequest, Reply,
    Secret, Ssh, SshError,
};
use serde::{Deserialize, Serialize};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const SCENARIO: &str = "scenario.json";
const LOG: &str = "log.json";
/// One line per credential the fake "sent" to the server: `text`, or `empty` when askpass
/// failed.
const SENT: &str = "sent.log";
/// The pid of a child the fake leaves behind (see [`Scenario::linger`]).
const LINGER: &str = "linger.pid";
/// Makes the binary play the app, running the fake in this directory.
const APP: &str = "PITCREW_FAKE_APP";

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
    /// A notice to show and then close, as ssh does around a security-key touch.
    #[serde(default)]
    notice: Option<Notice>,
    /// Printed with `{TAG}` replaced by the last word of the remote command (the probe's tag).
    #[serde(default)]
    stdout: String,
    /// This many more bytes of stdout.
    #[serde(default)]
    stdout_bytes: usize,
    #[serde(default)]
    stderr: String,
    /// What ssh itself logs (to the `-E` file) before exiting.
    #[serde(default)]
    ssh_log: String,
    #[serde(default)]
    sleep_ms: u64,
    #[serde(default)]
    exit: i32,
    /// Run the remote command with this login shell (`<shell> -c <command>`), like sshd.
    #[serde(default)]
    login_shell: Option<String>,
    /// Leave a child behind that holds stdout and stderr open (like a wrapper-script
    /// `ProxyCommand`), and exit at once (Unix).
    #[serde(default)]
    linger: bool,
    /// Run askpass with this key instead of the bridge's: it then fails the handshake.
    #[serde(default)]
    askpass_key: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct ScriptedPrompt {
    text: String,
    hint: Option<String>,
    expect: String,
    /// How often ssh asks before giving up (`NumberOfPasswordPrompts`, 3 by default).
    tries: u32,
    /// Whether the answer goes to the server (a password does; a host-key answer does not).
    sent: bool,
    /// What ssh logs when it gives up.
    fail_log: String,
}

#[derive(Serialize, Deserialize)]
struct Notice {
    text: String,
    close_after_ms: u64,
}

#[derive(Serialize, Deserialize)]
struct Log {
    pid: u32,
    args: Vec<String>,
    askpass: Option<String>,
    askpass_require: Option<String>,
    askpass_prompt: Option<String>,
    has_bridge_env: bool,
}

fn main() -> ExitCode {
    if let Some(code) = act_as_ssh() {
        return ExitCode::from(code);
    }
    if let Some(dir) = std::env::var_os(APP) {
        return act_as_app(Path::new(&dir));
    }
    run_tests()
}

// ─── The fake app ───────────────────────────────────────────────────────────────────────────

/// Runs one call against the fake in `dir`, and quits the whole process when asked for
/// anything: no destructors run, so nothing kills ssh on the way out.
fn act_as_app(dir: &Path) -> ExitCode {
    struct Quit;
    impl PromptHandler for Quit {
        fn prompt(&self, _: PromptRequest, _: PromptCancel) -> PromptFuture<'_> {
            std::process::exit(0)
        }
    }
    let ssh = Ssh::new(dir.join(format!("ssh{}", std::env::consts::EXE_SUFFIX)))
        .with_runtime_dir(dir.join("rt"))
        .with_prompts(askpass(), Arc::new(Quit));
    let _ = block_on(ssh.run("cluster", &["true"]));
    // Not asked: the scenario was wrong.
    ExitCode::from(3)
}

// ─── The fake ssh ───────────────────────────────────────────────────────────────────────────

/// `None` when this binary is not a fake (no scenario beside it): then it runs the tests.
fn act_as_ssh() -> Option<u8> {
    let dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
    let scenario: Scenario =
        serde_json::from_str(&std::fs::read_to_string(dir.join(SCENARIO)).ok()?).ok()?;
    Some(fake_ssh(&scenario, &dir))
}

fn fake_ssh(scenario: &Scenario, dir: &Path) -> u8 {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let log = Log {
        pid: std::process::id(),
        args: args.clone(),
        askpass: std::env::var("SSH_ASKPASS").ok(),
        askpass_require: std::env::var("SSH_ASKPASS_REQUIRE").ok(),
        askpass_prompt: std::env::var("SSH_ASKPASS_PROMPT").ok(),
        has_bridge_env: std::env::var("PITCREW_ASKPASS_ADDR").is_ok()
            && std::env::var("PITCREW_ASKPASS_KEY").is_ok(),
    };
    std::fs::write(dir.join(LOG), serde_json::to_vec(&log).unwrap()).unwrap();
    let ssh_log = args
        .iter()
        .position(|a| a == "-E")
        .and_then(|i| args.get(i + 1))
        .cloned();
    let log_line = |text: &str| match &ssh_log {
        Some(path) => {
            let mut file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .unwrap();
            writeln!(file, "{text}").unwrap();
        }
        None => eprintln!("{text}"),
    };

    for prompt in &scenario.prompts {
        let mut answered = false;
        for _ in 0..prompt.tries.max(1) {
            let answer = ask(
                prompt.text.as_str(),
                prompt.hint.as_deref(),
                scenario.askpass_key.as_deref(),
            );
            if prompt.sent {
                let mut sent = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(dir.join(SENT))
                    .unwrap();
                let what = if answer.as_deref().unwrap_or("").is_empty() {
                    "empty"
                } else {
                    "text"
                };
                writeln!(sent, "{what}").unwrap();
                sent.flush().unwrap();
            }
            if answer.as_deref() == Some(prompt.expect.as_str()) {
                answered = true;
                break;
            }
        }
        if !answered {
            log_line(&prompt.fail_log);
            return 255;
        }
    }

    if let (Some(notice), Ok(program)) = (&scenario.notice, std::env::var("SSH_ASKPASS")) {
        let mut child = std::process::Command::new(program)
            .arg(&notice.text)
            .env("SSH_ASKPASS_PROMPT", "none")
            .spawn()
            .unwrap();
        std::thread::sleep(Duration::from_millis(notice.close_after_ms));
        let _ = child.kill();
        let _ = child.wait();
    }

    let command = args
        .iter()
        .position(|a| a == "--")
        .and_then(|i| args.get(i + 2));
    if let (Some(shell), Some(command)) = (&scenario.login_shell, command) {
        return match std::process::Command::new(shell)
            .arg("-c")
            .arg(command)
            .status()
        {
            Ok(status) => status
                .code()
                .and_then(|c| u8::try_from(c).ok())
                .unwrap_or(1),
            Err(_) => 127,
        };
    }
    if scenario.linger && cfg!(unix) {
        // A background `sleep` that keeps our stdout and stderr, and writes its pid.
        let status = std::process::Command::new("sh")
            .arg("-c")
            .arg("sleep 30 & echo $! > \"$1\"")
            .arg("sh")
            .arg(dir.join(LINGER))
            .status()
            .unwrap();
        return if status.success() { 0 } else { 1 };
    }
    std::thread::sleep(Duration::from_millis(scenario.sleep_ms));
    let tag = command
        .and_then(|c| decode(c))
        .and_then(|line| line.split(' ').next_back().map(str::to_owned))
        .unwrap_or_default();
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(scenario.stdout.replace("{TAG}", &tag).as_bytes());
    let _ = out.write_all(&vec![b'x'; scenario.stdout_bytes]);
    let _ = out.flush();
    eprint!("{}", scenario.stderr);
    if !scenario.ssh_log.is_empty() {
        log_line(scenario.ssh_log.trim_end());
    }
    u8::try_from(scenario.exit).unwrap_or(1)
}

/// Runs askpass like ssh: its output without the newline when it exits 0, else nothing.
fn ask(text: &str, hint: Option<&str>, key: Option<&str>) -> Option<String> {
    let program = std::env::var("SSH_ASKPASS").ok()?;
    let mut command = std::process::Command::new(program);
    command.arg(text);
    if let Some(hint) = hint {
        command.env("SSH_ASKPASS_PROMPT", hint);
    }
    if let Some(key) = key {
        command.env("PITCREW_ASKPASS_KEY", key);
    }
    let out = command.output().ok()?;
    out.status.success().then(|| {
        String::from_utf8_lossy(&out.stdout)
            .trim_end_matches('\n')
            .to_owned()
    })
}

/// Undoes the shell-neutral wrapper: `/bin/sh -c 'eval "$(printf "\ooo…")"'`.
fn decode(wrapped: &str) -> Option<String> {
    let escapes = wrapped
        .strip_prefix("/bin/sh -c 'eval \"$(printf \"")?
        .strip_suffix("\")\"'")?;
    let bytes = escapes
        .as_bytes()
        .chunks(4)
        .map(|c| match c {
            [b'\\', rest @ ..] => u8::from_str_radix(std::str::from_utf8(rest).ok()?, 8).ok(),
            _ => None,
        })
        .collect::<Option<Vec<u8>>>()?;
    String::from_utf8(bytes).ok()
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

    /// The credentials the fake sent, in order.
    fn sent(&self) -> Vec<String> {
        std::fs::read_to_string(self.dir.path().join(SENT))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }
}

type Answer = fn(PromptKind) -> Option<Reply>;

/// Answers by kind; `None` means the user never answers. Keeps every request and its token.
struct Handler {
    seen: Mutex<Vec<(PromptRequest, PromptCancel)>>,
    answer: Answer,
    delay: Duration,
}

impl Handler {
    fn new(answer: Answer) -> Arc<Self> {
        Self::slow(answer, Duration::ZERO)
    }

    fn slow(answer: Answer, delay: Duration) -> Arc<Self> {
        Arc::new(Self {
            seen: Mutex::new(Vec::new()),
            answer,
            delay,
        })
    }

    fn kinds(&self) -> Vec<(String, PromptKind)> {
        let seen = self.seen.lock().unwrap();
        seen.iter().map(|(r, _)| (r.host.clone(), r.kind)).collect()
    }

    fn token(&self, i: usize) -> PromptCancel {
        self.seen.lock().unwrap()[i].1.clone()
    }
}

impl PromptHandler for Handler {
    fn prompt(&self, request: PromptRequest, cancel: PromptCancel) -> PromptFuture<'_> {
        let answer = (self.answer)(request.kind);
        self.seen.lock().unwrap().push((request, cancel));
        let delay = self.delay;
        Box::pin(async move {
            tokio::time::sleep(delay).await;
            match answer {
                Some(reply) => reply,
                None => std::future::pending().await,
            }
        })
    }
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
}

fn block_on<F: std::future::Future>(future: F) -> F::Output {
    runtime().block_on(future)
}

fn password(expect: &str) -> ScriptedPrompt {
    ScriptedPrompt {
        text: "someone@cluster's password: ".to_owned(),
        hint: None,
        expect: expect.to_owned(),
        tries: 3,
        sent: true,
        fail_log: "someone@cluster: Permission denied (publickey,password).".to_owned(),
    }
}

fn otp(expect: &str) -> ScriptedPrompt {
    ScriptedPrompt {
        text: "(someone@cluster) Verification code: ".to_owned(),
        hint: None,
        expect: expect.to_owned(),
        tries: 3,
        sent: true,
        fail_log: "someone@cluster: Permission denied (keyboard-interactive).".to_owned(),
    }
}

fn host_key() -> ScriptedPrompt {
    ScriptedPrompt {
        text: HOST_KEY_PROMPT.to_owned(),
        hint: None,
        expect: "yes".to_owned(),
        tries: 1,
        sent: false,
        fail_log: "Host key verification failed.".to_owned(),
    }
}

const HOST_KEY_PROMPT: &str = "The authenticity of host 'cluster (192.0.2.10)' can't be established.\n\
     ED25519 key fingerprint is SHA256:AAAAexampleexampleexampleexampleexample.\n\
     This key is not known by any other names.\n\
     Are you sure you want to continue connecting (yes/no/[fingerprint])? ";

fn eventually(what: &str, check: impl Fn() -> bool) {
    let start = Instant::now();
    while !check() {
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "timed out: {what}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

// ─── Cases ──────────────────────────────────────────────────────────────────────────────────

fn exact_argument_list() {
    let fake = Fake::new(&Scenario {
        stdout: "hi\n".into(),
        ..Scenario::default()
    });
    let ssh = fake.ssh();
    let out = block_on(ssh.run("cluster", &["echo", "hi there"])).unwrap();
    assert_eq!(out.stdout_text(), "hi\n");

    let log = fake.log();
    // `-E <runtime dir>/log-<tag>`, removed after the call.
    let ssh_log = PathBuf::from(&log.args[2]);
    assert_eq!(ssh_log.parent().unwrap(), fake.runtime_dir());
    assert!(
        ssh_log
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("log-")
    );
    assert!(!ssh_log.exists());

    let mut want: Vec<String> = [
        "-T",
        "-E",
        log.args[2].as_str(),
        "-o",
        "LogLevel=ERROR",
        "-o",
        "ForwardAgent=no",
        "-o",
        "ForwardX11=no",
        "-o",
        "PermitLocalCommand=no",
        "-o",
        "ClearAllForwardings=yes",
        "-o",
        "RemoteCommand=none",
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
    want.extend(["--".to_owned(), "cluster".to_owned()]);
    want.push(
        r#"/bin/sh -c 'eval "$(printf "\047\145\143\150\157\047\040\047\150\151\040\164\150\145\162\145\047")"'"#
            .to_owned(),
    );
    assert_eq!(log.args, want);
    assert_eq!(decode(want.last().unwrap()).unwrap(), "'echo' 'hi there'");
    // The runner puts these in our environment; they must not reach ssh.
    assert_eq!(log.askpass, None);
    assert_eq!(log.askpass_require, None);
    assert_eq!(log.askpass_prompt, None);
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
        .with_prompts(askpass(), Handler::new(|_| Some(Reply::Cancel)));
    block_on(ssh.run("user@cluster", &["true"])).unwrap();
    let log = fake.log();
    assert!(!log.args.iter().any(|a| a == "BatchMode=yes"));
    let dash = log.args.iter().position(|a| a == "--").unwrap();
    assert_eq!(&log.args[dash..dash + 2], ["--", "user@cluster"]);
    assert_eq!(decode(&log.args[dash + 2]).unwrap(), "'true'");
    assert_eq!(log.askpass.as_deref(), Some(askpass().as_str()));
    assert_eq!(log.askpass_require.as_deref(), Some("force"));
    assert_eq!(log.askpass_prompt, None);
    assert!(log.has_bridge_env);

    // A relative askpass would be looked up on PATH: refused before ssh runs.
    let fake = Fake::new(&Scenario::default());
    let ssh = fake
        .ssh()
        .with_prompts("pitcrew-askpass", Handler::new(|_| None));
    let err = block_on(ssh.run("cluster", &["true"])).unwrap_err();
    assert!(matches!(err, SshError::InvalidArgument(_)), "{err:?}");
    assert!(!fake.ran());

    // A command over the platform's limit (32,767 characters for all of ssh's command line on
    // Windows) is refused as an argument, before spawning.
    let big = "x".repeat(pitcrew_remote::quote::MAX_REMOTE_COMMAND / 4);
    let err = block_on(fake.ssh().run("cluster", &["echo", &big])).unwrap_err();
    assert!(matches!(err, SshError::InvalidArgument(_)), "{err:?}");
    assert!(!fake.ran());
}

const HOSTILE: [&str; 21] = [
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
    // Broke out of the old quoting under fish.
    "\\'; touch pwned; #",
    "x\\",
    "; touch pwned; #",
    "!!",
];

fn hostile_argv_survives_the_remote_shell() {
    let mut argv = vec!["printf", "%s\\0"];
    argv.extend(HOSTILE);
    let fake = Fake::new(&Scenario {
        login_shell: Some("sh".into()),
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
        assert_eq!(words, HOSTILE);
    }
}

fn hostile_hosts_never_reach_ssh() {
    let fake = Fake::new(&Scenario::default());
    for host in [
        "-oProxyCommand=touch x",
        "a b",
        "a\nb",
        "",
        "user@-oProxyCommand=x",
        "a;touch x",
        "$(touch x)",
        "`touch x`",
        "a|b",
    ] {
        let err = block_on(fake.ssh().run(host, &["true"])).unwrap_err();
        assert!(matches!(err, SshError::InvalidHost(_)), "{host:?}: {err:?}");
        let err = block_on(fake.ssh().resolve(host)).unwrap_err();
        assert!(matches!(err, SshError::InvalidHost(_)), "{host:?}: {err:?}");
    }
    assert!(!fake.ran());
}

fn askpass_round_trip() {
    let scenario = Scenario {
        prompts: vec![password("s3cr3t"), otp("123456")],
        stdout: "in\n".into(),
        ..Scenario::default()
    };
    let fake = Fake::new(&scenario);
    let handler = Handler::new(|kind| match kind {
        PromptKind::Password => Some(Reply::Text(Secret::new("s3cr3t"))),
        PromptKind::Otp => Some(Reply::Text(Secret::new("123456"))),
        _ => Some(Reply::Cancel),
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
    assert_eq!(fake.sent(), ["text", "text"]);

    // A wrong answer is an authentication failure, asked three times like ssh does.
    let fake = Fake::new(&scenario);
    let handler = Handler::new(|_| Some(Reply::Text(Secret::new("nope"))));
    let ssh = fake.ssh().with_prompts(askpass(), handler.clone());
    let err = block_on(ssh.run("cluster", &["true"])).unwrap_err();
    assert!(matches!(err, SshError::AuthFailed { .. }), "{err:?}");
    assert_eq!(handler.kinds().len(), 3);
    assert_eq!(fake.sent(), ["text", "text", "text"]);
}

/// The fake behaves like ssh: when askpass fails, it sends an empty password and asks again.
/// This is what PitCrew's askpass must never let happen.
fn the_fake_sends_empty_credentials_when_askpass_fails() {
    // A missing askpass would fail at every prompt: refused before ssh runs.
    let fake = Fake::new(&Scenario {
        prompts: vec![password("s3cr3t")],
        ..Scenario::default()
    });
    let missing = fake.dir.path().join("no-such-askpass");
    let ssh = fake.ssh().with_prompts(&missing, Handler::new(|_| None));
    let err = block_on(ssh.run("cluster", &["true"])).unwrap_err();
    assert!(matches!(err, SshError::InvalidArgument(_)), "{err:?}");
    assert!(!fake.ran());

    // One that exists but always fails (not ours): the fake, like ssh, sends empty passwords.
    if cfg!(unix) {
        let ssh = fake
            .ssh()
            .with_prompts("/bin/false", Handler::new(|_| None));
        let err = block_on(ssh.run("cluster", &["true"])).unwrap_err();
        assert!(matches!(err, SshError::AuthFailed { .. }), "{err:?}");
        assert_eq!(fake.sent(), ["empty", "empty", "empty"]);
    }
}

/// An askpass that fails the handshake while the app runs (here, it has the wrong key) gives
/// up and stops ssh. The server sees it give up after its hello and fails the call, rather than
/// leaving it to look like a signal (Unix) or wait forever (Windows). Nothing is sent.
fn a_failed_handshake_fails_the_call() {
    let fake = Fake::new(&Scenario {
        prompts: vec![password("s3cr3t")],
        askpass_key: Some("07".repeat(32)),
        stdout: "in\n".into(),
        ..Scenario::default()
    });
    let handler = Handler::new(|_| Some(Reply::Text(Secret::new("s3cr3t"))));
    let ssh = fake.ssh().with_prompts(askpass(), handler.clone());
    let limits = Limits {
        max_output: None,
        timeout: Some(Duration::from_secs(20)),
    };
    let err = block_on(ssh.run_limited("cluster", &["true"], limits)).unwrap_err();
    assert!(matches!(err, SshError::Bridge(_)), "{err:?}");
    assert!(handler.kinds().is_empty(), "nobody was asked");
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(fake.sent(), Vec::<String>::new());
}

/// Cancel kills ssh before askpass hears anything: no empty password is sent, nobody is asked
/// again, and the call fails as cancelled.
fn cancel_sends_no_empty_credential() {
    for answer in [
        (|_| Some(Reply::Cancel)) as Answer,
        // Accept for a password is refused, which is a cancel too.
        |_| Some(Reply::Accept),
    ] {
        let fake = Fake::new(&Scenario {
            prompts: vec![password("s3cr3t"), otp("123456")],
            stdout: "in\n".into(),
            ..Scenario::default()
        });
        let handler = Handler::new(answer);
        let ssh = fake.ssh().with_prompts(askpass(), handler.clone());
        let err = block_on(ssh.run("cluster", &["true"])).unwrap_err();
        assert!(matches!(err, SshError::Cancelled), "{err:?}");
        assert_eq!(
            handler.kinds(),
            [("cluster".to_owned(), PromptKind::Password)]
        );
        // Had ssh lived on, askpass would now fail and ssh would send "empty".
        std::thread::sleep(Duration::from_millis(500));
        assert_eq!(fake.sent(), Vec::<String>::new());
    }
}

fn host_key_prompt() {
    let scenario = Scenario {
        prompts: vec![host_key()],
        ..Scenario::default()
    };
    let fake = Fake::new(&scenario);
    let handler = Handler::new(|kind| match kind {
        PromptKind::HostKey => Some(Reply::Accept),
        _ => Some(Reply::Cancel),
    });
    let ssh = fake.ssh().with_prompts(askpass(), handler.clone());
    block_on(ssh.run("cluster", &["true"])).unwrap();
    assert_eq!(
        handler.kinds(),
        [("cluster".to_owned(), PromptKind::HostKey)]
    );
    let seen = handler.seen.lock().unwrap();
    assert!(seen[0].0.prompt.contains("SHA256:AAAA"));
    drop(seen);

    let fake = Fake::new(&scenario);
    let ssh = fake
        .ssh()
        .with_prompts(askpass(), Handler::new(|_| Some(Reply::Cancel)));
    let err = block_on(ssh.run("cluster", &["true"])).unwrap_err();
    assert!(matches!(err, SshError::Cancelled), "{err:?}");

    // Without a handler ssh runs in batch mode and cannot ask.
    let fake = Fake::new(&Scenario {
        ssh_log: "Host key verification failed.\n".into(),
        exit: 255,
        ..Scenario::default()
    });
    let err = block_on(fake.ssh().run("cluster", &["true"])).unwrap_err();
    assert!(matches!(err, SshError::HostKeyRejected { .. }), "{err:?}");
}

/// `UpdateHostKeys=ask` in the user's config: a yes/no question. Accept says yes; Cancel says
/// no and the call goes on, since no credential is involved.
fn updated_host_keys_are_a_yes_no_question() {
    let answers: [(&str, Answer); 2] = [
        ("yes", |_| Some(Reply::Accept)),
        ("no", |_| Some(Reply::Cancel)),
    ];
    for (expect, answer) in answers {
        let fake = Fake::new(&Scenario {
            prompts: vec![ScriptedPrompt {
                text: "Accept updated hostkeys? (yes/no): ".to_owned(),
                hint: None,
                expect: expect.to_owned(),
                tries: 1,
                sent: false,
                fail_log: "unexpected answer".to_owned(),
            }],
            stdout: "in\n".into(),
            ..Scenario::default()
        });
        let handler = Handler::new(answer);
        let ssh = fake.ssh().with_prompts(askpass(), handler.clone());
        let out = block_on(ssh.run("cluster", &["true"])).unwrap();
        assert_eq!(out.stdout_text(), "in\n", "{expect}");
        assert_eq!(
            handler.kinds(),
            [("cluster".to_owned(), PromptKind::Confirm)]
        );
    }
}

/// The app quits or crashes while a password prompt is open. Nothing kills ssh on the way out,
/// so askpass sees the bridge close without an answer. It must stop ssh rather than fail, or
/// ssh sends an empty password and asks again, up to three times.
fn quitting_mid_prompt_sends_nothing() {
    if !cfg!(unix) {
        // On Windows the Job Object ends ssh together with the app.
        return;
    }
    let fake = Fake::new(&Scenario {
        prompts: vec![password("s3cr3t")],
        stdout: "in\n".into(),
        ..Scenario::default()
    });
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .env(APP, fake.dir.path())
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(0), "the app was not asked");
    let pid = fake.log().pid;
    eventually("the fake ssh to be gone", || !alive(pid));
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(fake.sent(), Vec::<String>::new());
}

/// Whether process `pid` still exists (Unix).
fn alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        i32::try_from(pid)
            .ok()
            .and_then(rustix::process::Pid::from_raw)
            .is_some_and(|pid| rustix::process::test_kill_process(pid).is_ok())
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}

/// ssh exits, but a child it started (a wrapper-script `ProxyCommand`, say) holds its output
/// open. The call times out, and the child is killed with the group: output is read to the end
/// before ssh is reaped, so the group id is still safe to signal.
fn a_lingering_child_is_killed_with_the_group() {
    if !cfg!(unix) {
        return;
    }
    let fake = Fake::new(&Scenario {
        linger: true,
        ..Scenario::default()
    });
    let limits = Limits {
        max_output: None,
        timeout: Some(Duration::from_secs(1)),
    };
    let err = block_on(fake.ssh().run_limited("cluster", &["true"], limits)).unwrap_err();
    assert!(matches!(err, SshError::TimedOut(_)), "{err:?}");
    let child: u32 = std::fs::read_to_string(fake.dir.path().join(LINGER))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    eventually("the lingering child to be killed", || !alive(child));
}

/// ssh closes a notice (e.g. after a security-key touch) by killing askpass: the handler's
/// token fires and the call goes on.
fn a_closed_notice_goes_stale() {
    let fake = Fake::new(&Scenario {
        notice: Some(Notice {
            text: "Confirm user presence for key ED25519-SK SHA256:abc".into(),
            close_after_ms: 500,
        }),
        stdout: "done\n".into(),
        ..Scenario::default()
    });
    let handler = Handler::new(|_| None);
    let ssh = fake.ssh().with_prompts(askpass(), handler.clone());
    let out = block_on(ssh.run("cluster", &["true"])).unwrap();
    assert_eq!(out.stdout_text(), "done\n");
    assert_eq!(
        handler.kinds(),
        [("cluster".to_owned(), PromptKind::Notice)]
    );
    eventually("the notice's token", || handler.token(0).is_cancelled());
}

/// Dropping the call while a prompt waits: the prompt sees cancellation, and ssh (with its
/// askpass) is gone, so it never sends the empty password it would after askpass failed.
fn dropping_the_call_cancels_a_pending_prompt() {
    let fake = Fake::new(&Scenario {
        prompts: vec![password("s3cr3t")],
        ..Scenario::default()
    });
    let handler = Handler::new(|_| None);
    let ssh = fake.ssh().with_prompts(askpass(), handler.clone());
    let rt = runtime();
    let result = rt.block_on(async {
        tokio::time::timeout(Duration::from_millis(1500), ssh.run("cluster", &["true"])).await
    });
    assert!(result.is_err(), "the call should still be waiting");
    assert_eq!(handler.kinds().len(), 1);
    let token = handler.token(0);
    rt.block_on(async {
        tokio::time::timeout(Duration::from_secs(5), token.cancelled())
            .await
            .unwrap();
    });
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(fake.sent(), Vec::<String>::new());
}

fn timeouts_and_exit_codes() {
    let failing = |logged: &str| {
        let fake = Fake::new(&Scenario {
            ssh_log: logged.to_owned(),
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

    // So is a 255 that ssh did not log, whatever the command printed.
    let fake = Fake::new(&Scenario {
        stderr: "Host key verification failed.\n".into(),
        exit: 255,
        ..Scenario::default()
    });
    let out = block_on(fake.ssh().run("cluster", &["false"])).unwrap();
    assert_eq!(out.code, Some(255));
    assert_eq!(out.stderr, b"Host key verification failed.\n");

    // And one where ssh logged only something informational.
    let fake = Fake::new(&Scenario {
        ssh_log: "Warning: Permanently added 'cluster' (ED25519) to the list of known hosts.\n"
            .into(),
        exit: 255,
        ..Scenario::default()
    });
    let out = block_on(fake.ssh().run("cluster", &["false"])).unwrap();
    assert_eq!(out.code, Some(255));

    // A disconnect reason is the server's text: it shows a failure, but picks no error kind.
    let err =
        failing("Received disconnect from 192.0.2.1 port 22:2: x\nHost key verification failed.\n");
    assert!(matches!(err, SshError::Ssh { code: 255, .. }), "{err:?}");
}

fn limits_stop_ssh() {
    // Too much output.
    let fake = Fake::new(&Scenario {
        stdout_bytes: 300_000,
        ..Scenario::default()
    });
    let limits = Limits {
        max_output: Some(100_000),
        timeout: None,
    };
    let err = block_on(fake.ssh().run_limited("cluster", &["yes"], limits)).unwrap_err();
    assert!(
        matches!(err, SshError::OutputTooLarge { limit: 100_000 }),
        "{err:?}"
    );

    // A hung command.
    let fake = Fake::new(&Scenario {
        sleep_ms: 60_000,
        ..Scenario::default()
    });
    let limits = Limits {
        max_output: None,
        timeout: Some(Duration::from_secs(1)),
    };
    let start = Instant::now();
    let err = block_on(fake.ssh().run_limited("cluster", &["sleep", "60"], limits)).unwrap_err();
    assert!(matches!(err, SshError::TimedOut(_)), "{err:?}");
    assert!(start.elapsed() < Duration::from_secs(10));

    // A user who takes longer than the limit to answer does not trip it.
    let fake = Fake::new(&Scenario {
        prompts: vec![password("s3cr3t")],
        stdout: "in\n".into(),
        ..Scenario::default()
    });
    let handler = Handler::slow(
        |_| Some(Reply::Text(Secret::new("s3cr3t"))),
        Duration::from_secs(5),
    );
    let ssh = fake.ssh().with_prompts(askpass(), handler);
    let limits = Limits {
        max_output: None,
        timeout: Some(Duration::from_secs(2)),
    };
    let out = block_on(ssh.run_limited("cluster", &["true"], limits)).unwrap();
    assert_eq!(out.stdout_text(), "in\n");
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

    // `ssh -G` runs the config's `Match exec` commands, which may hang.
    let fake = Fake::new(&Scenario {
        sleep_ms: 60_000,
        ..Scenario::default()
    });
    let limits = Limits {
        timeout: Some(Duration::from_secs(1)),
        ..pitcrew_remote::RESOLVE_LIMITS
    };
    let start = Instant::now();
    let err = block_on(fake.ssh().resolve_with("cluster", limits)).unwrap_err();
    assert!(matches!(err, SshError::TimedOut(_)), "{err:?}");
    assert!(start.elapsed() < Duration::from_secs(10));
    assert_eq!(
        pitcrew_remote::RESOLVE_LIMITS.timeout,
        Some(Duration::from_secs(10))
    );
}

/// Runs the probe against canned output, where `{TAG}` stands for the call's tag.
fn probe_scenario(scenario: Scenario) -> Result<pitcrew_remote::Probe, SshError> {
    let fake = Fake::new(&scenario);
    let probe = block_on(fake.ssh().probe("box"));
    let log = fake.log();
    let line = decode(log.args.last().unwrap()).unwrap();
    assert!(line.starts_with("'sh' -c '"), "{line}");
    probe
}

fn probe_fixture(stdout: &str) -> pitcrew_remote::Probe {
    probe_scenario(Scenario {
        stdout: stdout.to_owned(),
        ..Scenario::default()
    })
    .unwrap()
}

fn probe_linux() {
    let p = probe_fixture(
        "@@pitcrew-probe-begin-{TAG}\nos=Linux\narch=x86_64\nhostname=workstation\n\
         home=/home/someone\ntmux_found=1\ntmux=tmux 3.3a\nsbatch=0\nsqueue=0\nfs=ext2/ext3\n\
         @@pitcrew-probe-end-{TAG}\n",
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
        "@@pitcrew-probe-begin-{TAG}\r\nos=Darwin\r\narch=arm64\r\nhostname=laptop.local\r\n\
         home=/Users/someone\r\ntmux_found=1\r\ntmux=tmux next-3.5\r\nsbatch=0\r\nsqueue=0\r\n\
         fs=apfs\r\n@@pitcrew-probe-end-{TAG}\r\n",
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
         @@pitcrew-probe-begin-{TAG}\nos=Linux\narch=x86_64\nhostname=login01.cluster.example.org\n\
         home=/shared/home/someone\ntmux_found=1\ntmux=tmux 2.7\nsbatch=1\nsqueue=1\nfs=nfs\n\
         @@pitcrew-probe-end-{TAG}\nlogout message\n",
    );
    assert_eq!(p.info.os, "linux");
    assert_eq!(p.info.scheduler, Some(Scheduler::Slurm));
    assert!(p.has_sbatch && p.has_squeue);
    assert!(p.info.home_on_network_fs);
    assert_eq!(p.home_fs.as_deref(), Some("nfs"));
}

fn probe_box_without_tmux() {
    let p = probe_fixture(
        "@@pitcrew-probe-begin-{TAG}\nos=Linux\narch=aarch64\nhostname=\nhome=/root\n\
         tmux_found=0\nsbatch=1\nsqueue=0\nfs=\n@@pitcrew-probe-end-{TAG}\n",
    );
    assert!(!p.info.has_tmux);
    assert_eq!(p.tmux_version, None);
    // sbatch alone is not a usable SLURM.
    assert_eq!(p.info.scheduler, None);
    assert_eq!(p.info.hostname, "box");
    // Unknown filesystem: possibly networked.
    assert_eq!(p.home_fs, None);
    assert!(p.info.home_on_network_fs);
}

fn probe_failures_are_errors() {
    let unexpected = |scenario: Scenario| {
        let err = probe_scenario(scenario).unwrap_err();
        assert!(matches!(err, SshError::UnexpectedOutput(_)), "{err:?}");
        err.to_string()
    };
    unexpected(Scenario {
        stdout: "This account is disabled.\n".into(),
        exit: 1,
        ..Scenario::default()
    });
    // A full report, but the script failed.
    let full = "@@pitcrew-probe-begin-{TAG}\nos=Linux\n@@pitcrew-probe-end-{TAG}\n";
    let why = unexpected(Scenario {
        stdout: full.into(),
        exit: 1,
        ..Scenario::default()
    });
    assert!(why.contains("exited"), "{why}");
    // Cut off before the end marker.
    let why = unexpected(Scenario {
        stdout: "@@pitcrew-probe-begin-{TAG}\nos=Linux\narch=x86_64\n".into(),
        ..Scenario::default()
    });
    assert!(why.contains("cut off"), "{why}");
    // Markers without this call's tag.
    unexpected(Scenario {
        stdout: "@@pitcrew-probe-begin-0000\nos=Linux\n@@pitcrew-probe-end-0000\n".into(),
        ..Scenario::default()
    });
    // A login shell that could change PitCrew's commands on their way to /bin/sh.
    let err = probe_scenario(Scenario {
        stdout: "@@pitcrew-probe-begin-{TAG}\nos=Linux\nshell=/usr/bin/xonsh\nfs=xfs\n\
                 @@pitcrew-probe-end-{TAG}\n"
            .into(),
        ..Scenario::default()
    })
    .unwrap_err();
    assert!(
        matches!(&err, SshError::UnsupportedShell(shell) if shell == "/usr/bin/xonsh"),
        "{err:?}"
    );
}

fn probe_is_bounded() {
    let fake = Fake::new(&Scenario {
        stdout: "@@pitcrew-probe-begin-{TAG}\n".into(),
        stdout_bytes: 2 * 1024 * 1024,
        ..Scenario::default()
    });
    let err = block_on(fake.ssh().probe("box")).unwrap_err();
    assert!(
        matches!(err, SshError::OutputTooLarge { limit } if limit == 1024 * 1024),
        "{err:?}"
    );
    assert_eq!(
        pitcrew_remote::PROBE_LIMITS.timeout,
        Some(Duration::from_secs(30))
    );

    // A hung `stat` on a dead NFS mount.
    let fake = Fake::new(&Scenario {
        sleep_ms: 60_000,
        ..Scenario::default()
    });
    let limits = Limits {
        timeout: Some(Duration::from_secs(1)),
        ..pitcrew_remote::PROBE_LIMITS
    };
    let start = Instant::now();
    let err = block_on(fake.ssh().probe_with("box", limits)).unwrap_err();
    assert!(matches!(err, SshError::TimedOut(_)), "{err:?}");
    assert!(start.elapsed() < Duration::from_secs(10));
}

/// The real script under the local `sh` (Unix only).
fn probe_script_runs_under_sh() {
    if !cfg!(unix) {
        return;
    }
    let fake = Fake::new(&Scenario {
        login_shell: Some("sh".into()),
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

/// Set once the runner has re-run itself with a hostile environment.
const REEXEC: &str = "PITCREW_FAKE_SSH_REEXEC";

fn run_tests() -> ExitCode {
    // Run the cases with ssh's askpass variables already in the environment, as a desktop
    // session may have them, to check that they never reach ssh.
    if std::env::var_os(REEXEC).is_none() {
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(std::env::args_os().skip(1))
            .env(REEXEC, "1")
            .env("SSH_ASKPASS", "/nonexistent/askpass")
            .env("SSH_ASKPASS_REQUIRE", "force")
            .env("SSH_ASKPASS_PROMPT", "confirm")
            .status()
            .unwrap();
        return if status.success() {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        };
    }
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
        ("askpass_round_trip", askpass_round_trip),
        (
            "the_fake_sends_empty_credentials_when_askpass_fails",
            the_fake_sends_empty_credentials_when_askpass_fails,
        ),
        (
            "cancel_sends_no_empty_credential",
            cancel_sends_no_empty_credential,
        ),
        ("host_key_prompt", host_key_prompt),
        (
            "updated_host_keys_are_a_yes_no_question",
            updated_host_keys_are_a_yes_no_question,
        ),
        ("a_closed_notice_goes_stale", a_closed_notice_goes_stale),
        (
            "dropping_the_call_cancels_a_pending_prompt",
            dropping_the_call_cancels_a_pending_prompt,
        ),
        (
            "quitting_mid_prompt_sends_nothing",
            quitting_mid_prompt_sends_nothing,
        ),
        (
            "a_failed_handshake_fails_the_call",
            a_failed_handshake_fails_the_call,
        ),
        ("timeouts_and_exit_codes", timeouts_and_exit_codes),
        ("limits_stop_ssh", limits_stop_ssh),
        (
            "a_lingering_child_is_killed_with_the_group",
            a_lingering_child_is_killed_with_the_group,
        ),
        ("resolve_uses_ssh_g", resolve_uses_ssh_g),
        ("probe_linux", probe_linux),
        ("probe_macos", probe_macos),
        ("probe_slurm_login_node", probe_slurm_login_node),
        ("probe_box_without_tmux", probe_box_without_tmux),
        ("probe_failures_are_errors", probe_failures_are_errors),
        ("probe_is_bounded", probe_is_bounded),
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
