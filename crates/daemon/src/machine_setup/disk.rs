//! Free space on the filesystem that holds a folder (PitCrew's state directory): what the person
//! may still write there, as `df` shows it.
//!
//! - **Unix:** `statvfs`, the blocks available to an unprivileged user times the fragment size.
//! - **Windows:** `System.IO.DriveInfo` for the folder's drive, through Windows PowerShell (by its
//!   path under `%SystemRoot%`, never from `PATH`). The folder goes to it in an environment
//!   variable, never in the script's text. Without unsafe code, the daemon has no other way to ask.

use std::path::Path;
#[cfg(windows)]
use std::time::Duration;

/// Bytes free for this user on the filesystem holding `folder`, or why they could not be read.
pub async fn free_bytes(folder: &Path) -> Result<u64, String> {
    if !folder.is_dir() {
        return Err(format!("{} is not a folder", folder.display()));
    }
    #[cfg(unix)]
    {
        let folder = folder.to_path_buf();
        tokio::task::spawn_blocking(move || {
            let stat = rustix::fs::statvfs(&folder).map_err(|e| e.to_string())?;
            Ok(stat.f_bavail.saturating_mul(stat.f_frsize))
        })
        .await
        .map_err(|e| e.to_string())?
    }
    #[cfg(windows)]
    {
        windows_free(folder).await
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = folder;
        Err("this platform cannot be asked".to_owned())
    }
}

#[cfg(windows)]
async fn windows_free(folder: &Path) -> Result<u64, String> {
    let root = std::env::var_os("SystemRoot").ok_or("SystemRoot is not set")?;
    let powershell = Path::new(&root)
        .join("System32")
        .join("WindowsPowerShell")
        .join("v1.0")
        .join("powershell.exe");
    let script = "[System.IO.DriveInfo]::new([System.IO.Path]::GetPathRoot($env:PITCREW_DISK_FOLDER)).AvailableFreeSpace";
    let mut command = tokio::process::Command::new(powershell);
    command
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .env("PITCREW_DISK_FOLDER", folder)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .creation_flags(0x0800_0000);
    let output = tokio::time::timeout(Duration::from_secs(20), command.output())
        .await
        .map_err(|_| "PowerShell did not answer in time".to_owned())?
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err("PowerShell could not read the drive".to_owned());
    }
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse()
        .map_err(|_| "PowerShell's answer was not a number".to_owned())
}

/// `bytes` for people: `12.3 GB`, `850 MB`, `0 MB` (decimal units, as file managers show).
pub fn human(bytes: u64) -> String {
    const GB: u64 = 1_000_000_000;
    const MB: u64 = 1_000_000;
    if bytes >= 100 * GB {
        format!("{} GB", bytes / GB)
    } else if bytes >= GB {
        let tenths = bytes / (GB / 10);
        format!("{}.{} GB", tenths / 10, tenths % 10)
    } else {
        format!("{} MB", bytes / MB)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_read_as_people_expect() {
        assert_eq!(human(0), "0 MB");
        assert_eq!(human(850_400_000), "850 MB");
        assert_eq!(human(1_000_000_000), "1.0 GB");
        assert_eq!(human(12_345_000_000), "12.3 GB");
        assert_eq!(human(250_000_000_000), "250 GB");
    }

    #[tokio::test]
    async fn this_folder_has_some_free_space() {
        let tmp = tempfile::tempdir().unwrap();
        let free = free_bytes(tmp.path()).await;
        assert!(free.is_ok(), "{free:?}");
        assert!(free_bytes(&tmp.path().join("missing")).await.is_err());
    }
}
