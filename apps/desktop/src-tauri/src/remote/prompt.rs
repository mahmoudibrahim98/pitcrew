//! SSH's questions, asked in the app (the contract's "Prompts"): a password, a key's passphrase,
//! a one-time code, whether to trust a host key, another yes/no question, or a notice.
//!
//! [`PromptHub`] is the [`PromptHandler`] every remote ssh call gets (through
//! `pitcrew-askpass`). For each question it emits `gateway://prompt` with a [`GatewayPrompt`] and
//! waits for `gateway_prompt_reply`. Every prompt ends with `gateway://prompt-closed` `{ id }`:
//! when it was answered, and when it went stale first (ssh stopped waiting, or the call ended),
//! so the dialog closes.
//!
//! - **Kinds and replies.** `password`, `passphrase` and `otp` take `answer`; `host_key` (with
//!   its `fingerprint`) and `confirm` (ssh's other yes/no questions, such as
//!   `UpdateHostKeys=ask`'s "Accept updated hostkeys?") take `accept`; a `notice` ("touch your
//!   security key") takes no answer and is closed when ssh moves on. A reply with neither
//!   cancels: ssh stops (for a `confirm`, it answers no). A reply of the wrong shape is `invalid`
//!   and the prompt stays open.
//! - **Answers** go to ssh once, inside a [`Secret`], and are never kept, logged or sent
//!   anywhere else.
//! - **The text** is ssh's (and partly the server's): control, bidi and invisible characters are
//!   removed (line breaks kept), and it is cut to [`MAX_TEXT`] characters. The UI shows it as
//!   text, with the host that asks.
//! - **A page that (re)loads** gets the open prompts again when it first asks for the workspaces
//!   ([`PromptHub::page_listening`]), so a prompt raised while no page listened (at start, while
//!   reconnecting) is not lost. Prompts are keyed by id: the UI shows one id once.

use crate::gateway::GatewayError;
use pitcrew_remote::{
    PromptCancel, PromptFuture, PromptHandler, PromptKind, PromptRequest, Reply, Secret,
};
use serde::Serialize;
use std::collections::HashMap;
use std::fmt;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use tokio::sync::oneshot;

/// The event a new prompt is emitted with.
pub const PROMPT_EVENT: &str = "gateway://prompt";
/// The event a prompt that ended is withdrawn with.
pub const PROMPT_CLOSED_EVENT: &str = "gateway://prompt-closed";
/// The longest prompt text shown, in characters.
pub const MAX_TEXT: usize = 2000;
/// The longest answer taken, in bytes.
pub const MAX_ANSWER: usize = 4096;

/// What the UI is asked, as the contract's `GatewayPrompt`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct GatewayPrompt {
    /// Its id, for `gateway_prompt_reply`.
    pub id: String,
    /// The host the ssh call is for, as given to ssh.
    pub host: String,
    /// What is asked.
    pub kind: PromptKindName,
    /// ssh's question, cleaned. Untrusted.
    pub text: String,
    /// For a host key, its fingerprint, to compare.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<String>,
}

/// The contract's prompt kinds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptKindName {
    /// An account password.
    Password,
    /// A key's passphrase.
    Passphrase,
    /// A one-time code.
    Otp,
    /// Whether to trust a new host key.
    HostKey,
    /// Another yes/no question.
    Confirm,
    /// Information only; closed when ssh moves on.
    Notice,
}

impl From<PromptKind> for PromptKindName {
    fn from(kind: PromptKind) -> Self {
        match kind {
            PromptKind::Password => Self::Password,
            PromptKind::Passphrase => Self::Passphrase,
            PromptKind::Otp => Self::Otp,
            PromptKind::HostKey => Self::HostKey,
            PromptKind::Confirm => Self::Confirm,
            PromptKind::Notice => Self::Notice,
        }
    }
}

impl PromptKindName {
    /// Whether the reply is typed text.
    fn takes_answer(self) -> bool {
        matches!(self, Self::Password | Self::Passphrase | Self::Otp)
    }

    /// Whether the reply is yes or no.
    fn takes_accept(self) -> bool {
        matches!(self, Self::HostKey | Self::Confirm)
    }
}

/// What the hub tells the app to emit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PromptEvent {
    /// `gateway://prompt`.
    Open(GatewayPrompt),
    /// `gateway://prompt-closed` with this id.
    Closed(String),
}

