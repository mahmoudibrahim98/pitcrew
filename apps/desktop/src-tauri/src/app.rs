//! The Tauri wiring.
//!
//! - **One main window**, created here rather than from the config, so its guards come with it:
//!   navigation away from the app's own origin is refused, new windows and downloads are denied,
//!   and devtools exist only in debug builds.
//! - **No remote URL is ever loaded.** Release builds serve `apps/ui/dist` from inside the app
//!   (`tauri://localhost`, or `http://tauri.localhost` on Windows); debug builds load the Vite dev
//!   server named by `devUrl`. A release build without the bundled UI does not compile.
//! - **The CSP** is in `tauri.conf.json`; **the capability** in `capabilities/main.json`: the
//!   gateway's commands and listening to its events, nothing else. The app alone emits
//!   `gateway://workspaces`, `gateway://navigate`, `gateway://prompt` and
//!   `gateway://prompt-closed`, to the main window only.
//! - **Remote workspaces** ([`crate::remote`]): their tunnels are made again at start, SSH's
//!   questions go to the main window, and their tunnels close when the app quits.
//! - **Single instance**: a second launch focuses the first one's window, and opens the deep link
//!   it was given, if any ([`crate::navigate`]).
//! - **In the background** ([`crate::shell`]): closing the window keeps the app in the tray, when
//!   there is one and the person has not chosen to quit on close.
//! - **Cleanup**: when the page reloads or the window closes, the gateway closes every socket that
//!   page opened. When the app quits, the supervisor stops the daemon if it started it.

use crate::daemon::endpoint::Endpoint;
use crate::daemon::supervisor::{Options, Supervisor};
use crate::daemon::{LocalConnector, follow, locate};
use crate::gateway::Gateway;
use crate::keychain::OsKeychain;
use crate::navigate::{self, Navigator};
use crate::preferences::PreferenceStore;
use crate::registry::{self, GatewayWorkspace, Registry};
use crate::remote::helpers::{self, Helpers};
use crate::remote::prompt::{PROMPT_CLOSED_EVENT, PROMPT_EVENT, PromptClosed};
use crate::remote::{PromptEvent, PromptHub, RemoteOptions, Remotes};
use crate::settings::Settings;
use crate::shell::Shell;
use crate::{commands, logging, scheme, tray};
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::webview::{NewWindowResponse, PageLoadEvent, PageLoadPayload};
use tauri::{
    App, AppHandle, Emitter as _, EventTarget, Manager as _, RunEvent, Runtime, Url, Webview,
    WebviewUrl, WebviewWindow, WebviewWindowBuilder, Window, WindowEvent,
};

#[cfg(all(not(debug_assertions), not(feature = "custom-protocol")))]
compile_error!(
    "a release build must bundle the UI: build with `--features custom-protocol` (the Tauri CLI \
     does), so it never loads the dev server"
);

/// The main window's label, named by `capabilities/main.json`.
pub const MAIN: &str = "main";

/// The event the gateway emits with the whole workspace list whenever it changes.
pub const WORKSPACES_EVENT: &str = "gateway://workspaces";

/// How long quitting waits for the daemon the app started to stop.
const QUIT_TIMEOUT: Duration = Duration::from_secs(10);

/// Runs the app until it quits.
#[must_use]
pub fn run() -> ExitCode {
    logging::init();
    let started = std::time::Instant::now();
    let builder = tauri::Builder::default()
        // Managed before any plugin, so a link handed over while the app starts is held, not
        // dropped.
        .manage(Navigator::default())
        // First, so a second instance hands over and exits before anything else starts. It
        // hands over its command line: a deep link, when the desktop opened one (on Windows the
        // plugin joins the arguments with `|` and splits them again).
        .plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
            navigate::open_links(app, args.iter().skip(1));
        }))
        .setup(move |app| {
            setup(app)?;
            tracing::info!(ms = started.elapsed().as_millis(), "the main window is up");
            Ok(())
        });
    let app = match configure(builder).build(crate::context()) {
        Ok(app) => app,
        Err(e) => {
            tracing::error!(error = %e, "the app cannot start");
            return ExitCode::FAILURE;
        }
    };
    app.run(|handle, event| match event {
        RunEvent::Exit => shutdown(handle),
        // macOS hands deep links over as Apple Events, to the running app, already parsed: dot
        // segments are resolved by then (see `navigate`).
        #[cfg(target_os = "macos")]
        RunEvent::Opened { urls } => {
            navigate::open_links(handle, urls.iter().map(Url::as_str));
        }
        #[cfg(target_os = "macos")]
        RunEvent::Reopen { .. } => navigate::focus_main(handle),
        _ => {}
    });
    ExitCode::SUCCESS
}

