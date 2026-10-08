//! Raw tokens, their ids and their hashes.
//!
//! A token is `pcd_` (device), `pca_` (agent) or `pcs_` (session: bound to one session PitCrew
//! started on its own behalf) followed by 32 random bytes in unpadded base64url. The prefix makes
//! a leaked token easy to spot and to grep for. Only the SHA-256 of a token is ever stored.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use pitcrew_protocol::api::TokenScope;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt;
use std::str::FromStr;
use ulid::Ulid;

/// Random bytes in a token (256 bits).
pub const TOKEN_BYTES: usize = 32;
/// Prefix of device tokens.
pub const DEVICE_PREFIX: &str = "pcd_";
/// Prefix of agent tokens.
pub const AGENT_PREFIX: &str = "pca_";
/// Prefix of session tokens.
pub const SESSION_PREFIX: &str = "pcs_";

/// The prefix for a scope.
#[must_use]
pub const fn prefix(scope: TokenScope) -> &'static str {
    match scope {
        TokenScope::Device => DEVICE_PREFIX,
        TokenScope::Agent => AGENT_PREFIX,
        TokenScope::Session(_) => SESSION_PREFIX,
    }
}

/// The prefix a token claims its scope by, if it is well formed. Says nothing about validity,
/// nor, for a session token, about its session: only the registry knows that.
#[must_use]
pub fn claimed_prefix(token: &str) -> Option<&'static str> {
    let (prefix, body) = [DEVICE_PREFIX, AGENT_PREFIX, SESSION_PREFIX]
        .into_iter()
        .find_map(|prefix| Some((prefix, token.strip_prefix(prefix)?)))?;
    let bytes = URL_SAFE_NO_PAD.decode(body).ok()?;
    (bytes.len() == TOKEN_BYTES).then_some(prefix)
}

/// The scope a device or agent token claims by its prefix, if it is well formed; `None` for a
/// session token too, whose session only the registry knows. Says nothing about validity.
#[must_use]
pub fn claimed_scope(token: &str) -> Option<TokenScope> {
    match claimed_prefix(token)? {
        DEVICE_PREFIX => Some(TokenScope::Device),
        AGENT_PREFIX => Some(TokenScope::Agent),
        _ => None,
    }
}

/// A raw bearer token. It exists only when minted, to be handed to its holder once. It is never
/// stored, and `Debug` shows only its prefix.
#[derive(Clone)]
pub struct SecretToken(String);

impl SecretToken {
    /// A new random token for `scope`.
    pub(crate) fn generate(scope: TokenScope) -> Result<Self, getrandom::Error> {
        let mut bytes = [0u8; TOKEN_BYTES];
        getrandom::fill(&mut bytes)?;
        Ok(Self(format!(
            "{}{}",
            prefix(scope),
            URL_SAFE_NO_PAD.encode(bytes)
        )))
    }

    /// The token text, to give to its holder. Do not log it.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// Consumes the token, returning its text.
    #[must_use]
    pub fn into_string(self) -> String {
        self.0
    }
}

impl fmt::Debug for SecretToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let prefix = self.0.get(..DEVICE_PREFIX.len()).unwrap_or("");
        write!(f, "SecretToken({prefix}<redacted>)")
    }
}

/// The SHA-256 of a token.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct TokenHash(pub(crate) [u8; 32]);

impl TokenHash {
    pub(crate) fn of(token: &str) -> Self {
        let digest = Sha256::digest(token.as_bytes());
        let mut out = [0u8; 32];
        out.copy_from_slice(&digest);
        Self(out)
    }

    /// Compares without an early exit, so timing does not reveal how many bytes matched.
    pub(crate) fn ct_eq(&self, other: &Self) -> bool {
        let mut diff = 0u8;
        for (a, b) in self.0.iter().zip(other.0.iter()) {
            diff |= a ^ b;
        }
        std::hint::black_box(diff) == 0
    }

    pub(crate) fn to_hex(self) -> String {
        use fmt::Write as _;
        let mut s = String::with_capacity(64);
        for b in self.0 {
            let _ = write!(s, "{b:02x}");
        }
        s
    }

