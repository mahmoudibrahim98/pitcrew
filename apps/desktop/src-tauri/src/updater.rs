//! Signed desktop updates, held in Rust until the person accepts the displayed version.
use crate::{app::MAIN, shell::Shell};
use semver::Version;
use serde::{Deserialize, Serialize};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;
use tauri::{AppHandle, Emitter as _, Manager as _, Runtime};
use tauri_plugin_updater::{Update, UpdaterExt as _};

const REPO: &str = "https://github.com/mahmoudibrahim98/pitcrew";
const API: &str = "https://api.github.com/repos/mahmoudibrahim98/pitcrew/releases?per_page=100";
const DAILY: Duration = Duration::from_secs(24 * 60 * 60);

/// Serializes checks, channel changes and installation; the UI holds no update resource.
#[derive(Default)]
pub struct Updates {
    gate: tokio::sync::Mutex<()>,
    pending: Mutex<Option<Update>>,
}
impl std::fmt::Debug for Updates {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Updates").finish_non_exhaustive()
    }
}
impl Updates {
    fn pending(&self) -> MutexGuard<'_, Option<Update>> {
        self.pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// The only update information sent to the main window.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    enabled: bool,
    prereleases: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    notes_url: Option<String>,
}
fn public_key<R: Runtime>(app: &AppHandle<R>) -> &str {
    app.config()
        .plugins
        .0
        .get("updater")
        .and_then(|v| v.get("pubkey"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
}
fn enabled<R: Runtime>(app: &AppHandle<R>) -> bool {
    let key = public_key(app);
    !key.is_empty() && (!cfg!(target_os = "linux") || std::env::var_os("APPIMAGE").is_some())
}
fn status<R: Runtime>(app: &AppHandle<R>) -> Status {
    let pending = app.state::<Updates>();
    let version = pending.pending().as_ref().map(|u| u.version.clone());
    Status {
        enabled: enabled(app),
        prereleases: app.state::<Shell>().preferences.get().update_prereleases,
        notes_url: version
            .as_ref()
            .map(|v| format!("{REPO}/releases/tag/v{v}")),
        version,
    }
}
fn main_only<R: Runtime>(window: &tauri::WebviewWindow<R>) -> Result<(), String> {
    if window.label() == MAIN {
        Ok(())
    } else {
        Err("Updates are available only in the main window".into())
    }
}
fn newer(current: &Version, candidate: &Version, prereleases: bool) -> bool {
    candidate.cmp_precedence(current).is_gt() && (prereleases || candidate.pre.is_empty())
}

#[derive(Debug, Deserialize)]
struct Release {
    tag_name: String,
    draft: bool,
    assets: Vec<Asset>,
}
#[derive(Debug, Deserialize)]
struct Asset {
    name: String,
}
fn select_release(text: &[u8], current: &Version) -> Result<Option<Version>, String> {
    let releases: Vec<Release> =
        serde_json::from_slice(text).map_err(|_| "Invalid release list")?;
    Ok(releases
        .into_iter()
        .filter(|r| !r.draft && r.assets.iter().any(|a| a.name == "latest.json"))
        .filter_map(|r| Version::parse(r.tag_name.strip_prefix('v')?).ok())
        .filter(|v| newer(current, v, true))
        .max())
}
async fn check<R: Runtime>(app: &AppHandle<R>) -> Result<Status, String> {
    let state = app.state::<Updates>();
    let _guard = state
        .gate
        .try_lock()
        .map_err(|_| "An update operation is already running")?;
    check_locked(app).await
}
async fn check_locked<R: Runtime>(app: &AppHandle<R>) -> Result<Status, String> {
    if !enabled(app) {
        return Ok(status(app));
    }
    let prereleases = app.state::<Shell>().preferences.get().update_prereleases;
    let mut endpoint = format!("{REPO}/releases/latest/download/latest.json");
    if prereleases {
        let client = reqwest::Client::builder()
            .user_agent("PitCrew updater")
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|_| "Cannot check releases")?;
        let mut response = client
            .get(API)
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .map_err(|_| "Cannot reach GitHub releases")?;
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| "Cannot read releases")? {
            if bytes.len() + chunk.len() > 2 * 1024 * 1024 {
                return Err("Release list is too large".into());
            }
            bytes.extend_from_slice(&chunk);
        }
        // package_info is the compiled Tauri version, including the release build's override.
        let Some(version) = select_release(&bytes, &app.package_info().version)? else {
            *app.state::<Updates>().pending() = None;
            return publish(app);
        };
        endpoint = format!("{REPO}/releases/download/v{version}/latest.json");
    }
    let endpoint = endpoint.parse().map_err(|_| "Invalid update endpoint")?;
    let update = app
        .updater_builder()
        .endpoints(vec![endpoint])
        .map_err(|_| "Invalid update endpoint")?
        .timeout(Duration::from_secs(60))
        .version_comparator(move |current, release| newer(&current, &release.version, prereleases))
        .build()
        .map_err(|_| "Cannot configure updater")?
        .check()
        .await
        .map_err(|_| "Cannot check for updates")?;
    if let Some(u) = &update {
        // The feed cannot redirect an install to an arbitrary origin or version.
        let prefix = format!("{REPO}/releases/download/v{}/", u.version);
        if !u.download_url.as_str().starts_with(&prefix) || u.signature.is_empty() {
            return Err("Invalid signed update feed".into());
        }
    }
    *app.state::<Updates>().pending() = update;
    publish(app)
}
fn publish<R: Runtime>(app: &AppHandle<R>) -> Result<Status, String> {
    let result = status(app);
    app.emit_to(MAIN, "gateway://update", &result)
        .map_err(|_| "Cannot report update status")?;
    Ok(result)
}