/// `gateway://prompt-closed`'s payload.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct PromptClosed {
    /// The prompt's id.
    pub id: String,
}

type Emit = Arc<dyn Fn(&PromptEvent) + Send + Sync>;

struct Pending {
    /// Tells the oldest first.
    seq: u64,
    prompt: GatewayPrompt,
    reply: oneshot::Sender<Reply>,
}

/// Asks the person through the UI. See the module docs.
pub struct PromptHub {
    emit: Emit,
    pending: Mutex<HashMap<String, Pending>>,
    next: AtomicU64,
    /// No page has listened since the main page last (re)loaded.
    replay: AtomicBool,
}

impl fmt::Debug for PromptHub {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PromptHub")
            .field("open", &self.lock().len())
            .finish_non_exhaustive()
    }
}

impl PromptHub {
    /// A hub that hands its events to `emit` (the app emits them to the main window).
    #[must_use]
    pub fn new(emit: impl Fn(&PromptEvent) + Send + Sync + 'static) -> Self {
        Self {
            emit: Arc::new(emit),
            pending: Mutex::default(),
            next: AtomicU64::new(0),
            replay: AtomicBool::new(true),
        }
    }

    /// The prompts waiting for an answer, oldest first.
    #[must_use]
    pub fn open(&self) -> Vec<GatewayPrompt> {
        let mut open: Vec<(u64, GatewayPrompt)> = self
            .lock()
            .values()
            .map(|p| (p.seq, p.prompt.clone()))
            .collect();
        open.sort_by_key(|(seq, _)| *seq);
        open.into_iter().map(|(_, prompt)| prompt).collect()
    }

    /// The main page started loading: what it may have missed is emitted again once it listens.
    pub fn page_started(&self) {
        self.replay.store(true, Ordering::SeqCst);
    }

    /// The main page listens (it asked for the workspaces): the first time since it loaded,
    /// every open prompt is emitted again. The UI keys prompts by id, so one it saw already is
    /// not shown twice.
    pub fn page_listening(&self) {
        if self.replay.swap(false, Ordering::SeqCst) {
            for prompt in self.open() {
                (self.emit)(&PromptEvent::Open(prompt));
            }
        }
    }

    /// `gateway_prompt_reply`: `answer` for a password, passphrase or one-time code, `accept` for
    /// a host key or a `confirm`, neither for a notice; neither cancels any prompt. The answer
    /// goes to ssh once.
    ///
    /// # Errors
    /// `invalid`, with the prompt still open: both given, an answer over [`MAX_ANSWER`] bytes, or
    /// a reply of the wrong shape for the prompt's kind. `invalid` too when no such prompt is
    /// open (it was answered, or went stale).
    pub fn reply(
        &self,
        id: &str,
        answer: Option<Secret>,
        accept: Option<bool>,
    ) -> Result<(), GatewayError> {
        let invalid = |why: &str| Err(GatewayError::invalid(why.to_owned()));
        let mut pending = self.lock();
        let Some(kind) = pending.get(id).map(|p| p.prompt.kind) else {
            return Err(GatewayError::invalid(format!(
                "no prompt {} is open",
                crate::gateway::error::shorten(id)
            )));
        };
        let reply = match (answer, accept) {
            (Some(_), Some(_)) => return invalid("reply with answer or accept, not both"),
            (Some(answer), None) if answer.expose().len() > MAX_ANSWER => {
                return Err(GatewayError::invalid(format!(
                    "an answer is at most {MAX_ANSWER} bytes"
                )));
            }
            (Some(_), None) if !kind.takes_answer() => {
                return invalid("this prompt takes accept (or neither), not an answer");
            }
            (None, Some(_)) if !kind.takes_accept() => {
                return invalid("this prompt takes an answer (or neither), not accept");
            }
            (Some(answer), None) => Reply::Text(answer),
            (None, Some(true)) => Reply::Accept,
            (None, Some(false) | None) => Reply::Cancel,
        };
        let taken = pending.remove(id);
        drop(pending);
        let Some(pending) = taken else {
            return invalid("the prompt closed");
        };
        let cancelled = matches!(reply, Reply::Cancel);
        // The waiting call takes it; if it went stale meanwhile, the answer is dropped here.
        let _ = pending.reply.send(reply);
        tracing::debug!(prompt = %pending.prompt.id, host = %pending.prompt.host, cancelled, "prompt answered");
        Ok(())
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<String, Pending>> {
        self.pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Withdraws prompt `id`: it was answered, or went stale.
    fn close(&self, id: &str) {
        self.lock().remove(id);
        (self.emit)(&PromptEvent::Closed(id.to_owned()));
    }
}

/// Withdraws its prompt however the wait ends, also when the call's future is dropped.
struct Closing<'a> {
    hub: &'a PromptHub,
    id: String,
}

impl Drop for Closing<'_> {
    fn drop(&mut self) {
        self.hub.close(&self.id);
    }
}

