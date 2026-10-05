mod common;
use common::*;
use pitcrew_hub_work::projection::Directory;
use pitcrew_protocol::{
    api::{ErrorCode, PersonaEdit},
    settings::SaveProfile,
};
use serde_json::json;

fn profile(handle: &str) -> SaveProfile {
    serde_json::from_value(json!({"name":" Sam Updated ", "handle":handle, "avatar":{"initials":"SU","colour":"#abcdef"}})).unwrap()
}
#[test]
fn profile_is_owned_validated_idempotent_and_replayable() {
    let temp = tempfile::tempdir().unwrap();
    let work = seeded(temp.path());
    let sam = person(SAM);
    assert_eq!(
        work.save_profile(&agent(WRITER), profile("@new"))
            .unwrap_err()
            .code(),
        ErrorCode::Forbidden
    );
    assert_eq!(
        work.save_profile(&sam, profile("@writer"))
            .unwrap_err()
            .code(),
        ErrorCode::Conflict
    );
    assert_eq!(
        work.save_profile(&sam, profile("@office"))
            .unwrap_err()
            .code(),
        ErrorCode::Invalid
    );
    for (initials, colour) in [
        ("", "#abcdef"),
        ("ABCDE", "#abcdef"),
        ("\u{202e}", "#abcdef"),
        ("SU", "red"),
    ] {
        let mut input = profile("@sam");
        input.avatar.initials = initials.into();
        input.avatar.colour = colour.into();
        assert_eq!(
            work.save_profile(&sam, input).unwrap_err().code(),
            ErrorCode::Invalid
        );
    }
    let saved = work.save_profile(&sam, profile("@sam-new")).unwrap();
    assert_eq!(saved.name, "Sam Updated");
    assert_eq!(saved.id, sam.member);
    assert!(saved.owner.is_none());
    let rev = work.workspace_at().unwrap().rev;
    work.save_profile(&sam, profile("@sam-new")).unwrap();
    assert_eq!(work.workspace_at().unwrap().rev, rev);
    work.store().rebuild(Directory::NAME).unwrap();
    assert_eq!(work.member(&sam.member).unwrap(), saved);
}

#[test]
fn workspace_persistence_failure_keeps_the_live_name_and_recipes_replay() {
    let temp = tempfile::tempdir().unwrap();
    let work = seeded(temp.path());
    let sam = person(SAM);
    let before = work.workspace_at().unwrap().workspace;
    assert!(
        work.rename_workspace(&sam, "New workspace", |_| Err(
            pitcrew_hub_work::WorkError::unavailable("Synthetic failure")
        ))
        .is_err()
    );
    assert_eq!(work.workspace_at().unwrap().workspace, before);
    assert_eq!(
        work.rename_workspace(&person(WRITER), "New workspace", |_| Ok(()))
            .unwrap_err()
            .code(),
        ErrorCode::Forbidden
    );
    assert_eq!(
        work.rename_workspace(&sam, " New workspace ", |_| Ok(()))
            .unwrap()
            .name,
        "New workspace"
    );
    let old = work.personas().unwrap().remove(0);
    let input = || {
        serde_json::from_value::<PersonaEdit>(json!({"name":"Updated recipe", "engine":"codex", "model":"synthetic-model", "instructions":"Synthetic instructions", "permission_mode":"plan"})).unwrap()
    };
    assert_eq!(
        work.save_persona(&person(WRITER), Some(old.id), input())
            .unwrap_err()
            .code(),
        ErrorCode::Forbidden
    );
    let saved = work.save_persona(&sam, Some(old.id), input()).unwrap();
    work.store().rebuild(Directory::NAME).unwrap();
    assert!(work.personas().unwrap().contains(&saved));
}
