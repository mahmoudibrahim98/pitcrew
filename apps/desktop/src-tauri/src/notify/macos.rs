//! Notifications on macOS: the notification centre (`mac-notification-sys`, as Tauri's
//! notification plugin reaches it). A click calls back with the notification's target.
//!
//! Waiting for a click holds a thread until the notification is clicked or dismissed, so at most
//! [`MAX_WAITING`] wait; past that, notifications are sent without waiting (a click then only
//! brings the app forward). A release build sends as the app's bundle; a debug build is not a
//! bundle, so it sends as the system's default.

use super::{Notice, Notifier, OnClick};
use mac_notification_sys::{Notification, NotificationResponse};
use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Notifications that may wait for a click at once.
const MAX_WAITING: usize = 4;

/// The notification centre.
pub struct MacNotifier {
    bundle: Option<String>,
    on_click: OnClick,
    runtime: tokio::runtime::Handle,
    waiting: Arc<AtomicUsize>,
}

impl fmt::Debug for MacNotifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MacNotifier").finish_non_exhaustive()
    }
}

impl MacNotifier {
    /// Notifications as the bundle `identifier` (release builds).
    #[must_use]
    pub fn new(identifier: &str, on_click: OnClick, runtime: tokio::runtime::Handle) -> Self {
        Self {
            bundle: (!cfg!(debug_assertions)).then(|| identifier.to_owned()),
            on_click,
            runtime,
            waiting: Arc::new(AtomicUsize::new(0)),
        }
    }
}

impl Notifier for MacNotifier {
    fn show(&self, notice: Notice) {
        let bundle = self.bundle.clone();
        let on_click = Arc::clone(&self.on_click);
        let waiting = Arc::clone(&self.waiting);
        self.runtime.spawn_blocking(move || {
            if let Some(bundle) = &bundle {
                // Only the first call sets it; later ones say it is set already.
                let _ = mac_notification_sys::set_application(bundle);
            }
            let wait = waiting.fetch_add(1, Ordering::SeqCst) < MAX_WAITING;
            if !wait {
                waiting.fetch_sub(1, Ordering::SeqCst);
            }
            let mut notification = Notification::new();
            notification.title(&notice.title).message(&notice.body);
            if wait {
                notification.wait_for_click(true);
            } else {
                notification.asynchronous(true);
            }
            let response = notification.send();
            if wait {
                waiting.fetch_sub(1, Ordering::SeqCst);
            }
            match response {
                Ok(NotificationResponse::Click) => on_click(notice.target),
                Ok(_) => {}
                Err(e) => tracing::info!(error = %e, "cannot show a notification"),
            }
        });
    }
}
