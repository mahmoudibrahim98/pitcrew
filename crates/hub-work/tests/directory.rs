//! Directory writes validate before appending and replay without losing membership.
mod common;
use common::{SAM, WRITER, agent, app, call, expect, get, person, seeded};
use serde_json::json;

#[tokio::test]
async fn directory_writes_validate_authorize_and_rebuild() {
    let dir = tempfile::tempdir().expect("temp");
    let work = seeded(dir.path());
    let app = app(&work);
    let caller = person(SAM);
    let recipe = json!({"name":" Synthetic writer ","engine":"codex","model":"demo-model","instructions":"Synthetic\nexamples.","permission_mode":"plan"});
    let rev = work.store().latest_rev().expect("rev");
    for body in [
        json!({"name":" ","engine":"codex"}),
        json!({"name":"bad\u{85}","engine":"codex"}),
        json!({"name":"x","engine":"invalid"}),
        json!({"name":"x","engine":"claude","model":" --dangerously-skip-permissions "}),
        json!({"name":"x","engine":"claude","permission_mode":"bypass_permissions"}),
        json!({"name":"x","engine":"codex","instructions":"x".repeat(32001)}),
    ] {
        expect(
            &call(&app, Some(caller), "POST", "/v1/personas", Some(body)).await,
            400,
        );
    }
    expect(
        &call(
            &app,
            Some(agent(WRITER)),
            "POST",
            "/v1/personas",
            Some(recipe.clone()),
        )
        .await,
        403,
    );
    assert_eq!(work.store().latest_rev().expect("rev"), rev);
    let created = call(
        &app,
        Some(caller),
        "POST",
        "/v1/personas",
        Some(recipe.clone()),
    )
    .await;
    expect(&created, 201);
    assert_eq!(created.1["name"], "Synthetic writer");
    let id = created.1["id"].as_str().expect("id");
    let member = work
        .members()
        .expect("members")
        .into_iter()
        .find(|m| m.persona == Some(id.parse().expect("persona id")))
        .expect("agent member");
    assert_eq!(member.owner, Some(caller.member));
    let edited = call(
        &app,
        Some(caller),
        "PUT",
        &format!("/v1/personas/per_{id}"),
        Some(json!({"name":"Renamed writer","engine":"claude"})),
    )
    .await;
    expect(&edited, 200);
    assert_eq!(edited.1["id"], created.1["id"]);
    assert_eq!(
        work.members()
            .expect("members")
            .iter()
            .find(|m| m.id == member.id)
            .expect("member")
            .name,
        "Renamed writer"
    );
    let team_rev = work.store().latest_rev().expect("rev");
    let missing = "01J00000000000000000000000";
    for body in [
        json!({"name":"crew","lead":SAM,"members":[missing]}),
        json!({"name":"crew","lead":missing,"members":[]}),
        json!({"name":"crew","lead":SAM,"members":vec![SAM;257]}),
    ] {
        expect(
            &call(&app, Some(caller), "POST", "/v1/teams", Some(body)).await,
            400,
        );
    }
    assert_eq!(work.store().latest_rev().expect("rev"), team_rev);
    let team = call(
        &app,
        Some(caller),
        "POST",
        "/v1/teams",
        Some(json!({"name":" Synthetic crew ","lead":SAM,"members":[member.id,member.id]})),
    )
    .await;
    expect(&team, 201);
    assert_eq!(team.1["members"], json!([SAM, member.id]));
    let team_id = team.1["id"].as_str().expect("id");
    let edit = call(
        &app,
        Some(caller),
        "PUT",
        &format!("/v1/teams/{team_id}"),
        Some(json!({"name":"Renamed crew","lead":member.id,"members":[]})),
    )
    .await;
    expect(&edit, 200);
    assert_eq!(edit.1["members"], json!([member.id]));
    for path in [format!("/v1/personas/{id}"), format!("/v1/teams/{team_id}")] {
        expect(
            &call(&app, Some(agent(WRITER)), "PUT", &path, Some(json!({}))).await,
            403,
        );
    }
    for path in [
        format!("/v1/personas/{missing}"),
        format!("/v1/teams/{missing}"),
        "/v1/personas/bad".into(),
        "/v1/teams/bad".into(),
    ] {
        expect(
            &call(
                &app,
                Some(caller),
                "PUT",
                &path,
                Some(json!("not an object")),
            )
            .await,
            404,
        );
    }
    let before = (
        work.personas().expect("personas"),
        work.teams().expect("teams"),
        work.members().expect("members"),
    );
    work.store().rebuild("work.directory").expect("rebuild");
    assert_eq!(
        (
            work.personas().expect("personas"),
            work.teams().expect("teams"),
            work.members().expect("members")
        ),
        before
    );
    expect(&get(&app, caller, "/v1/personas").await, 200);
}

