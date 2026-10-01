//! Notifications on Windows: WinRT toasts (`tauri-winrt-notification`, as Tauri's notification
//! plugin uses them). A click on the toast, while the app runs, calls back with its target.
//!
//! A toast needs the AppUserModelID of an installed shortcut: the installer's, which is the app's
//! identifier. A debug build is not installed, so it borrows PowerShell's, as Tauri's plugin does.
//! The text goes into the toast's XML as text (`SetInnerText`), never as markup.

use super::{Notice, Notifier, OnClick};
use std::fmt;
use std::sync::Arc;
use tauri_winrt_notification::Toast;

/// WinRT toasts.
pub struct ToastNotifier {
    app_id: String,
    on_click: OnClick,
    runtime: tokio::runtime::Handle,
}

impl fmt::Debug for ToastNotifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ToastNotifier")
            .field("app_id", &self.app_id)
            .finish_non_exhaustive()
    }
}

impl ToastNotifier {
    /// Toasts as the app `identifier` (release builds) or as PowerShell (debug builds).
    #[must_use]
    pub fn new(identifier: &str, on_click: OnClick, runtime: tokio::runtime::Handle) -> Self {
        let app_id = if cfg!(debug_assertions) {
            Toast::POWERSHELL_APP_ID.to_owned()
        } else {
            identifier.to_owned()
        };
        Self {
            app_id,
            on_click,
            runtime,
        }
    }
}

impl Notifier for ToastNotifier {
    fn show(&self, notice: Notice) {
        let app_id = self.app_id.clone();
        let on_click = Arc::clone(&self.on_click);
        self.runtime.spawn_blocking(move || {
            let target = notice.target;
            let shown = Toast::new(&app_id)
                .title(&notice.title)
                .text1(&notice.body)
                .on_activated(move |_action| {
                    on_click(target.clone());
                    Ok(())
                })
                .show();
            if let Err(e) = shown {
                tracing::info!(error = %e, "cannot show a notification");
            }
        });
    }
}
