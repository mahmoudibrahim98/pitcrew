//! OS notifications for new asks addressed to the person, while the window is not focused.
//!
//! - **Text:** the title names the asker and the kind ("Writer has a question"); the body is the
//!   ask's text. Both are cleaned ([`clean`]): control and bidi characters removed, whitespace
//!   collapsed, cut to a length (200 characters for the body).
//! - **Rate-limited** ([`RateLimiter`]): asks are gathered for a moment, and notifications are at
//!   least a cooldown apart, so a burst becomes one notification ("3 agents need you").
//! - **Clicking one** navigates to the ask's place: its task, or the workspace's Inbox
//!   (`gateway://navigate`, see [`crate::navigate`]).
//! - **A setting** turns them off (the tray menu; `preferences.json`).
//! - **Platforms** ([`Notifier`]): the freedesktop notification service over D-Bus on Linux,
//!   WinRT toasts on Windows, the notification centre on macOS. Nothing is asked of, or given to,
//!   the webview: no notification permission is in its capability.

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(windows)]
mod windows;

use crate::attention::NewAsk;
use crate::navigate::NavigateTarget;
use pitcrew_protocol::ids::MemberId;
use pitcrew_protocol::model::AskKind;
use std::collections::BTreeSet;
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};
use tokio::sync::Notify;

/// The longest notification body, in characters.
pub const MAX_BODY: usize = 200;
/// The longest notification title, in characters.
pub const MAX_TITLE: usize = 100;
/// The longest asker's name in a title, in characters.
const MAX_NAME: usize = 48;
/// Distinct askers counted in one summary; past that it reads "48+".
const MAX_ASKERS: usize = 48;

/// A notification to show.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notice {
    /// The title, cleaned.
    pub title: String,
    /// The body, cleaned.
    pub body: String,
    /// Where clicking it goes; `None` only shows the window.
    pub target: Option<NavigateTarget>,
}

/// Called when the person clicks a notification, with its target.
pub type OnClick = Arc<dyn Fn(Option<NavigateTarget>) + Send + Sync>;

/// Shows notifications on one platform.
pub trait Notifier: Send + Sync + 'static {
    /// Shows `notice`, without waiting. A click calls the notifier's [`OnClick`].
    fn show(&self, notice: Notice);
}

/// `text` for a notification: control characters (newlines and tabs become spaces) and bidi and
/// zero-width characters removed, whitespace collapsed, and cut to `max` characters, the last
/// being `…` when cut.
#[must_use]
pub fn clean(text: &str, max: usize) -> String {
    let mut out = String::with_capacity(text.len().min(max * 4));
    let mut count = 0;
    let mut space = false;
    let mut cut = false;
    for c in text.chars() {
        let c = match c {
            '\n' | '\r' | '\t' | '\u{2028}' | '\u{2029}' => ' ',
            c if c.is_control() || is_invisible(c) => continue,
            c => c,
        };
        if c.is_whitespace() {
            space = !out.is_empty();
            continue;
        }
        let needed = usize::from(space) + 1;
        if count + needed > max {
            cut = true;
            break;
        }
        if space {
            out.push(' ');
            count += 1;
            space = false;
        }
        out.push(c);
        count += 1;
    }
    if cut && max > 0 {
        while count + 1 > max {
            if out.pop().is_none() {
                break;
            }
            count -= 1;
        }
        let trimmed = out.trim_end().len();
        out.truncate(trimmed);
        out.push('…');
    }
    out
}

/// Bidi controls, zero-width characters and the byte-order mark.
fn is_invisible(c: char) -> bool {
    matches!(
        c,
        '\u{061C}'
            | '\u{200B}'..='\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{2069}'
            | '\u{FEFF}'
    )
}

/// What an asker of `kind` is doing, after their name.
fn phrase(kind: AskKind) -> &'static str {
    match kind {
        AskKind::Question => "has a question",
        AskKind::Decision => "needs a decision",
        AskKind::Review => "asks for a review",
        AskKind::Approval => "asks for approval",
        AskKind::Mention => "mentioned you",
    }
}

