//! The tray: an icon with a menu.
//!
//! - "Open PitCrew" shows and focuses the window (also a left click on the icon, on Windows).
//! - One line per workspace with its "needs you" count; it opens that workspace's Inbox.
//! - The two preferences, as check items: notifications, and quitting when the window closes.
//! - "Quit PitCrew" quits; the daemon stops only if the app started it.
//!
//! **Is there a tray?** Always on Windows and macOS. On Linux, only when a tray host is running
//! (`org.kde.StatusNotifierWatcher` on the session bus, with a host registered) and the
//! appindicator library loads; otherwise there is no icon, and closing the window quits, as
//! before.
//!
//! The menu is rebuilt when a count, a workspace or a preference changes, at most every 250 ms.

use crate::attention::Count;
use crate::navigate::{NavigateTarget, Navigator, focus_main};
use crate::notify::clean;
use crate::registry::WorkspaceState;
use crate::shell::Shell;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tauri::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager as _, Runtime};

/// The tray icon's id.
pub const TRAY_ID: &str = "pitcrew";

const OPEN: &str = "open";
const QUIT: &str = "quit";
const NOTIFY: &str = "pref:notifications";
const QUIT_ON_CLOSE: &str = "pref:quit-on-close";
const WORKSPACE: &str = "ws:";
/// Workspaces listed in the menu at most.
const MAX_LINES: usize = 20;
/// The longest workspace name in the menu, in characters.
const MAX_NAME: usize = 40;
/// How long changes are gathered before the menu is rebuilt.
const SETTLE: Duration = Duration::from_millis(250);

/// A workspace's line: its name and what needs the person there.
#[must_use]
pub fn workspace_line(name: &str, state: WorkspaceState, count: Option<Count>) -> String {
    let status = match (state, count) {
        (
            WorkspaceState::Ready,
            Some(Count {
                open: 0,
                more: false,
            }),
        ) => "nothing needs you".to_owned(),
        (WorkspaceState::Ready, Some(Count { open, more })) => {
            let verb = if open == 1 && !more { "needs" } else { "need" };
            format!("{open}{} {verb} you", if more { "+" } else { "" })
        }
        (WorkspaceState::Ready, None) => "counting…".to_owned(),
        (WorkspaceState::Connecting, _) => "connecting".to_owned(),
        (WorkspaceState::Unreachable, _) => "unreachable".to_owned(),
        (WorkspaceState::NeedsPairing, _) => "needs pairing".to_owned(),
    };
    let name = clean(name, MAX_NAME);
    let name = if name.is_empty() {
        "Workspace".to_owned()
    } else {
        name
    };
    menu_text(&format!("{name} — {status}"))
}

/// The icon's tooltip.
#[must_use]
pub fn tooltip(total: Count) -> String {
    match total {
        Count {
            open: 0,
            more: false,
        } => "PitCrew".to_owned(),
        Count { open, more } => format!("PitCrew — {open}{} need you", if more { "+" } else { "" }),
    }
}

/// Menu text shows `&` as itself (a single one marks a mnemonic).
fn menu_text(text: &str) -> String {
    text.replace('&', "&&")
}

/// Creates the tray icon, if this desktop has a tray. Call on the main thread (in `setup`).
pub fn create<R: Runtime>(app: &AppHandle<R>) -> bool {
    #[cfg(target_os = "linux")]
    if !linux_host_present() {
        tracing::info!("no tray on this desktop: closing the window quits");
        return false;
    }
    let Some(icon) = app.default_window_icon().cloned() else {
        tracing::warn!("no icon for the tray");
        return false;
    };
    let menu = match menu(app) {
        Ok(menu) => menu,
        Err(e) => {
            tracing::warn!(error = %e, "cannot build the tray menu");
            return false;
        }
    };
    let builder = TrayIconBuilder::with_id(TRAY_ID)
        .icon(icon)
        .tooltip("PitCrew")
        .menu(&menu)
        .show_menu_on_left_click(!cfg!(windows))
        .on_menu_event(on_menu_event)
        .on_tray_icon_event(on_icon_event);
    // libappindicator is loaded when the first icon is built, and its loader panics when the
    // library is missing: that means no tray, not no app.
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| builder.build(app))) {
        Ok(Ok(_)) => {
            tracing::info!("tray icon up");
            true
        }
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "cannot create the tray icon: closing the window quits");
            false
        }
        Err(_) => {
            tracing::warn!("the tray library is missing: closing the window quits");
            false
        }
    }
}