/// The parts of the app that tests drive too, with the mock runtime: the commands and the
/// window hooks. The gateway itself is managed state (`app.manage(Gateway::new(…))`), and so are
/// the [`Navigator`] and the [`Shell`] when there are.
pub fn configure<R: Runtime>(builder: tauri::Builder<R>) -> tauri::Builder<R> {
    builder
        .invoke_handler(tauri::generate_handler![
            commands::gateway_workspaces,
            commands::gateway_local_host,
            commands::gateway_request,
            commands::gateway_socket_open,
            commands::gateway_socket_send,
            commands::gateway_socket_close,
            commands::gateway_ssh_hosts,
            commands::gateway_wsl_distros,
            commands::gateway_remote_probe,
            commands::gateway_remote_plan,
            commands::gateway_remote_add,
            commands::gateway_remote_cancel,
            commands::gateway_workspace_retry,
            commands::gateway_workspace_remove,
            commands::gateway_prompt_reply,
        ])
        .on_page_load(on_page_load)
        .on_window_event(on_window_event)
}

/// A page started loading (a reload, or the first load): the sockets its predecessor opened
/// close, and navigation waits until the new page listens.
pub fn on_page_load<R: Runtime>(webview: &Webview<R>, payload: &PageLoadPayload<'_>) {
    if payload.event() != PageLoadEvent::Started {
        return;
    }
    if let Some(gateway) = webview.try_state::<Gateway>() {
        gateway.page_started(webview.label());
    }
    if webview.label() == MAIN {
        if let Some(navigator) = webview.try_state::<Navigator>() {
            navigator.page_started();
        }
        if let Some(remotes) = webview.try_state::<Remotes>() {
            remotes.prompts().page_started();
        }
    }
}

/// The main window's close keeps the app in the tray (when [`Shell::keeps_running_on_close`]);
/// a window that is gone has its sockets closed.
pub fn on_window_event<R: Runtime>(window: &Window<R>, event: &WindowEvent) {
    match event {
        WindowEvent::CloseRequested { api, .. } if window.label() == MAIN => {
            if let Some(shell) = window.try_state::<Shell>()
                && shell.keeps_running_on_close()
            {
                api.prevent_close();
                let _ = window.hide();
                tracing::debug!("the window closed into the tray");
                shell.closed_to_tray();
            }
        }
        WindowEvent::Destroyed => {
            if let Some(gateway) = window.try_state::<Gateway>() {
                gateway.forget(window.label());
            }
        }
        _ => {}
    }
}

/// Sends the workspace list to the main window as `gateway://workspaces`.
pub fn emit_workspaces<R: Runtime>(app: &AppHandle<R>, list: &[GatewayWorkspace]) {
    let target = EventTarget::WebviewWindow {
        label: MAIN.to_owned(),
    };
    if let Err(e) = app.emit_to(target, WORKSPACES_EVENT, list) {
        tracing::warn!(error = %e, "cannot emit the workspace list");
    }
}

/// Sends an SSH prompt to the main window as `gateway://prompt`, or withdraws one with
/// `gateway://prompt-closed`.
pub fn emit_prompt<R: Runtime>(app: &AppHandle<R>, event: &PromptEvent) {
    let target = EventTarget::WebviewWindow {
        label: MAIN.to_owned(),
    };
    let emitted = match event {
        PromptEvent::Open(prompt) => app.emit_to(target, PROMPT_EVENT, prompt),
        PromptEvent::Closed(id) => {
            app.emit_to(target, PROMPT_CLOSED_EVENT, PromptClosed { id: id.clone() })
        }
    };
    if let Err(e) = emitted {
        tracing::warn!(error = %e, "cannot emit a prompt event");
    }
}

/// The remote workspaces' options from the settings: the ssh to use (`ssh` on `PATH`, or a
/// configured one that passes the same checks as `pitcrewd`), `pitcrew-askpass` next to the app
/// (or as set), and the helpers installed with it (`helpers/` beside the program or in the app's
/// resources) or as set. Either way the helpers' checksums are the ones compiled into the app
/// ([`Helpers::new`]).
#[must_use]
pub fn remote_options(
    settings: &Settings,
    beside: Option<&std::path::Path>,
    resources: Option<&std::path::Path>,
) -> RemoteOptions {
    let askpass = helpers::locate_askpass(settings.askpass.as_deref(), beside);
    if let Err(e) = &askpass {
        tracing::warn!(error = %e, "remote machines cannot ask for passwords");
    }
    let helpers = match &settings.helpers {
        Some(dir) => Helpers::new(vec![dir.clone()]),
        None => Helpers::new(
            [resources, beside]
                .into_iter()
                .flatten()
                .map(|dir| dir.join("helpers"))
                .collect(),
        )
        .with_native(beside),
    };
    let ssh = match &settings.ssh {
        None => Ok(std::path::PathBuf::from("ssh")),
        Some(path) => locate::check_trusted(path)
            .map(|()| path.clone())
            .map_err(|why| format!("not running the configured ssh, {}: {why}", path.display())),
    };
    if let Err(e) = &ssh {
        tracing::warn!(error = %e, "remote machines cannot be reached");
    }
    RemoteOptions::new(ssh, askpass, helpers)
}