fn name_of(ask: &NewAsk) -> String {
    let name = clean(ask.from.as_deref().unwrap_or(""), MAX_NAME);
    if name.is_empty() {
        "An agent".to_owned()
    } else {
        name
    }
}

/// The notification for one ask in `workspace`.
#[must_use]
pub fn notice_for(workspace: &str, ask: &NewAsk) -> Notice {
    let text = if ask.body.trim().is_empty() {
        ask.title.clone()
    } else {
        format!("{} — {}", ask.title, ask.body)
    };
    let target = match &ask.task {
        Some(task) => NavigateTarget::task(workspace, task.0.to_string()),
        None => NavigateTarget::inbox(workspace),
    };
    Notice {
        title: clean(&format!("{} {}", name_of(ask), phrase(ask.kind)), MAX_TITLE),
        body: clean(&text, MAX_BODY),
        target: Some(target),
    }
}

/// Asks held back while notifications wait.
#[derive(Debug, Default)]
struct Held {
    asks: usize,
    askers: BTreeSet<MemberId>,
    more_askers: bool,
    latest: Option<(String, NewAsk)>,
}

impl Held {
    fn add(&mut self, workspace: &str, ask: NewAsk) {
        self.asks += 1;
        if self.askers.len() < MAX_ASKERS || self.askers.contains(&ask.asker) {
            self.askers.insert(ask.asker);
        } else {
            self.more_askers = true;
        }
        self.latest = Some((workspace.to_owned(), ask));
    }

    fn notice(self) -> Option<Notice> {
        let (workspace, latest) = self.latest?;
        if self.asks == 1 {
            return Some(notice_for(&workspace, &latest));
        }
        let title = if self.askers.len() > 1 {
            let more = if self.more_askers { "+" } else { "" };
            format!("{}{more} agents need you", self.askers.len())
        } else {
            format!("{} needs you", name_of(&latest))
        };
        Some(Notice {
            title: clean(&title, MAX_TITLE),
            body: format!("{} new asks", self.asks),
            target: Some(NavigateTarget::inbox(workspace)),
        })
    }
}

/// Turns asks into notifications: the first of a burst waits `gather`, so the burst is shown as
/// one; notifications are at least `cooldown` apart, and what arrives meanwhile is shown as one
/// when it ends.
#[derive(Debug)]
pub struct RateLimiter {
    gather: Duration,
    cooldown: Duration,
    last: Option<Instant>,
    due: Option<Instant>,
    held: Held,
}

impl RateLimiter {
    /// A limiter: a burst within `gather` is one notification; at most one per `cooldown`.
    #[must_use]
    pub fn new(gather: Duration, cooldown: Duration) -> Self {
        Self {
            gather,
            cooldown,
            last: None,
            due: None,
            held: Held::default(),
        }
    }

    /// Holds `ask`, and returns when the notification that will hold it is due.
    pub fn offer(&mut self, workspace: &str, ask: NewAsk, now: Instant) -> Instant {
        self.held.add(workspace, ask);
        let (gather, cooldown, last) = (self.gather, self.cooldown, self.last);
        *self.due.get_or_insert_with(|| {
            let gathered = now + gather;
            last.map_or(gathered, |last| gathered.max(last + cooldown))
        })
    }

    /// When the next notification is due, if one is waiting.
    #[must_use]
    pub fn due(&self) -> Option<Instant> {
        self.due
    }

    /// The notification due by `now`, if any: one ask's own, or a summary of several.
    pub fn take(&mut self, now: Instant) -> Option<Notice> {
        if self.due.is_none_or(|due| now < due) {
            return None;
        }
        self.due = None;
        let notice = std::mem::take(&mut self.held).notice()?;
        self.last = Some(now);
        Some(notice)
    }

