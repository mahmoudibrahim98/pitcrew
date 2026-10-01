//! `pitcrew_protocol::runner::decode_line` on one arbitrary line, for every message type that
//! crosses the hub-runner connection. A runner on a remote machine, or anything that can write
//! to its socket, controls these bytes.
//!
//! Checks: no panic; whatever decodes encodes to one line (no embedded newline) that decodes
//! back to the same value.
#![no_main]

use libfuzzer_sys::fuzz_target;
use pitcrew_protocol::runner::{
    CommandOutcome, HubToRunner, RunnerCommand, RunnerToHub, decode_line, encode_line,
};
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::fmt::Debug;

fuzz_target!(|data: &[u8]| {
    let Ok(line) = std::str::from_utf8(data) else {
        return;
    };
    check::<RunnerToHub>(line);
    check::<HubToRunner>(line);
    check::<RunnerCommand>(line);
    check::<CommandOutcome>(line);
});

fn check<T: Serialize + DeserializeOwned + PartialEq + Debug>(line: &str) {
    let Ok(message) = decode_line::<T>(line) else {
        return;
    };
    let encoded = encode_line(&message).expect("a decoded message encodes");
    let body = encoded
        .strip_suffix('\n')
        .expect("an encoded line ends with a newline");
    assert!(
        !body.contains(['\n', '\r']),
        "an encoded line breaks framing"
    );
    let back: T = decode_line(&encoded)
        .unwrap_or_else(|e| panic!("an encoded line does not decode: {e}\n{encoded}"));
    assert_eq!(
        back, message,
        "the round trip changed the message\n{encoded}"
    );
}