    pub(crate) fn from_hex(s: &str) -> Option<Self> {
        // Hex digits only: `from_str_radix` alone would also take a leading `+` ("+f" is 15).
        if s.len() != 64 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let mut out = [0u8; 32];
        for (i, byte) in out.iter_mut().enumerate() {
            *byte = u8::from_str_radix(s.get(i * 2..i * 2 + 2)?, 16).ok()?;
        }
        Some(Self(out))
    }
}

impl fmt::Debug for TokenHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("TokenHash(..)")
    }
}

/// A token's public handle, for listing, revoking and rotating. Safe to show and log.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TokenId(pub Ulid);

impl TokenId {
    /// The prefix used when the id is shown.
    pub const PREFIX: &'static str = "tok";

    /// A new id.
    #[must_use]
    pub fn new() -> Self {
        Self(Ulid::generate())
    }
}

impl Default for TokenId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for TokenId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}_{}", Self::PREFIX, self.0)
    }
}

impl FromStr for TokenId {
    type Err = ulid::DecodeError;

    /// Accepts both `tok_01J…` and the bare ULID.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let raw = s.strip_prefix("tok_").unwrap_or(s);
        Ulid::from_string(raw).map(Self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_tokens_have_their_scope_prefix_and_256_bits() {
        let device = SecretToken::generate(TokenScope::Device).unwrap();
        let agent = SecretToken::generate(TokenScope::Agent).unwrap();
        assert!(device.expose().starts_with("pcd_"));
        assert!(agent.expose().starts_with("pca_"));
        assert_eq!(claimed_scope(device.expose()), Some(TokenScope::Device));
        assert_eq!(claimed_scope(agent.expose()), Some(TokenScope::Agent));
        let session =
            SecretToken::generate(TokenScope::Session(pitcrew_protocol::ids::SessionId::new()))
                .unwrap();
        assert!(session.expose().starts_with("pcs_"));
        assert_eq!(claimed_prefix(session.expose()), Some(SESSION_PREFIX));
        assert_eq!(claimed_scope(session.expose()), None);
        assert_ne!(
            device.expose(),
            SecretToken::generate(TokenScope::Device).unwrap().expose()
        );
    }

    #[test]
    fn malformed_tokens_claim_no_scope() {
        assert_eq!(claimed_scope(""), None);
        assert_eq!(claimed_scope("dev-device-token"), None);
        assert_eq!(claimed_scope("pcd_short"), None);
        assert_eq!(claimed_scope("pcd_!!!"), None);
        assert_eq!(claimed_prefix("pcs_short"), None);
        assert_eq!(claimed_prefix("pcx_AAAA"), None);
    }

    #[test]
    fn debug_never_prints_the_token() {
        let token = SecretToken::generate(TokenScope::Device).unwrap();
        let shown = format!("{token:?}");
        assert!(!shown.contains(&token.expose()[4..]));
        assert_eq!(shown, "SecretToken(pcd_<redacted>)");
    }

    #[test]
    fn hashes_round_trip_through_hex_and_compare() {
        let a = TokenHash::of("pcd_a");
        let b = TokenHash::of("pcd_b");
        assert_eq!(TokenHash::from_hex(&a.to_hex()), Some(a));
        assert!(a.ct_eq(&a));
        assert!(!a.ct_eq(&b));
        assert_eq!(TokenHash::from_hex("zz"), None);
    }

    #[test]
    fn hex_hashes_are_hex_digits_only() {
        let hex = TokenHash::of("pcd_a").to_hex();
        assert_eq!(
            TokenHash::from_hex(&hex.to_uppercase()),
            TokenHash::from_hex(&hex)
        );
        for bad in [
            "+f".repeat(32),
            "-f".repeat(32),
            format!("+{}", &hex[1..]),
            format!("{} ", &hex[..63]),
            "é".repeat(32),
            hex[..62].to_owned(),
            format!("{hex}00"),
        ] {
            assert_eq!(TokenHash::from_hex(&bad), None, "{bad}");
        }
    }

    #[test]
    fn token_ids_parse_both_forms() {
        let id = TokenId::new();
        assert_eq!(id.to_string().parse::<TokenId>().unwrap(), id);
        assert_eq!(id.0.to_string().parse::<TokenId>().unwrap(), id);
    }
}