/// The supervisor, kept to stop the daemon when the app quits.
struct Daemon(Mutex<Option<Supervisor>>);

fn setup(app: &mut App) -> Result<(), Box<dyn std::error::Error>> {
    let handle = app.handle().clone();
    let paths = app.path();
    let config_dir = paths.app_config_dir()?;
    let settings = Settings::load(&config_dir).with_env(|name| std::env::var_os(name));
    let state_dir = settings
        .state_dir_or_default()
        .ok_or("cannot find this user's local data folder; set stateDir in settings.json")?;
    let endpoint = Endpoint::private_default(&state_dir)?;
    let beside = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(std::path::Path::to_path_buf));
    let program = locate::locate(
        settings.pitcrewd.as_deref(),
        beside.as_deref(),
        std::env::var_os("PATH").as_deref(),
    )
    .map_err(|e| e.to_string());
    match &program {
        Ok(path) => {
            tracing::info!(pitcrewd = %path.display(), state = %state_dir.display(), "local daemon")
        }
        Err(e) => tracing::warn!(error = %e, "no pitcrewd to start"),
    }

    let registry = Arc::new(Registry::load(
        paths.app_local_data_dir()?.join(registry::FILE_NAME),
    ));
    let emitter = handle.clone();
    registry.on_change(move |list| {
        emit_workspaces(&emitter, list);
        if let Some(shell) = emitter.try_state::<Shell>() {
            shell.attention.sync(list);
        }
        tray::refresh(&emitter);
    });
    let local = Arc::new(LocalConnector::new(endpoint.clone()));
    registry.attach_local(Arc::clone(&local) as Arc<dyn crate::gateway::Connector>);
    app.manage(Gateway::new(Arc::clone(&registry)));
    let shell = Shell::start(
        &handle,
        Arc::clone(&registry),
        PreferenceStore::load(&config_dir),
    );
    app.manage(shell);
    if let Some(shell) = app.try_state::<Shell>() {
        shell.create_tray(&handle);
    }
    std::thread::spawn(scheme::register);

    let runtime = tauri::async_runtime::handle();
    let supervisor = Supervisor::start(
        Options::new(program, settings.state_dir.clone(), endpoint),
        runtime.inner(),
    );
    local.notify_failures(supervisor.poke_handle());
    runtime.spawn(follow(supervisor.state(), local, Arc::clone(&registry)));
    app.manage(Daemon(Mutex::new(Some(supervisor))));

    let asker = handle.clone();
    let prompts = Arc::new(PromptHub::new(move |event| emit_prompt(&asker, event)));
    let resources = paths.resource_dir().ok();
    let remotes = Remotes::new(
        remote_options(&settings, beside.as_deref(), resources.as_deref()),
        registry,
        Arc::new(OsKeychain::default()),
        prompts,
        runtime.inner().clone(),
    );
    remotes.resume();
    remotes.watch_wakes();
    app.manage(remotes);

    main_window(app)?;
    // A deep link that launched the app: held until the page listens.
    let args: Vec<String> = std::env::args_os()
        .skip(1)
        .filter_map(|arg| arg.into_string().ok())
        .collect();
    if let Some(target) = navigate::targets_in(&args).pop()
        && let Some(navigator) = app.try_state::<Navigator>()
    {
        navigator.navigate(&handle, target);
    }
    Ok(())
}

/// The main window, with its guards.
///
/// # Errors
/// The window cannot be created.
pub fn main_window<R: Runtime, M: tauri::Manager<R>>(
    manager: &M,
) -> tauri::Result<WebviewWindow<R>> {
    let origins = app_origins(manager.config());
    WebviewWindowBuilder::new(manager, MAIN, WebviewUrl::App("index.html".into()))
        .title("PitCrew")
        .inner_size(1280.0, 820.0)
        .min_inner_size(900.0, 600.0)
        .devtools(cfg!(debug_assertions))
        .on_navigation(move |url| {
            let allowed = is_app_url(&origins, url);
            if !allowed {
                tracing::warn!(
                    scheme = url.scheme(),
                    host = url.host_str().unwrap_or(""),
                    "refused to navigate away from the app"
                );
            }
            allowed
        })
        .on_new_window(|url, _features| {
            tracing::warn!(
                scheme = url.scheme(),
                host = url.host_str().unwrap_or(""),
                "refused to open a window"
            );
            NewWindowResponse::Deny
        })
        .on_download(|_webview, _event| false)
        .build()
}

