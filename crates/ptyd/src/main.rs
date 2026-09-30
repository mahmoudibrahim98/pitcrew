//! # pitcrew-ptyd
//!
//! pitcrew-ptyd: a small, rarely-updated process that owns PTYs where tmux is unavailable, so daemon upgrades do not kill sessions.
//!
//! **Owned by stream B.** The work packages are in `docs/build/streams/B.md`. Build against
//! `pitcrew-protocol` and `pitcrew-interfaces` only, never another stream's internals.

fn main() {
    println!(
        "{} {} (protocol {}): not implemented yet; see docs/build/streams/B.md",
        env!("CARGO_BIN_NAME"),
        env!("CARGO_PKG_VERSION"),
        pitcrew_protocol::PROTOCOL_VERSION
    );
}
