//! Stored events remain replayable while requests reject unknown fields.
use pitcrew_protocol::model::PermissionMode;
use pitcrew_protocol::onboarding::{SafetySettings, SaveSafety};
use serde_json::json;

#[test]
fn safety_wire_reuses_permission_mode_and_separates_request_strictness() {
    let value = json!({
        "permission_mode": "accept_edits", "back_office_enabled": true,
        "back_office_caps": {"max_auto_accept_per_hour": 10}
    });
    let settings: SafetySettings = serde_json::from_value(value.clone()).expect("settings");
    assert_eq!(settings.permission_mode, PermissionMode::AcceptEdits);
    assert_eq!(serde_json::to_value(settings).expect("serialize"), value);
    let mut future = value;
    future["future_policy"] = json!(true);
    future["back_office_caps"]["future_budget"] = json!(2);
    assert!(serde_json::from_value::<SafetySettings>(future.clone()).is_ok());
    assert!(serde_json::from_value::<SaveSafety>(future.clone()).is_err());
    future
        .as_object_mut()
        .expect("object")
        .remove("future_policy");
    assert!(serde_json::from_value::<SaveSafety>(future).is_err());
}
