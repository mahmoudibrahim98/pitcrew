//! What keeps PitCrew useful in the background: "needs you" ([`crate::attention`]), the
//! notifications ([`crate::notify`]), the tray ([`crate::tray`]) and the preferences that steer
//! them. One managed [`Shell`] holds them; the window's close and the tray's menu ask it what to
//! do.

use crate::app::MAIN;
use crate::attention::{Attention, AttentionSink, Count, NewAsk};
use crate::navigate::{Navigator, focus_main};
use crate::notify::{Notice, Notifications, RateLimiter};
use crate::preferences::PreferenceStore;
use crate::registry::Registry;
use crate::{notify, tray};
use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tauri::{AppHandle, Manager as _, Runtime};

/// The background parts, managed by the app.
pub struct Shell {
    /// The workspaces.
    pub registry: Arc<Registry>,
    /// Their "needs you" counts.
    pub attention: Arc<Attention>,
    /// Notifications for new asks.
    pub notifications: Notifications,
    /// The person's preferences.
    pub preferences: PreferenceStore,
    tray: AtomicBool,
    /// A tray rebuild is scheduled.
    pub(crate) tray_refresh: AtomicBool,
}

impl fmt::Debug for Shell {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Shell")
            .field("tray", &self.tray_available())
            .field("preferences", &self.preferences.get())
            .finish_non_exhaustive()
    }
}

impl Shell {
    /// Starts the background parts for `app`: notifications through the platform's notifier, and
    /// a watcher for each ready workspace in `registry`. The tray is created separately
    /// ([`Shell::create_tray`]), on the main thread.
    pub fn start<R: Runtime>(
        app: &AppHandle<R>,
        registry: Arc<Registry>,
        preferences: PreferenceStore,
    ) -> Self {
        let runtime = tauri::async_runtime::handle().inner().clone();
        let clicked = app.clone();
        let notifier = notify::platform(
            &app.config().identifier,
            Arc::new(
                move |target| match (target, clicked.try_state::<Navigator>()) {
                    (Some(target), Some(navigator)) => navigator.navigate(&clicked, target),
                    _ => focus_main(&clicked),
                },
            ),
            runtime.clone(),
        );
        let gate = app.clone();
        let notifications = Notifications::new(
            notifier,
            RateLimiter::new(Notifications::GATHER, Notifications::COOLDOWN),
            move || allowed(&gate),
            &runtime,
        );
        let attention = Arc::new(Attention::new(
            Arc::clone(&registry),
            Arc::new(Sink(app.clone())),
            runtime,
        ));
        attention.sync(&registry.list());
        Self::from_parts(registry, attention, notifications, preferences)
    }

    /// A shell from its parts, without a tray yet ([`Shell::start`] builds the parts for the
    /// app; tests build their own).
    #[must_use]
    pub fn from_parts(
        registry: Arc<Registry>,
        attention: Arc<Attention>,
        notifications: Notifications,
        preferences: PreferenceStore,
    ) -> Self {
        Self {
            registry,
            attention,
            notifications,
            preferences,
            tray: AtomicBool::new(false),
            tray_refresh: AtomicBool::new(false),
        }
    }

    /// Creates the tray icon (on the main thread), and remembers whether there is one.
    pub fn create_tray<R: Runtime>(&self, app: &AppHandle<R>) {
        self.tray.store(tray::create(app), Ordering::SeqCst);
    }

    /// Whether the tray icon is up.
    #[must_use]
    pub fn tray_available(&self) -> bool {
        self.tray.load(Ordering::SeqCst)
    }

    /// Whether closing the window keeps the app running: there is a tray, and the person did not
    /// choose to quit on close.
    #[must_use]
    pub fn keeps_running_on_close(&self) -> bool {
        self.tray_available() && !self.preferences.get().quit_on_close
    }

    /// The window was closed into the tray: the first time ever, say so.
    pub fn closed_to_tray(&self) {
        let before = self.preferences.get();
        if before.tray_hint_shown {
            return;
        }
        let _ = self.preferences.update(|p| p.tray_hint_shown = true);
        if before.notifications {
            self.notifications.show_now(Notice {
                title: "PitCrew is still running".to_owned(),
                body: "It stays in the tray and tells you when an agent needs you. To quit when \
                       the window closes instead, use the tray menu."
                    .to_owned(),
                target: None,
            });
        }
    }

