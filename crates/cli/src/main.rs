//! # pitcrew-cli
//!
//! pitcrew: the agent-facing CLI and the hook entry point.
//!
//! **Owned by stream I.** The work packages are in `docs/build/streams/I.md`. Build against
//! `pitcrew-protocol` and `pitcrew-interfaces` only, never another stream's internals.

fn main() {
    println!(
        "{} {} (protocol {}): not implemented yet; see docs/build/streams/I.md",
        env!("CARGO_BIN_NAME"),
        env!("CARGO_PKG_VERSION"),
        pitcrew_protocol::PROTOCOL_VERSION
    );
}
