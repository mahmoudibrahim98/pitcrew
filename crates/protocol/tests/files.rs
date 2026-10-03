use pitcrew_protocol::{api::ErrorCode, files::WriteFile};
use serde_json::json;

#[test]
fn revision_is_required_but_null_is_exclusive_creation() -> Result<(), serde_json::Error> {
    assert!(
        serde_json::from_value::<WriteFile>(json!({"encoding":"utf8", "content":"new"})).is_err()
    );
    let write: WriteFile =
        serde_json::from_value(json!({"revision":null, "encoding":"utf8", "content":"new"}))?;
    assert_eq!(write.revision, None);
    assert_eq!(
        serde_json::to_value(&write)?["revision"],
        serde_json::Value::Null
    );
    assert_eq!(ErrorCode::TooLarge.http_status(), 413);
    assert_eq!(ErrorCode::Unsupported.http_status(), 501);
    Ok(())
}
