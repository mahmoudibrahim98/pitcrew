//! The askpass bridge: ssh's password, passphrase, one-time-code and host-key prompts are asked
//! in the desktop, and the answers are never stored or logged.
//!
//! For each ssh call with a [`PromptHandler`], the caller starts a private listener (a unix
//! socket in a 0700 directory, or a named pipe on Windows) and runs ssh with
//! `SSH_ASKPASS=pitcrew-askpass`, `SSH_ASKPASS_REQUIRE=force`, and two variables: the
//! listener's address and a fresh random key. ssh runs `pitcrew-askpass "<prompt>"`, which
//! connects and asks.
//!
//! **Both sides prove they hold the key** (HMAC-SHA256 over fresh nonces) before anything is
//! shown to the user or any answer is sent. On Unix the 0700 directory and a peer-uid check
//! already keep other users out; on Windows the pipe keeps the default ACL, so the key is what
//! stops another local user from squatting the name to answer host-key prompts, or connecting
//! to it to phish a password.
//!
//! Wire format, one JSON object per line:
//! 1. client → `{"v":1,"nonce":Nc}`
//! 2. server → `{"nonce":Ns,"proof":HMAC(K,"server",Nc,Ns)}`; the client checks it.
//! 3. client → `{"proof":HMAC(K,"client",Nc,Ns,kind,prompt),"kind":…,"prompt":…}`; the server
//!    checks it, asks the handler, and answers:
//! 4. server → `{"reply":"text","text":…}` or `{"reply":"accept"}`.
//!
//! **A cancel is never answered.** When askpass fails, OpenSSH sends an empty password (a
//! failed login that faillock or fail2ban count) and asks again. So on a cancel the server
//! tells [`crate::Ssh::run`], which kills ssh and everything it started (its process group, on
//! Unix) before the connection closes. Later prompts in the same call are held, unanswered and
//! unseen by the user, until then.

pub mod client;
pub(crate) mod server;

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::fmt;
use std::future::Future;
use std::pin::Pin;
use tokio::sync::watch;

/// Environment variable naming the listener (a socket path or a pipe name).
pub const ADDR_ENV: &str = "PITCREW_ASKPASS_ADDR";
/// Environment variable holding the hex key for this ssh call.
pub const KEY_ENV: &str = "PITCREW_ASKPASS_KEY";
/// Longest accepted protocol line. Host-key prompts with fingerprints are a few hundred bytes.
pub(crate) const MAX_LINE: usize = 64 * 1024;

/// What ssh is asking for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptKind {
    /// An account password.
    Password,
    /// The passphrase of a private key.
    Passphrase,
    /// A one-time code (TOTP, Duo, a verification code).
    Otp,
    /// Whether to trust an unknown host key. Show it as a trust dialog with the fingerprint.
    HostKey,
    /// A yes/no confirmation, e.g. before using an agent key.
    Confirm,
    /// Information only (e.g. "touch your security key"); ssh closes it when done.
    Notice,
}

impl PromptKind {
    /// Whether the answer is a secret to type (as opposed to a yes/no).
    #[must_use]
    pub fn is_secret(self) -> bool {
        matches!(self, Self::Password | Self::Passphrase | Self::Otp)
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Password => "password",
            Self::Passphrase => "passphrase",
            Self::Otp => "otp",
            Self::HostKey => "host_key",
            Self::Confirm => "confirm",
            Self::Notice => "notice",
        }
    }
}

/// A prompt to show the user.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PromptRequest {
    /// The host name the ssh call was for, as the caller gave it.
    pub host: String,
    /// What is asked: **an untrusted hint** for choosing the dialog. It is guessed from the
    /// prompt text, and a server writes the text of keyboard-interactive prompts. Prompts that
    /// carry OpenSSH's `(user@host) ` prefix, which marks server text, are never classed as a
    /// passphrase or a host key; older OpenSSH has no prefix.
    pub kind: PromptKind,
    /// ssh's prompt text, verbatim. For a host key it includes the fingerprint. It may come
    /// from the server: always show it, as plain text with control characters escaped, so the
    /// user can see what is really asked.
    pub prompt: String,
}