#[tokio::test]
async fn optional_workstream_is_validated_and_appended_with_its_project() {
    let dir = tempfile::tempdir().expect("temp");
    let work = seeded(dir.path());
    let app = app(&work);
    let caller = person(SAM);
    let rev = work.store().latest_rev().expect("rev");
    expect(
        &call(
            &app,
            Some(caller),
            "POST",
            "/v1/projects",
            Some(json!({"key":"ATM","name":"Atomic project","first_workstream":" "})),
        )
        .await,
        400,
    );
    assert_eq!(work.store().latest_rev().expect("rev"), rev);
    let body = json!({"key":"ATM","name":"Atomic project","first_workstream":"First stream","root":{"machine":"01JB000000000000000MCH0001","path":"/home/sam/atomic"}});
    let created = call(
        &app,
        Some(caller),
        "POST",
        "/v1/projects",
        Some(body.clone()),
    )
    .await;
    expect(&created, 201);
    let id = created.1["id"].as_str().expect("id");
    let streams = get(&app, caller, &format!("/v1/workstreams?project={id}")).await;
    assert_eq!(streams.1.as_array().expect("array").len(), 1);
    assert_eq!(streams.1[0]["locations"][0], body["root"]);
    assert_eq!(work.store().latest_rev().expect("rev"), rev + 2);
    expect(
        &call(&app, Some(caller), "POST", "/v1/projects", Some(body)).await,
        409,
    );
    assert_eq!(work.store().latest_rev().expect("rev"), rev + 2);
}

#[tokio::test]
async fn editing_requires_ownership_of_every_linked_member_but_unlinked_recipes_are_editable() {
    use pitcrew_protocol::{
        events::{Event, EventBody},
        ids::{EventId, MemberId, PersonaId},
        model::{Engine, Member, MemberKind, PermissionMode, Persona},
    };
    let dir = tempfile::tempdir().expect("temp");
    let work = seeded(dir.path());
    let app = app(&work);
    let other = MemberId::new();
    let id = PersonaId::new();
    let recipe = Persona {
        id,
        name: "Synthetic shared recipe".into(),
        engine: Engine::Claude,
        model: None,
        instructions: None,
        permission_mode: PermissionMode::Default,
    };
    let own = Member {
        id: MemberId::new(),
        kind: MemberKind::Agent,
        handle: "@shared-own".into(),
        name: recipe.name.clone(),
        owner: Some(person(SAM).member),
        persona: Some(id),
    };
    let foreign = Member {
        id: MemberId::new(),
        kind: MemberKind::Agent,
        handle: "@shared-other".into(),
        name: recipe.name.clone(),
        owner: Some(other),
        persona: Some(id),
    };
    let append = |bodies: Vec<EventBody>| {
        let events: Vec<_> = bodies
            .into_iter()
            .map(|body| Event {
                id: EventId::new(),
                at: 1790900000000,
                workspace: work.workspace(),
                author: person(SAM).member,
                on_behalf_of: None,
                body,
            })
            .collect();
        work.store().append(&events).expect("append");
    };
    append(vec![EventBody::PersonaSaved {
        persona: recipe.clone(),
    }]);
    expect(
        &call(
            &app,
            Some(person(SAM)),
            "PUT",
            &format!("/v1/personas/{id}"),
            Some(json!({"name":"Unlinked edit","engine":"claude"})),
        )
        .await,
        200,
    );
    append(vec![
        EventBody::MemberAdded {
            member: own.clone(),
        },
        EventBody::MemberAdded {
            member: foreign.clone(),
        },
    ]);
    let rev = work.store().latest_rev().expect("rev");
    expect(
        &call(
            &app,
            Some(person(SAM)),
            "PUT",
            &format!("/v1/personas/{id}"),
            Some(json!({"name":"Stolen","engine":"claude"})),
        )
        .await,
        403,
    );
    assert_eq!(work.store().latest_rev().expect("rev"), rev);
    assert_eq!(
        work.members()
            .expect("members")
            .into_iter()
            .find(|m| m.id == foreign.id)
            .expect("foreign")
            .name,
        foreign.name
    );
    let caller = person(SAM);
    let created = call(
        &app,
        Some(caller),
        "POST",
        "/v1/personas",
        Some(json!({"name":"Synthetic one","engine":"opencode"})),
    )
    .await;
    let second = call(
        &app,
        Some(caller),
        "POST",
        "/v1/personas",
        Some(json!({"name":"Synthetic two","engine":"opencode"})),
    )
    .await;
    for (recipe, handle) in [(created.1, "@opencode"), (second.1, "@opencode-2")] {
        assert_eq!(
            work.members()
                .expect("members")
                .iter()
                .find(|m| m.persona.map(|p| p.0.to_string())
                    == recipe["id"].as_str().map(str::to_owned))
                .expect("member")
                .handle,
            handle
        );
    }
}
