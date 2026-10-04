//! Reversible, read-in-place session inclusion (API v1 session import).
use crate::model::{Engine, Session, TimestampMs};
use serde::{Deserialize, Serialize};

/// How existing and future sessions are included.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum ImportMode {
    /// Include all indexed sessions.
    #[default]
    All,
    /// Match the supplied dimensions.
    Filtered,
    /// Include only sessions started after the commit.
    None,
}

/// Inclusion rules; empty arrays mean no restriction.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ImportFilter {
    /// The choice.
    pub mode: ImportMode,
    /// Inclusive UTC calendar date.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub since: Option<String>,
    /// Allowed engines.
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(optional, as = "Option<_>"))]
    pub engines: Vec<Engine>,
    /// Working directories and their descendants.
    #[serde(default)]
    #[cfg_attr(feature = "ts", ts(optional, as = "Option<_>"))]
    pub folders: Vec<String>,
}

/// The durable choice and its fresh-start boundary.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ImportChoice {
    /// Inclusion rules.
    pub filter: ImportFilter,
    /// Commit time, absent before the first choice.
    pub committed_at: Option<TimestampMs>,
}

/// Dry run count.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ImportDryRun {
    /// Included indexed sessions.
    pub count: usize,
}

/// Committed count.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct ImportResult {
    /// Included indexed sessions.
    pub imported: usize,
}

impl ImportFilter {
    /// Validates filtered dimensions.
    /// # Errors
    /// A malformed date or empty folder.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.mode != ImportMode::Filtered {
            return Ok(());
        }
        if self.since.as_ref().is_some_and(|s| date_ms(s).is_none()) {
            return Err("since must be a real YYYY-MM-DD date.");
        }
        if self.folders.iter().any(|s| s.trim().is_empty()) {
            return Err("folders must not contain empty names.");
        }
        Ok(())
    }
}

impl ImportChoice {
    /// Whether a session matches, without inspecting its transcript.
    #[must_use]
    pub fn includes(&self, session: &Session) -> bool {
        match self.filter.mode {
            ImportMode::All => true,
            ImportMode::None => self.committed_at.is_some_and(|at| session.started > at),
            ImportMode::Filtered => {
                let f = &self.filter;
                f.since
                    .as_ref()
                    .is_none_or(|s| date_ms(s).is_some_and(|at| session.started >= at))
                    && (f.engines.is_empty() || f.engines.contains(&session.engine))
                    && (f.folders.is_empty()
                        || f.folders
                            .iter()
                            .any(|folder| folder_matches(folder, &session.cwd)))
            }
        }
    }
}

fn folder_matches(folder: &str, cwd: &str) -> bool {
    let folder = folder.replace('\\', "/");
    let folder = folder.trim_end_matches('/');
    let cwd = cwd.replace('\\', "/");
    cwd == folder
        || cwd
            .strip_prefix(folder)
            .is_some_and(|tail| tail.starts_with('/'))
}

/// A strict Gregorian date, as midnight UTC.
#[must_use]
pub fn date_ms(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.len() != 10
        || b[4] != b'-'
        || b[7] != b'-'
        || !b
            .iter()
            .enumerate()
            .all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit())
    {
        return None;
    }
    let mut y: i64 = s[..4].parse().ok()?;
    let m: i64 = s[5..7].parse().ok()?;
    let d: i64 = s[8..].parse().ok()?;
    let leap = y % 4 == 0 && (y % 100 != 0 || y % 400 == 0);
    let days = match m {
        2 => {
            if leap {
                29
            } else {
                28
            }
        }
        4 | 6 | 9 | 11 => 30,
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        _ => return None,
    };
    if d < 1 || d > days {
        return None;
    }
    y -= i64::from(m <= 2);
    let era = y.div_euclid(400);
    let yo = y - era * 400;
    let mp = m + if m > 2 { -3 } else { 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    Some((era * 146097 + yo * 365 + yo / 4 - yo / 100 + doy - 719468) * 86400000)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn strict_dates_and_folder_boundaries() {
        assert_eq!(date_ms("1970-01-01"), Some(0));
        assert!(date_ms("2024-02-29").is_some());
        for s in ["2026-02-29", "2026-09-31", "2026-1-01", "invalid"] {
            assert!(date_ms(s).is_none());
        }
        assert!(folder_matches(
            "/home/sam/project/",
            "/home/sam/project/src"
        ));
        assert!(!folder_matches(
            "/home/sam/project",
            "/home/sam/project-other"
        ));
        assert!(folder_matches("C:\\work", "C:/work/src"));
    }
}