    /// Forgets what is held (the person is looking at the window, or turned notifications off).
    pub fn drop_held(&mut self) {
        self.held = Held::default();
        self.due = None;
    }
}

/// The app's notifications: asks in, rate-limited notices out, while `allowed` says so (the
/// setting is on and the window is not focused).
pub struct Notifications {
    inner: Arc<Inner>,
    task: tokio::task::JoinHandle<()>,
}

struct Inner {
    notifier: Arc<dyn Notifier>,
    limiter: Mutex<RateLimiter>,
    allowed: Box<dyn Fn() -> bool + Send + Sync>,
    wake: Notify,
}

impl fmt::Debug for Notifications {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Notifications").finish_non_exhaustive()
    }
}

impl Notifications {
    /// The default pacing: a burst within 2 s is one notification; at most one per 30 s.
    pub const GATHER: Duration = Duration::from_secs(2);
    /// See [`Self::GATHER`].
    pub const COOLDOWN: Duration = Duration::from_secs(30);

    /// Notifications through `notifier`, paced by `limiter`, shown only while `allowed`.
    #[must_use]
    pub fn new(
        notifier: Arc<dyn Notifier>,
        limiter: RateLimiter,
        allowed: impl Fn() -> bool + Send + Sync + 'static,
        runtime: &tokio::runtime::Handle,
    ) -> Self {
        let inner = Arc::new(Inner {
            notifier,
            limiter: Mutex::new(limiter),
            allowed: Box::new(allowed),
            wake: Notify::new(),
        });
        let task = runtime.spawn(pace(Arc::clone(&inner)));
        Self { inner, task }
    }

    /// A new ask for the person in `workspace`.
    pub fn new_ask(&self, workspace: &str, ask: NewAsk) {
        if !(self.inner.allowed)() {
            return;
        }
        self.inner.limiter().offer(workspace, ask, Instant::now());
        self.inner.wake.notify_one();
    }

    /// Shows `notice` now, outside the pacing (the app's own messages).
    pub fn show_now(&self, notice: Notice) {
        self.inner.notifier.show(notice);
    }
}

impl Drop for Notifications {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Inner {
    fn limiter(&self) -> MutexGuard<'_, RateLimiter> {
        self.limiter
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// Shows each notification when it is due, unless it is no longer allowed by then.
async fn pace(inner: Arc<Inner>) {
    loop {
        let due = inner.limiter().due();
        match due {
            Some(due) => {
                tokio::select! {
                    () = tokio::time::sleep_until(tokio::time::Instant::from_std(due)) => {}
                    () = inner.wake.notified() => continue,
                }
            }
            None => {
                inner.wake.notified().await;
                continue;
            }
        }
        let notice = if (inner.allowed)() {
            inner.limiter().take(Instant::now())
        } else {
            inner.limiter().drop_held();
            None
        };
        if let Some(notice) = notice {
            tracing::debug!(to = ?notice.target.as_ref().map(|t| t.kind.name()), "notify");
            inner.notifier.show(notice);
        }
    }
}

/// The platform's notifier. `identifier` is the app's (its AppUserModelID on Windows, its bundle
/// id on macOS).
#[must_use]
pub fn platform(
    identifier: &str,
    on_click: OnClick,
    runtime: tokio::runtime::Handle,
) -> Arc<dyn Notifier> {
    #[cfg(target_os = "linux")]
    let notifier: Arc<dyn Notifier> = {
        let _ = identifier;
        Arc::new(linux::DbusNotifier::session(on_click, runtime))
    };
    #[cfg(windows)]
    let notifier: Arc<dyn Notifier> =
        Arc::new(windows::ToastNotifier::new(identifier, on_click, runtime));
    #[cfg(target_os = "macos")]
    let notifier: Arc<dyn Notifier> =
        Arc::new(macos::MacNotifier::new(identifier, on_click, runtime));
    #[cfg(not(any(target_os = "linux", windows, target_os = "macos")))]
    let notifier: Arc<dyn Notifier> = {
        let _ = (identifier, on_click, runtime);
        Arc::new(Nowhere)
    };
    notifier
}

/// No notifications on this platform.
#[cfg(not(any(target_os = "linux", windows, target_os = "macos")))]
struct Nowhere;

#[cfg(not(any(target_os = "linux", windows, target_os = "macos")))]
impl Notifier for Nowhere {
    fn show(&self, _notice: Notice) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use pitcrew_protocol::ids::{AskId, TaskId};

