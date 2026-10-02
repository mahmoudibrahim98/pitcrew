//! Remote workspaces end to end, through Tauri's IPC on the mock runtime with the app's real
//! ACL: probe, plan, add, requests and sockets through the tunnel, remove; SLURM's exact job
//! script; SSH's prompts in the app; and no token anywhere it must not be.
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
    use pitcrew_desktop::gateway::Gateway;
    use pitcrew_desktop::keychain::{MemoryStore, TokenStore as _};
    use pitcrew_desktop::logging;
    use pitcrew_desktop::navigate::Navigator;
    use pitcrew_desktop::registry::{self, Registry};
    use pitcrew_desktop::remote::prompt::{PROMPT_CLOSED_EVENT, PROMPT_EVENT};
    use pitcrew_desktop::remote::{Helpers, PromptHub, RemoteOptions, Remotes};
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

    /// How the prompt responder answers.
    #[derive(Clone, Debug, Default)]
    struct Answers {
        /// The password to type; `None` cancels.
        password: Option<String>,
        /// Whether to trust an unknown host key.
        accept_host_key: bool,
    }

    struct World {
        app: tauri::App<MockRuntime>,
        main: WebviewWindow<MockRuntime>,
        registry: Arc<Registry>,
        registry_file: PathBuf,
        tokens: Arc<MemoryStore>,
        machine: Machine,
        /// `(event, payload)`, in order.
        events: Arc<Mutex<Vec<(String, String)>>>,
        /// What went to the webview on channels: `(channel, text)`.
        channels: Arc<Mutex<Vec<(u32, String)>>>,
        /// Every command's result or error, as text.
        results: Arc<Mutex<Vec<String>>>,
        answers: Arc<Mutex<Answers>>,
        _data: tempfile::TempDir,
    }

    fn world(slurm: bool, tweak: impl FnOnce(&mut RemoteOptions, &Machine)) -> World {
        let machine = Machine::new(slurm, pitcrewd());
        let data = tempfile::tempdir().unwrap();
        let registry_file = data.path().join("data").join(registry::FILE_NAME);
        let registry = Arc::new(Registry::load(registry_file.clone()));
        let tokens = Arc::new(MemoryStore::default());
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
            Arc::clone(&tokens) as Arc<dyn pitcrew_desktop::keychain::TokenStore>,
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
        let answers: Arc<Mutex<Answers>> = Arc::default();
        let results: Arc<Mutex<Vec<String>>> = Arc::default();
        {
            let answers = Arc::clone(&answers);
            let results = Arc::clone(&results);
            let window = main.clone();
            std::thread::spawn(move || {
                for payload in to_answer {
                    let prompt: Value = serde_json::from_str(&payload).unwrap();
                    let now = answers.lock().unwrap().clone();
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
            _data: data,
        }
    }

    impl World {
        /// Invokes `cmd` from the main window, as the webview would, and keeps the outcome.
        fn call(&self, cmd: &str, args: Value) -> Result<Value, Value> {
            let result = invoke(&self.main, cmd, args);
            self.results.lock().unwrap().push(text_of(&result));
            result
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

        /// Waits until `check` holds.
        fn wait(&self, what: &str, check: impl Fn(&Self) -> bool) {
            let deadline = Instant::now() + Duration::from_secs(60);
            while !check(self) {
                assert!(Instant::now() < deadline, "timed out waiting for {what}");
                std::thread::sleep(Duration::from_millis(50));
            }
        }

        /// Everything the webview could have seen, as text.
        fn seen_by_the_webview(&self) -> Vec<String> {
            let mut all = self.results.lock().unwrap().clone();
            all.extend(self.channels.lock().unwrap().iter().map(|(_, t)| t.clone()));
            all.extend(self.events.lock().unwrap().iter().map(|(_, p)| p.clone()));
            all
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

    // ─── The cases ───────────────────────────────────────────────────────────────────────

    /// Probe, plan and add with the direct launcher: the workspace is `ready`, requests and
    /// sockets reach the real hub through the tunnel, the transport is remembered, a probe sees
    /// the helper running, and remove stops it and deletes the keychain entry. No token reaches
    /// the webview or the logs.
    fn a_direct_helper_end_to_end() {
        let w = world(false, |_, _| {});

        // The host list comes from the (temporary) ssh config: concrete hosts only.
        let hosts = w.call("gateway_ssh_hosts", json!({})).unwrap();
        assert_eq!(hosts, json!({ "hosts": [HOST] }));
        // Another window has no capability for any of the new commands.
        let other = WebviewWindowBuilder::new(&w.app, "other", Default::default())
            .build()
            .unwrap();
        for (cmd, args) in [
            ("gateway_ssh_hosts", json!({})),
            ("gateway_remote_probe", json!({ "host": HOST })),
            ("gateway_prompt_reply", json!({ "id": "x" })),
            (
                "gateway_workspace_remove",
                json!({ "workspace": "x", "stopHelper": false }),
            ),
        ] {
            assert!(
                invoke(&other, cmd, args).is_err(),
                "{cmd} from another window"
            );
        }

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
            workspace["body"]
                .as_str()
                .unwrap()
                .contains(added["name"].as_str().unwrap()),
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

        // The token: in the keychain (here in memory), and the hub's own.
        let token = hub_token(&w.machine);
        assert_eq!(
            w.tokens.get(&id).unwrap().map(|t| t.expose().to_owned()),
            Some(token.clone())
        );
        assert!(!saved.contains(&token));

        // A second probe sees the helper running.
        let probe = w
            .call("gateway_remote_probe", json!({ "host": HOST }))
            .unwrap();
        assert_eq!(
            probe["helper"],
            json!({ "version": version, "running": true })
        );

        // The local workspace cannot be removed; an unknown one is unknown.
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
        no_token(&w, &token);
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
    /// errors, channel messages, events) and in no log line.
    fn no_token(w: &World, token: &str) {
        let secret = token.split_once('_').map_or(token, |(_, rest)| rest);
        assert!(secret.len() >= 16, "a real token");
        let seen = w.seen_by_the_webview();
        assert!(seen.len() > 20, "{seen:#?}");
        for text in &seen {
            assert!(!text.contains(secret), "the webview saw the token: {text}");
            assert!(!text.contains("pitcrew.bearer."), "{text}");
        }
        let logs = log_text();
        assert!(
            logs.contains("added a remote workspace"),
            "the log is captured"
        );
        for line in logs.lines() {
            assert!(!line.contains(secret), "a log line holds the token: {line}");
        }
    }

    /// SLURM: the plan returns the exact script, and adding submits exactly that text. Here the
    /// job stays pending: the add gives up after its wait, cancels the job it submitted, and
    /// registers nothing.
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

        // A job option SLURM would read as another directive is refused; so is an unknown site,
        // and a job for another launcher.
        for req in [
            json!({ "host": HOST, "launcher": "slurm", "job": { "partition": "gpu --uid=0" } }),
            json!({ "host": HOST, "launcher": "slurm", "site": "nowhere" }),
            json!({ "host": HOST, "launcher": "direct", "job": { "partition": "gpu" } }),
            json!({ "host": HOST, "launcher": "slurm", "job": { "time": "UNLIMITED" } }),
        ] {
            let e = w
                .call("gateway_remote_plan", json!({ "req": req }))
                .unwrap_err();
            assert_eq!(e["code"], "invalid", "{req}: {e}");
        }

        let plan = w
            .call(
                "gateway_remote_plan",
                json!({ "req": { "host": HOST, "launcher": "slurm", "site": "generic",
                        "job": { "partition": "gpu", "account": "proj0001", "time": "01:00:00",
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
        let steps: Vec<String> = serde_json::from_value(plan["steps"].clone()).unwrap();
        assert!(
            steps[1].contains("Submit the job script below"),
            "{steps:?}"
        );
        assert!(
            !w.machine.slurm.join("submitted.sh").exists(),
            "nothing submitted yet"
        );

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
        // The job this add submitted was cancelled again, by its name and this user.
        let cancelled = std::fs::read_to_string(w.machine.slurm.join("scancel.log")).unwrap();
        assert!(cancelled.contains("--name=pitcrew-helper-"), "{cancelled}");
        assert!(cancelled.trim_end().ends_with("4242"), "{cancelled}");
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
    /// registered, no empty password sent.
    fn prompts_round_trip_and_a_cancel_fails_the_add_cleanly() {
        let w = world(false, |_, _| {});
        w.machine.unknown_host_key();
        w.machine.require_password("correct-horse");
        *w.answers.lock().unwrap() = Answers {
            password: Some("correct-horse".into()),
            accept_host_key: true,
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

        let plan = w
            .call(
                "gateway_remote_plan",
                json!({ "req": { "host": HOST, "launcher": "direct" } }),
            )
            .unwrap();

        // Now the person cancels the password prompt.
        w.answers.lock().unwrap().password = None;
        let asked_before = w.machine.asked().len();
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
            !w.machine.asked()[asked_before..].contains(&"empty".to_owned()),
            "{:?}",
            w.machine.asked()
        );
        assert!(!w.machine.asked().contains(&"empty".to_owned()));
        let closed = w.events(PROMPT_CLOSED_EVENT);
        for prompt in w.events(PROMPT_EVENT) {
            assert!(closed.contains(&json!({ "id": prompt["id"] })), "{prompt}");
        }
        assert!(w.remotes().prompts().open().is_empty());
        // The password never reached a log line, an event or a result.
        for text in w.seen_by_the_webview() {
            assert!(!text.contains("correct-horse"), "{text}");
        }
        for line in log_text().lines() {
            assert!(!line.contains("correct-horse"), "{line}");
        }
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

        // No pitcrew-askpass: nothing runs ssh.
        let w = world(false, |options, _| {
            options.askpass = Err("pitcrew-askpass is not next to the app".into());
        });
        let e = w
            .call("gateway_remote_probe", json!({ "host": HOST }))
            .unwrap_err();
        assert_eq!(e["code"], "internal");
        assert!(
            e["message"].as_str().unwrap().contains("pitcrew-askpass"),
            "{e}"
        );
        assert!(w.machine.calls().is_empty(), "{:?}", w.machine.calls());
    }
}
