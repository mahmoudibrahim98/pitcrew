//! # pitcrew-desktop
//!
//! The Tauri desktop shell (Rust side). Stream K replaces this stub with the Tauri app.
//!
//! **Owned by stream K.** The work packages are in `docs/build/streams/K.md`. Build against
//! `pitcrew-protocol` and `pitcrew-interfaces` only, never another stream's internals.

fn main() {
    println!(
        "{} {} (protocol {}): not implemented yet; see docs/build/streams/K.md",
        env!("CARGO_BIN_NAME"),
        env!("CARGO_PKG_VERSION"),
        pitcrew_protocol::PROTOCOL_VERSION
    );
}
