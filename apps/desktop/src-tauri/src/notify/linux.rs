//! Notifications on Linux: the freedesktop notification service (`org.freedesktop.Notifications`)
//! on the session bus, with a `default` action, so a click comes back as `ActionInvoked`.
//!
//! - Connected on first use; a failed call drops the connection, and the next notification tries
//!   again. No service (a desktop without one): logged once, nothing shown.
//! - The body and the title are escaped when the service reads markup (`body-markup`; the spec
//!   reads it only in the body, some services in the title too), so an ask's text can never be a
//!   link or an image.
//! - Clicks are matched to the notifications this app showed: at most [`KEEP`] are remembered,
//!   and each is forgotten when the service closes it.

use super::{Notice, Notifier, OnClick};
use crate::navigate::NavigateTarget;
use futures_util::StreamExt as _;
use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// Notifications remembered for their clicks.
const KEEP: usize = 32;

#[zbus::proxy(
    interface = "org.freedesktop.Notifications",
    default_service = "org.freedesktop.Notifications",
    default_path = "/org/freedesktop/Notifications",
    gen_blocking = false
)]
trait Fdo {
    fn get_capabilities(&self) -> zbus::Result<Vec<String>>;

    #[allow(clippy::too_many_arguments)]
    fn notify(
        &self,
        app_name: &str,
        replaces_id: u32,
        app_icon: &str,
        summary: &str,
        body: &str,
        actions: &[&str],
        hints: HashMap<&str, zbus::zvariant::Value<'_>>,
        expire_timeout: i32,
    ) -> zbus::Result<u32>;

    #[zbus(signal)]
    fn action_invoked(&self, id: u32, action_key: String) -> zbus::Result<()>;

    #[zbus(signal)]
    fn notification_closed(&self, id: u32, reason: u32) -> zbus::Result<()>;
}

type Connecting = Pin<Box<dyn Future<Output = zbus::Result<zbus::Connection>> + Send>>;
type Connect = Box<dyn Fn() -> Connecting + Send + Sync>;
type Shown = Arc<Mutex<VecDeque<(u32, Option<NavigateTarget>)>>>;

/// The freedesktop notification service.
pub struct DbusNotifier {
    inner: Arc<Inner>,
}

impl fmt::Debug for DbusNotifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DbusNotifier").finish_non_exhaustive()
    }
}

struct Inner {
    connect: Connect,
    on_click: OnClick,
    runtime: tokio::runtime::Handle,
    live: tokio::sync::Mutex<Option<Live>>,
    shown: Shown,
    warned: AtomicBool,
}

struct Live {
    proxy: FdoProxy<'static>,
    markup: bool,
    actions: bool,
    listener: tokio::task::JoinHandle<()>,
}

impl Drop for Live {
    fn drop(&mut self) {
        self.listener.abort();
    }
}

impl DbusNotifier {
    /// The service on the session bus.
    #[must_use]
    pub fn session(on_click: OnClick, runtime: tokio::runtime::Handle) -> Self {
        Self::with_connect(
            Box::new(|| Box::pin(zbus::Connection::session())),
            on_click,
            runtime,
        )
    }

    /// The service on the connections `connect` makes (tests).
    fn with_connect(connect: Connect, on_click: OnClick, runtime: tokio::runtime::Handle) -> Self {
        Self {
            inner: Arc::new(Inner {
                connect,
                on_click,
                runtime,
                live: tokio::sync::Mutex::new(None),
                shown: Arc::default(),
                warned: AtomicBool::new(false),
            }),
        }
    }
}

impl Notifier for DbusNotifier {
    fn show(&self, notice: Notice) {
        let inner = Arc::clone(&self.inner);
        self.inner
            .runtime
            .spawn(async move { inner.show(notice).await });
    }
}

impl Inner {
    async fn show(&self, notice: Notice) {
        let mut live = self.live.lock().await;
        if live.is_none() {
            match self.connect().await {
                Ok(connected) => *live = Some(connected),
                Err(e) => {
                    if !self.warned.swap(true, Ordering::Relaxed) {
                        tracing::info!(error = %e, "no notification service; notifications are not shown");
                    }
                    return;
                }
            }
        }
        let Some(current) = live.as_ref() else {
            return;
        };
        // The spec reads markup only in the body, but some services read it in the summary too.
        let (summary, body) = if current.markup {
            (escape(&notice.title), escape(&notice.body))
        } else {
            (notice.title.clone(), notice.body.clone())
        };
        let actions: &[&str] = if current.actions {
            &["default", "Open"]
        } else {
            &[]
        };
        let shown = current
            .proxy
            .notify(
                "PitCrew",
                0,
                "",
                &summary,
                &body,
                actions,
                HashMap::new(),
                -1,
            )
            .await;
        match shown {
            Ok(id) => {
                let mut kept = self
                    .shown
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                kept.retain(|(known, _)| *known != id);
                if kept.len() >= KEEP {
                    kept.pop_front();
                }
                kept.push_back((id, notice.target));
            }
            Err(e) => {
                tracing::info!(error = %e, "cannot show a notification");
                // Reconnect next time.
                *live = None;
            }
        }
    }

