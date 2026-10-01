//! The Tauri wiring.
//!
//! - **One main window**, created here rather than from the config, so its guards come with it:
//!   navigation away from the app's own origin is refused, new windows and downloads are denied,
//!   and devtools exist only in debug builds.
//! - **No remote URL is ever loaded.** Release builds serve `apps/ui/dist` from inside the app
//!   (`tauri://localhost`, or `http://tauri.localhost` on Windows); debug builds load the Vite dev
//!   server named by `devUrl`. A release build without the bundled UI does not compile.
//! - **The CSP** is in `tauri.conf.json`; **the capability** in `capabilities/main.json`: the
//!   gateway's commands and listening to `gateway://workspaces`, nothing else.
//! - **Single instance**: a second launch focuses the first one's window.
//! - **Cleanup**: when the page reloads or the window closes, the gateway closes every socket that
//!   page opened. When the app quits, the supervisor stops the daemon if it started it.

use crate::daemon::endpoint::Endpoint;
use crate::daemon::supervisor::{Options, Supervisor};
use crate::daemon::{LocalConnector, follow, locate};
use crate::gateway::Gateway;
use crate::registry::{self, GatewayWorkspace, Registry};
use crate::settings::Settings;
use crate::{commands, logging};
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
        // First, so a second instance hands over and exits before anything else starts.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            focus_main(app);
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
    app.run(|handle, event| {
        if let RunEvent::Exit = event {
            shutdown(handle);
        }
    });
    ExitCode::SUCCESS
}

/// The parts of the app that tests drive too, with the mock runtime: the commands and the
/// cleanup hooks. The gateway itself is managed state (`app.manage(Gateway::new(…))`).
pub fn configure<R: Runtime>(builder: tauri::Builder<R>) -> tauri::Builder<R> {
    builder
        .invoke_handler(tauri::generate_handler![
            commands::gateway_workspaces,
            commands::gateway_request,
            commands::gateway_socket_open,
            commands::gateway_socket_send,
            commands::gateway_socket_close,
        ])
        .on_page_load(on_page_load)
        .on_window_event(on_window_event)
}

/// A page started loading (a reload, or the first load): the sockets its predecessor opened
/// close.
pub fn on_page_load<R: Runtime>(webview: &Webview<R>, payload: &PageLoadPayload<'_>) {
    if payload.event() == PageLoadEvent::Started
        && let Some(gateway) = webview.try_state::<Gateway>()
    {
        gateway.page_started(webview.label());
    }
}

/// A window closed: the sockets its page opened close.
pub fn on_window_event<R: Runtime>(window: &Window<R>, event: &WindowEvent) {
    if let WindowEvent::Destroyed = event
        && let Some(gateway) = window.try_state::<Gateway>()
    {
        gateway.forget(window.label());
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

/// The supervisor, kept to stop the daemon when the app quits.
struct Daemon(Mutex<Option<Supervisor>>);

fn setup(app: &mut App) -> Result<(), Box<dyn std::error::Error>> {
    let handle = app.handle().clone();
    let paths = app.path();
    let settings = Settings::load(&paths.app_config_dir()?).with_env(|name| std::env::var_os(name));
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
    registry.on_change(move |list| emit_workspaces(&emitter, list));
    let local = Arc::new(LocalConnector::new(endpoint.clone()));
    registry.attach_local(Arc::clone(&local) as Arc<dyn crate::gateway::Connector>);
    app.manage(Gateway::new(Arc::clone(&registry)));

    let runtime = tauri::async_runtime::handle();
    let supervisor = Supervisor::start(
        Options::new(program, settings.state_dir.clone(), endpoint),
        runtime.inner(),
    );
    local.notify_failures(supervisor.poke_handle());
    runtime.spawn(follow(supervisor.state(), local, registry));
    app.manage(Daemon(Mutex::new(Some(supervisor))));

    main_window(app)?;
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

fn focus_main<R: Runtime>(app: &AppHandle<R>) {
    if let Some(window) = app.get_webview_window(MAIN) {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

/// When the app quits: stop the daemon if this app started it.
fn shutdown<R: Runtime>(app: &AppHandle<R>) {
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
