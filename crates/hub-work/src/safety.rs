//! Authored safety preferences and their replayable projection.
use crate::commands::require_person;
use crate::{Result, WorkError, WorkService};
use pitcrew_protocol::{api::Caller, events::EventBody, onboarding::SafetySettings};
use pitcrew_store::sql::{OptionalExtension, Transaction, params};
use pitcrew_store::{BoxError, Projection, StoredEvent};

/// Workspace safety settings, rebuilt from authored events.
#[derive(Debug, Default)]
pub struct Safety;
impl Safety {
    /// Projection name.
    pub const NAME: &'static str = "work.safety";
}
impl Projection for Safety {
    fn name(&self) -> &str {
        Self::NAME
    }
    fn version(&self) -> u32 {
        2
    }
    fn reset(&self, tx: &Transaction<'_>) -> std::result::Result<(), BoxError> {
        tx.execute_batch("CREATE TABLE IF NOT EXISTS work_safety (id INTEGER PRIMARY KEY CHECK(id = 1), settings TEXT NOT NULL); DELETE FROM work_safety;")?;
        Ok(())
    }
    fn apply(
        &self,
        tx: &Transaction<'_>,
        stored: &StoredEvent,
    ) -> std::result::Result<(), BoxError> {
        if let EventBody::SafetyChanged { settings } = &stored.event.body
            && settings.validate().is_ok()
        {
            tx.execute("INSERT INTO work_safety VALUES (1, ?1) ON CONFLICT(id) DO UPDATE SET settings=excluded.settings", params![serde_json::to_string(settings)?])?;
        }
        Ok(())
    }
}
pub(crate) fn settings(conn: &pitcrew_store::sql::Connection) -> Result<SafetySettings> {
    let text: Option<String> = conn
        .query_row("SELECT settings FROM work_safety WHERE id=1", [], |row| {
            row.get(0)
        })
        .optional()?;
    Ok(text
        .map(|s| serde_json::from_str(&s))
        .transpose()?
        .unwrap_or_default())
}
impl WorkService {
    /// Workspace defaults; absent preferences use the CLI's own prompts.
    /// # Errors
    /// Database or decoding errors.
    pub fn safety(&self) -> Result<SafetySettings> {
        self.read(settings)
    }

    /// Whether the workspace has explicitly saved its policy.
    /// # Errors
    /// Database errors.
    pub fn safety_saved(&self) -> Result<bool> {
        self.read(|conn| {
            Ok(
                conn.query_row("SELECT EXISTS(SELECT 1 FROM work_safety)", [], |row| {
                    row.get(0)
                })?,
            )
        })
    }
    /// Whether this office member has budget for an automatic acceptance right now.
    /// Legacy hubs keep their per-task policy until an explicit workspace preference is saved.
    pub(crate) fn auto_accept_allowed(
        &self,
        member: pitcrew_protocol::ids::MemberId,
    ) -> Result<bool> {
        use crate::codec::IdText;
        let settings = self.safety()?;
        self.read(|conn| {
            let saved: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM work_safety)", [], |row| row.get(0))?;
            if !saved { return Ok(true); }
            if !settings.back_office_enabled { return Ok(false); }
            let used: u32 = conn.query_row("SELECT COUNT(*) FROM events WHERE author=?1 AND at>=?2 AND ((type='task_moved' AND json_extract(data, '$.to')='done') OR type='brief_accepted')", params![member.text(), self.now().saturating_sub(3_600_000)], |row| row.get(0))?;
            Ok(used < settings.back_office_caps.max_auto_accept_per_hour)
        })
    }

    /// Persist an explicit person-selected policy once, with authorship.
    /// # Errors
    /// Agents, invalid caps, or database failures.
    pub fn save_safety(&self, caller: &Caller, settings: SafetySettings) -> Result<SafetySettings> {
        require_person(caller, "Saving safety settings")?;
        settings.validate().map_err(WorkError::invalid)?;
        if settings.permission_mode == pitcrew_protocol::model::PermissionMode::BypassPermissions {
            return Err(WorkError::invalid(
                "Bypass permissions cannot be saved as the workspace default while the runner disallows it.",
            ));
        }
        let _guard = self.lock();
        let saved: bool = self.read(|conn| {
            Ok(
                conn.query_row("SELECT EXISTS(SELECT 1 FROM work_safety)", [], |row| {
                    row.get(0)
                })?,
            )
        })?;
        if !saved || self.safety()? != settings {
            self.append(&[self.by(
                caller,
                EventBody::SafetyChanged {
                    settings: settings.clone(),
                },
            )])?;
        }
        Ok(settings)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pitcrew_protocol::{api::TokenScope, events::Event, ids::EventId, model::BriefTarget};
    use std::sync::Arc;

    #[test]
    fn explicit_defaults_disable_acceptance_and_the_budget_survives_replay() {
        let temp = tempfile::tempdir().unwrap();
        let demo = pitcrew_fixtures::demo_workspace().unwrap();
        let store = Arc::new(
            pitcrew_store::Store::open_with(
                temp.path().join("work.db"),
                Default::default(),
                crate::projections(),
            )
            .unwrap(),
        );
        let work = WorkService::new(store, demo.workspace.clone())
            .with_clock(Arc::new(|| 1_790_800_000_000));
        work.seed(&demo).unwrap();
        let person = Caller {
            member: "01JB000000000000000MEM0001".parse().unwrap(),
            scope: TokenScope::Device,
            on_behalf_of: None,
        };
        let office = "01JB000000000000000MEM0006".parse().unwrap();
        assert!(work.auto_accept_allowed(office).unwrap());
        work.save_safety(&person, SafetySettings::default())
            .unwrap();
        assert!(
            !work.auto_accept_allowed(office).unwrap(),
            "explicit defaults turn off acceptance"
        );
        let mut settings = SafetySettings {
            back_office_enabled: true,
            ..Default::default()
        };
        settings.back_office_caps.max_auto_accept_per_hour = 1;
        work.save_safety(&person, settings.clone()).unwrap();
        assert!(work.auto_accept_allowed(office).unwrap());
        let acceptance = |at| Event {
            id: EventId::new(),
            at,
            workspace: work.workspace(),
            author: office,
            on_behalf_of: Some(person.member),
            body: EventBody::BriefAccepted {
                target: BriefTarget::Project(demo.projects[0].id),
                text: "Synthetic brief".into(),
                next: None,
                pinned: false,
                receipts: vec![],
            },
        };
        work.store()
            .append(&[acceptance(1_790_796_000_000)])
            .unwrap();
        assert!(
            work.auto_accept_allowed(office).unwrap(),
            "outside the rolling hour"
        );
        work.store()
            .append(&[acceptance(1_790_800_000_000)])
            .unwrap();
        assert!(!work.auto_accept_allowed(office).unwrap());
        work.store().rebuild(Safety::NAME).unwrap();
        assert!(
            !work.auto_accept_allowed(office).unwrap(),
            "replay must not reset the budget"
        );
        settings.back_office_caps.max_auto_accept_per_hour = 0;
        work.save_safety(&person, settings).unwrap();
        assert!(!work.auto_accept_allowed(office).unwrap());
    }
}
