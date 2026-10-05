//! Profile and agent recipes use the existing authored directory events.
use crate::{
    Result, WorkError, WorkService,
    commands::require_person,
    query,
    setup::{checked_handle, checked_text},
};
use pitcrew_protocol::{
    api::Caller,
    events::EventBody,
    ids::PersonaId,
    model::{Member, MemberKind, PermissionMode, Persona},
    settings::{SavePersona, SaveProfile},
};

impl WorkService {
    /// Rename a known machine without replacing its runtime information.
    /// # Errors
    /// Unknown machine, unauthorized caller, invalid name or database failure.
    pub fn rename_machine(
        &self,
        caller: &Caller,
        id: pitcrew_protocol::ids::MachineId,
        name: &str,
    ) -> Result<pitcrew_protocol::model::Machine> {
        self.require_owner(caller)?;
        let _guard = self.lock();
        let mut machine = self
            .read(|c| query::machine(c, &id))?
            .ok_or_else(|| WorkError::not_found("Unknown machine."))?;
        let next = checked_text(name, "name", 60)?;
        if machine.name != next {
            machine.name = next;
            self.append(&[self.by(
                caller,
                EventBody::MachineAdded {
                    machine: machine.clone(),
                },
            )])?;
        }
        Ok(machine)
    }
    /// Persist a workspace name before exposing it to connected clients.
    /// # Errors
    /// Unauthorized caller, invalid name, persistence or database failure.
    pub fn rename_workspace(
        &self,
        caller: &Caller,
        name: &str,
        persist: impl FnOnce(&pitcrew_protocol::model::Workspace) -> Result<()>,
    ) -> Result<pitcrew_protocol::model::Workspace> {
        self.require_owner(caller)?;
        let _guard = self.lock();
        let workspace = pitcrew_protocol::model::Workspace {
            id: self.workspace(),
            name: checked_text(name, "name", 80)?,
        };
        persist(&workspace)?;
        self.set_workspace_name(workspace.name.clone());
        Ok(workspace)
    }
    /// Only the person who set up the hub may edit workspace defaults.
    /// # Errors
    /// Unknown person, another person, or database failure.
    pub fn require_owner(&self, caller: &Caller) -> Result<()> {
        require_person(caller, "Workspace settings")?;
        if self.read(query::first_person)? != Some(caller.member) {
            return Err(WorkError::forbidden(
                "Only the workspace owner may change these settings.",
            ));
        }
        Ok(())
    }

    /// Replace the token's own profile, preserving its membership and ownership.
    /// # Errors
    /// Invalid fields, handle conflict, unknown member or database error.
    pub fn save_profile(&self, caller: &Caller, input: SaveProfile) -> Result<Member> {
        require_person(caller, "Profile settings")?;
        let _guard = self.lock();
        let mut member = self.member(&caller.member)?;
        if member.kind != MemberKind::Human {
            return Err(WorkError::forbidden("A profile needs a person."));
        }
        let name = checked_text(&input.name, "name", 80)?;
        let handle = checked_handle(&input.handle)?;
        if handle == crate::office::OFFICE_HANDLE {
            return Err(WorkError::invalid("That handle is reserved."));
        }
        let initials = checked_text(&input.avatar.initials, "initials", 4)?;
        if initials.chars().any(pitcrew_protocol::text::is_hidden) {
            return Err(WorkError::invalid("Initials must be visible."));
        }
        let colour = &input.avatar.colour;
        if colour.len() != 7
            || !colour.starts_with('#')
            || !colour.as_bytes()[1..].iter().all(u8::is_ascii_hexdigit)
        {
            return Err(WorkError::invalid("Colour must be #RRGGBB."));
        }
        if self
            .read(|c| query::member_with_handle(c, &handle))?
            .is_some_and(|m| m.id != member.id)
        {
            return Err(WorkError::conflict("That handle is already taken."));
        }
        let before = member.clone();
        member.name = name;
        member.handle = handle;
        member.avatar = Some(pitcrew_protocol::model::Avatar {
            initials,
            colour: colour.to_ascii_lowercase(),
        });
        if member != before {
            self.append(&[self.by(
                caller,
                EventBody::MemberAdded {
                    member: member.clone(),
                },
            )])?;
        }
        Ok(member)
    }

    /// Edit an existing default-agent recipe for future sessions.
    /// # Errors
    /// Unknown recipe, unauthorized owner, invalid fields or database error.
    pub fn save_persona(
        &self,
        caller: &Caller,
        id: PersonaId,
        input: SavePersona,
    ) -> Result<Persona> {
        self.require_owner(caller)?;
        let _guard = self.lock();
        let before = self
            .read(|c| query::persona(c, &id))?
            .ok_or_else(|| WorkError::not_found("Unknown default agent."))?;
        let name = checked_text(&input.name, "name", 80)?;
        for (value, max, field) in [
            (&input.model, 200, "model"),
            (&input.instructions, 32768, "instructions"),
        ] {
            if value
                .as_ref()
                .is_some_and(|s| s.chars().count() > max || s.contains('\0'))
            {
                return Err(WorkError::invalid(format!("Invalid {field}.")));
            }
        }
        if input.permission_mode == PermissionMode::BypassPermissions {
            return Err(WorkError::invalid("Bypass permissions is unavailable."));
        }
        let persona = Persona {
            id,
            name,
            engine: input.engine,
            model: input.model.filter(|s| !s.trim().is_empty()),
            instructions: input.instructions.filter(|s| !s.trim().is_empty()),
            permission_mode: input.permission_mode,
        };
        if persona != before {
            self.append(&[self.by(
                caller,
                EventBody::PersonaSaved {
                    persona: persona.clone(),
                },
            )])?;
        }
        Ok(persona)
    }
}
