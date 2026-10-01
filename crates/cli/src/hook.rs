//! `pitcrew hook <engine> <event> [payload]`: runs on every agent turn, so it must cost almost
//! nothing and never get in the agent's way.
//!
//! - The payload is the CLI's hook JSON: from stdin (Claude Code), or the argument after the
//!   event (Codex's `notify` passes it that way). At most 1 MiB; a larger one is dropped.
//! - It is `POST`ed to `/v1/hooks/{engine}/{event}` with short timeouts.
//! - The caller (`main`) always exits 0 and prints nothing, whatever happens here, and stops the
//!   process after [`DEADLINE`] even if the daemon hangs.

use crate::config::{Endpoint, Env, token_from_env};
use crate::error::{Error, Result};
use crate::http::{self, Request};
use crate::transport::Timeouts;
use pitcrew_protocol::model::Engine;
use std::ffi::OsString;
use std::io::Read;
use std::time::Duration;

/// Largest payload sent, as the API allows.
pub const MAX_PAYLOAD: usize = 1 << 20;

/// The whole hook gives up after this long.
pub const DEADLINE: Duration = Duration::from_millis(500);

/// Set to anything to have `pitcrew hook` print why it did not deliver an event (to stderr).
pub const DEBUG_VAR: &str = "PITCREW_HOOK_DEBUG";

/// Sends one hook event. `args` are the arguments after `hook`.
///
/// # Errors
/// Why the event was not delivered. The caller ignores it unless [`DEBUG_VAR`] is set.
pub fn run(
    args: &[OsString],
    env: Env<'_>,
    stdin: &mut dyn Read,
    stdin_is_terminal: bool,
) -> Result<()> {
    let (engine, event, payload) = parse_args(args)?;
    // Nothing to do without a token or a daemon; check before touching stdin.
    let token = token_from_env(env)?;
    let endpoint = Endpoint::from_env(env)?;
    let body = match payload {
        Some(payload) => payload.into_bytes(),
        None if stdin_is_terminal => Vec::new(),
        None => read_capped(stdin)?,
    };
    let body = check_payload(body)?;

    let mut conn = endpoint.connect(Timeouts::HOOK)?;
    let target = format!("/v1/hooks/{engine}/{event}");
    let request = Request {
        method: "POST",
        target: &target,
        host: endpoint.host_header(),
        token: Some(&token),
        body: Some(&body),
    };
    let response = http::exchange(&mut conn, &request, 64 * 1024)
        .map_err(|e| Error::internal(format!("POST {target}: {e}")))?;
    if response.is_success() {
        Ok(())
    } else {
        Err(Error::from_response(response.status, &response.body))
    }
}

/// `<engine> <event> [payload]`, checked as the API checks them.
fn parse_args(args: &[OsString]) -> Result<(String, String, Option<String>)> {
    let text = |i: usize, what: &str| -> Result<Option<String>> {
        args.get(i)
            .map(|a| {
                a.clone()
                    .into_string()
                    .map_err(|_| Error::invalid(format!("the {what} is not valid UTF-8")))
            })
            .transpose()
    };
    let usage = || Error::invalid("usage: pitcrew hook <engine> <event> [payload]");
    let engine = text(0, "engine")?.ok_or_else(usage)?.to_ascii_lowercase();
    let event = text(1, "event")?.ok_or_else(usage)?;
    let payload = text(2, "payload")?;
    if args.len() > 3 {
        return Err(usage());
    }
    serde_json::from_value::<Engine>(serde_json::Value::String(engine.clone()))
        .map_err(|_| Error::invalid(format!("unknown engine {engine:?}")))?;
    if !is_event_name(&event) {
        return Err(Error::invalid(format!("malformed event name {event:?}")));
    }
    Ok((engine, event, payload))
}

/// `[A-Za-z][A-Za-z0-9_-]{0,63}`.
fn is_event_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    bytes.first().is_some_and(u8::is_ascii_alphabetic)
        && bytes.len() <= 64
        && bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
}

fn read_capped(stdin: &mut dyn Read) -> Result<Vec<u8>> {
    let mut body = Vec::new();
    stdin
        .take(MAX_PAYLOAD as u64 + 1)
        .read_to_end(&mut body)
        .map_err(|e| Error::internal(format!("cannot read the payload: {e}")))?;
    Ok(body)
}

/// An empty payload becomes `{}`; anything else must look like a JSON object and fit the cap.
/// The daemon parses it; this only avoids sending what it would certainly refuse.
fn check_payload(body: Vec<u8>) -> Result<Vec<u8>> {
    if body.len() > MAX_PAYLOAD {
        return Err(Error::invalid("the payload is over 1 MiB; not sent"));
    }
    match body.iter().find(|b| !b.is_ascii_whitespace()) {
        None => Ok(b"{}".to_vec()),
        Some(b'{') => Ok(body),
        Some(_) => Err(Error::invalid("the payload is not a JSON object; not sent")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<OsString> {
        list.iter().map(OsString::from).collect()
    }

    #[test]
    fn arguments_are_checked_like_the_api_does() {
        let (engine, event, payload) = parse_args(&args(&["Claude", "SessionStart"])).unwrap();
        assert_eq!(
            (engine.as_str(), event.as_str()),
            ("claude", "SessionStart")
        );
        assert!(payload.is_none());
        let (_, _, payload) = parse_args(&args(&["codex", "notify", "{\"a\":1}"])).unwrap();
        assert_eq!(payload.as_deref(), Some("{\"a\":1}"));

        for bad in [
            &["gemini", "Stop"][..],
            &["claude", "1Stop"],
            &["claude", "Stop/../x"],
            &["claude", ""],
            &["claude"],
            &[],
            &["claude", "Stop", "{}", "extra"],
        ] {
            assert!(parse_args(&args(bad)).is_err(), "{bad:?}");
        }
        assert!(is_event_name(&"a".repeat(64)));
        assert!(!is_event_name(&"a".repeat(65)));
    }

    #[test]
    fn payloads() {
        assert_eq!(check_payload(Vec::new()).unwrap(), b"{}");
        assert_eq!(check_payload(b" \n".to_vec()).unwrap(), b"{}");
        assert_eq!(
            check_payload(b" {\"a\":1}".to_vec()).unwrap(),
            b" {\"a\":1}"
        );
        assert!(check_payload(b"[1]".to_vec()).is_err());
        let mut big = b"{".to_vec();
        big.resize(MAX_PAYLOAD + 1, b' ');
        assert!(check_payload(big).is_err());
    }
}
