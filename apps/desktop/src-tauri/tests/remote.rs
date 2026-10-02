//! Remote workspaces end to end, through Tauri's IPC on the mock runtime with the app's real
//! ACL: probe, plan, add, requests and sockets through the tunnel, retry, restart, remove;
//! SLURM's exact job script; SSH's prompts in the app; a hub claiming another workspace's id; and
//! no token anywhere it must not be.
//!
//! The machine (`hpc-login`) is fake: this computer's `/bin/sh` in a temporary home, behind a
//! fake `ssh` that plays OpenSSH's calls, ControlMasters and forwards included (`remote/fake.rs`).
//! What runs there is real: `pitcrew-remote`'s scripts deploy and start the helper, and the
//! helper is the real `pitcrewd`, serving the demo workspace (`serve --demo`).
//!
//! `pitcrewd` is `PITCREW_TEST_PITCREWD` if set, else built once from the root workspace into
//! this target's temporary folder.
//!
//! This binary is also the fake `ssh` and `pitcrew-askpass`; hence `harness = false`. Cases run
//! one after another, all under one log capture at trace level.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::process::ExitCode;

#[cfg(not(unix))]
fn main() -> ExitCode {
    println!("skipped: the remote cases run the fake machine with a local Unix sh");
    ExitCode::SUCCESS
}

#[cfg(unix)]
fn main() -> ExitCode {
    if let Some(code) = fake::act() {
        return code;
    }
    unix::run()
}

#[cfg(unix)]
#[path = "remote/fake.rs"]
mod fake;

#[cfg(unix)]
mod unix {
    use crate::fake::{self, FINGERPRINT, HOST, Machine};
    use pitcrew_desktop::app::{self, MAIN, WORKSPACES_EVENT};
    use pitcrew_desktop::gateway::{BoxFuture, Connected, Connector, Gateway, GatewayError};
    use pitcrew_desktop::keychain::{MemoryStore, TokenStore};
    use pitcrew_desktop::logging;
    use pitcrew_desktop::navigate::Navigator;
    use pitcrew_desktop::registry::{
        self, Connection, LauncherKind, Registry, RemoteConnection, WorkspaceKind, WorkspaceRecord,
        WorkspaceState,
    };
    use pitcrew_desktop::remote::prompt::{PROMPT_CLOSED_EVENT, PROMPT_EVENT};
    use pitcrew_desktop::remote::{GatewayPrompt, Helpers, PromptHub, RemoteOptions, Remotes};
    use pitcrew_desktop::token::DeviceToken;
    use serde_json::{Value, json};
    use std::io::Write;
    use std::path::{Path, PathBuf};
    use std::process::{Command, ExitCode};
    use std::sync::mpsc;
    use std::sync::{Arc, Mutex, OnceLock};
    use std::time::{Duration, Instant};
    use tauri::ipc::{CallbackFn, InvokeBody, InvokeResponseBody};
    use tauri::test::{INVOKE_KEY, MockRuntime, get_ipc_response, mock_builder};
    use tauri::webview::InvokeRequest;
    use tauri::{Listener as _, Manager as _, WebviewWindow, WebviewWindowBuilder};
    use tracing_subscriber::EnvFilter;
    use tracing_subscriber::fmt::MakeWriter;

    /// The demo workspace's id and name, as the real hub reports them (`crates/fixtures`).
    const DEMO: &str = "01JB000000000000000WSP0001";
    const DEMO_NAME: &str = "Demo Lab";
    /// The fake machine's password, when it asks for one.
    const PASSWORD: &str = "correct-horse";

    /// A connector to nowhere, for a workspace registered by hand.
    struct Nowhere;

