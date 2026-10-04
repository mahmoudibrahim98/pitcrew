//! Device-only onboarding hooks and workspace safety preferences.
use serde::{Deserialize, Serialize};

/// Exact text shown before a hook installation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct HooksDiffFile {
    /// Configuration path on the hub's machine.
    pub path: String,
    /// Absent for a new file.
    pub before: Option<String>,
    /// Proposed replacement, without normalization.
    pub after: String,
}
/// One discovered engine's installer status.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct HooksEngine {
    /// CLI name.
    pub engine: String,
    /// Installer status, including conflicting configurations.
    pub status: String,
    /// Explanation, never logged by the route.
    pub detail: String,
}
/// Server-held plan's preview.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct HooksDiff {
    /// Opaque revision, bound to the person and machine.
    pub revision: String,
    /// Exact files changed.
    pub files: Vec<HooksDiffFile>,
    /// All discovered engines, even when no change is needed.
    pub engines: Vec<HooksEngine>,
}
/// Confirmation of a previously displayed plan.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct InstallHooks {
    /// Revision from the preview.
    pub revision: String,
}
/// Workspace default permission mode; skipping prompts is opt-in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "kebab-case")]
pub enum WorkspacePermissionMode {
    /// The CLI's own prompts.
    #[default]
    Default,
    /// Plan before executing.
    Plan,
    /// Accept edits, still prompt for commands.
    AcceptEdits,
    /// Explicitly skip prompts.
    BypassPermissions,
}
/// Back-office acceptance budget.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct BackOfficeCaps {
    /// Zero disables automatic acceptance; at most 100.
    pub max_auto_accept_per_hour: u32,
}
/// Durable workspace safety settings.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct SafetySettings {
    /// Default for a new session without an override.
    pub permission_mode: WorkspacePermissionMode,
    /// Allow low-risk automatic acceptance.
    pub back_office_enabled: bool,
    /// Acceptance budget.
    pub back_office_caps: BackOfficeCaps,
}
impl Default for SafetySettings {
    fn default() -> Self {
        Self {
            permission_mode: WorkspacePermissionMode::Default,
            back_office_enabled: false,
            back_office_caps: BackOfficeCaps {
                max_auto_accept_per_hour: 20,
            },
        }
    }
}
impl SafetySettings {
    /// Validate the budget before appending any event.
    /// # Errors
    /// Returns an error when the hourly budget exceeds 100.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.back_office_caps.max_auto_accept_per_hour > 100 {
            Err("The hourly cap must be between 0 and 100.")
        } else {
            Ok(())
        }
    }
}

impl From<WorkspacePermissionMode> for crate::model::PermissionMode {
    fn from(mode: WorkspacePermissionMode) -> Self {
        match mode {
            WorkspacePermissionMode::Default => Self::Default,
            WorkspacePermissionMode::Plan => Self::Plan,
            WorkspacePermissionMode::AcceptEdits => Self::AcceptEdits,
            WorkspacePermissionMode::BypassPermissions => Self::BypassPermissions,
        }
    }
}
