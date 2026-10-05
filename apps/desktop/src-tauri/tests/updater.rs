//! The install boundary verifies minisign with a newly generated temporary key.
use base64::{Engine as _, engine::general_purpose::STANDARD};
use minisign::KeyPair;
use pitcrew_desktop::updater::verify_artifact;

#[test]
fn signed_artifact_accepts_original_and_refuses_tampering() -> Result<(), Box<dyn std::error::Error>>
{
    let temp = tempfile::tempdir()?;
    let KeyPair { pk, sk } = KeyPair::generate_unencrypted_keypair()?;
    std::fs::write(temp.path().join("throwaway.key"), sk.to_bytes())?;
    let original = b"synthetic updater artifact";
    let signature = minisign::sign(
        Some(&pk),
        &sk,
        &original[..],
        Some("timestamp:0\tversion:1.2.3"),
        None,
    )?;
    let signature = STANDARD.encode(signature.to_bytes());
    let pubkey = STANDARD.encode(pk.to_box()?.to_bytes());
    assert!(verify_artifact(original, &signature, &pubkey, "1.2.3").is_ok());
    assert!(verify_artifact(b"tampered artifact", &signature, &pubkey, "1.2.3").is_err());
    let other = KeyPair::generate_unencrypted_keypair()?;
    assert!(
        verify_artifact(
            original,
            &signature,
            &STANDARD.encode(other.pk.to_box()?.to_bytes()),
            "1.2.3"
        )
        .is_err()
    );
    assert!(verify_artifact(original, "bad signature", &pubkey, "1.2.3").is_err());
    assert!(verify_artifact(original, &signature, "", "1.2.3").is_err());
    assert!(verify_artifact(original, &signature, &pubkey, "2.0.0").is_err());
    let unversioned = minisign::sign(Some(&pk), &sk, &original[..], None, None)?;
    assert!(
        verify_artifact(
            original,
            &STANDARD.encode(unversioned.to_bytes()),
            &pubkey,
            "1.2.3"
        )
        .is_err()
    );
    Ok(())
}

#[test]
fn tauri_feed_parser_requires_signature_and_semantic_version()
-> Result<(), Box<dyn std::error::Error>> {
    let valid = serde_json::json!({ "version": "1.2.3-beta.1", "platforms": {
        "windows-x86_64": { "signature": "synthetic", "url": "https://example.com/update.exe" }
    }});
    let release: tauri_plugin_updater::RemoteRelease = serde_json::from_value(valid.clone())?;
    assert_eq!(release.version.to_string(), "1.2.3-beta.1");
    assert_eq!(release.signature("windows-x86_64")?, "synthetic");
    assert!(release.download_url("linux-x86_64").is_err());
    let mut unsigned = valid.clone();
    unsigned["platforms"]["windows-x86_64"]
        .as_object_mut()
        .ok_or("Missing platform")?
        .remove("signature");
    assert!(serde_json::from_value::<tauri_plugin_updater::RemoteRelease>(unsigned).is_err());
    let mut malformed = valid;
    malformed["version"] = "bad version".into();
    assert!(serde_json::from_value::<tauri_plugin_updater::RemoteRelease>(malformed).is_err());
    Ok(())
}
