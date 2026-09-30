//! `serde_json` into the wire types the desktop and the hub read from each other: `Event`,
//! `StreamFrame`, `TranscriptPage` and `TranscriptItem`, plus `HostInfo` and `ApiError`.
//!
//! Checks: no panic; whatever decodes survives a round trip unchanged.
#![no_main]

use libfuzzer_sys::fuzz_target;
use pitcrew_fuzz::roundtrip;
use pitcrew_protocol::api::{ApiError, HostInfo, StreamFrame};
use pitcrew_protocol::events::Event;
use pitcrew_protocol::transcript::{TranscriptItem, TranscriptPage};
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::fmt::Debug;

fuzz_target!(|data: &[u8]| {
    check::<Event>(data);
    check::<StreamFrame>(data);
    check::<TranscriptPage>(data);
    check::<TranscriptItem>(data);
    check::<HostInfo>(data);
    check::<ApiError>(data);
});

fn check<T: Serialize + DeserializeOwned + PartialEq + Debug>(data: &[u8]) {
    if let Ok(value) = serde_json::from_slice::<T>(data) {
        roundtrip(&value);
    }
}