/// The user's answer.
#[derive(Debug)]
pub enum Reply {
    /// Typed text: a password, passphrase, code or host-key fingerprint. Refused (as a cancel)
    /// for [`PromptKind::Confirm`].
    Text(Secret),
    /// Yes: trust the host key, or confirm. Refused (as a cancel) for a password, passphrase or
    /// one-time code.
    Accept,
    /// No, or the dialog was closed. ssh is stopped at once; the call fails with
    /// [`crate::SshError::Cancelled`].
    Cancel,
}

/// The answer to a prompt, some time later.
pub type PromptFuture<'a> = Pin<Box<dyn Future<Output = Reply> + Send + 'a>>;

/// Asks the user. The desktop implements this (stream K). Implementations must never log or
/// keep the answer.
pub trait PromptHandler: Send + Sync + 'static {
    /// Shows `request` and resolves to the user's answer.
    ///
    /// A prompt can go stale before the user answers: ssh stopped waiting for it (it closed a
    /// notice, or died), or the call ended (it finished, failed, timed out, or its future was
    /// dropped). Then `cancel` fires and the returned future is dropped without being polled
    /// again. Keep a clone of `cancel` wherever the dialog lives, and close the dialog when it
    /// fires.
    fn prompt(&self, request: PromptRequest, cancel: PromptCancel) -> PromptFuture<'_>;
}

/// Fires when a prompt goes stale; see [`PromptHandler::prompt`]. Cheap to clone.
#[derive(Clone, Debug)]
pub struct PromptCancel(watch::Receiver<bool>);

/// Fires the [`PromptCancel`] it came with, when told to or when dropped.
#[derive(Debug)]
pub struct CancelTrigger(watch::Sender<bool>);

impl PromptCancel {
    /// A new token and its trigger. [`crate::Ssh`] makes these itself; this is for driving a
    /// handler by hand, e.g. in its tests.
    #[must_use]
    pub fn pair() -> (CancelTrigger, Self) {
        let (tx, rx) = watch::channel(false);
        (CancelTrigger(tx), Self(rx))
    }

    /// Whether the prompt is stale.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        *self.0.borrow() || self.0.has_changed().is_err()
    }

    /// Completes once the prompt is stale.
    pub async fn cancelled(&self) {
        let mut rx = self.0.clone();
        // An error means the trigger was dropped, which also cancels.
        let _ = rx.wait_for(|stale| *stale).await;
    }
}

impl CancelTrigger {
    /// Fires the token now.
    pub fn cancel(&self) {
        self.0.send_replace(true);
    }
}

/// A secret string. Its `Debug` is redacted, and its bytes are overwritten when dropped (best
/// effort: copies made elsewhere, e.g. by a UI toolkit, are out of reach).
pub struct Secret(String);

impl Secret {
    /// Wraps `text`.
    #[must_use]
    pub fn new(text: impl Into<String>) -> Self {
        Self(text.into())
    }

    /// The secret text.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        let mut bytes = std::mem::take(&mut self.0).into_bytes();
        bytes.fill(0);
        std::hint::black_box(&bytes);
    }
}

/// Decides what a prompt asks for from its text and ssh's `SSH_ASKPASS_PROMPT` hint
/// (`confirm` for yes/no questions, `none` for notices; unset otherwise). The hint comes from
/// ssh itself; the text may come from the server, so the result is only a hint for the UI.
#[must_use]
pub fn classify(prompt: &str, hint: Option<&str>) -> PromptKind {
    match hint {
        Some("confirm") => return PromptKind::Confirm,
        Some("none") => return PromptKind::Notice,
        _ => {}
    }
    let text = prompt.to_lowercase();
    let has_word = |word: &str| {
        text.split(|c: char| !c.is_alphanumeric())
            .any(|w| w == word)
    };
    // OpenSSH writes keyboard-interactive prompts, whose text is the server's, as
    // "(user@host) <text>". Its own prompts never start with '('. Server text must not pass for
    // a local key's passphrase or a host-key question.
    let from_server = prompt.starts_with('(');
    if !from_server
        && (text.contains("continue connecting") || text.contains("authenticity of host"))
    {
        PromptKind::HostKey
    } else if !from_server && text.contains("passphrase") {
        PromptKind::Passphrase
    } else if text.contains("verification code")
        || text.contains("one-time")
        || text.contains("one time")
        || text.contains("passcode")
        || text.contains("authenticator")
        || text.contains("second factor")
        || ["otp", "totp", "token", "duo", "2fa", "mfa"]
            .iter()
            .any(|w| has_word(w))
    {
        PromptKind::Otp
    } else {
        PromptKind::Password
    }
}