/// Rebuilds the menu and tooltip soon (changes within [`SETTLE`] are rebuilt once).
pub fn refresh<R: Runtime>(app: &AppHandle<R>) {
    let Some(shell) = app.try_state::<Shell>() else {
        return;
    };
    if !shell.tray_available() || shell.tray_refresh.swap(true, Ordering::SeqCst) {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(SETTLE).await;
        if let Some(shell) = app.try_state::<Shell>() {
            shell.tray_refresh.store(false, Ordering::SeqCst);
        }
        rebuild(&app);
    });
}

fn rebuild<R: Runtime>(app: &AppHandle<R>) {
    let (Some(tray), Some(shell)) = (app.tray_by_id(TRAY_ID), app.try_state::<Shell>()) else {
        return;
    };
    match menu(app) {
        Ok(menu) => {
            if let Err(e) = tray.set_menu(Some(menu)) {
                tracing::warn!(error = %e, "cannot update the tray menu");
            }
        }
        Err(e) => tracing::warn!(error = %e, "cannot build the tray menu"),
    }
    let _ = tray.set_tooltip(Some(tooltip(shell.total())));
}

fn menu<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<Menu<R>> {
    let menu = Menu::new(app)?;
    menu.append(&MenuItem::with_id(
        app,
        OPEN,
        "Open PitCrew",
        true,
        None::<&str>,
    )?)?;
    menu.append(&PredefinedMenuItem::separator(app)?)?;
    let Some(shell) = app.try_state::<Shell>() else {
        return Ok(menu);
    };
    let list = shell.registry.list();
    let counts = shell.attention.counts();
    if list.is_empty() {
        menu.append(&MenuItem::with_id(
            app,
            "none",
            "No workspaces yet",
            false,
            None::<&str>,
        )?)?;
    }
    for workspace in list.iter().take(MAX_LINES) {
        let line = workspace_line(
            &workspace.name,
            workspace.state,
            counts.get(&workspace.id).copied(),
        );
        let id = format!("{WORKSPACE}{}", workspace.id);
        menu.append(&MenuItem::with_id(app, id, line, true, None::<&str>)?)?;
    }
    menu.append(&PredefinedMenuItem::separator(app)?)?;
    let preferences = shell.preferences.get();
    menu.append(&CheckMenuItem::with_id(
        app,
        NOTIFY,
        "Notify me when an agent needs me",
        true,
        preferences.notifications,
        None::<&str>,
    )?)?;
    menu.append(&CheckMenuItem::with_id(
        app,
        QUIT_ON_CLOSE,
        "Quit when the window closes",
        true,
        preferences.quit_on_close,
        None::<&str>,
    )?)?;
    menu.append(&PredefinedMenuItem::separator(app)?)?;
    menu.append(&MenuItem::with_id(
        app,
        QUIT,
        "Quit PitCrew",
        true,
        None::<&str>,
    )?)?;
    Ok(menu)
}

