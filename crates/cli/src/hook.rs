//! `pitcrew hook <engine> <event> [--chain] [payload]`: runs on every agent turn, so it must cost
//! almost nothing and never get in the agent's way.
//!
//! - The payload is the CLI's hook JSON: from stdin (Claude Code), or the argument after the
//!   event (Codex's `notify` passes it that way). At most 1 MiB; a larger one is dropped.
//! - It is `POST`ed to `/v1/hooks/{engine}/{event}` with short timeouts.
//! - `--chain` (accepted only for `codex notify`, and only ever written there by `pitcrew hooks
//!   install --chain`, as `["<exe>", "hook", "codex", "notify", "--chain"]`) also runs whatever
//!   program `--chain` recorded as the original `notify`, directly via [`std::process::Command`]
//!   — no shell, no `cmd.exe` — with its own recorded arguments and the same payload Codex gave
//!   us, appended exactly as Codex itself appends it. This runs **first**, before any of our own
//!   delivery (which needs a token, a reachable daemon, and a valid payload, none of which the
//!   original notifier ever needed): a person running Codex from a plain terminal with no
//!   `PITCREW_TOKEN` set, or with the daemon down, must still get their original notification,
//!   not silently lose it because *our* delivery happened to fail first. It is spawned, not
//!   waited on, so a slow or hanging original notifier can never make a hook run over
//!   [`DEADLINE`] or block the agent; its own process group (Unix) keeps it running even if
//!   Codex kills ours, and [`CHAINED_VAR`] on its environment stops it from ever chaining again,
//!   even if the original is itself (directly or through a shell) another `pitcrew hook codex
//!   notify --chain`.
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

/// Set on a `--chain`-spawned original's own environment, and checked before ever chaining: stops
/// a cycle where the recorded "original" is itself (or runs, even through a shell) `pitcrew hook
/// codex notify --chain` — which would otherwise spawn itself forever.
const CHAINED_VAR: &str = "PITCREW_CHAINED";

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
    let (engine, event, chain, payload) = parse_args(args)?;

    // Before anything of our own can fail: a token, a reachable daemon and a valid payload are
    // all things the *original* notifier never needed, so none of them may stand between Codex
    // and it.
    if chain {
        run_chained(env, payload.as_deref());
    }

    let token = token_from_env(env)?;
    let endpoint = Endpoint::from_env(env)?;
    let body = match &payload {
        Some(payload) => payload.clone().into_bytes(),
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

/// Runs whatever `install --chain` recorded as the original `notify`, if any: spawned, never
/// waited on, so a slow or hanging original notifier can never make this hook run over
/// [`DEADLINE`]. A no-op if [`CHAINED_VAR`] is already set (we are, ourselves, something a chain
/// spawned) — never chain from inside a chain. Silent by design, like every other way this hook
/// can fail to deliver — the caller only ever sees `--chain`'s effect on `notify` itself, never a
/// reason it didn't run.
fn run_chained(env: Env<'_>, payload: Option<&str>) {
    if env(CHAINED_VAR).is_some() {
        return;
    }
    let Some(original) = crate::install::codex_chained_original(env) else {
        return;
    };
    let Some((program, rest)) = original.split_first() else {
        return;
    };
    let mut cmd = std::process::Command::new(program);
    cmd.args(rest);
    if let Some(payload) = payload {
        cmd.arg(payload);
    }
    cmd.env(CHAINED_VAR, "1");
    // Not inherited: a spawned child keeps an inherited stdout/stderr pipe open for as long as
    // it runs, which would make whoever is reading *our* output (Codex, a test harness) wait for
    // the original notifier to exit too, defeating "the hook always exits quickly".
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    // Its own process group: Codex killing our own process group (a common way to tear a tool
    // and its children down together) must not take the original notifier down with us.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        cmd.process_group(0);
    }
    let _ = cmd.spawn();
}

/// `<engine> <event> [--chain] [payload]`, checked as the API checks them. `--chain` is
/// recognised only for `codex notify` — the only shape `install --chain` ever writes; anywhere
/// else, a literal argument spelled `--chain` is just an (unusual, but not our business) payload.
fn parse_args(args: &[OsString]) -> Result<(String, String, bool, Option<String>)> {
    let text = |i: usize, what: &str| -> Result<Option<String>> {
        args.get(i)
            .map(|a| {
                a.clone()
                    .into_string()
                    .map_err(|_| Error::invalid(format!("the {what} is not valid UTF-8")))
            })
            .transpose()
    };
    let usage = || Error::invalid("usage: pitcrew hook <engine> <event> [--chain] [payload]");
    let engine = text(0, "engine")?.ok_or_else(usage)?.to_ascii_lowercase();
    let event = text(1, "event")?.ok_or_else(usage)?;
    let chain = engine == "codex"
        && event == "notify"
        && args.get(2).and_then(|a| a.to_str()) == Some("--chain");
    let payload_index = if chain { 3 } else { 2 };
    let payload = text(payload_index, "payload")?;
    if args.len() > payload_index + 1 {
        return Err(usage());
    }
    serde_json::from_value::<Engine>(serde_json::Value::String(engine.clone()))
        .map_err(|_| Error::invalid(format!("unknown engine {engine:?}")))?;
    if !is_event_name(&event) {
        return Err(Error::invalid(format!("malformed event name {event:?}")));
    }
    Ok((engine, event, chain, payload))
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
        let (engine, event, chain, payload) =
            parse_args(&args(&["Claude", "SessionStart"])).unwrap();
        assert_eq!(
            (engine.as_str(), event.as_str(), chain),
            ("claude", "SessionStart", false)
        );
        assert!(payload.is_none());
        let (_, _, chain, payload) = parse_args(&args(&["codex", "notify", "{\"a\":1}"])).unwrap();
        assert!(!chain);
        assert_eq!(payload.as_deref(), Some("{\"a\":1}"));

        for bad in [
            &["gemini", "Stop"][..],
            &["claude", "1Stop"],
            &["claude", "Stop/../x"],
            &["claude", ""],
            &["claude"],
            &[],
            &["claude", "Stop", "{}", "extra"],
            &["codex", "notify", "--chain", "{}", "extra"],
        ] {
            assert!(parse_args(&args(bad)).is_err(), "{bad:?}");
        }
        assert!(is_event_name(&"a".repeat(64)));
        assert!(!is_event_name(&"a".repeat(65)));
    }

    #[test]
    fn chain_flag_is_recognised_only_for_codex_notify() {
        let (_, _, chain, payload) =
            parse_args(&args(&["codex", "notify", "--chain", "{\"a\":1}"])).unwrap();
        assert!(chain);
        assert_eq!(payload.as_deref(), Some("{\"a\":1}"));

        // No payload at all, just --chain.
        let (_, _, chain, payload) = parse_args(&args(&["codex", "notify", "--chain"])).unwrap();
        assert!(chain);
        assert!(payload.is_none());

        // Anywhere else, a literal argument spelled "--chain" is just an (odd) payload, not the
        // flag — install --chain never writes this shape for any engine/event but codex/notify.
        let (_, _, chain, payload) = parse_args(&args(&["claude", "Stop", "--chain"])).unwrap();
        assert!(!chain);
        assert_eq!(payload.as_deref(), Some("--chain"));

        let (_, _, chain, payload) = parse_args(&args(&["codex", "exec", "--chain"])).unwrap();
        assert!(!chain);
        assert_eq!(payload.as_deref(), Some("--chain"));
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