/// An origin: scheme, host and port.
type Origin = (String, String, Option<u16>);

fn origin_of(url: &Url) -> Origin {
    (
        url.scheme().to_owned(),
        url.host_str().unwrap_or("").to_owned(),
        url.port_or_known_default(),
    )
}

/// The app's own origins: the dev server in debug builds, the bundled UI's otherwise. On Windows
/// the bundled UI is served from `http://tauri.localhost`: the window does not set
/// `use_https_scheme`, so `https://tauri.localhost` is not the app.
#[must_use]
pub fn app_origins(config: &tauri::Config) -> Vec<(String, String, Option<u16>)> {
    if tauri::is_dev() {
        config.build.dev_url.iter().map(origin_of).collect()
    } else if cfg!(windows) {
        vec![("http".into(), "tauri.localhost".into(), Some(80))]
    } else {
        vec![("tauri".into(), "localhost".into(), None)]
    }
}

/// Whether `url` is on one of the app's own origins.
#[must_use]
pub fn is_app_url(origins: &[(String, String, Option<u16>)], url: &Url) -> bool {
    origins.contains(&origin_of(url))
}

/// When the app quits: stop watching, close the remote tunnels, and stop the daemon if this app
/// started it.
fn shutdown<R: Runtime>(app: &AppHandle<R>) {
    if let Some(shell) = app.try_state::<Shell>() {
        shell.stop();
    }
    if let Some(remotes) = app.try_state::<Remotes>() {
        tauri::async_runtime::block_on(async {
            if tokio::time::timeout(QUIT_TIMEOUT, remotes.shutdown())
                .await
                .is_err()
            {
                tracing::warn!("the remote connections did not close in time");
            }
        });
    }
    let supervisor = app
        .try_state::<Daemon>()
        .and_then(|daemon| daemon.0.lock().ok().and_then(|mut s| s.take()));
    if let Some(supervisor) = supervisor {
        tauri::async_runtime::block_on(async {
            if tokio::time::timeout(QUIT_TIMEOUT, supervisor.shutdown())
                .await
                .is_err()
            {
                tracing::warn!("the local daemon did not stop in time");
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A configured ssh is checked like `pitcrewd`: one others could write is not run.
    #[cfg(unix)]
    #[test]
    fn a_configured_ssh_others_can_write_is_not_used() {
        use std::os::unix::fs::PermissionsExt as _;
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("bin");
        std::fs::create_dir(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        let ssh = dir.join("ssh");
        std::fs::write(&ssh, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&ssh, std::fs::Permissions::from_mode(0o755)).unwrap();
        let settings = Settings {
            ssh: Some(ssh.clone()),
            ..Settings::default()
        };
        assert_eq!(remote_options(&settings, None, None).ssh, Ok(ssh.clone()));
        std::fs::set_permissions(&ssh, std::fs::Permissions::from_mode(0o777)).unwrap();
        let refused = remote_options(&settings, None, None).ssh.unwrap_err();
        assert!(
            refused.contains("not running the configured ssh"),
            "{refused}"
        );
        // Without a setting, ssh comes from PATH, as the person runs it.
        assert_eq!(
            remote_options(&Settings::default(), None, None).ssh,
            Ok(std::path::PathBuf::from("ssh"))
        );
    }

    #[test]
    fn only_the_apps_own_origin_is_allowed() {
        let origins = vec![("tauri".to_owned(), "localhost".to_owned(), None)];
        let ok = |u: &str| is_app_url(&origins, &Url::parse(u).unwrap());
        assert!(ok("tauri://localhost/"));
        assert!(ok("tauri://localhost/w/01J/tasks?x=1#y"));
        assert!(!ok("tauri://evil/"));
        assert!(!ok("https://localhost/"));
        assert!(!ok("https://example.com/"));
        assert!(!ok("file:///etc/passwd"));
        assert!(!ok("about:blank"));

        let dev = vec![origin_of(&Url::parse("http://127.0.0.1:5173").unwrap())];
        let ok = |u: &str| is_app_url(&dev, &Url::parse(u).unwrap());
        assert!(ok("http://127.0.0.1:5173/w/01J"));
        assert!(!ok("http://127.0.0.1:5174/"));
        assert!(!ok("http://localhost:5173/"));
        assert!(!ok("https://127.0.0.1:5173/"));
    }
}
