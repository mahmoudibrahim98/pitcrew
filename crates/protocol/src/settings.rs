//! Strict requests for settings changed after setup.
use crate::model::Avatar;
use serde::Deserialize;

/// A machine's display name after setup.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SaveMachine {
    /// One to sixty characters.
    pub name: String,
}

/// A full replacement of the caller's profile.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SaveProfile {
    /// Display name.
    pub name: String,
    /// Unique handle.
    pub handle: String,
    /// Avatar appearance.
    pub avatar: Avatar,
}