    /// Connects, reads the service's capabilities, and listens for clicks and closes.
    async fn connect(&self) -> zbus::Result<Live> {
        let connection = (self.connect)().await?;
        let proxy = FdoProxy::new(&connection).await?;
        let capabilities = proxy.get_capabilities().await?;
        let mut invoked = proxy.receive_action_invoked().await?;
        let mut closed = proxy.receive_notification_closed().await?;
        let shown = Arc::clone(&self.shown);
        let on_click = Arc::clone(&self.on_click);
        let listener = self.runtime.spawn(async move {
            loop {
                tokio::select! {
                    Some(signal) = invoked.next() => {
                        let Ok(args) = signal.args() else { continue };
                        let target = take(&shown, args.id);
                        if args.action_key == "default" && let Some(target) = target {
                            on_click(target);
                        }
                    }
                    Some(signal) = closed.next() => {
                        if let Ok(args) = signal.args() {
                            take(&shown, args.id);
                        }
                    }
                    else => break,
                }
            }
        });
        let has = |name: &str| capabilities.iter().any(|c| c == name);
        Ok(Live {
            markup: has("body-markup"),
            actions: has("actions"),
            proxy,
            listener,
        })
    }
}

/// Forgets notification `id`; its target if this app showed it (`Some(None)` for one that only
/// shows the window).
fn take(shown: &Shown, id: u32) -> Option<Option<NavigateTarget>> {
    let mut kept = shown
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let at = kept.iter().position(|(known, _)| *known == id)?;
    kept.remove(at).map(|(_, target)| target)
}