// ─── Wire messages ──────────────────────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Hello {
    pub v: u32,
    pub nonce: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ServerHello {
    pub nonce: String,
    pub proof: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Ask {
    pub proof: String,
    pub kind: PromptKind,
    pub prompt: String,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "reply", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum WireReply {
    Text { text: String },
    Accept,
}

impl WireReply {
    /// What may be sent for `kind`: an answer of the wrong type is refused, like a cancel, so a
    /// mislabelled prompt cannot turn a click into "yes" for a password or a typed secret into
    /// an answer to a yes/no question.
    pub(crate) fn for_prompt(kind: PromptKind, reply: Reply) -> Option<Self> {
        match reply {
            Reply::Text(secret) if kind != PromptKind::Confirm => Some(Self::Text {
                text: secret.expose().to_owned(),
            }),
            Reply::Accept if !kind.is_secret() => Some(Self::Accept),
            Reply::Text(_) | Reply::Accept | Reply::Cancel => None,
        }
    }
}

impl fmt::Debug for WireReply {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Text { .. } => f.write_str("Text(<redacted>)"),
            Self::Accept => f.write_str("Accept"),
        }
    }
}

impl Drop for WireReply {
    fn drop(&mut self) {
        if let Self::Text { text } = self {
            let mut bytes = std::mem::take(text).into_bytes();
            bytes.fill(0);
            std::hint::black_box(&bytes);
        }
    }
}

// ─── Keys and proofs ────────────────────────────────────────────────────────────────────────

pub(crate) const KEY_LEN: usize = 32;
pub(crate) const NONCE_LEN: usize = 16;

pub(crate) fn random<const N: usize>() -> std::io::Result<[u8; N]> {
    let mut bytes = [0u8; N];
    getrandom::fill(&mut bytes).map_err(std::io::Error::other)?;
    Ok(bytes)
}

pub(crate) fn server_proof(key: &[u8; KEY_LEN], nc: &[u8], ns: &[u8]) -> [u8; 32] {
    hmac(key, &[b"server", nc, ns])
}

pub(crate) fn client_proof(
    key: &[u8; KEY_LEN],
    nc: &[u8],
    ns: &[u8],
    kind: PromptKind,
    prompt: &str,
) -> [u8; 32] {
    hmac(
        key,
        &[
            b"client",
            nc,
            ns,
            kind.as_str().as_bytes(),
            prompt.as_bytes(),
        ],
    )
}

/// HMAC-SHA256 (RFC 2104) over the length-prefixed `parts`, so no two part lists collide.
fn hmac(key: &[u8; KEY_LEN], parts: &[&[u8]]) -> [u8; 32] {
    const BLOCK: usize = 64;
    let mut ipad = [0x36u8; BLOCK];
    let mut opad = [0x5cu8; BLOCK];
    for (i, k) in key.iter().enumerate() {
        ipad[i] ^= k;
        opad[i] ^= k;
    }
    let mut inner = Sha256::new();
    inner.update(ipad);
    for part in parts {
        inner.update((part.len() as u64).to_be_bytes());
        inner.update(part);
    }
    let inner = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(opad);
    outer.update(inner);
    let mut out = [0u8; 32];
    out.copy_from_slice(&outer.finalize());
    out
}

/// Compares in time independent of where the inputs differ.
pub(crate) fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

pub(crate) fn to_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

/// Lower- or upper-case hex digits only: `from_str_radix` alone would take a leading `+`.
pub(crate) fn from_hex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) || !text.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(text.get(i..i + 2)?, 16).ok())
        .collect()
}

