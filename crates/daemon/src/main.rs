//! # pitcrew-daemon
//!
//! pitcrewd: the composition root that wires hub, runner and API together.
//!
//! **Owned by stream 0.** The work packages are in `docs/build/streams/0.md`. Build against
//! `pitcrew-protocol` and `pitcrew-interfaces` only, never another stream's internals.

fn main() {
    println!(
        "{} {} (protocol {}): not implemented yet; see docs/build/streams/0.md",
        env!("CARGO_BIN_NAME"),
        env!("CARGO_PKG_VERSION"),
        pitcrew_protocol::PROTOCOL_VERSION
    );
}
