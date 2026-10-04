//! Device-only directory writes. Validation and the event append share the command lock.
use crate::commands::{known_member, require_person};
use crate::error::{Result, WorkError};
use crate::{WorkService, query};
use pitcrew_protocol::api::{Caller, PersonaEdit, TeamEdit};
use pitcrew_protocol::events::EventBody;
use pitcrew_protocol::ids::{MemberId, PersonaId, TeamId};
use pitcrew_protocol::model::{Engine, Member, MemberKind, PermissionMode, Persona, Team};
use std::collections::HashSet;

fn text(value: String, field: &str, max: usize) -> Result<String> {
    let value =
        value.trim_matches(|c: char| (c.is_whitespace() && c != '\u{85}') || c == '\u{feff}');
    if value.is_empty() || value.chars().count() > max || value.chars().any(char::is_control) {
        return Err(WorkError::invalid(format!(
            "{field} must be 1–{max} characters without controls."
        )));
    }
    Ok(value.to_owned())
}

impl WorkService {
    /// Creates or replaces an agent recipe, together with its member identity.
    ///
    /// # Errors
    /// Forbidden for agents; not found for an unknown edit target; invalid for malformed fields.
    pub fn save_persona(
        &self,
        caller: &Caller,
        id: Option<PersonaId>,
        edit: PersonaEdit,
    ) -> Result<Persona> {
        require_person(caller, "Saving an agent")?;
        let _guard = self.lock();
        if let Some(id) = id {
            self.read(|c| query::persona(c, &id))?
                .ok_or_else(|| WorkError::not_found("No such persona."))?;
        }
        self.read(|c| known_member(c, &caller.member, "caller"))?;
        let name = text(edit.name, "name", 80)?;
        let model = edit.model.map(|v| text(v, "model", 200)).transpose()?;
        if edit
            .instructions
            .as_ref()
            .is_some_and(|s| s.chars().count() > 32_000)
        {
            return Err(WorkError::invalid(
                "instructions must be at most 32000 characters.",
            ));
        }
        let persona = Persona {
            id: id.unwrap_or_default(),
            name,
            engine: edit.engine,
            model,
            instructions: edit.instructions,
            permission_mode: edit.permission_mode,
        };
        let mut events = vec![self.by(
            caller,
            EventBody::PersonaSaved {
                persona: persona.clone(),
            },
        )];
        if id.is_none() {
            let member_id = MemberId::new();
            let member = Member {
                id: member_id,
                kind: MemberKind::Agent,
                handle: format!("@agent-{}", member_id.0.to_string().to_lowercase()),
                name: persona.name.clone(),
                owner: Some(caller.member),
                persona: Some(persona.id),
            };
            events.push(self.by(caller, EventBody::MemberAdded { member }));
        } else {
            for mut member in self
                .members()?
                .into_iter()
                .filter(|m| m.persona == Some(persona.id))
            {
                member.name.clone_from(&persona.name);
                events.push(self.by(caller, EventBody::MemberAdded { member }));
            }
        }
        self.append(&events)?;
        Ok(persona)
    }

    /// Creates or replaces a team after validating all references.
    ///
    /// # Errors
    /// Forbidden for agents; not found for an unknown edit target; invalid for fields/references.
    pub fn save_team(&self, caller: &Caller, id: Option<TeamId>, edit: TeamEdit) -> Result<Team> {
        require_person(caller, "Saving a team")?;
        let _guard = self.lock();
        if let Some(id) = id
            && !self.teams()?.iter().any(|t| t.id == id)
        {
            return Err(WorkError::not_found("No such team."));
        }
        self.read(|c| known_member(c, &caller.member, "caller"))?;
        let name = text(edit.name, "name", 80)?;
        if edit.members.len() > 256 {
            return Err(WorkError::invalid("A team has at most 256 members."));
        }
        let mut seen = HashSet::new();
        let mut members: Vec<_> = edit
            .members
            .into_iter()
            .filter(|m| seen.insert(*m))
            .collect();
        if seen.insert(edit.lead) {
            members.insert(0, edit.lead);
        }
        if members.len() > 256 {
            return Err(WorkError::invalid("A team has at most 256 members."));
        }
        self.read(|c| {
            if let Some((_, id)) = query::first_unknown_member(c, &members)? {
                return Err(WorkError::invalid(format!("No member {id}.")));
            }
            Ok(())
        })?;
        let team = Team {
            id: id.unwrap_or_default(),
            name,
            lead: edit.lead,
            members,
        };
        self.append(&[self.by(caller, EventBody::TeamSaved { team: team.clone() })])?;
        Ok(team)
    }
}

impl WorkService {
    /// Ensures a person's scan-detected engines have dispatchable, owned members and recipes.
    /// Existing owned members with the same persona engine are reused; service actors without
    /// personas do not count. All new recipes/members commit in one batch under the writer lock.
    ///
    /// # Errors
    /// Forbidden for agent callers, invalid for an unknown person, or storage errors.
    pub fn ensure_engine_agents(&self, caller: &Caller, engines: &[Engine]) -> Result<Vec<Member>> {
        require_person(caller, "Creating detected agents")?;
        let _guard = self.lock();
        let mut owned = self.read(|c| {
            known_member(c, &caller.member, "caller")?;
            let personas = query::personas(c)?;
            let mut owned = HashSet::new();
            for member in query::members(c)?
                .into_iter()
                .filter(|m| m.kind == MemberKind::Agent && m.owner == Some(caller.member))
            {
                if let Some(persona) = personas.iter().find(|p| Some(p.id) == member.persona) {
                    owned.insert(persona.engine);
                }
            }
            Ok(owned)
        })?;
        let mut events = Vec::new();
        let mut created = Vec::new();
        for engine in engines {
            if !owned.insert(*engine) {
                continue;
            }
            let persona = Persona {
                id: PersonaId::new(),
                name: format!("{engine:?}"),
                engine: *engine,
                model: None,
                instructions: None,
                permission_mode: PermissionMode::Default,
            };
            let id = MemberId::new();
            let member = Member {
                id,
                kind: MemberKind::Agent,
                handle: format!("@agent-{}", id.0.to_string().to_lowercase()),
                name: persona.name.clone(),
                owner: Some(caller.member),
                persona: Some(persona.id),
            };
            events.push(self.by(caller, EventBody::PersonaSaved { persona }));
            events.push(self.by(
                caller,
                EventBody::MemberAdded {
                    member: member.clone(),
                },
            ));
            created.push(member);
        }
        if !events.is_empty() {
            self.append(&events)?;
        }
        Ok(created)
    }
}
