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
/// Strict replacement fields for an existing recipe; creation still ignores server-owned fields.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SavePersona {
    /// Display name.
    pub name: String,
    /// CLI engine.
    pub engine: crate::model::Engine,
    /// Optional model.
    pub model: Option<String>,
    /// Optional standing instructions.
    pub instructions: Option<String>,
    /// Permission mode for new sessions.
    pub permission_mode: crate::model::PermissionMode,
}