pub(crate) fn key_from_hex(text: &str) -> Option<[u8; KEY_LEN]> {
    from_hex(text)?.try_into().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompts_are_classified() {
        let cases = [
            ("someone@cluster's password: ", None, PromptKind::Password),
            ("Password: ", None, PromptKind::Password),
            (
                "Enter passphrase for key '/home/u/.ssh/id_ed25519': ",
                None,
                PromptKind::Passphrase,
            ),
            ("Verification code: ", None, PromptKind::Otp),
            ("One-time password (OATH) for `u': ", None, PromptKind::Otp),
            (
                "Duo two-factor login\nPasscode or option (1-2): ",
                None,
                PromptKind::Otp,
            ),
            ("Enter your TOTP token: ", None, PromptKind::Otp),
            (
                "The authenticity of host 'cluster (192.0.2.1)' can't be established.\n\
                 ED25519 key fingerprint is SHA256:abc.\n\
                 Are you sure you want to continue connecting (yes/no/[fingerprint])? ",
                None,
                PromptKind::HostKey,
            ),
            ("Allow use of key?", Some("confirm"), PromptKind::Confirm),
            (
                "Confirm user presence for key",
                Some("none"),
                PromptKind::Notice,
            ),
            ("Response: ", None, PromptKind::Password),
            ("Footprint password: ", None, PromptKind::Password),
            // Keyboard-interactive: the server wrote everything after the prefix.
            ("(u@cluster) Password: ", None, PromptKind::Password),
            ("(u@cluster) Verification code: ", None, PromptKind::Otp),
            (
                "(u@cluster) Enter passphrase for key '/home/u/.ssh/id_ed25519': ",
                None,
                PromptKind::Password,
            ),
            (
                "(u@cluster) The authenticity of host 'x' can't be established.\n\
                 Are you sure you want to continue connecting (yes/no/[fingerprint])? ",
                None,
                PromptKind::Password,
            ),
        ];
        for (prompt, hint, want) in cases {
            assert_eq!(classify(prompt, hint), want, "{prompt:?}");
        }
    }

    #[test]
    fn answers_of_the_wrong_type_are_refused() {
        use PromptKind::*;
        let text = || Reply::Text(Secret::new("x"));
        for kind in [Password, Passphrase, Otp] {
            assert!(
                WireReply::for_prompt(kind, Reply::Accept).is_none(),
                "{kind:?}"
            );
            assert!(WireReply::for_prompt(kind, text()).is_some(), "{kind:?}");
        }
        assert!(WireReply::for_prompt(Confirm, text()).is_none());
        assert!(WireReply::for_prompt(Confirm, Reply::Accept).is_some());
        assert!(WireReply::for_prompt(HostKey, Reply::Accept).is_some());
        // A host key may be answered with its fingerprint.
        assert!(WireReply::for_prompt(HostKey, text()).is_some());
        for kind in [Password, Passphrase, Otp, HostKey, Confirm, Notice] {
            assert!(
                WireReply::for_prompt(kind, Reply::Cancel).is_none(),
                "{kind:?}"
            );
        }
    }

    #[tokio::test]
    async fn cancel_tokens_fire_when_told_or_dropped() {
        let (trigger, cancel) = PromptCancel::pair();
        assert!(!cancel.is_cancelled());
        trigger.cancel();
        assert!(cancel.is_cancelled());
        cancel.cancelled().await;

        let (trigger, cancel) = PromptCancel::pair();
        let waiting = tokio::spawn({
            let cancel = cancel.clone();
            async move { cancel.cancelled().await }
        });
        drop(trigger);
        waiting.await.unwrap();
        assert!(cancel.is_cancelled());
    }

    #[test]
    fn hmac_matches_openssl() {
        // printf '\0\0\0\0\0\0\0\003abc' | openssl dgst -sha256 -mac HMAC -macopt hexkey:0b…0b
        let key = [0x0b; KEY_LEN];
        assert_eq!(
            to_hex(&hmac(&key, &[b"abc"])),
            "e3028d8389ed94e64aaa23100abd1ec5cf1a9a897994ff74a175f60c0d4752be"
        );
        assert_ne!(hmac(&key, &[b"ab", b"c"]), hmac(&key, &[b"a", b"bc"]));
        assert_ne!(hmac(&key, &[b"abc"]), hmac(&[0x0c; KEY_LEN], &[b"abc"]));
    }

    #[test]
    fn hex_round_trips() {
        let bytes = [0u8, 1, 0xab, 0xff];
        assert_eq!(from_hex(&to_hex(&bytes)).unwrap(), bytes);
        assert_eq!(from_hex("ABcd").unwrap(), [0xab, 0xcd]);
        assert!(from_hex("abc").is_none());
        assert!(from_hex("zz").is_none());
        assert!(from_hex("+a").is_none());
        assert!(from_hex("-1").is_none());
        assert!(key_from_hex("00").is_none());
    }

    #[test]
    fn secrets_do_not_print() {
        let s = Secret::new("hunter2");
        assert_eq!(format!("{s:?}"), "Secret(<redacted>)");
        let r = WireReply::Text {
            text: "hunter2".into(),
        };
        assert!(!format!("{r:?}").contains("hunter2"));
    }
}