/// Starts the check loop; it never downloads or installs anything.
pub fn start<R: Runtime>(app: &AppHandle<R>) {
    app.manage(Updates::default());
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            if check(&app).await.is_err() {
                tracing::debug!("update check unavailable");
            }
            tokio::time::sleep(DAILY).await;
        }
    });
}

#[tauri::command]
pub fn gateway_update_status<R: Runtime>(
    window: tauri::WebviewWindow<R>,
    app: AppHandle<R>,
) -> Result<Status, String> {
    main_only(&window)?;
    Ok(status(&app))
}
#[tauri::command]
pub async fn gateway_update_check<R: Runtime>(
    window: tauri::WebviewWindow<R>,
    app: AppHandle<R>,
) -> Result<Status, String> {
    main_only(&window)?;
    check(&app).await
}
#[tauri::command]
pub async fn gateway_update_channel<R: Runtime>(
    window: tauri::WebviewWindow<R>,
    app: AppHandle<R>,
    prereleases: bool,
) -> Result<Status, String> {
    main_only(&window)?;
    let state = app.state::<Updates>();
    let _guard = state
        .gate
        .try_lock()
        .map_err(|_| "An update operation is already running")?;
    app.state::<Shell>()
        .preferences
        .update(|p| p.update_prereleases = prereleases)
        .map_err(|_| "Cannot save update preferences")?;
    *state.pending() = None;
    publish(&app)?;
    check_locked(&app).await
}
#[tauri::command]
pub async fn gateway_update_install<R: Runtime>(
    window: tauri::WebviewWindow<R>,
    app: AppHandle<R>,
    version: String,
) -> Result<(), String> {
    main_only(&window)?;
    let state = app.state::<Updates>();
    let _guard = state
        .gate
        .try_lock()
        .map_err(|_| "An update operation is already running")?;
    let update = state
        .pending()
        .as_ref()
        .filter(|u| u.version == version)
        .cloned()
        .ok_or("This update is no longer available; check again")?;
    if !enabled(&app) {
        return Err("Updates are disabled".into());
    }
    // download() performs mandatory signature verification; install() is never called on error.
    let bytes = update
        .download(|_, _| {}, || {})
        .await
        .map_err(|_| "Update download or signature verification failed")?;
    verify_artifact(&bytes, &update.signature, public_key(&app), &update.version)?;
    update
        .install(bytes)
        .map_err(|_| "Update installation failed")?;
    app.restart();
}

