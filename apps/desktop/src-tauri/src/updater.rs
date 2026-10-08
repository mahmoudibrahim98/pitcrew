//! Signed desktop updates, held in Rust until the person accepts the displayed version.
//!
//! A portable copy ([`crate::portable`]) never downloads or installs anything. One built from a
//! release tag (its marker's release channel) checks GitHub's published releases for a newer one
//! that carries the portable zip ([`PORTABLE_ASSET`]), with or without a compiled public key, and
//! accepting it opens that release's page, where the zip is. A development build (from `main` or
//! a pull request) offers nothing.
use crate::portable::Channel;
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
/// The portable zip's name among a release's assets (`release.yml` attaches it).
pub const PORTABLE_ASSET: &str = "pitcrew-windows-x64-portable.zip";

/// Serializes checks, channel changes and installation; the UI holds no update resource.
#[derive(Default)]
pub struct Updates {
    gate: tokio::sync::Mutex<()>,
    pending: Mutex<Option<Update>>,
    /// A portable copy's newer release, by version: shown, never installed.
    portable_pending: Mutex<Option<String>>,
    /// A portable copy's channel; `None` when installed.
    portable: Option<Channel>,
}
impl std::fmt::Debug for Updates {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Updates").finish_non_exhaustive()
    }
}
impl Updates {
    /// No pending update yet; `portable` is a portable copy's channel.
    #[must_use]
    pub fn new(portable: Option<Channel>) -> Self {
        Self {
            portable,
            ..Self::default()
        }
    }
    fn pending(&self) -> MutexGuard<'_, Option<Update>> {
        self.pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
    fn portable_pending(&self) -> MutexGuard<'_, Option<String>> {
        self.portable_pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
    /// The version shown to the person, whichever way it was found.
    fn pending_version(&self) -> Option<String> {
        if self.portable.is_some() {
            self.portable_pending().clone()
        } else {
            self.pending().as_ref().map(|u| u.version.clone())
        }
    }
}