fn on_menu_event<R: Runtime>(app: &AppHandle<R>, event: MenuEvent) {
    let id = event.id().as_ref();
    match id {
        OPEN => focus_main(app),
        QUIT => {
            tracing::info!("quit from the tray");
            app.exit(0);
        }
        NOTIFY | QUIT_ON_CLOSE => {
            if let Some(shell) = app.try_state::<Shell>() {
                let _ = shell.preferences.update(|p| {
                    if id == NOTIFY {
                        p.notifications = !p.notifications;
                    } else {
                        p.quit_on_close = !p.quit_on_close;
                    }
                });
            }
            refresh(app);
        }
        _ => {
            // Only the registry's own ids are in the menu; still, only a known one is opened.
            let known = id.strip_prefix(WORKSPACE).filter(|ws| {
                app.try_state::<Shell>()
                    .is_some_and(|shell| shell.registry.list().iter().any(|w| w.id == *ws))
            });
            if let (Some(workspace), Some(navigator)) = (known, app.try_state::<Navigator>()) {
                navigator.navigate(app, NavigateTarget::inbox(workspace));
            }
        }
    }
}

fn on_icon_event<R: Runtime>(tray: &TrayIcon<R>, event: TrayIconEvent) {
    if cfg!(windows)
        && let TrayIconEvent::Click {
            button: MouseButton::Left,
            button_state: MouseButtonState::Up,
            ..
        } = event
    {
        focus_main(tray.app_handle());
    }
}

/// Whether a tray host is on the session bus: a StatusNotifierWatcher with a host registered.
#[cfg(target_os = "linux")]
fn linux_host_present() -> bool {
    const WATCHER: &str = "org.kde.StatusNotifierWatcher";
    let check = || -> Result<bool, Box<dyn std::error::Error>> {
        let connection = zbus::blocking::connection::Builder::session()?
            .method_timeout(Duration::from_secs(2))
            .build()?;
        let bus = zbus::blocking::fdo::DBusProxy::new(&connection)?;
        if !bus.name_has_owner(zbus::names::BusName::try_from(WATCHER)?)? {
            return Ok(false);
        }
        let watcher =
            zbus::blocking::Proxy::new(&connection, WATCHER, "/StatusNotifierWatcher", WATCHER)?;
        Ok(watcher
            .get_property::<bool>("IsStatusNotifierHostRegistered")
            .unwrap_or(true))
    };
    check().unwrap_or_else(|e| {
        tracing::info!(error = %e, "cannot ask the session bus about a tray");
        false
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_and_tooltips() {
        let ready = WorkspaceState::Ready;
        let count = |open, more| Some(Count { open, more });
        assert_eq!(
            workspace_line("Demo Lab", ready, count(0, false)),
            "Demo Lab — nothing needs you"
        );
        assert_eq!(
            workspace_line("Demo Lab", ready, count(1, false)),
            "Demo Lab — 1 needs you"
        );
        assert_eq!(
            workspace_line("Demo Lab", ready, count(3, false)),
            "Demo Lab — 3 need you"
        );
        assert_eq!(
            workspace_line("Demo Lab", ready, count(512, true)),
            "Demo Lab — 512+ need you"
        );
        assert_eq!(
            workspace_line("Demo Lab", ready, None),
            "Demo Lab — counting…"
        );
        assert_eq!(
            workspace_line("Demo Lab", WorkspaceState::Unreachable, count(3, false)),
            "Demo Lab — unreachable"
        );
        assert_eq!(
            workspace_line("R&D\u{202e}\n", WorkspaceState::Connecting, None),
            "R&&D — connecting"
        );
        assert_eq!(
            workspace_line("", WorkspaceState::NeedsPairing, None),
            "Workspace — needs pairing"
        );
        let long = workspace_line(&"x".repeat(200), ready, None);
        assert!(long.chars().count() < 60, "{long}");

        assert_eq!(tooltip(Count::default()), "PitCrew");
        assert_eq!(
            tooltip(Count {
                open: 2,
                more: false
            }),
            "PitCrew — 2 need you"
        );
        assert_eq!(
            tooltip(Count {
                open: 9,
                more: true
            }),
            "PitCrew — 9+ need you"
        );
    }
}
