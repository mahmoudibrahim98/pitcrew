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
//! 4. server → `{"reply":"text","text":…}`, `{"reply":"accept"}` or `{"reply":"cancel"}`.

pub mod client;
pub(crate) mod server;

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::fmt;

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
    /// What is asked.
    pub kind: PromptKind,
    /// ssh's prompt text, verbatim. For a host key it includes the fingerprint.
    pub prompt: String,
}

/// The user's answer.
#[derive(Debug)]
pub enum Reply {
    /// Typed text: a password, passphrase or code.
    Text(Secret),
    /// Yes: trust the host key, or confirm.
    Accept,
    /// No, or the dialog was closed. ssh gives up on this attempt.
    Cancel,
}

/// Asks the user. The desktop implements this (stream K); it may block until the user answers.
/// Implementations must never log or keep the answer.
pub trait PromptHandler: Send + Sync + 'static {
    /// Shows `request` and returns the answer.
    fn prompt(&self, request: &PromptRequest) -> Reply;
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
/// (`confirm` for yes/no questions, `none` for notices; unset otherwise).
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
    if text.contains("continue connecting") || text.contains("authenticity of host") {
        PromptKind::HostKey
    } else if text.contains("passphrase") {
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
    Cancel,
}

impl fmt::Debug for WireReply {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Text { .. } => f.write_str("Text(<redacted>)"),
            Self::Accept => f.write_str("Accept"),
            Self::Cancel => f.write_str("Cancel"),
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

pub(crate) fn from_hex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
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
        ];
        for (prompt, hint, want) in cases {
            assert_eq!(classify(prompt, hint), want, "{prompt:?}");
        }
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
        assert!(from_hex("abc").is_none());
        assert!(from_hex("zz").is_none());
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
