//! Strict requests for settings changed after setup.
use crate::model::{Avatar, Engine, PermissionMode};
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

/// A full replacement of an existing agent recipe.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SavePersona {
    /// Display name.
    pub name: String,
    /// CLI to run.
    pub engine: Engine,
    /// Model override, omitted for the CLI default.
    pub model: Option<String>,
    /// Standing instructions.
    pub instructions: Option<String>,
    /// New sessions' permission mode.
    pub permission_mode: PermissionMode,
}