/// The only update information sent to the main window.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    enabled: bool,
    prereleases: bool,
    /// A portable copy: an update is shown, never installed.
    portable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    notes_url: Option<String>,
    /// A portable copy's pending update: its release's page, where the portable zip is.
    #[serde(skip_serializing_if = "Option::is_none")]
    download_url: Option<String>,
}
impl Status {
    fn new(enabled: bool, prereleases: bool, portable: bool, version: Option<String>) -> Self {
        Self {
            enabled,
            prereleases,
            portable,
            notes_url: version.as_deref().map(release_page),
            download_url: version.as_deref().filter(|_| portable).map(release_page),
            version,
        }
    }
}
/// A release's fixed GitHub page.
fn release_page(version: &str) -> String {
    format!("{REPO}/releases/tag/v{version}")
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
/// Whether checks run: a release-channel portable copy always checks (it only shows a version
/// and a fixed GitHub page), a development build never; an installed copy needs the compiled
/// public key, and on Linux an AppImage.
fn checks(portable: Option<Channel>, public_key: &str, appimage: bool) -> bool {
    match portable {
        Some(Channel::Release) => true,
        Some(Channel::Development) => false,
        None => !public_key.is_empty() && (!cfg!(target_os = "linux") || appimage),
    }
}
fn enabled<R: Runtime>(app: &AppHandle<R>) -> bool {
    checks(
        app.state::<Updates>().portable,
        public_key(app),
        std::env::var_os("APPIMAGE").is_some(),
    )
}
fn status<R: Runtime>(app: &AppHandle<R>) -> Status {
    let updates = app.state::<Updates>();
    Status::new(
        enabled(app),
        app.state::<Shell>().preferences.get().update_prereleases,
        updates.portable.is_some(),
        updates.pending_version(),
    )
}
/// What accepting an update says in a portable copy, once the release's page is open.
fn portable_answer(version: &str) -> String {
    format!(
        "PitCrew {version} is out. This portable copy does not install updates: its release page \
         is open in your browser. Download {PORTABLE_ASSET} there, quit PitCrew and unzip it over \
         this folder; your data and agent hooks stay as they are."
    )
}
/// Opens `url`, a fixed GitHub page, in the system browser.
fn open_in_browser(url: &str) -> Result<(), String> {
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
        .map_err(|_| "Cannot open the page in the browser")?;
    Ok(())
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
/// The greatest version newer than `current` among the published releases in `text` that carry
/// `asset`; pre-releases only with `prereleases`.
fn select_release(
    text: &[u8],
    current: &Version,
    asset: &str,
    prereleases: bool,
) -> Result<Option<Version>, String> {
    let releases: Vec<Release> =
        serde_json::from_slice(text).map_err(|_| "Invalid release list")?;
    Ok(releases
        .into_iter()
        .filter(|r| !r.draft && r.assets.iter().any(|a| a.name == asset))
        .filter_map(|r| Version::parse(r.tag_name.strip_prefix('v')?).ok())
        .filter(|v| newer(current, v, prereleases))
        .max())
}
/// The latest 100 published releases, as GitHub's API lists them.
async fn fetch_releases() -> Result<Vec<u8>, String> {
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
    Ok(bytes)
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
    if app.state::<Updates>().portable.is_some() {
        // A release-channel portable copy: no signed feed, only a release that carries the zip.
        // No such release, or none newer, is no update.
        let bytes = fetch_releases().await?;
        let found = select_release(
            &bytes,
            &app.package_info().version,
            PORTABLE_ASSET,
            prereleases,
        )?;
        *app.state::<Updates>().portable_pending() = found.map(|v| v.to_string());
        return publish(app);
    }
    let mut endpoint = format!("{REPO}/releases/latest/download/latest.json");
    if prereleases {
        let bytes = fetch_releases().await?;
        // package_info is the compiled Tauri version, including the release build's override.
        let Some(version) =
            select_release(&bytes, &app.package_info().version, "latest.json", true)?
        else {
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
    app.manage(Updates::new(crate::portable::channel()));
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
    *state.portable_pending() = None;
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
    if state.portable.is_some() {
        // Never installed: the release's page opens instead, for the pending version only.
        let version = state
            .pending_version()
            .filter(|pending| *pending == version)
            .ok_or("This update is no longer available; check again")?;
        open_in_browser(&release_page(&version))?;
        return Err(portable_answer(&version));
    }
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
    let version = app
        .state::<Updates>()
        .pending_version()
        .filter(|pending| *pending == version)
        .ok_or("Update no longer available")?;
    open_in_browser(&release_page(&version)).map_err(|_| "Cannot open release notes".to_owned())
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
    fn a_release_portable_copy_checks_without_a_key_a_development_one_never() {
        let release = Some(Channel::Release);
        let development = Some(Channel::Development);
        assert!(checks(release, "", false));
        assert!(checks(release, "key", true));
        assert!(!checks(development, "key", true));
        assert!(!checks(development, "", false));
        assert!(!checks(None, "", true));
        assert_eq!(checks(None, "key", false), !cfg!(target_os = "linux"));
        assert!(checks(None, "key", true));
    }
    #[test]
    fn a_portable_status_links_the_release_page_and_never_an_installer() {
        let portable =
            serde_json::to_value(Status::new(true, false, true, Some("1.2.3".into()))).unwrap();
        assert_eq!(
            portable,
            serde_json::json!({
                "enabled": true,
                "prereleases": false,
                "portable": true,
                "version": "1.2.3",
                "notesUrl": format!("{REPO}/releases/tag/v1.2.3"),
                "downloadUrl": format!("{REPO}/releases/tag/v1.2.3"),
            })
        );
        // A development build: no checks, nothing pending, no links.
        assert_eq!(
            serde_json::to_value(Status::new(false, true, true, None)).unwrap(),
            serde_json::json!({ "enabled": false, "prereleases": true, "portable": true })
        );
        // Installed: the release notes, no download page.
        assert_eq!(
            serde_json::to_value(Status::new(true, false, false, Some("1.2.3".into()))).unwrap(),
            serde_json::json!({
                "enabled": true,
                "prereleases": false,
                "portable": false,
                "version": "1.2.3",
                "notesUrl": format!("{REPO}/releases/tag/v1.2.3"),
            })
        );
        let answer = portable_answer("1.2.3");
        assert!(answer.contains("PitCrew 1.2.3"), "{answer}");
        assert!(answer.contains("does not install updates"), "{answer}");
        assert!(answer.contains("unzip it over this folder"), "{answer}");
        let updates = Updates::new(Some(Channel::Release));
        assert_eq!(updates.portable, Some(Channel::Release));
        assert_eq!(updates.pending_version(), None);
        *updates.portable_pending() = Some("1.2.3".into());
        assert_eq!(updates.pending_version().as_deref(), Some("1.2.3"));
        // An installed copy never reads the portable slot.
        assert_eq!(Updates::default().portable, None);
    }
    #[test]
    fn a_portable_copy_is_offered_only_releases_that_carry_its_zip() {
        let text = br#"[
            {"tag_name":"v3.0.0","draft":true,"assets":[{"name":"pitcrew-windows-x64-portable.zip"}]},
            {"tag_name":"v2.1.0-beta.1","draft":false,"assets":[{"name":"pitcrew-windows-x64-portable.zip"}]},
            {"tag_name":"v2.0.0","draft":false,"assets":[{"name":"latest.json"}]},
            {"tag_name":"v1.5.0","draft":false,"assets":[{"name":"latest.json"},{"name":"pitcrew-windows-x64-portable.zip"}]},
            {"tag_name":"v0.9.0","draft":false,"assets":[{"name":"pitcrew-windows-x64-portable.zip"}]}
        ]"#;
        let current = Version::parse("1.0.0").unwrap();
        let stable = select_release(text, &current, PORTABLE_ASSET, false).unwrap();
        assert_eq!(stable, Some(Version::parse("1.5.0").unwrap()));
        let pre = select_release(text, &current, PORTABLE_ASSET, true).unwrap();
        assert_eq!(pre, Some(Version::parse("2.1.0-beta.1").unwrap()));
        // Nothing newer with the zip, or no release at all: no update.
        let newest = Version::parse("2.1.0").unwrap();
        assert_eq!(
            select_release(text, &newest, PORTABLE_ASSET, true).unwrap(),
            None
        );
        assert_eq!(
            select_release(b"[]", &current, PORTABLE_ASSET, false).unwrap(),
            None
        );
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
            select_release(text, &Version::parse("1.0.0").unwrap(), "latest.json", true).unwrap(),
            Some(Version::parse("2.0.0-beta.1").unwrap())
        );
        assert!(
            select_release(
                b"{}",
                &Version::parse("1.0.0").unwrap(),
                "latest.json",
                true
            )
            .is_err()
        );
    }
}