    const WS: &str = "01JA0000000000000000000000";

    fn ask(asker: u8, kind: AskKind, from: Option<&str>, title: &str) -> NewAsk {
        NewAsk {
            id: AskId::new(),
            kind,
            asker: format!("01JM00000000000000000000{asker:02}")
                .parse()
                .unwrap(),
            from: from.map(str::to_owned),
            title: title.into(),
            body: String::new(),
            task: None,
        }
    }

    #[test]
    fn text_is_cleaned_and_capped() {
        assert_eq!(clean("  a\n\tb\r\n  c  ", 50), "a b c");
        assert_eq!(clean("bell\u{7}\u{0}\u{1b}[31mred", 50), "bell[31mred");
        assert_eq!(
            clean("safe\u{202e}txt.exe\u{2066}x\u{2069}\u{200b}\u{feff}", 50),
            "safetxt.exex"
        );
        assert_eq!(clean("\u{2028}line\u{2029}", 50), "line");
        assert_eq!(clean("", 10), "");
        let long = "word ".repeat(100);
        let cut = clean(&long, MAX_BODY);
        assert_eq!(cut.chars().count(), MAX_BODY);
        assert!(cut.ends_with("word…") || cut.ends_with('…'), "{cut}");
        assert!(!cut.contains("  "));
        let wide = "é".repeat(300);
        assert_eq!(clean(&wide, 200).chars().count(), 200);
        assert_eq!(clean("abc", 3), "abc");
        assert_eq!(clean("abcd", 3), "ab…");
    }

    #[test]
    fn one_asks_notice() {
        let mut a = ask(2, AskKind::Decision, Some("Writer\u{202e}"), "Which venue?");
        a.body = "ICML or\nNeurIPS".into();
        let n = notice_for(WS, &a);
        assert_eq!(n.title, "Writer needs a decision");
        assert_eq!(n.body, "Which venue? — ICML or NeurIPS");
        assert_eq!(n.target, Some(NavigateTarget::inbox(WS)));

        let task: TaskId = "01JB000000000000000TASK001".parse().unwrap();
        a.task = Some(task);
        a.from = None;
        let n = notice_for(WS, &a);
        assert_eq!(n.title, "An agent needs a decision");
        assert_eq!(
            n.target,
            Some(NavigateTarget::task(WS, "01JB000000000000000TASK001"))
        );
        for (kind, words) in [
            (AskKind::Question, "Writer has a question"),
            (AskKind::Review, "Writer asks for a review"),
            (AskKind::Approval, "Writer asks for approval"),
            (AskKind::Mention, "Writer mentioned you"),
        ] {
            assert_eq!(
                notice_for(WS, &ask(2, kind, Some("Writer"), "x")).title,
                words
            );
        }
    }