    impl Connector for Nowhere {
        fn connect(&self) -> BoxFuture<'_, Result<Connected, GatewayError>> {
            Box::pin(async { Err(GatewayError::unreachable("nowhere")) })
        }
    }

    // ─── The runner ───────────────────────────────────────────────────────────────────────

    type Case = (&'static str, fn());

    const CASES: &[Case] = &[
        ("a_direct_helper_end_to_end", a_direct_helper_end_to_end),
        (
            "slurm_submits_exactly_the_planned_script",
            slurm_submits_exactly_the_planned_script,
        ),
        (
            "prompts_round_trip_and_a_cancel_fails_the_add_cleanly",
            prompts_round_trip_and_a_cancel_fails_the_add_cleanly,
        ),
        (
            "a_hub_cannot_take_another_workspaces_id",
            a_hub_cannot_take_another_workspaces_id,
        ),
        (
            "a_saved_remote_comes_back_after_a_restart",
            a_saved_remote_comes_back_after_a_restart,
        ),
        (
            "a_cancelled_sign_in_while_reconnecting_is_retried",
            a_cancelled_sign_in_while_reconnecting_is_retried,
        ),
        ("retries_are_coalesced", retries_are_coalesced),
        (
            "an_old_ssh_gets_no_prompt_answered",
            an_old_ssh_gets_no_prompt_answered,
        ),
        (
            "a_name_set_on_the_hub_shows_after_a_reconnect",
            a_name_set_on_the_hub_shows_after_a_reconnect,
        ),
        ("an_add_can_be_cancelled", an_add_can_be_cancelled),
        (
            "plans_expire_and_missing_programs_are_clear_errors",
            plans_expire_and_missing_programs_are_clear_errors,
        ),
    ];

    pub fn run() -> ExitCode {
        logs();
        let filter = std::env::args().skip(1).find(|a| !a.starts_with('-'));
        let mut failed = Vec::new();
        let mut ran = 0;
        for (name, case) in CASES {
            if filter.as_deref().is_some_and(|f| !name.contains(f)) {
                continue;
            }
            ran += 1;
            println!("test {name} ...");
            let started = Instant::now();
            match std::panic::catch_unwind(case) {
                Ok(()) => println!("test {name} ... ok ({} ms)", started.elapsed().as_millis()),
                Err(_) => {
                    println!("test {name} ... FAILED");
                    failed.push(*name);
                }
            }
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

    // ─── Logs, captured for the whole run ────────────────────────────────────────────────

    #[derive(Clone, Default)]
    struct Capture(Arc<Mutex<Vec<u8>>>);

    impl Write for Capture {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> MakeWriter<'a> for Capture {
        type Writer = Capture;
        fn make_writer(&'a self) -> Self::Writer {
            self.clone()
        }
    }

    static LOGS: OnceLock<Capture> = OnceLock::new();

    fn logs() -> &'static Capture {
        LOGS.get_or_init(|| {
            let capture = Capture::default();
            tracing::subscriber::set_global_default(logging::subscriber(
                EnvFilter::new("trace"),
                capture.clone(),
            ))
            .unwrap();
            logging::bridge_log();
            capture
        })
    }

    fn log_text() -> String {
        String::from_utf8_lossy(&logs().0.lock().unwrap()).into_owned()
    }

    // ─── The real pitcrewd ───────────────────────────────────────────────────────────────

    /// `PITCREW_TEST_PITCREWD`, else the root workspace's `pitcrewd`, built once.
    fn pitcrewd() -> &'static Path {
        static PITCREWD: OnceLock<PathBuf> = OnceLock::new();
        PITCREWD.get_or_init(|| {
            if let Some(path) = std::env::var_os("PITCREW_TEST_PITCREWD") {
                return PathBuf::from(path);
            }
            let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
            let target = Path::new(env!("CARGO_TARGET_TMPDIR")).join("pitcrewd");
            println!(
                "building pitcrewd (root workspace) into {}",
                target.display()
            );
            let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
            let status = Command::new(cargo)
                .args([
                    "build",
                    "--locked",
                    "-p",
                    "pitcrew-daemon",
                    "--bin",
                    "pitcrewd",
                ])
                .arg("--manifest-path")
                .arg(root.join("Cargo.toml"))
                .env("CARGO_TARGET_DIR", &target)
                .status()
                .unwrap();
            assert!(status.success(), "building pitcrewd failed");
            target.join("debug").join("pitcrewd")
        })
    }

    // ─── The app around a machine ────────────────────────────────────────────────────────

    /// How the prompt responder (the UI) answers.
    #[derive(Clone, Debug, Default)]
    struct Answers {
        /// The password to type; `None` cancels.
        password: Option<String>,
        /// Whether to trust an unknown host key.
        accept_host_key: bool,
        /// Leave prompts open: the case answers them itself.
        hold: bool,
    }

    struct World {
        app: tauri::App<MockRuntime>,
        main: WebviewWindow<MockRuntime>,
        registry: Arc<Registry>,
        registry_file: PathBuf,
        tokens: Arc<MemoryStore>,
        machine: Arc<Machine>,
        /// `(event, payload)`, in order.
        events: Arc<Mutex<Vec<(String, String)>>>,
        /// What went to the webview on channels: `(channel, text)`.
        channels: Arc<Mutex<Vec<(u32, String)>>>,
        /// Every command's result or error, as text.
        results: Arc<Mutex<Vec<String>>>,
        answers: Arc<Mutex<Answers>>,
        data: Arc<tempfile::TempDir>,
    }

    /// A fresh machine and a fresh app.
    fn world(slurm: bool, tweak: impl FnOnce(&mut RemoteOptions, &Machine)) -> World {
        build(
            Arc::new(Machine::new(slurm, pitcrewd())),
            Arc::new(tempfile::tempdir().unwrap()),
            Arc::new(MemoryStore::default()),
            Answers::default(),
            tweak,
        )
    }

    /// The app over `machine`, with its registry file in `data` and its keychain `tokens`, as
    /// `app::setup` makes it: the saved remote workspaces are resumed, the UI answering prompts
    /// as `answers` says.
    fn build(
        machine: Arc<Machine>,
        data: Arc<tempfile::TempDir>,
        tokens: Arc<MemoryStore>,
        answers: Answers,
        tweak: impl FnOnce(&mut RemoteOptions, &Machine),
    ) -> World {
        let registry_file = data.path().join("data").join(registry::FILE_NAME);
        let registry = Arc::new(Registry::load(registry_file.clone()));
        let channels: Arc<Mutex<Vec<(u32, String)>>> = Arc::default();
        let captured = Arc::clone(&channels);
        let builder = mock_builder().channel_interceptor(move |_w, callback, _i, body| {
            let text = match body {
                InvokeResponseBody::Json(text) => text.clone(),
                InvokeResponseBody::Raw(bytes) => String::from_utf8_lossy(bytes).into_owned(),
            };
            captured.lock().unwrap().push((callback.0, text));
            true
        });
        let app = app::configure(builder)
            .manage(Gateway::new(Arc::clone(&registry)))
            .manage(Navigator::default())
            .build(pitcrew_desktop::context())
            .unwrap();
        let handle = app.handle().clone();
        registry.on_change(move |list| app::emit_workspaces(&handle, list));
        let asker = app.handle().clone();
        let prompts = Arc::new(PromptHub::new(move |e| app::emit_prompt(&asker, e)));
        let mut options = RemoteOptions::new(
            Ok(machine.ssh.clone()),
            Ok(machine.askpass.clone()),
            Helpers::in_dir(machine.helpers.clone()),
        );
        options.runtime_dir = Some(machine.runtime.clone());
        options.ssh_config = Some(machine.ssh_config.clone());
        options.home = Some(machine.laptop_home.clone());
        options.sites_dir = Some(machine.laptop_home.join("sites"));
        tweak(&mut options, &machine);
        let remotes = Remotes::new(
            options,
            Arc::clone(&registry),
            Arc::clone(&tokens) as Arc<dyn TokenStore>,
            prompts,
            tauri::async_runtime::handle().inner().clone(),
        );
        app.manage(remotes);
        let main = WebviewWindowBuilder::new(&app, MAIN, Default::default())
            .build()
            .unwrap();

        let events: Arc<Mutex<Vec<(String, String)>>> = Arc::default();
        let (prompted, to_answer) = mpsc::channel::<String>();
        for event in [WORKSPACES_EVENT, PROMPT_EVENT, PROMPT_CLOSED_EVENT] {
            let seen = Arc::clone(&events);
            let prompted = prompted.clone();
            app.listen_any(event, move |e| {
                seen.lock()
                    .unwrap()
                    .push((event.to_owned(), e.payload().to_owned()));
                if event == PROMPT_EVENT {
                    let _ = prompted.send(e.payload().to_owned());
                }
            });
        }
        // The UI: it answers each prompt as `answers` says, through the IPC.
        let answers = Arc::new(Mutex::new(answers));
        let results: Arc<Mutex<Vec<String>>> = Arc::default();
        {
            let answers = Arc::clone(&answers);
            let results = Arc::clone(&results);
            let window = main.clone();
            std::thread::spawn(move || {
                for payload in to_answer {
                    let prompt: Value = serde_json::from_str(&payload).unwrap();
                    let now = answers.lock().unwrap().clone();
                    if now.hold {
                        continue;
                    }
                    let args = match prompt["kind"].as_str() {
                        Some("host_key") => {
                            json!({ "id": prompt["id"], "accept": now.accept_host_key })
                        }
                        _ => match now.password {
                            Some(password) => json!({ "id": prompt["id"], "answer": password }),
                            None => json!({ "id": prompt["id"] }),
                        },
                    };
                    let result = invoke(&window, "gateway_prompt_reply", args);
                    results.lock().unwrap().push(text_of(&result));
                }
            });
        }
        app.state::<Remotes>().resume();
        World {
            app,
            main,
            registry,
            registry_file,
            tokens,
            machine,
            events,
            channels,
            results,
            answers,
            data,
        }
    }

    impl World {
        /// Invokes `cmd` from the main window, as the webview would, and keeps the outcome.
        fn call(&self, cmd: &str, args: Value) -> Result<Value, Value> {
            let result = invoke(&self.main, cmd, args);
            self.results.lock().unwrap().push(text_of(&result));
            result
        }

        /// Plans and adds the machine with the direct launcher; the workspace's id.
        fn add_direct(&self, channel: u32) -> Result<String, Value> {
            let plan = self.call(
                "gateway_remote_plan",
                json!({ "req": { "host": HOST, "launcher": "direct" } }),
            )?;
            let added = self.call(
                "gateway_remote_add",
                json!({ "plan": plan["plan"], "events": format!("__CHANNEL__:{channel}") }),
            )?;
            Ok(added["id"].as_str().unwrap().to_owned())
        }

        fn channel(&self, id: u32) -> Vec<Value> {
            self.channels
                .lock()
                .unwrap()
                .iter()
                .filter(|(c, _)| *c == id)
                .map(|(_, text)| serde_json::from_str(text).unwrap_or(Value::String(text.clone())))
                .collect()
        }

        fn events(&self, name: &str) -> Vec<Value> {
            self.events
                .lock()
                .unwrap()
                .iter()
                .filter(|(n, _)| n == name)
                .map(|(_, p)| serde_json::from_str(p).unwrap())
                .collect()
        }

        fn remotes(&self) -> tauri::State<'_, Remotes> {
            self.app.state::<Remotes>()
        }

        /// Workspace `id`'s state now.
        fn state(&self, id: &str) -> Option<WorkspaceState> {
            self.registry
                .list()
                .into_iter()
                .find(|w| w.id == id)
                .map(|w| w.state)
        }

        /// Waits until `check` holds.
        fn wait(&self, what: &str, check: impl Fn(&Self) -> bool) {
            let deadline = Instant::now() + Duration::from_secs(60);
            while !check(self) {
                assert!(
                    Instant::now() < deadline,
                    "timed out waiting for {what}: {:?}",
                    self.registry.list()
                );
                std::thread::sleep(Duration::from_millis(50));
            }
        }

        /// Waits for a prompt to be open, and returns the oldest.
        fn open_prompt(&self) -> GatewayPrompt {
            self.wait("an open prompt", |w| {
                !w.remotes().prompts().open().is_empty()
            });
            self.remotes().prompts().open().remove(0)
        }

        /// Everything the webview could have seen, as text.
        fn seen_by_the_webview(&self) -> Vec<String> {
            let mut all = self.results.lock().unwrap().clone();
            all.extend(self.channels.lock().unwrap().iter().map(|(_, t)| t.clone()));
            all.extend(self.events.lock().unwrap().iter().map(|(_, p)| p.clone()));
            all
        }

        /// The app quits and starts again, over the same machine, keychain and registry file,
        /// its UI answering as `answers` says.
        fn restart(&self, answers: Answers) -> World {
            self.shutdown();
            build(
                Arc::clone(&self.machine),
                Arc::clone(&self.data),
                Arc::clone(&self.tokens),
                answers,
                |_, _| {},
            )
        }

        fn shutdown(&self) {
            tauri::async_runtime::block_on(self.remotes().shutdown());
        }
    }

    impl Drop for World {
        fn drop(&mut self) {
            self.shutdown();
        }
    }

    fn invoke(window: &WebviewWindow<MockRuntime>, cmd: &str, args: Value) -> Result<Value, Value> {
        get_ipc_response(
            window,
            InvokeRequest {
                cmd: cmd.into(),
                callback: CallbackFn(0),
                error: CallbackFn(1),
                url: "tauri://localhost".parse().unwrap(),
                body: InvokeBody::Json(args),
                headers: Default::default(),
                invoke_key: INVOKE_KEY.to_string(),
            },
        )
        .map(|body| body.deserialize::<Value>().unwrap())
    }

    fn text_of(result: &Result<Value, Value>) -> String {
        match result {
            Ok(v) => format!("ok {v}"),
            Err(e) => format!("err {e}"),
        }
    }

    /// The step messages of an add's progress channel: `(step, state)`.
    fn steps_of(messages: &[Value]) -> Vec<(String, String)> {
        messages
            .iter()
            .map(|m| {
                (
                    m["step"].as_str().unwrap_or("").to_owned(),
                    m["state"].as_str().unwrap_or("").to_owned(),
                )
            })
            .collect()
    }

    /// The hub's device token, read on the machine as the person could.
    fn hub_token(machine: &Machine) -> String {
        let out = Command::new(pitcrewd())
            .args(["token", "show-path"])
            .env_clear()
            .env("HOME", &machine.home)
            .env("PATH", "/usr/bin:/bin")
            .output()
            .unwrap();
        let path = String::from_utf8(out.stdout).unwrap();
        std::fs::read_to_string(path.trim())
            .unwrap()
            .trim()
            .to_owned()
    }

    /// The token, and the part after its prefix, appear in nothing the webview saw (results,
    /// errors, channel messages, events) and in no log line. `logged` proves the case's lines
    /// are in the capture.
    fn no_token(w: &World, token: &str, logged: &str) {
        let secret = token.split_once('_').map_or(token, |(_, rest)| rest);
        assert!(secret.len() >= 16, "a real token");
        let seen = w.seen_by_the_webview();
        assert!(seen.len() > 5, "{seen:#?}");
        for text in &seen {
            assert!(!text.contains(secret), "the webview saw the token: {text}");
            assert!(!text.contains("pitcrew.bearer."), "{text}");
        }
        let logs = log_text();
        assert!(logs.contains(logged), "the log is captured");
        for line in logs.lines() {
            assert!(!line.contains(secret), "a log line holds the token: {line}");
        }
    }

    /// Whether this computer has tmux where the fake machine's `PATH` finds it.
    fn has_tmux() -> bool {
        ["/usr/bin/tmux", "/bin/tmux"]
            .iter()
            .any(|p| Path::new(p).exists())
    }

    // ─── The cases ───────────────────────────────────────────────────────────────────────

    /// Probe, plan and add with the direct launcher, on a machine whose start-up files talk:
    /// the workspace is `ready`, requests and sockets reach the real hub through the tunnel, the
    /// transport is remembered, a probe sees the helper running, another window can call none
    /// of the remote commands (with a real prompt open and a real workspace), and remove stops
    /// the helper and deletes the keychain entry. No token reaches the webview or the logs.
    fn a_direct_helper_end_to_end() {
        let w = world(false, |_, _| {});
        // Login banners (and a false token marker) before every command's output.
        w.machine.noisy();

        // The host list comes from the (temporary) ssh config: concrete hosts only.
        let hosts = w.call("gateway_ssh_hosts", json!({})).unwrap();
        assert_eq!(hosts, json!({ "hosts": [HOST] }));

        let probe = w
            .call("gateway_remote_probe", json!({ "host": HOST }))
            .unwrap();
        assert_eq!(probe["host"], HOST);
        assert_eq!(
            probe["os"],
            if cfg!(target_os = "macos") {
                "macos"
            } else {
                "linux"
            }
        );
        assert!(probe.get("helper").is_none(), "nothing there yet: {probe}");
        assert!(probe.get("slurm").is_none(), "{probe}");
        assert_eq!(probe.get("tmux").is_some(), has_tmux(), "{probe}");
        if let Some(tmux) = probe.get("tmux") {
            assert!(!tmux["version"].as_str().unwrap().is_empty(), "{probe}");
        }
        let e = w
            .call(
                "gateway_remote_probe",
                json!({ "host": "-oProxyCommand=evil" }),
            )
            .unwrap_err();
        assert_eq!(e["code"], "invalid");
        let e = w
            .call("gateway_remote_probe", json!({ "host": "elsewhere" }))
            .unwrap_err();
        assert_eq!(e["code"], "unreachable", "{e}");

        let plan = w
            .call(
                "gateway_remote_plan",
                json!({ "req": { "host": HOST, "launcher": "direct" } }),
            )
            .unwrap();
        let steps: Vec<String> = serde_json::from_value(plan["steps"].clone()).unwrap();
        assert_eq!(steps.len(), 4, "{plan}");
        let version = fake::version_of(pitcrewd());
        assert!(
            steps[0].contains(&format!("pitcrewd {version}")),
            "{steps:?}"
        );
        assert!(steps[0].contains("/.pitcrew/bin"), "{steps:?}");
        assert!(plan.get("jobScript").is_none());
        // Planning changed nothing on the machine.
        assert!(!w.machine.home.join(".pitcrew").exists());

        let added = w
            .call(
                "gateway_remote_add",
                json!({ "plan": plan["plan"], "events": "__CHANNEL__:70" }),
            )
            .unwrap();
        assert_eq!(added["kind"], "remote", "{added}");
        assert_eq!(added["state"], "ready", "{added}");
        assert_eq!(added["name"], DEMO_NAME, "{added}");
        let id = added["id"].as_str().unwrap().to_owned();
        let mut expected = Vec::new();
        for step in &steps {
            expected.push((step.clone(), "running".to_owned()));
            expected.push((step.clone(), "done".to_owned()));
        }
        expected.push(("add".to_owned(), "done".to_owned()));
        // Upload progress comes as more `running` messages of the first step.
        let mut deduped: Vec<(String, String)> = Vec::new();
        for item in steps_of(&w.channel(70)) {
            if deduped.last() != Some(&item) {
                deduped.push(item);
            }
        }
        assert_eq!(deduped, expected);

        // A plan is used once.
        let e = w
            .call(
                "gateway_remote_add",
                json!({ "plan": plan["plan"], "events": "__CHANNEL__:71" }),
            )
            .unwrap_err();
        assert_eq!(e["code"], "invalid");
        assert_eq!(
            w.channel(71).last().unwrap(),
            &json!({ "step": "add", "state": "failed", "detail": e["message"] })
        );

        // Another window can call none of the remote commands: Tauri's ACL refuses them (a
        // plain string, not a GatewayError) before any of them runs, with a real prompt open,
        // a real workspace and a real (used) plan.
        w.machine.require_password(PASSWORD);
        w.answers.lock().unwrap().hold = true;
        let probing = {
            let window = w.main.clone();
            std::thread::spawn(move || {
                invoke(&window, "gateway_remote_probe", json!({ "host": HOST }))
            })
        };
        let open = w.open_prompt();
        assert_eq!(serde_json::to_value(open.kind).unwrap(), "password");
        let other = WebviewWindowBuilder::new(&w.app, "other", Default::default())
            .build()
            .unwrap();
        for (cmd, args) in [
            ("gateway_ssh_hosts", json!({})),
            ("gateway_remote_probe", json!({ "host": HOST })),
            (
                "gateway_remote_plan",
                json!({ "req": { "host": HOST, "launcher": "direct" } }),
            ),
            (
                "gateway_remote_add",
                json!({ "plan": plan["plan"], "events": "__CHANNEL__:73" }),
            ),
            ("gateway_remote_cancel", json!({ "plan": plan["plan"] })),
            ("gateway_workspace_retry", json!({ "workspace": id })),
            (
                "gateway_workspace_remove",
                json!({ "workspace": id, "stopHelper": true }),
            ),
            (
                "gateway_prompt_reply",
                json!({ "id": open.id, "answer": "from another window" }),
            ),
        ] {
            let refused = invoke(&other, cmd, args).unwrap_err();
            let message = refused.as_str().unwrap_or_else(|| {
                panic!("{cmd} from another window was not refused by the ACL: {refused}")
            });
            assert!(
                message.contains(cmd) && message.contains("not allowed"),
                "{cmd}: {message}"
            );
        }
        // Nothing ran: the prompt is still open, the workspace still there and ready.
        assert!(
            w.remotes().prompts().open().iter().any(|p| p.id == open.id),
            "the other window's reply did not reach the prompt"
        );
        assert_eq!(w.state(&id), Some(WorkspaceState::Ready));
        assert!(w.channel(73).is_empty());
        // The main window answers it; later prompts the responder answers.
        {
            let mut answers = w.answers.lock().unwrap();
            answers.hold = false;
            answers.password = Some(PASSWORD.into());
        }
        w.call(
            "gateway_prompt_reply",
            json!({ "id": open.id, "answer": PASSWORD }),
        )
        .unwrap();
        // That probe sees the helper running.
        let probe = probing.join().unwrap().unwrap();
        assert_eq!(
            probe["helper"],
            json!({ "version": version, "running": true })
        );

        // The workspace is in the list, ready, and its requests reach the real hub.
        let list = w.call("gateway_workspaces", json!({})).unwrap();
        assert!(
            list.as_array()
                .unwrap()
                .iter()
                .any(|ws| ws["id"] == id && ws["state"] == "ready"),
            "{list}"
        );
        let workspace = w
            .call(
                "gateway_request",
                json!({ "req": { "workspace": id, "method": "GET", "path": "/v1/workspace" } }),
            )
            .unwrap();
        assert_eq!(workspace["status"], 200, "{workspace}");
        assert!(
            workspace["body"].as_str().unwrap().contains(DEMO_NAME),
            "{workspace}"
        );
        let me = w
            .call(
                "gateway_request",
                json!({ "req": { "workspace": id, "method": "GET", "path": "/v1/me" } }),
            )
            .unwrap();
        assert_eq!(me["status"], 200, "{me}");
        // A socket: the live stream's hello.
        let opened = w
            .call(
                "gateway_socket_open",
                json!({ "workspace": id, "path": "/v1/stream", "events": "__CHANNEL__:72" }),
            )
            .unwrap();
        w.wait("the stream's hello", |w| {
            w.channel(72).iter().any(|m| {
                m["type"] == "text" && m["data"].as_str().unwrap_or("").contains("\"hello\"")
            })
        });
        w.call(
            "gateway_socket_close",
            json!({ "socket": opened["socket"] }),
        )
        .unwrap();

        // The transport worth remembering is saved with the workspace; no secret is.
        w.wait("the transport saved", |w| {
            std::fs::read_to_string(&w.registry_file)
                .unwrap_or_default()
                .contains("\"transport\": \"forwarded\"")
        });
        let saved = std::fs::read_to_string(&w.registry_file).unwrap();
        assert!(saved.contains("\"type\": \"remote\""), "{saved}");
        assert!(saved.contains(&format!("\"host\": \"{HOST}\"")), "{saved}");
        assert!(saved.contains("\"launcher\": \"direct\""), "{saved}");

        // The token: in the keychain (here in memory), and the hub's own (read between the
        // markers, whatever the start-up files printed).
        let token = hub_token(&w.machine);
        assert_eq!(
            w.tokens.get(&id).unwrap().map(|t| t.expose().to_owned()),
            Some(token.clone())
        );
        assert!(!saved.contains(&token));

        // An unknown workspace is unknown; stopHelper is required.
        let e = w
            .call(
                "gateway_workspace_remove",
                json!({ "workspace": "nope", "stopHelper": false }),
            )
            .unwrap_err();
        assert_eq!(e["code"], "unknown_workspace");
        let e = w
            .call("gateway_workspace_remove", json!({ "workspace": id }))
            .unwrap_err();
        assert_eq!(e["code"], "invalid", "stopHelper is required");

        // Remove, stopping the helper: the token is deleted, the workspace forgotten.
        let pid = w.machine.endpoint().unwrap()["pid"].as_i64().unwrap();
        w.call(
            "gateway_workspace_remove",
            json!({ "workspace": id, "stopHelper": true }),
        )
        .unwrap();
        assert_eq!(w.tokens.get(&id).unwrap(), None);
        assert!(w.registry.list().iter().all(|ws| ws.id != id));
        assert!(
            !std::fs::read_to_string(&w.registry_file)
                .unwrap()
                .contains(&id)
        );
        assert!(
            w.machine.endpoint().is_none(),
            "the helper's record is gone"
        );
        let pid = rustix::process::Pid::from_raw(i32::try_from(pid).unwrap()).unwrap();
        w.wait("the helper gone", |_| {
            rustix::process::test_kill_process(pid).is_err()
        });
        let e = w
            .call(
                "gateway_request",
                json!({ "req": { "workspace": id, "method": "GET", "path": "/v1/me" } }),
            )
            .unwrap_err();
        assert_eq!(e["code"], "unknown_workspace");
        let workspaces = w.events(WORKSPACES_EVENT);
        assert!(
            workspaces
                .iter()
                .any(|l| l.as_array().unwrap().iter().any(|ws| ws["id"] == id))
        );
        assert!(
            workspaces
                .last()
                .unwrap()
                .as_array()
                .unwrap()
                .iter()
                .all(|ws| ws["id"] != id)
        );

        // No token anywhere the webview or a log could see.
        no_token(&w, &token, "added a remote workspace");
    }

    /// SLURM: the plan returns the exact script, and adding submits exactly that text, even
    /// when the site recipe it was made from changed in between. Here the job stays pending: the
    /// add gives up after its wait, cancels the job it submitted, and registers nothing.
    fn slurm_submits_exactly_the_planned_script() {
        let w = world(true, |options, _| {
            options.job_wait = Duration::from_secs(3);
            options.job_poll = Duration::from_millis(500);
        });
        let probe = w
            .call("gateway_remote_probe", json!({ "host": HOST }))
            .unwrap();
        assert_eq!(
            probe["slurm"],
            json!({ "version": "slurm 23.02.7", "defaultPartition": "batch", "srunOverlap": false })
        );

        // A job option SLURM would read as another directive is refused; so is an unknown or
        // malformed site, and a job for another launcher.
        for req in [
            json!({ "host": HOST, "launcher": "slurm", "job": { "partition": "gpu --uid=0" } }),
            json!({ "host": HOST, "launcher": "slurm", "site": "nowhere" }),
            json!({ "host": HOST, "launcher": "slurm", "site": "../../etc/passwd" }),
            json!({ "host": HOST, "launcher": "direct", "job": { "partition": "gpu" } }),
            json!({ "host": HOST, "launcher": "slurm", "job": { "time": "UNLIMITED" } }),
        ] {
            let e = w
                .call("gateway_remote_plan", json!({ "req": req }))
                .unwrap_err();
            assert_eq!(e["code"], "invalid", "{req}: {e}");
        }

        // The person's own recipe for the cluster.
        let sites = w.machine.laptop_home.join("sites");
        std::fs::create_dir_all(&sites).unwrap();
        let recipe = sites.join("lab.toml");
        std::fs::write(
            &recipe,
            "description = \"The lab's cluster\"\npartition = \"gpu\"\nmodules = [\"python/3.12\"]\n",
        )
        .unwrap();
        let plan = w
            .call(
                "gateway_remote_plan",
                json!({ "req": { "host": HOST, "launcher": "slurm", "site": "lab",
                        "job": { "account": "proj0001", "time": "01:00:00",
                                 "cpus": 2, "memory": "4G", "gpus": "1" } } }),
            )
            .unwrap();
        let script = plan["jobScript"].as_str().unwrap().to_owned();
        assert!(
            script.starts_with("#!/bin/sh\n# pitcrew-job-script-begin\n"),
            "{script}"
        );
        for line in [
            "#SBATCH --partition=gpu",
            "#SBATCH --account=proj0001",
            "#SBATCH --time=01:00:00",
            "#SBATCH --cpus-per-task=2",
            "#SBATCH --mem=4G",
            "#SBATCH --gres=gpu:1",
        ] {
            assert!(script.lines().any(|l| l == line), "{line} in {script}");
        }
        assert!(script.contains("python/3.12"), "{script}");
        let steps: Vec<String> = serde_json::from_value(plan["steps"].clone()).unwrap();
        assert!(
            steps[1].contains("Submit the job script below"),
            "{steps:?}"
        );
        assert!(
            !w.machine.slurm.join("submitted.sh").exists(),
            "nothing submitted yet"
        );

        // The recipe changes after the plan: what is submitted is still what was shown.
        std::fs::write(
            &recipe,
            "description = \"The lab's cluster\"\npartition = \"cpu\"\nmodules = [\"python/3.13\"]\nsbatch = [\"--constraint=a100\"]\n",
        )
        .unwrap();

        let e = w
            .call(
                "gateway_remote_add",
                json!({ "plan": plan["plan"], "events": "__CHANNEL__:80" }),
            )
            .unwrap_err();
        assert_eq!(e["code"], "unreachable", "{e}");
        assert!(e["message"].as_str().unwrap().contains("4242"), "{e}");
        // Exactly the shown script was submitted.
        assert_eq!(
            std::fs::read_to_string(w.machine.slurm.join("submitted.sh")).unwrap(),
            script
        );
        // The job this add submitted was cancelled again, by its name and this user, so the
        // error does not say it may still be queued.
        let cancelled = std::fs::read_to_string(w.machine.slurm.join("scancel.log")).unwrap();
        assert!(cancelled.contains("--name=pitcrew-helper-"), "{cancelled}");
        assert!(cancelled.trim_end().ends_with("4242"), "{cancelled}");
        assert!(
            !e["message"]
                .as_str()
                .unwrap()
                .contains("may still be queued"),
            "{e}"
        );
        let messages = w.channel(80);
        assert!(
            messages.iter().any(|m| m["state"] == "running"
                && m["detail"]
                    .as_str()
                    .unwrap_or("")
                    .contains("pending (Priority)")),
            "{messages:?}"
        );
        assert_eq!(
            steps_of(&messages).last().unwrap(),
            &("add".to_owned(), "failed".to_owned())
        );
        assert!(w.registry.list().is_empty());
        assert!(w.events(WORKSPACES_EVENT).is_empty());
        // The plan is gone too.
        let e = w
            .call(
                "gateway_remote_add",
                json!({ "plan": plan["plan"], "events": "__CHANNEL__:81" }),
            )
            .unwrap_err();
        assert_eq!(e["code"], "invalid");
    }

    /// A host key and a password asked in the app, answered through `gateway_prompt_reply`;
    /// then a prompt answered with neither fails the add cleanly: nothing deployed, nothing
    /// registered, no empty password sent. Replies of the wrong shape, and answers over 4 KiB,
    /// are refused with the prompt still open.
    fn prompts_round_trip_and_a_cancel_fails_the_add_cleanly() {
        let w = world(false, |_, _| {});
        w.machine.unknown_host_key();
        w.machine.require_password(PASSWORD);
        *w.answers.lock().unwrap() = Answers {
            password: Some(PASSWORD.into()),
            accept_host_key: true,
            hold: false,
        };

        let probe = w
            .call("gateway_remote_probe", json!({ "host": HOST }))
            .unwrap();
        assert_eq!(probe["host"], HOST);
        let prompts = w.events(PROMPT_EVENT);
        assert_eq!(prompts[0]["kind"], "host_key", "{prompts:?}");
        assert_eq!(prompts[0]["host"], HOST);
        assert_eq!(prompts[0]["fingerprint"], FINGERPRINT);
        assert!(
            prompts[0]["text"]
                .as_str()
                .unwrap()
                .contains("authenticity of host"),
            "{prompts:?}"
        );
        assert_eq!(prompts[1]["kind"], "password");
        assert_eq!(prompts[1]["text"], format!("sam@{HOST}'s password:"));
        assert!(
            prompts
                .iter()
                .all(|p| p.get("fingerprint").is_none() || p["kind"] == "host_key")
        );
        assert_eq!(w.machine.asked()[..2], ["yes", "text"]);
        // Every prompt was withdrawn once answered.
        let closed: Vec<Value> = w.events(PROMPT_CLOSED_EVENT);
        for prompt in &prompts {
            assert!(closed.contains(&json!({ "id": prompt["id"] })), "{prompt}");
        }

        // The server asks, with words that read like this computer asking for a key's
        // passphrase: it is a password prompt all the same (the answer goes to that host).
        let hostile = "Enter passphrase for key '~/.ssh/id_ed25519':";
        w.machine.keyboard_interactive(Some(hostile));
        let before = w.events(PROMPT_EVENT).len();
        w.call("gateway_remote_probe", json!({ "host": HOST }))
            .unwrap();
        let asked = w.events(PROMPT_EVENT)[before].clone();
        assert_eq!(asked["kind"], "password", "{asked}");
        assert_eq!(asked["host"], HOST);
        assert_eq!(asked["text"], format!("(sam@{HOST}) {hostile}"));
        assert!(asked.get("fingerprint").is_none(), "{asked}");
        w.machine.keyboard_interactive(None);

        // A reply for no open prompt, and malformed replies, are refused.
        let e = w
            .call(
                "gateway_prompt_reply",
                json!({ "id": prompts[1]["id"], "answer": "late" }),
            )
            .unwrap_err();
        assert_eq!(e["code"], "invalid");
        let e = w
            .call("gateway_prompt_reply", json!({ "id": "x", "answer": 7 }))
            .unwrap_err();
        assert_eq!(e["code"], "invalid");

        // With a prompt open: an answer over 4 KiB, and an accept for a password, are refused,
        // and the prompt stays open for the right answer.
        w.answers.lock().unwrap().hold = true;
        let probing = {
            let window = w.main.clone();
            std::thread::spawn(move || {
                invoke(&window, "gateway_remote_probe", json!({ "host": HOST }))
            })
        };
        let open = w.open_prompt();
        let e = w
            .call(
                "gateway_prompt_reply",
                json!({ "id": open.id, "answer": "x".repeat(4097) }),
            )
            .unwrap_err();
        assert_eq!(e["code"], "invalid");
        assert!(e["message"].as_str().unwrap().contains("4096"), "{e}");
        let e = w
            .call(
                "gateway_prompt_reply",
                json!({ "id": open.id, "accept": true }),
            )
            .unwrap_err();
        assert_eq!(e["code"], "invalid");
        assert!(w.remotes().prompts().open().iter().any(|p| p.id == open.id));
        w.answers.lock().unwrap().hold = false;
        w.call(
            "gateway_prompt_reply",
            json!({ "id": open.id, "answer": PASSWORD }),
        )
        .unwrap();
        probing.join().unwrap().unwrap();

        let plan = w
            .call(
                "gateway_remote_plan",
                json!({ "req": { "host": HOST, "launcher": "direct" } }),
            )
            .unwrap();

        // Now the person cancels the password prompt.
        w.answers.lock().unwrap().password = None;
        let e = w
            .call(
                "gateway_remote_add",
                json!({ "plan": plan["plan"], "events": "__CHANNEL__:90" }),
            )
            .unwrap_err();
        assert_eq!(e["code"], "unreachable", "{e}");
        assert!(e["message"].as_str().unwrap().contains("cancelled"), "{e}");
        let steps = steps_of(&w.channel(90));
        assert_eq!(
            steps.last().unwrap(),
            &("add".to_owned(), "failed".to_owned())
        );
        assert_eq!(
            steps[steps.len() - 2].1,
            "failed",
            "the deploy step failed: {steps:?}"
        );
        // Cleanly: nothing deployed or started, nothing registered or kept, no empty password
        // sent (ssh was stopped before it could send one).
        assert!(
            !w.machine.home.join(".pitcrew/bin").exists()
                || std::fs::read_dir(w.machine.home.join(".pitcrew/bin"))
                    .unwrap()
                    .flatten()
                    .all(|e| e.file_name().to_string_lossy().starts_with('.')),
            "nothing deployed"
        );
        assert!(w.machine.endpoint().is_none());
        assert!(w.registry.list().is_empty());
        assert!(
            !w.machine.asked().contains(&"empty".to_owned()),
            "{:?}",
            w.machine.asked()
        );
        let closed = w.events(PROMPT_CLOSED_EVENT);
        for prompt in w.events(PROMPT_EVENT) {
            assert!(closed.contains(&json!({ "id": prompt["id"] })), "{prompt}");
        }
        assert!(w.remotes().prompts().open().is_empty());
        // The password never reached a log line, an event or a result.
        for text in w.seen_by_the_webview() {
            assert!(!text.contains(PASSWORD), "{text}");
        }
        for line in log_text().lines() {
            assert!(!line.contains(PASSWORD), "{line}");
        }
    }

    /// The hub's id is not trusted: a hub reporting the id of a remote workspace on another
    /// machine, or of the local workspace, is refused, and that workspace's entry and token are
    /// untouched. The helper the failed add started is stopped again; when that fails too, the
    /// error says it may still be running. The hub's token is nowhere it must not be.
    fn a_hub_cannot_take_another_workspaces_id() {
        let w = world(false, |_, _| {});
        // The demo hub's id, already held by a workspace on another machine.
        w.registry
            .claim_remote(
                WorkspaceRecord {
                    id: DEMO.into(),
                    name: "Original".into(),
                    kind: WorkspaceKind::Remote,
                    connection: Connection::Remote(Box::new(RemoteConnection {
                        host: "other-login".into(),
                        launcher: LauncherKind::Direct,
                        root: "/home/sam/.pitcrew".into(),
                        platform: fake::platform().target().into(),
                        site: None,
                        job: None,
                        last_hop: None,
                        transport: None,
                    })),
                },
                Arc::new(Nowhere),
                WorkspaceState::Unreachable,
            )
            .unwrap();
        let original =
            DeviceToken::new("pcd_original-token-of-the-workspace-already-here").unwrap();
        w.tokens.set(DEMO, &original).unwrap();

        let e = w.add_direct(110).unwrap_err();
        assert_eq!(e["code"], "invalid", "{e}");
        let message = e["message"].as_str().unwrap();
        assert!(
            message.contains("already added as \"Original\"; remove it first"),
            "{message}"
        );
        assert!(
            !message.contains("may still be running"),
            "the undo worked: {message}"
        );
        assert_eq!(w.tokens.get(DEMO).unwrap(), Some(original.clone()));
        let Some(WorkspaceRecord {
            connection: Connection::Remote(kept),
            name,
            ..
        }) = w.registry.record(DEMO)
        else {
            panic!("the original workspace is gone");
        };
        assert_eq!(
            (kept.host.as_str(), name.as_str()),
            ("other-login", "Original")
        );
        let steps = steps_of(&w.channel(110));
        assert_eq!(
            steps[steps.len() - 2..],
            [
                (
                    "Pair: keep its device token in this computer's keychain".to_owned(),
                    "failed".to_owned()
                ),
                ("add".to_owned(), "failed".to_owned())
            ]
        );
        // The helper this add started was stopped again.
        assert!(w.machine.endpoint().is_none(), "the helper still runs");
        let token = hub_token(&w.machine);

        // The local workspace's id: refused too. This time stopping the helper fails as well,
        // and the error says so.
        w.registry.remove(DEMO).unwrap();
        w.tokens.delete(DEMO).unwrap();
        w.registry.set_local(DEMO, "Here").unwrap();
        w.machine.fail_stop(true);
        let e = w.add_direct(111).unwrap_err();
        assert_eq!(e["code"], "invalid", "{e}");
        let message = e["message"].as_str().unwrap();
        assert!(
            message.contains("this computer's own workspace"),
            "{message}"
        );
        assert!(
            message.contains(&format!("PitCrew's helper may still be running on {HOST}")),
            "{message}"
        );
        assert_eq!(
            w.channel(111).last().unwrap()["detail"],
            e["message"],
            "the failed add's detail says it too"
        );
        assert!(w.machine.endpoint().is_some(), "the stop really failed");
        assert_eq!(w.tokens.get(DEMO).unwrap(), None);
        assert_eq!(w.registry.record(DEMO).unwrap().kind, WorkspaceKind::Local);
        assert_eq!(w.registry.list().len(), 1);
        w.machine.fail_stop(false);

        // A pairing that failed after reading the token leaks it nowhere.
        no_token(
            &w,
            &token,
            "a hub claimed the id of a workspace already here",
        );
        assert!(!log_text().contains("original-token-of-the-workspace"));
    }

    /// The app quits and starts again: the saved remote workspace's tunnel is made again, and it
    /// is `ready` with its token from the keychain. A sign-in that reconnect asks for before the
    /// page listens is held for the page. Without its token the workspace `needs_pairing`.
    fn a_saved_remote_comes_back_after_a_restart() {
        let w = world(false, |_, _| {});
        let id = w.add_direct(120).unwrap();
        assert_eq!(id, DEMO);

        let again = w.restart(Answers::default());
        again.wait("the saved workspace ready again", |w| {
            w.state(&id) == Some(WorkspaceState::Ready)
        });
        let me = again
            .call(
                "gateway_request",
                json!({ "req": { "workspace": id, "method": "GET", "path": "/v1/me" } }),
            )
            .unwrap();
        assert_eq!(me["status"], 200, "{me}");

        // The machine now asks for a password: the reconnect at launch asks before the page
        // listens. The prompt is held, and emitted again (same id) when the page first asks for
        // the workspaces; not again on later asks.
        w.machine.require_password(PASSWORD);
        let held = again.restart(Answers {
            hold: true,
            ..Answers::default()
        });
        let open = held.open_prompt();
        assert_eq!(serde_json::to_value(open.kind).unwrap(), "password");
        let emitted = |w: &World| {
            w.events(PROMPT_EVENT)
                .iter()
                .filter(|p| p["id"] == open.id.as_str())
                .count()
        };
        let before = emitted(&held);
        held.call("gateway_workspaces", json!({})).unwrap();
        assert_eq!(emitted(&held), before + 1, "emitted again for the page");
        held.call("gateway_workspaces", json!({})).unwrap();
        assert_eq!(emitted(&held), before + 1, "only once");
        assert_eq!(held.state(&id), Some(WorkspaceState::Connecting));
        held.call(
            "gateway_prompt_reply",
            json!({ "id": open.id, "answer": PASSWORD }),
        )
        .unwrap();
        held.wait("ready once the page answered", |w| {
            w.state(&id) == Some(WorkspaceState::Ready)
        });

        // The keychain lost the token: connected, but it needs pairing, and nothing is sent.
        held.tokens.delete(&id).unwrap();
        let third = held.restart(Answers {
            password: Some(PASSWORD.into()),
            ..Answers::default()
        });
        third.wait("the workspace needs pairing", |w| {
            w.state(&id) == Some(WorkspaceState::NeedsPairing)
        });
        let e = third
            .call(
                "gateway_request",
                json!({ "req": { "workspace": id, "method": "GET", "path": "/v1/me" } }),
            )
            .unwrap_err();
        assert_eq!(e["code"], "needs_pairing", "{e}");
    }

    /// A sign-in cancelled while the tunnel reconnects leaves the workspace `unreachable` until
    /// `gateway_workspace_retry`, which brings it back to `ready`.
    fn a_cancelled_sign_in_while_reconnecting_is_retried() {
        let w = world(false, |_, _| {});
        let id = w.add_direct(130).unwrap();
        assert_eq!(w.state(&id), Some(WorkspaceState::Ready));

        // The server closes the connection; reconnecting asks for a password, which the
        // person cancels.
        w.machine.require_password(PASSWORD);
        w.answers.lock().unwrap().password = None;
        w.machine.drop_link();
        w.wait("the link dropped", |w| w.machine.dropped());
        w.wait("unreachable after the cancel", |w| {
            w.state(&id) == Some(WorkspaceState::Unreachable)
        });
        // It stays so: nothing tries again by itself.
        std::thread::sleep(Duration::from_secs(3));
        assert_eq!(w.state(&id), Some(WorkspaceState::Unreachable));

        // The person tries again, and answers this time.
        w.answers.lock().unwrap().password = Some(PASSWORD.into());
        let retried = w
            .call("gateway_workspace_retry", json!({ "workspace": id }))
            .unwrap();
        assert_eq!(retried, Value::Null);
        w.wait("ready after the retry", |w| {
            w.state(&id) == Some(WorkspaceState::Ready)
        });
        let me = w
            .call(
                "gateway_request",
                json!({ "req": { "workspace": id, "method": "GET", "path": "/v1/me" } }),
            )
            .unwrap();
        assert_eq!(me["status"], 200, "{me}");
        assert!(w.machine.asked().contains(&"text".to_owned()));

        let e = w
            .call("gateway_workspace_retry", json!({ "workspace": "nope" }))
            .unwrap_err();
        assert_eq!(e["code"], "unknown_workspace");
        let e = w.call("gateway_workspace_retry", json!({})).unwrap_err();
        assert_eq!(e["code"], "invalid");
    }

    /// Retries while one attempt runs (its sign-in waiting for the person) leave it be, and make
    /// one more attempt after it (it failed), not one each; a retry of a connected workspace does
    /// nothing. An attempt is one sign-in: one link, one prompt.
    fn retries_are_coalesced() {
        let w = world(false, |_, _| {});
        let id = w.add_direct(150).unwrap();
        // The connection drops; reconnecting asks for a password, which the person cancels.
        w.machine.require_password(PASSWORD);
        w.answers.lock().unwrap().hold = true;
        w.machine.drop_link();
        let open = w.open_prompt();
        w.call("gateway_prompt_reply", json!({ "id": open.id }))
            .unwrap();
        w.wait("unreachable after the cancel", |w| {
            w.state(&id) == Some(WorkspaceState::Unreachable)
        });
        let links = w.machine.calls_of("link");
        let prompts = w.events(PROMPT_EVENT).len();

        // A click starts an attempt, which asks; four more clicks while it waits for the person
        // leave it be: the same prompt stays open, and nothing else signs in.
        let retry = |w: &World| {
            let retried = w
                .call("gateway_workspace_retry", json!({ "workspace": id }))
                .unwrap();
            assert_eq!(retried, Value::Null);
        };
        retry(&w);
        let first = w.open_prompt();
        for _ in 0..4 {
            retry(&w);
        }
        std::thread::sleep(Duration::from_secs(1));
        assert_eq!(w.state(&id), Some(WorkspaceState::Connecting));
        let open: Vec<String> = w
            .remotes()
            .prompts()
            .open()
            .into_iter()
            .map(|p| p.id)
            .collect();
        assert_eq!(open, [first.id.clone()], "the same prompt, still open");
        assert_eq!(w.machine.calls_of("link"), links + 1, "one attempt");
        assert_eq!(w.events(PROMPT_EVENT).len(), prompts + 1);

        // That attempt fails (its sign-in is cancelled): one more comes, as asked meanwhile.
        w.call("gateway_prompt_reply", json!({ "id": first.id }))
            .unwrap();
        w.wait("the next attempt's prompt", |w| {
            w.remotes()
                .prompts()
                .open()
                .iter()
                .any(|p| p.id != first.id)
        });
        let second = w.open_prompt();
        assert_eq!(w.machine.calls_of("link"), links + 2);
        // It signs in, and nothing comes after it.
        w.call(
            "gateway_prompt_reply",
            json!({ "id": second.id, "answer": PASSWORD }),
        )
        .unwrap();
        w.wait("ready", |w| w.state(&id) == Some(WorkspaceState::Ready));
        std::thread::sleep(Duration::from_secs(2));
        assert_eq!(w.machine.calls_of("link"), links + 2);
        assert_eq!(w.events(PROMPT_EVENT).len(), prompts + 2);

        // Connected: a retry leaves it alone.
        w.call("gateway_workspace_retry", json!({ "workspace": id }))
            .unwrap();
        std::thread::sleep(Duration::from_millis(500));
        assert_eq!(w.machine.calls_of("link"), links + 2);
        assert_eq!(w.state(&id), Some(WorkspaceState::Ready));
        let me = w
            .call(
                "gateway_request",
                json!({ "req": { "workspace": id, "method": "GET", "path": "/v1/me" } }),
            )
            .unwrap();
        assert_eq!(me["status"], 200, "{me}");
    }

    /// An ssh older than 8.4 (the fake says 8.1) gets no prompt answered: none is shown, ssh is
    /// stopped before it sends anything, and the error says why. `ssh -V` is asked once. Keys
    /// that need no prompt still work.
    fn an_old_ssh_gets_no_prompt_answered() {
        let w = world(false, |_, _| {});
        w.machine
            .ssh_version("OpenSSH_8.1p1, OpenSSL 1.1.1k  FIPS 25 Mar 2021");
        // The UI would answer anything it is asked.
        *w.answers.lock().unwrap() = Answers {
            password: Some(PASSWORD.into()),
            accept_host_key: true,
            hold: false,
        };
        let refused = |w: &World| {
            let e = w
                .call("gateway_remote_probe", json!({ "host": HOST }))
                .unwrap_err();
            assert_eq!(e["code"], "unreachable", "{e}");
            let message = e["message"].as_str().unwrap();
            assert!(
                message.contains("ssh 8.4 or newer is needed to sign in from the app"),
                "{message}"
            );
            assert!(message.contains("OpenSSH 8.1"), "{message}");
        };
        // A password, asked by ssh and by the server; a new host key.
        w.machine.require_password(PASSWORD);
        refused(&w);
        w.machine
            .keyboard_interactive(Some("Enter passphrase for key '~/.ssh/id_ed25519':"));
        refused(&w);
        w.machine.keyboard_interactive(None);
        w.machine.unknown_host_key();
        refused(&w);
        // Nothing was shown, nothing answered, nothing sent (not even an empty password).
        assert!(
            w.events(PROMPT_EVENT).is_empty(),
            "{:?}",
            w.events(PROMPT_EVENT)
        );
        assert!(w.machine.asked().is_empty(), "{:?}", w.machine.asked());
        assert_eq!(w.machine.calls_of("version"), 1, "{:?}", w.machine.calls());

        // With keys, nothing is asked, and it works.
        w.machine.sign_in_with_keys();
        let probe = w
            .call("gateway_remote_probe", json!({ "host": HOST }))
            .unwrap();
        assert_eq!(probe["host"], HOST);
        assert_eq!(w.machine.calls_of("version"), 1);
    }

    /// A name set on the hub (by first-run setup, on a fresh hub) shows once the tunnel connects
    /// again, cleaned as at pairing, saved, and sent to the page.
    fn a_name_set_on_the_hub_shows_after_a_reconnect() {
        let name_of = |w: &World, id: &str| {
            w.registry
                .list()
                .into_iter()
                .find(|ws| ws.id == id)
                .map(|ws| ws.name)
                .unwrap_or_default()
        };
        let w = world(false, |_, _| {});
        // A fresh hub: "Workspace" until it is set up.
        w.machine.fresh_hub();
        let id = w.add_direct(160).unwrap();
        assert_ne!(id, DEMO);
        assert_eq!(name_of(&w, &id), "Workspace");
        let body = json!({
            "workspace_name": "Thesis\u{202e} lab",
            "person": { "name": "Sam Rivera", "handle": "@sam" },
            "machine_name": "Cluster login",
        });
        let setup = w
            .call(
                "gateway_request",
                json!({ "req": { "workspace": id, "method": "POST", "path": "/v1/setup",
                                 "body": body.to_string() } }),
            )
            .unwrap();
        assert!(setup["status"].as_u64().unwrap() < 300, "{setup}");
        // Until the tunnel connects again, the hub is not asked.
        assert_eq!(name_of(&w, &id), "Workspace");

        w.machine.drop_link();
        w.wait("the hub's new name", |w| name_of(w, &id) == "Thesis lab");
        assert!(
            std::fs::read_to_string(&w.registry_file)
                .unwrap()
                .contains("\"name\": \"Thesis lab\""),
            "saved"
        );
        w.wait("the new name sent to the page", |w| {
            w.events(WORKSPACES_EVENT).iter().any(|list| {
                list.as_array()
                    .unwrap()
                    .iter()
                    .any(|ws| ws["id"] == id.as_str() && ws["name"] == "Thesis lab")
            })
        });
    }

    /// `gateway_remote_cancel` stops a running add and undoes what it started: a submitted job
    /// that waits to start is cancelled, a helper started for a connection that does not come is
    /// stopped. Nothing is registered, and the add ends `failed`. A plan cancelled before its
    /// add is dropped; cancelling a finished add or an unknown plan does nothing.
    fn an_add_can_be_cancelled() {
        // SLURM: cancelled while the job is pending (the add would wait a minute).
        let w = world(true, |options, _| {
            options.job_wait = Duration::from_secs(60);
            options.job_poll = Duration::from_millis(300);
        });
        let plan = w
            .call(
                "gateway_remote_plan",
                json!({ "req": { "host": HOST, "launcher": "slurm" } }),
            )
            .unwrap();
        let adding = {
            let window = w.main.clone();
            let plan = plan["plan"].clone();
            std::thread::spawn(move || {
                invoke(
                    &window,
                    "gateway_remote_add",
                    json!({ "plan": plan, "events": "__CHANNEL__:140" }),
                )
            })
        };
        w.wait("the job pending", |w| {
            w.channel(140)
                .iter()
                .any(|m| m["detail"].as_str().unwrap_or("").contains("pending"))
        });
        let cancelled = Instant::now();
        let none = w
            .call("gateway_remote_cancel", json!({ "plan": plan["plan"] }))
            .unwrap();
        assert_eq!(none, Value::Null);
        let e = adding.join().unwrap().unwrap_err();
        assert!(
            cancelled.elapsed() < Duration::from_secs(20),
            "it stopped waiting"
        );
        assert_eq!(e["code"], "unreachable", "{e}");
        let message = e["message"].as_str().unwrap();
        assert!(message.contains("cancelled"), "{message}");
        assert!(!message.contains("may still be queued"), "{message}");
        let cancelled_jobs = std::fs::read_to_string(w.machine.slurm.join("scancel.log")).unwrap();
        assert!(
            cancelled_jobs.trim_end().ends_with("4242"),
            "{cancelled_jobs}"
        );
        assert_eq!(
            steps_of(&w.channel(140)).last().unwrap(),
            &("add".to_owned(), "failed".to_owned())
        );
        assert!(w.registry.list().is_empty());
        // The add has finished: cancelling it again, or an unknown plan, does nothing.
        for plan in [plan["plan"].clone(), json!("no-such-plan")] {
            assert_eq!(
                w.call("gateway_remote_cancel", json!({ "plan": plan }))
                    .unwrap(),
                Value::Null
            );
        }
        let e = w
            .call("gateway_remote_cancel", json!({ "plan": 7 }))
            .unwrap_err();
        assert_eq!(e["code"], "invalid");
        drop(w);

        // Direct: the helper is started, then the connection does not come; cancelled while
        // it waits, the helper is stopped again.
        let w = world(false, |options, _| {
            options.connect_wait = Duration::from_secs(60);
        });
        w.machine.stall_links(true);
        let plan = w
            .call(
                "gateway_remote_plan",
                json!({ "req": { "host": HOST, "launcher": "direct" } }),
            )
            .unwrap();
        let steps: Vec<String> = serde_json::from_value(plan["steps"].clone()).unwrap();
        let adding = {
            let window = w.main.clone();
            let plan = plan["plan"].clone();
            std::thread::spawn(move || {
                invoke(
                    &window,
                    "gateway_remote_add",
                    json!({ "plan": plan, "events": "__CHANNEL__:141" }),
                )
            })
        };
        w.wait("connecting", |w| {
            steps_of(&w.channel(141)).contains(&(steps[2].clone(), "running".to_owned()))
        });
        assert!(w.machine.endpoint().is_some(), "the helper runs");
        w.call("gateway_remote_cancel", json!({ "plan": plan["plan"] }))
            .unwrap();
        let e = adding.join().unwrap().unwrap_err();
        assert!(e["message"].as_str().unwrap().contains("cancelled"), "{e}");
        assert!(w.machine.endpoint().is_none(), "the helper was stopped");
        assert!(w.registry.list().is_empty());
        assert_eq!(
            steps_of(&w.channel(141))[steps_of(&w.channel(141)).len() - 2..],
            [
                (steps[2].clone(), "failed".to_owned()),
                ("add".to_owned(), "failed".to_owned())
            ]
        );
        w.machine.stall_links(false);

        // A plan cancelled before its add is dropped: the add finds no plan, and starts nothing.
        let plan = w
            .call(
                "gateway_remote_plan",
                json!({ "req": { "host": HOST, "launcher": "direct" } }),
            )
            .unwrap();
        w.call("gateway_remote_cancel", json!({ "plan": plan["plan"] }))
            .unwrap();
        let e = w
            .call(
                "gateway_remote_add",
                json!({ "plan": plan["plan"], "events": "__CHANNEL__:142" }),
            )
            .unwrap_err();
        assert_eq!(e["code"], "invalid", "{e}");
        assert!(w.machine.endpoint().is_none());
    }

    /// An expired plan is refused; a machine without a helper for its platform, or an app
    /// without `pitcrew-askpass`, fails with a clear error.
    fn plans_expire_and_missing_programs_are_clear_errors() {
        let w = world(false, |options, _| {
            options.plan_ttl = Duration::from_millis(300)
        });
        let plan = w
            .call(
                "gateway_remote_plan",
                json!({ "req": { "host": HOST, "launcher": "direct" } }),
            )
            .unwrap();
        std::thread::sleep(Duration::from_millis(700));
        let e = w
            .call(
                "gateway_remote_add",
                json!({ "plan": plan["plan"], "events": "__CHANNEL__:100" }),
            )
            .unwrap_err();
        assert_eq!(e["code"], "invalid");
        assert!(e["message"].as_str().unwrap().contains("expired"), "{e}");
        assert!(
            !w.machine.home.join(".pitcrew").exists(),
            "nothing happened"
        );
        drop(w);

        // No helper for the machine's platform.
        let w = world(false, |_, machine| {
            std::fs::remove_file(machine.helpers.join(fake::platform().artefact())).unwrap();
        });
        let e = w
            .call(
                "gateway_remote_plan",
                json!({ "req": { "host": HOST, "launcher": "direct" } }),
            )
            .unwrap_err();
        assert_eq!(e["code"], "invalid");
        assert!(
            e["message"].as_str().unwrap().contains("no helper for"),
            "{e}"
        );
        drop(w);

        // No pitcrew-askpass, or a configured ssh that failed its checks: nothing runs ssh.
        for (askpass, ssh, said) in [
            (
                Err("pitcrew-askpass is not next to the app".to_owned()),
                None,
                "pitcrew-askpass",
            ),
            (
                Ok(()),
                Some("not running the configured ssh: it can be written by other users".to_owned()),
                "configured ssh",
            ),
        ] {
            let w = world(false, |options, _| {
                if let Err(why) = askpass {
                    options.askpass = Err(why);
                }
                if let Some(why) = ssh {
                    options.ssh = Err(why);
                }
            });
            let e = w
                .call("gateway_remote_probe", json!({ "host": HOST }))
                .unwrap_err();
            assert_eq!(e["code"], "internal");
            assert!(e["message"].as_str().unwrap().contains(said), "{e}");
            assert!(w.machine.calls().is_empty(), "{:?}", w.machine.calls());
        }
    }
}