impl PromptHandler for PromptHub {
    fn prompt(&self, request: PromptRequest, cancel: PromptCancel) -> PromptFuture<'_> {
        Box::pin(async move {
            let id = crate::remote::new_id();
            let prompt = GatewayPrompt {
                id: id.clone(),
                host: request.host.clone(),
                kind: request.kind.into(),
                text: clean_text(&request.prompt),
                fingerprint: (request.kind == PromptKind::HostKey)
                    .then(|| fingerprint(&request.prompt))
                    .flatten(),
            };
            let (reply, answered) = oneshot::channel();
            self.lock().insert(
                id.clone(),
                Pending {
                    seq: self.next.fetch_add(1, Ordering::Relaxed),
                    prompt: prompt.clone(),
                    reply,
                },
            );
            let _closing = Closing { hub: self, id };
            tracing::debug!(prompt = %prompt.id, host = %prompt.host, kind = ?prompt.kind, "asking");
            (self.emit)(&PromptEvent::Open(prompt));
            tokio::select! {
                reply = answered => reply.unwrap_or(Reply::Cancel),
                () = cancel.cancelled() => Reply::Cancel,
            }
        })
    }
}

/// ssh's text as the UI may show it: line breaks kept (`\r\n` and `\r` become `\n`), tabs as
/// spaces, every other control character and every bidi or invisible character removed, cut
/// to [`MAX_TEXT`] characters.
#[must_use]
pub fn clean_text(text: &str) -> String {
    let text = text.replace("\r\n", "\n").replace('\r', "\n");
    let mut out = String::with_capacity(text.len().min(MAX_TEXT * 4));
    let mut count = 0;
    for c in text.chars() {
        let c = match c {
            '\n' => '\n',
            '\t' | '\u{2028}' | '\u{2029}' => ' ',
            c if c.is_control() || crate::notify::is_invisible(c) => continue,
            c => c,
        };
        if count == MAX_TEXT {
            out.push('…');
            break;
        }
        out.push(c);
        count += 1;
    }
    out.trim_end().to_owned()
}