    /// Every workspace's count, added up.
    #[must_use]
    pub fn total(&self) -> Count {
        self.attention
            .counts()
            .into_values()
            .fold(Count::default(), Count::plus)
    }

    /// Stops the watchers (the app is quitting).
    pub fn stop(&self) {
        self.attention.stop();
    }
}

/// Notifications are shown while they are on and the window is not in front of the person.
fn allowed<R: Runtime>(app: &AppHandle<R>) -> bool {
    let on = app
        .try_state::<Shell>()
        .is_some_and(|shell| shell.preferences.get().notifications);
    let looking = app.get_webview_window(MAIN).is_some_and(|window| {
        window.is_visible().unwrap_or(false)
            && window.is_focused().unwrap_or(false)
            && !window.is_minimized().unwrap_or(false)
    });
    on && !looking
}

/// "Needs you" goes to the tray and the notifications.
struct Sink<R: Runtime>(AppHandle<R>);

impl<R: Runtime> AttentionSink for Sink<R> {
    fn counts_changed(&self) {
        tray::refresh(&self.0);
    }

    fn new_ask(&self, workspace: &str, ask: NewAsk) {
        if let Some(shell) = self.0.try_state::<Shell>() {
            shell.notifications.new_ask(workspace, ask);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notify::Notifier;
    use crate::preferences::Preferences;
    use std::sync::Mutex;
    use std::time::Duration;

    #[derive(Default)]
    struct Shown(Mutex<Vec<Notice>>);

    impl Notifier for Shown {
        fn show(&self, notice: Notice) {
            self.0.lock().unwrap().push(notice);
        }
    }

    struct Nowhere;

    impl AttentionSink for Nowhere {
        fn counts_changed(&self) {}
        fn new_ask(&self, _workspace: &str, _ask: NewAsk) {}
    }

    fn shell(preferences: Preferences, shown: &Arc<Shown>) -> Shell {
        let runtime = tokio::runtime::Handle::current();
        let registry = Arc::new(Registry::in_memory());
        Shell::from_parts(
            Arc::clone(&registry),
            Arc::new(Attention::new(registry, Arc::new(Nowhere), runtime.clone())),
            Notifications::new(
                Arc::clone(shown) as Arc<dyn Notifier>,
                RateLimiter::new(Duration::ZERO, Duration::ZERO),
                || true,
                &runtime,
            ),
            PreferenceStore::in_memory(preferences),
        )
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn closing_keeps_the_app_in_the_tray_and_says_so_once() {
        let shown = Arc::new(Shown::default());
        let s = shell(Preferences::default(), &shown);
        assert!(!s.keeps_running_on_close(), "no tray: closing quits");
        s.tray.store(true, Ordering::SeqCst);
        assert!(s.keeps_running_on_close());

        s.closed_to_tray();
        s.closed_to_tray();
        let notices = shown.0.lock().unwrap().clone();
        assert_eq!(notices.len(), 1, "said once: {notices:?}");
        assert_eq!(notices[0].title, "PitCrew is still running");
        assert_eq!(notices[0].target, None, "a click only shows the window");
        assert!(notices[0].body.chars().count() <= crate::notify::MAX_BODY);
        assert!(s.preferences.get().tray_hint_shown);

        s.preferences.update(|p| p.quit_on_close = true).unwrap();
        assert!(
            !s.keeps_running_on_close(),
            "the person chose to quit on close"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn with_notifications_off_the_hint_is_not_shown() {
        let shown = Arc::new(Shown::default());
        let s = shell(
            Preferences {
                notifications: false,
                ..Preferences::default()
            },
            &shown,
        );
        s.tray.store(true, Ordering::SeqCst);
        s.closed_to_tray();
        assert!(shown.0.lock().unwrap().is_empty());
        assert!(s.preferences.get().tray_hint_shown, "and not later either");
    }
}