/// Verifies the bytes at the installation boundary using Tauri's minisign implementation.
///
/// # Errors
/// The signature, its encoding, or the compiled public key is invalid.
pub fn verify_artifact(
    bytes: &[u8],
    signature: &str,
    public_key: &str,
    version: &str,
) -> Result<(), String> {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    let decode = |value: &str| -> Result<String, String> {
        let bytes = STANDARD
            .decode(value)
            .map_err(|_| "Invalid update signature encoding")?;
        String::from_utf8(bytes).map_err(|_| "Invalid update signature encoding".into())
    };
    let key = minisign_verify::PublicKey::decode(&decode(public_key)?)
        .map_err(|_| "Invalid updater public key")?;
    let signature = minisign_verify::Signature::decode(&decode(signature)?)
        .map_err(|_| "Invalid update signature")?;
    key.verify(bytes, &signature, true)
        .map_err(|_| "Update signature verification failed")?;
    // Only after verification: the trusted comment is authenticated by minisign's global signature.
    let signed = signature
        .trusted_comment()
        .split('\t')
        .find_map(|field| field.strip_prefix("version:"));
    if signed != Some(version) {
        return Err("Update signature does not match the offered version".into());
    }
    Ok(())
}
#[tauri::command]
pub fn gateway_update_notes<R: Runtime>(
    window: tauri::WebviewWindow<R>,
    app: AppHandle<R>,
    version: String,
) -> Result<(), String> {
    main_only(&window)?;
    let state = app.state::<Updates>();
    let pending = state.pending();
    let update = pending
        .as_ref()
        .filter(|u| u.version == version)
        .ok_or("Update no longer available")?;
    let url = format!("{REPO}/releases/tag/v{}", update.version);
    #[cfg(windows)]
    let mut command = {
        let mut c = std::process::Command::new("rundll32.exe");
        c.arg("url.dll,FileProtocolHandler");
        c
    };
    #[cfg(target_os = "macos")]
    let mut command = std::process::Command::new("/usr/bin/open");
    #[cfg(target_os = "linux")]
    let mut command = std::process::Command::new("xdg-open");
    command
        .arg(url)
        .spawn()
        .map_err(|_| "Cannot open release notes")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn comparison_refuses_downgrades_equal_and_unwanted_prereleases() {
        let current = Version::parse("1.2.3").unwrap();
        for (version, opted, expected) in [
            ("1.2.3", true, false),
            ("1.2.2", true, false),
            ("1.3.0-beta.1", false, false),
            ("1.3.0-beta.1", true, true),
            ("1.3.0", false, true),
            ("1.2.3+build.2", true, false),
        ] {
            assert_eq!(
                newer(&current, &Version::parse(version).unwrap(), opted),
                expected
            );
        }
    }
    #[test]
    fn release_list_ignores_drafts_missing_feeds_and_bad_tags() {
        let text = br#"[
            {"tag_name":"v9.0.0","draft":true,"prerelease":false,"assets":[{"name":"latest.json"}]},
            {"tag_name":"v8.0.0","draft":false,"prerelease":false,"assets":[]},
            {"tag_name":"v2.0.0-beta.1","draft":false,"prerelease":true,"assets":[{"name":"latest.json"}]},
            {"tag_name":"../evil","draft":false,"prerelease":false,"assets":[{"name":"latest.json"}]}
        ]"#;
        assert_eq!(
            select_release(text, &Version::parse("1.0.0").unwrap()).unwrap(),
            Some(Version::parse("2.0.0-beta.1").unwrap())
        );
        assert!(select_release(b"{}", &Version::parse("1.0.0").unwrap()).is_err());
    }
}
