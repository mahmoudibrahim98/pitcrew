//! Signed desktop updates, held in Rust until the person accepts the displayed version.
//!
//! A portable copy ([`crate::portable`]) checks too, with or without a compiled public key, but
//! never downloads or installs anything: accepting an update opens the page with the newest
//! portable zip ([`PORTABLE_DOWNLOADS`]) instead.
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
/// Where a portable copy's update comes from: the portable workflow's successful runs for pushes
/// to `main`, each with `pitcrew-windows-x64-portable.zip` as its artifact. `event:push` matters:
/// `branch:` alone also matches a pull request whose head branch is named `main`, a fork's
/// included.
pub const PORTABLE_DOWNLOADS: &str = "https://github.com/mahmoudibrahim98/pitcrew/actions/workflows/release-portable.yml?query=branch%3Amain+event%3Apush+is%3Asuccess";

/// Serializes checks, channel changes and installation; the UI holds no update resource.
#[derive(Default)]
pub struct Updates {
    gate: tokio::sync::Mutex<()>,
    pending: Mutex<Option<Update>>,
    /// A portable copy: it never installs an update.
    portable: bool,
}
impl std::fmt::Debug for Updates {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Updates").finish_non_exhaustive()
    }
}
impl Updates {
    /// No pending update yet; `portable` for a portable copy.
    #[must_use]
    pub fn new(portable: bool) -> Self {
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
    /// A portable copy's pending update: where the newest portable zip is.
    #[serde(skip_serializing_if = "Option::is_none")]
    download_url: Option<String>,
}
impl Status {
    fn new(enabled: bool, prereleases: bool, portable: bool, version: Option<String>) -> Self {
        Self {
            enabled,
            prereleases,
            portable,
            notes_url: version
                .as_ref()
                .map(|v| format!("{REPO}/releases/tag/v{v}")),
            download_url: version
                .as_ref()
                .filter(|_| portable)
                .map(|_| PORTABLE_DOWNLOADS.to_owned()),
            version,
        }
    }
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
/// Whether checks run: a portable copy always checks (it only shows a version and a fixed
/// GitHub page); an installed one needs the compiled public key, and on Linux an AppImage.
fn checks(portable: bool, public_key: &str, appimage: bool) -> bool {
    portable || (!public_key.is_empty() && (!cfg!(target_os = "linux") || appimage))
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
    let version = updates.pending().as_ref().map(|u| u.version.clone());
    Status::new(
        enabled(app),
        app.state::<Shell>().preferences.get().update_prereleases,
        updates.portable,
        version,
    )
}
/// What accepting an update says in a portable copy, once the download page is open.
fn portable_answer(version: &str) -> String {
    format!(
        "PitCrew {version} is out. This portable copy does not install updates: the page with \
         the newest portable zip is open in your browser. Download it, unzip it into a new \
         folder and start PitCrew from there; your data stays where it is."
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
    app.manage(Updates::new(crate::portable::here()));
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
    if state.portable {
        open_in_browser(PORTABLE_DOWNLOADS)?;
        return Err(portable_answer(&update.version));
    }
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
    open_in_browser(&url).map_err(|_| "Cannot open release notes".to_owned())
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
    fn a_portable_copy_checks_without_a_key_and_an_installed_one_needs_it() {
        assert!(checks(true, "", false));
        assert!(checks(true, "key", true));
        assert!(!checks(false, "", true));
        assert_eq!(checks(false, "key", false), !cfg!(target_os = "linux"));
        assert!(checks(false, "key", true));
    }
    #[test]
    fn a_portable_status_links_the_newest_portable_zip_and_never_offers_an_install_url() {
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
                "downloadUrl": PORTABLE_DOWNLOADS,
            })
        );
        assert!(PORTABLE_DOWNLOADS.starts_with(&format!("{REPO}/actions/workflows/")));
        // Only pushes to main: a pull request from a fork's `main` must never be offered.
        assert!(
            PORTABLE_DOWNLOADS.ends_with("?query=branch%3Amain+event%3Apush+is%3Asuccess"),
            "{PORTABLE_DOWNLOADS}"
        );
        // Nothing pending: no links at all.
        assert_eq!(
            serde_json::to_value(Status::new(true, true, true, None)).unwrap(),
            serde_json::json!({ "enabled": true, "prereleases": true, "portable": true })
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
        assert!(Updates::new(true).portable && !Updates::default().portable);
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
