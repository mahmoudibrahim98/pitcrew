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
/// Back-office acceptance budget.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct BackOfficeCaps {
    /// Zero disables automatic acceptance; at most 100.
    pub max_auto_accept_per_hour: u32,
}
/// Durable workspace safety settings.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct SafetySettings {
    /// Default for a new session without an override.
    pub permission_mode: crate::model::PermissionMode,
    /// Allow low-risk automatic acceptance.
    pub back_office_enabled: bool,
    /// Acceptance budget.
    pub back_office_caps: BackOfficeCaps,
}
impl Default for SafetySettings {
    fn default() -> Self {
        Self {
            permission_mode: crate::model::PermissionMode::Default,
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

/// Strict request shape, separate from the forward-compatible stored event.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SaveSafety {
    /// Default permission mode.
    pub permission_mode: crate::model::PermissionMode,
    /// Allow automatic acceptance.
    pub back_office_enabled: bool,
    /// Hourly acceptance budget.
    pub back_office_caps: SaveBackOfficeCaps,
}
/// Strict request budget.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SaveBackOfficeCaps {
    /// At most 100.
    pub max_auto_accept_per_hour: u32,
}
impl From<SaveSafety> for SafetySettings {
    fn from(request: SaveSafety) -> Self {
        Self {
            permission_mode: request.permission_mode,
            back_office_enabled: request.back_office_enabled,
            back_office_caps: BackOfficeCaps {
                max_auto_accept_per_hour: request.back_office_caps.max_auto_accept_per_hour,
            },
        }
    }
}
