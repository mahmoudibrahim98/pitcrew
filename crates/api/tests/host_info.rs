//! `GET /v1/host/info` has the contract's JSON shape (`docs/build/contracts/api-v1.md`, and the
//! mock hub's `hostInfo`).

#![allow(clippy::unwrap_used)]

mod common;

use common::{Fixture, call, get_request};
use pitcrew_protocol::api::HostInfo;
use std::collections::BTreeSet;

fn keys(value: &serde_json::Value) -> BTreeSet<&str> {
    value
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect()
}

#[tokio::test]
async fn host_info_matches_the_contract() {
    let f = Fixture::new();
    let (status, body) = call(f.app(), get_request("/v1/host/info", None)).await;
    assert_eq!(status, 200);

    assert_eq!(
        keys(&body),
        BTreeSet::from([
            "name",
            "version",
            "protocol",
            "protocol_min",
            "roles",
            "machine",
            "capabilities"
        ])
    );
    assert_eq!(body["name"], "pitcrewd");
    assert_eq!(body["version"], "0.0.0-test");
    assert_eq!(body["protocol"], pitcrew_protocol::PROTOCOL_VERSION);
    assert_eq!(body["protocol_min"], pitcrew_protocol::PROTOCOL_MIN);
    assert_eq!(body["roles"], serde_json::json!(["hub", "runner"]));
    assert!(body["capabilities"].is_array());

    let machine = &body["machine"];
    let required = BTreeSet::from(["hostname", "os", "arch", "has_tmux", "home_on_network_fs"]);
    let mut allowed = required.clone();
    allowed.insert("scheduler");
    let present = keys(machine);
    assert!(required.is_subset(&present), "{present:?}");
    assert!(present.is_subset(&allowed), "{present:?}");
    assert_eq!(machine["os"], std::env::consts::OS);
    assert_eq!(machine["arch"], std::env::consts::ARCH);
    assert!(machine["hostname"].as_str().is_some_and(|h| !h.is_empty()));

    let parsed: HostInfo = serde_json::from_value(body).unwrap();
    assert!(parsed.protocol_min <= parsed.protocol);
}
