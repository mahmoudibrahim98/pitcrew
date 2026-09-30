//! Protocol versioning between the desktop, hubs and runners.

/// The protocol version this build speaks. Bump it on any breaking change to the types in this
/// crate, and record the change in `docs/build/contracts.md`.
pub const PROTOCOL_VERSION: u32 = 1;

/// The oldest protocol version this build still accepts from a peer.
pub const PROTOCOL_MIN: u32 = 1;

/// Returns `true` if a peer speaking protocol `version` can talk to this build.
#[must_use]
pub const fn is_compatible(version: u32) -> bool {
    version >= PROTOCOL_MIN && version <= PROTOCOL_VERSION
}