    #[test]
    fn a_burst_becomes_one_notification() {
        let t0 = Instant::now();
        let s = Duration::from_secs;
        let mut r = RateLimiter::new(s(2), s(30));
        assert_eq!(r.take(t0), None);

        // Three asks from three agents within the gathering time: one summary.
        let due = r.offer(WS, ask(1, AskKind::Question, Some("A"), "1"), t0);
        assert_eq!(due, t0 + s(2));
        assert_eq!(
            r.offer(WS, ask(2, AskKind::Question, Some("B"), "2"), t0 + s(1)),
            due
        );
        r.offer(WS, ask(3, AskKind::Review, Some("C"), "3"), t0 + s(1));
        assert_eq!(r.take(t0 + s(1)), None, "not yet due");
        let n = r.take(t0 + s(2)).unwrap();
        assert_eq!(n.title, "3 agents need you");
        assert_eq!(n.body, "3 new asks");
        assert_eq!(n.target, Some(NavigateTarget::inbox(WS)));
        assert_eq!(r.take(t0 + s(3)), None, "nothing left");

        // One ask during the cooldown waits for its end, and is shown as itself.
        let due = r.offer(WS, ask(1, AskKind::Question, Some("A"), "Later"), t0 + s(5));
        assert_eq!(due, t0 + s(32));
        assert_eq!(r.take(t0 + s(20)), None);
        let n = r.take(t0 + s(32)).unwrap();
        assert_eq!(
            (n.title.as_str(), n.body.as_str()),
            ("A has a question", "Later")
        );

        // Two from the same agent: named, counted.
        r.offer(WS, ask(4, AskKind::Question, Some("D"), "x"), t0 + s(100));
        r.offer(WS, ask(4, AskKind::Question, Some("D"), "y"), t0 + s(100));
        let n = r.take(t0 + s(102)).unwrap();
        assert_eq!(
            (n.title.as_str(), n.body.as_str()),
            ("D needs you", "2 new asks")
        );

        // Dropped while the window is focused: nothing comes.
        r.offer(WS, ask(1, AskKind::Question, Some("A"), "z"), t0 + s(200));
        r.drop_held();
        assert_eq!(r.due(), None);
        assert_eq!(r.take(t0 + s(300)), None);
    }

    #[test]
    fn many_askers_are_bounded() {
        let mut r = RateLimiter::new(Duration::ZERO, Duration::ZERO);
        let t0 = Instant::now();
        for n in 0..=60u8 {
            r.offer(WS, ask(n, AskKind::Question, None, "q"), t0);
        }
        let n = r.take(t0).unwrap();
        assert_eq!(n.title, format!("{MAX_ASKERS}+ agents need you"));
        assert_eq!(n.body, "61 new asks");
    }

    /// Records what it is asked to show.
    #[derive(Default)]
    struct Recorder(Mutex<Vec<Notice>>);

    impl Notifier for Recorder {
        fn show(&self, notice: Notice) {
            self.0.lock().unwrap().push(notice);
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_pacing_runs_on_its_own() {
        let shown = Arc::new(Recorder::default());
        let allowed = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let gate = Arc::clone(&allowed);
        let notifications = Notifications::new(
            Arc::clone(&shown) as Arc<dyn Notifier>,
            RateLimiter::new(Duration::from_millis(50), Duration::from_millis(300)),
            move || gate.load(std::sync::atomic::Ordering::SeqCst),
            &tokio::runtime::Handle::current(),
        );
        for n in 0..3 {
            notifications.new_ask(WS, ask(n, AskKind::Question, Some("A"), "q"));
        }
        let wait_for = |n: usize| {
            let shown = Arc::clone(&shown);
            async move {
                for _ in 0..200 {
                    if shown.0.lock().unwrap().len() >= n {
                        return;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                panic!("waited for {n} notifications");
            }
        };
        wait_for(1).await;
        assert_eq!(shown.0.lock().unwrap()[0].title, "3 agents need you");

        // Not allowed (the window is focused): nothing, even later.
        allowed.store(false, std::sync::atomic::Ordering::SeqCst);
        notifications.new_ask(WS, ask(9, AskKind::Question, Some("A"), "hidden"));
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert_eq!(shown.0.lock().unwrap().len(), 1);

        // Allowed again: after the cooldown, the next one.
        allowed.store(true, std::sync::atomic::Ordering::SeqCst);
        notifications.new_ask(WS, ask(9, AskKind::Approval, Some("B"), "deploy"));
        wait_for(2).await;
        assert_eq!(shown.0.lock().unwrap()[1].title, "B asks for approval");
    }
}