/// A host key's fingerprint in ssh's question (`SHA256:…`, or `MD5:…` with
/// `FingerprintHash=md5`).
#[must_use]
pub fn fingerprint(text: &str) -> Option<String> {
    text.split(|c: char| c.is_whitespace() || c == '(' || c == ')')
        .find_map(|word| {
            let word = word.trim_end_matches(['.', ',', ';']);
            let (scheme, rest) = word.split_once(':')?;
            let ok = matches!(scheme, "SHA256" | "MD5")
                && (8..=128).contains(&rest.len())
                && rest
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '=' | ':'));
            ok.then(|| word.to_owned())
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn hub() -> (Arc<PromptHub>, Arc<Mutex<Vec<PromptEvent>>>) {
        let events: Arc<Mutex<Vec<PromptEvent>>> = Arc::default();
        let seen = Arc::clone(&events);
        let hub = Arc::new(PromptHub::new(move |e| {
            seen.lock().unwrap().push(e.clone())
        }));
        (hub, events)
    }

    fn request(kind: PromptKind, prompt: &str) -> PromptRequest {
        PromptRequest {
            host: "hpc-login".into(),
            kind,
            prompt: prompt.into(),
        }
    }

    async fn opened(events: &Mutex<Vec<PromptEvent>>, n: usize) -> GatewayPrompt {
        for _ in 0..200 {
            let opened: Vec<GatewayPrompt> = events
                .lock()
                .unwrap()
                .iter()
                .filter_map(|e| match e {
                    PromptEvent::Open(p) => Some(p.clone()),
                    PromptEvent::Closed(_) => None,
                })
                .collect();
            if let Some(p) = opened.get(n) {
                return p.clone();
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("no prompt {n}");
    }

    #[tokio::test]
    async fn a_password_round_trip() {
        let (hub, events) = hub();
        let (_trigger, cancel) = PromptCancel::pair();
        let asking = tokio::spawn({
            let hub = Arc::clone(&hub);
            async move {
                hub.prompt(
                    request(PromptKind::Password, "(sam@hpc-login) Password: "),
                    cancel,
                )
                .await
            }
        });
        let prompt = opened(&events, 0).await;
        assert_eq!(prompt.kind, PromptKindName::Password);
        assert_eq!(prompt.host, "hpc-login");
        assert_eq!(prompt.text, "(sam@hpc-login) Password:");
        assert_eq!(hub.open().len(), 1);
        let e = hub
            .reply(&prompt.id, Some(Secret::new("x")), Some(true))
            .unwrap_err();
        assert_eq!(e.code, crate::gateway::ErrorCode::Invalid);
        hub.reply(&prompt.id, Some(Secret::new("hunter2")), None)
            .unwrap();
        match asking.await.unwrap() {
            Reply::Text(secret) => assert_eq!(secret.expose(), "hunter2"),
            other => panic!("{other:?}"),
        }
        assert!(hub.open().is_empty());
        assert_eq!(
            events.lock().unwrap().last(),
            Some(&PromptEvent::Closed(prompt.id.clone()))
        );
        // Answered once: a second reply finds nothing.
        assert!(hub.reply(&prompt.id, None, Some(true)).is_err());
        let printed = format!("{:?} {hub:?}", events.lock().unwrap());
        assert!(!printed.contains("hunter2"), "{printed}");
    }

    #[tokio::test]
    async fn neither_cancels_and_a_stale_prompt_is_withdrawn() {
        let (hub, events) = hub();
        let (_trigger, cancel) = PromptCancel::pair();
        let asking = tokio::spawn({
            let hub = Arc::clone(&hub);
            async move {
                hub.prompt(request(PromptKind::Otp, "Verification code: "), cancel)
                    .await
            }
        });
        let prompt = opened(&events, 0).await;
        assert_eq!(prompt.kind, PromptKindName::Otp);
        hub.reply(&prompt.id, None, None).unwrap();
        assert!(matches!(asking.await.unwrap(), Reply::Cancel));

        let (trigger, cancel) = PromptCancel::pair();
        let asking = tokio::spawn({
            let hub = Arc::clone(&hub);
            async move {
                hub.prompt(request(PromptKind::Password, "Password: "), cancel)
                    .await
            }
        });
        let prompt = opened(&events, 1).await;
        // A page that reloads gets it again.
        hub.page_listening();
        assert_eq!(opened(&events, 2).await, prompt);
        trigger.cancel();
        assert!(matches!(asking.await.unwrap(), Reply::Cancel));
        assert!(hub.open().is_empty());
        assert_eq!(
            events.lock().unwrap().last(),
            Some(&PromptEvent::Closed(prompt.id.clone()))
        );
        assert!(
            hub.reply(&prompt.id, Some(Secret::new("late")), None)
                .is_err()
        );
    }

    #[tokio::test]
    async fn a_host_key_has_its_fingerprint_and_takes_accept() {
        let (hub, events) = hub();
        let (_trigger, cancel) = PromptCancel::pair();
        let text = "The authenticity of host 'hpc-login (192.0.2.10)' can't be established.\r\n\
                    ED25519 key fingerprint is SHA256:dGhpcy1pcy1hLWZha2Uta2V5.\r\n\
                    Are you sure you want to continue connecting (yes/no/[fingerprint])? ";
        let asking = tokio::spawn({
            let hub = Arc::clone(&hub);
            async move { hub.prompt(request(PromptKind::HostKey, text), cancel).await }
        });
        let prompt = opened(&events, 0).await;
        assert_eq!(prompt.kind, PromptKindName::HostKey);
        assert_eq!(
            prompt.fingerprint.as_deref(),
            Some("SHA256:dGhpcy1pcy1hLWZha2Uta2V5")
        );
        assert_eq!(prompt.text.lines().count(), 3);
        hub.reply(&prompt.id, None, Some(true)).unwrap();
        assert!(matches!(asking.await.unwrap(), Reply::Accept));
        assert_eq!(
            serde_json::to_value(&prompt).unwrap()["kind"],
            serde_json::json!("host_key")
        );
    }

    /// Asks `kind` and returns the open prompt, and the call waiting for its reply.
    async fn ask(
        hub: &Arc<PromptHub>,
        events: &Mutex<Vec<PromptEvent>>,
        n: usize,
        kind: PromptKind,
    ) -> (GatewayPrompt, tokio::task::JoinHandle<Reply>) {
        let (trigger, cancel) = PromptCancel::pair();
        let asking = tokio::spawn({
            let hub = Arc::clone(hub);
            async move {
                let reply = hub.prompt(request(kind, "Question? "), cancel).await;
                drop(trigger);
                reply
            }
        });
        (opened(events, n).await, asking)
    }

    #[tokio::test]
    async fn an_answer_is_at_most_4_kib() {
        let (hub, events) = hub();
        let (prompt, asking) = ask(&hub, &events, 0, PromptKind::Password).await;
        let e = hub
            .reply(
                &prompt.id,
                Some(Secret::new("x".repeat(MAX_ANSWER + 1))),
                None,
            )
            .unwrap_err();
        assert_eq!(e.code, crate::gateway::ErrorCode::Invalid);
        assert!(e.message.contains("4096"), "{}", e.message);
        assert_eq!(hub.open().len(), 1, "the prompt stays open");
        hub.reply(&prompt.id, Some(Secret::new("x".repeat(MAX_ANSWER))), None)
            .unwrap();
        match asking.await.unwrap() {
            Reply::Text(secret) => assert_eq!(secret.expose().len(), MAX_ANSWER),
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn each_kind_takes_its_own_reply() {
        let (hub, events) = hub();
        for (n, (kind, name, wrong, right)) in [
            (
                PromptKind::Otp,
                "otp",
                (None, Some(true)),
                (Some("123456"), None),
            ),
            (
                PromptKind::HostKey,
                "host_key",
                (Some("yes"), None),
                (None, Some(true)),
            ),
            (
                PromptKind::Confirm,
                "confirm",
                (Some("yes"), None),
                (None, Some(false)),
            ),
            (
                PromptKind::Notice,
                "notice",
                (None, Some(true)),
                (None, None),
            ),
        ]
        .into_iter()
        .enumerate()
        {
            let (prompt, asking) = ask(&hub, &events, n, kind).await;
            assert_eq!(serde_json::to_value(prompt.kind).unwrap(), name);
            let (answer, accept) = wrong;
            let e = hub
                .reply(&prompt.id, answer.map(Secret::new), accept)
                .unwrap_err();
            assert_eq!(e.code, crate::gateway::ErrorCode::Invalid, "{name}");
            assert_eq!(hub.open().len(), 1, "{name}: still open");
            let (answer, accept) = right;
            hub.reply(&prompt.id, answer.map(Secret::new), accept)
                .unwrap();
            let reply = asking.await.unwrap();
            match name {
                "otp" => assert!(matches!(reply, Reply::Text(_))),
                "host_key" => assert!(matches!(reply, Reply::Accept)),
                // No to a yes/no question, and nothing to a notice: both are a cancel here,
                // which pitcrew-remote turns into "no" for a confirm and stops ssh for a notice.
                _ => assert!(matches!(reply, Reply::Cancel)),
            }
        }
    }

    #[test]
    fn text_is_cleaned() {
        assert_eq!(
            clean_text("Pass\u{1b}[31mword\u{7}:\tnow\r\nline\u{202e}two\u{200b}\n\n"),
            "Pass[31mword: now\nlinetwo"
        );
        let long = "x".repeat(MAX_TEXT + 10);
        assert_eq!(clean_text(&long).chars().count(), MAX_TEXT + 1);
        for (kind, name) in [
            (PromptKind::Password, "password"),
            (PromptKind::Passphrase, "passphrase"),
            (PromptKind::Otp, "otp"),
            (PromptKind::HostKey, "host_key"),
            (PromptKind::Confirm, "confirm"),
            (PromptKind::Notice, "notice"),
        ] {
            assert_eq!(
                serde_json::to_value(PromptKindName::from(kind)).unwrap(),
                serde_json::json!(name)
            );
        }
        assert_eq!(fingerprint("no key here"), None);
        assert_eq!(
            fingerprint("key fingerprint is MD5:aa:bb:cc:dd:ee:ff:00:11."),
            Some("MD5:aa:bb:cc:dd:ee:ff:00:11".into())
        );
        assert_eq!(fingerprint("SHA256:x; rm -rf /"), None);
    }
}