/// Text that the service reads as markup shows as itself.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    const PATH: &str = "/org/freedesktop/Notifications";
    const IFACE: &str = "org.freedesktop.Notifications";

    #[derive(Clone, Debug, PartialEq)]
    struct Seen {
        app: String,
        summary: String,
        body: String,
        actions: Vec<String>,
    }

    struct FakeService {
        capabilities: Vec<String>,
        seen: Arc<Mutex<Vec<Seen>>>,
        next: u32,
    }

    #[zbus::interface(name = "org.freedesktop.Notifications")]
    impl FakeService {
        fn get_capabilities(&self) -> Vec<String> {
            self.capabilities.clone()
        }

        #[allow(clippy::too_many_arguments)]
        fn notify(
            &mut self,
            app_name: String,
            _replaces_id: u32,
            _app_icon: String,
            summary: String,
            body: String,
            actions: Vec<String>,
            _hints: HashMap<String, zbus::zvariant::OwnedValue>,
            _expire_timeout: i32,
        ) -> u32 {
            self.next += 1;
            self.seen.lock().unwrap().push(Seen {
                app: app_name,
                summary,
                body,
                actions,
            });
            self.next
        }
    }

    /// A notifier and a fake service, over a private connection.
    async fn pair(
        capabilities: &[&str],
    ) -> (
        DbusNotifier,
        zbus::Connection,
        Arc<Mutex<Vec<Seen>>>,
        Arc<Mutex<Vec<Option<NavigateTarget>>>>,
    ) {
        let (a, b) = std::os::unix::net::UnixStream::pair().unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let service = FakeService {
            capabilities: capabilities.iter().map(|c| (*c).to_owned()).collect(),
            seen: Arc::clone(&seen),
            next: 0,
        };
        let server = zbus::connection::Builder::async_io_unix_stream(a)
            .server(zbus::Guid::generate())
            .unwrap()
            .p2p()
            .serve_at(PATH, service)
            .unwrap()
            .build();
        let client = zbus::connection::Builder::async_io_unix_stream(b)
            .p2p()
            .build();
        let (server, client) = tokio::join!(server, client);
        let (server, client) = (server.unwrap(), client.unwrap());
        let clicks = Arc::new(Mutex::new(Vec::new()));
        let seen_clicks = Arc::clone(&clicks);
        let notifier = DbusNotifier::with_connect(
            Box::new(move || {
                let client = client.clone();
                Box::pin(async move { Ok(client) })
            }),
            Arc::new(move |target| seen_clicks.lock().unwrap().push(target)),
            tokio::runtime::Handle::current(),
        );
        (notifier, server, seen, clicks)
    }

    async fn until(what: &str, check: impl Fn() -> bool) {
        for _ in 0..500 {
            if check() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("timed out waiting for {what}");
    }

    async fn emit<B>(server: &zbus::Connection, member: &str, body: &B)
    where
        B: serde::Serialize + zbus::zvariant::DynamicType,
    {
        server
            .emit_signal(None::<&str>, PATH, IFACE, member, body)
            .await
            .unwrap();
    }

    fn notice(n: u32) -> Notice {
        Notice {
            title: format!("Writer <i>&</i> has a question {n}"),
            body: "Use <b>bold</b> & <a href=\"x\">links</a>?".into(),
            target: Some(NavigateTarget::inbox(format!(
                "01JA00000000000000000000{n:02}"
            ))),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn shown_escaped_and_clicked() {
        let (notifier, server, seen, clicks) = pair(&["actions", "body", "body-markup"]).await;
        notifier.show(notice(1));
        until("the notification", || seen.lock().unwrap().len() == 1).await;
        let first = seen.lock().unwrap()[0].clone();
        assert_eq!(first.app, "PitCrew");
        assert_eq!(
            first.summary,
            "Writer &lt;i&gt;&amp;&lt;/i&gt; has a question 1"
        );
        assert_eq!(
            first.body,
            "Use &lt;b&gt;bold&lt;/b&gt; &amp; &lt;a href=\"x\"&gt;links&lt;/a&gt;?"
        );
        assert_eq!(first.actions, ["default", "Open"]);

        // A click on it: its target.
        emit(&server, "ActionInvoked", &(1u32, "default")).await;
        until("the click", || clicks.lock().unwrap().len() == 1).await;
        assert_eq!(
            clicks.lock().unwrap()[0],
            Some(NavigateTarget::inbox("01JA0000000000000000000001"))
        );

        // A closed one, another action, or one this app never showed: nothing.
        notifier.show(notice(2));
        until("the second", || seen.lock().unwrap().len() == 2).await;
        emit(&server, "NotificationClosed", &(2u32, 2u32)).await;
        notifier.show(notice(3));
        until("the third", || seen.lock().unwrap().len() == 3).await;
        // A malformed close is ignored: 3 is still clickable.
        emit(&server, "NotificationClosed", &(3u32, "x")).await;
        emit(&server, "ActionInvoked", &(2u32, "default")).await;
        emit(&server, "ActionInvoked", &(99u32, "default")).await;
        emit(&server, "ActionInvoked", &(3u32, "default")).await;
        until("the second click", || clicks.lock().unwrap().len() == 2).await;
        // Each notification is clicked once; another action is not a click.
        emit(&server, "ActionInvoked", &(3u32, "default")).await;
        notifier.show(notice(4));
        until("the fourth", || seen.lock().unwrap().len() == 4).await;
        emit(&server, "ActionInvoked", &(4u32, "other")).await;
        emit(&server, "ActionInvoked", &(4u32, "default")).await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(
            *clicks.lock().unwrap(),
            [
                Some(NavigateTarget::inbox("01JA0000000000000000000001")),
                Some(NavigateTarget::inbox("01JA0000000000000000000003")),
            ]
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn plain_text_without_markup_or_actions() {
        let (notifier, _server, seen, _clicks) = pair(&["body"]).await;
        notifier.show(notice(1));
        until("the notification", || seen.lock().unwrap().len() == 1).await;
        let first = seen.lock().unwrap()[0].clone();
        assert_eq!(first.body, "Use <b>bold</b> & <a href=\"x\">links</a>?");
        assert_eq!(first.summary, "Writer <i>&</i> has a question 1");
        assert!(first.actions.is_empty());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn no_service_is_quiet() {
        let notifier = DbusNotifier::with_connect(
            Box::new(|| {
                Box::pin(async { Err(zbus::Error::Address("no session bus here".into())) })
            }),
            Arc::new(|_| panic!("no click")),
            tokio::runtime::Handle::current(),
        );
        notifier.show(notice(1));
        notifier.show(notice(2));
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(notifier.inner.warned.load(Ordering::Relaxed));
    }

    #[test]
    fn markup_is_escaped() {
        assert_eq!(escape("a < b && c > d"), "a &lt; b &amp;&amp; c &gt; d");
        assert_eq!(escape("plain"), "plain");
    }
}
