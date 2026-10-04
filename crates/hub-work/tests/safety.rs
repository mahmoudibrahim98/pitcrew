mod common;
use common::*;
use pitcrew_protocol::{events::EventBody, onboarding::SafetySettings};
use serde_json::json;

#[tokio::test]
async fn safety_is_validated_authored_idempotent_and_rebuilt() {
    let temp = tempfile::tempdir().unwrap();
    let work = seeded(temp.path());
    let router = app(&work);
    let unsaved = get(&router, person(SAM), "/v1/safety").await;
    assert_eq!(unsaved.1["saved"], false);
    let settings = json!({"permission_mode":"plan", "back_office_enabled":true, "back_office_caps":{"max_auto_accept_per_hour":7}});
    assert_eq!(work.safety().unwrap(), SafetySettings::default());
    for method in ["GET", "PUT"] {
        assert_eq!(
            call(
                &router,
                Some(agent(WRITER)),
                method,
                "/v1/safety",
                Some(settings.clone())
            )
            .await
            .0,
            403
        );
    }
    for cap in [json!(-1), json!(101), json!(1.5), json!("7")] {
        let mut invalid = settings.clone();
        invalid["back_office_caps"]["max_auto_accept_per_hour"] = cap;
        assert_eq!(
            call(
                &router,
                Some(person(SAM)),
                "PUT",
                "/v1/safety",
                Some(invalid)
            )
            .await
            .0,
            400
        );
    }
    assert_eq!(
        call(
            &router,
            Some(person(SAM)),
            "PUT",
            "/v1/safety",
            Some(settings.clone())
        )
        .await,
        (200, settings.clone())
    );
    let mut bypass = settings.clone();
    bypass["permission_mode"] = json!("bypass_permissions");
    let rejected = call(
        &router,
        Some(person(SAM)),
        "PUT",
        "/v1/safety",
        Some(bypass),
    )
    .await;
    assert_eq!(rejected.0, 400);
    assert!(
        rejected.1["message"]
            .as_str()
            .unwrap()
            .contains("runner disallows")
    );
    let rev = work.store().latest_rev().unwrap();
    assert_eq!(
        call(
            &router,
            Some(person(SAM)),
            "PUT",
            "/v1/safety",
            Some(settings.clone())
        )
        .await
        .0,
        200
    );
    assert_eq!(work.store().latest_rev().unwrap(), rev);
    work.store().rebuild("work.safety").unwrap();
    assert_eq!(
        serde_json::to_value(work.safety().unwrap()).unwrap(),
        settings
    );
    let event = work.store().since(rev - 1, 1).unwrap().pop().unwrap();
    assert_eq!(event.event.author, person(SAM).member);
    assert!(matches!(event.event.body, EventBody::SafetyChanged { .. }));
}
