//! # pitcrew-desktop
//!
//! The PitCrew desktop app (ADR-0003): a Tauri 2 shell around the UI in `apps/ui`, a gateway that
//! is the webview's only way to reach a workspace's daemon, and the supervisor of the person's
//! local `pitcrewd`. All logic stays in `pitcrewd`; this is the thin shell.
//!
//! - [`gateway`]: the commands of `docs/build/contracts/desktop-gateway.md`. The webview never
//!   holds a token: the gateway reads it, adds it, and forwards the call over the daemon's
//!   socket or pipe.
//! - [`daemon`]: finding, starting and supervising the local `pitcrewd`, and connecting to it with
//!   the client checks of `pitcrew_api::client`.
//! - [`registry`]: the workspaces, saved in the app's local data directory.
//! - [`remote`]: remote workspaces: adding a machine (probe, plan, add), its tunnel, SSH's
//!   prompts in the app, and removing it.
//! - [`keychain`]: where remote workspaces' device tokens live.
//! - [`app`]: the Tauri wiring: the window, its CSP and capability, single instance, cleanup.
//! - [`navigate`]: deep links (`pitcrew://…`) and notification clicks, as `gateway://navigate`.
//! - [`shell`]: the app in the background: "needs you" ([`attention`]), notifications
//!   ([`notify`]), the tray ([`tray`]) and the [`preferences`] behind them.
//!
//! **Owned by stream K.**

pub mod app;
pub mod attention;
pub mod commands;
pub mod daemon;
pub mod gateway;
pub mod keychain;
pub mod logging;
pub mod navigate;
pub mod notify;
pub mod preferences;
pub mod redact;
pub mod registry;
pub mod remote;
pub mod scheme;
pub mod settings;
pub mod shell;
pub mod token;
pub mod tray;

pub use app::run;

/// The app's context (config, ACL, icons, and in release builds the bundled UI), for `run` and
/// for tests on the mock runtime.
#[must_use]
pub fn context<R: tauri::Runtime>() -> tauri::Context<R> {
    tauri::generate_context!()
}
