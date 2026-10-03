//! Local WSL distribution discovery, using the existing bounded process runner.

use crate::{Limits, Ssh, SshError};
use serde::Serialize;
use std::path::PathBuf;
use std::time::Duration;

/// A distribution reported by `wsl.exe --list --verbose`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct WslDistro {
    /// The exact registered name, including spaces.
    pub name: String,
    /// Whether this is the default distribution.
    pub default: bool,
    /// Whether it is currently running.
    pub running: bool,
    /// WSL generation; only 2 is supported for connections.
    pub version: u32,
}

/// Missing WSL is an ordinary unavailable result.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct WslDistros {
    /// Whether WSL can list distributions.
    pub available: bool,
    /// Registered distributions in their reported order.
    pub distros: Vec<WslDistro>,
}

/// Configurable executable; tests can substitute their own stand-in.
#[derive(Clone, Debug)]
pub struct Wsl {
    program: PathBuf,
}

impl Default for Wsl {
    fn default() -> Self {
        Self::new("wsl.exe")
    }
}

impl Wsl {
    /// Uses this executable without invoking a shell on Windows.
    #[must_use]
    pub fn new(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
        }
    }

    /// Lists without starting or changing any distribution.
    ///
    /// # Errors
    /// Output limits, timeouts, I/O failures other than a missing executable, or malformed output.
    pub async fn distros(&self) -> Result<WslDistros, SshError> {
        let mut command = Ssh::new(&self.program).minimal_env().command();
        command.args(["--list", "--verbose"]);
        let limits = Limits {
            max_output: Some(1024 * 1024),
            timeout: Some(Duration::from_secs(15)),
        };
        let result = crate::ssh::drive(command, None, limits, None).await;
        match result {
            Err(SshError::Spawn(e)) if e.kind() == std::io::ErrorKind::NotFound => {
                Ok(unavailable())
            }
            Ok((status, _, _)) if !status.success() => Ok(unavailable()),
            Ok((_, stdout, _)) => Ok(WslDistros {
                available: true,
                distros: {
                    let mut distros = parse_distros(&stdout)?;
                    // State labels are localized; the quiet running list contains names only.
                    let mut command = Ssh::new(&self.program).minimal_env().command();
                    command.args(["--list", "--running", "--quiet"]);
                    let (status, running, stderr) =
                        crate::ssh::drive(command, None, limits, None).await?;
                    if !status.success() {
                        return Err(SshError::Ssh {
                            code: status.code().unwrap_or(-1),
                            stderr: String::from_utf8_lossy(&stderr).into_owned(),
                        });
                    }
                    let running = decode(&running)?;
                    for distro in &mut distros {
                        distro.running = running
                            .trim_start_matches('\u{feff}')
                            .lines()
                            .any(|name| name.trim_end_matches('\r') == distro.name);
                    }
                    distros
                },
            }),
            Err(e) => Err(e),
        }
    }
}

fn unavailable() -> WslDistros {
    WslDistros {
        available: false,
        distros: Vec::new(),
    }
}

pub(crate) fn validate_distro(name: &str) -> Result<(), SshError> {
    if name.is_empty()
        || name.len() > 256
        || name.starts_with('-')
        || name.trim() != name
        || name.chars().any(char::is_control)
    {
        Err(SshError::InvalidArgument(
            "invalid WSL distribution name".into(),
        ))
    } else {
        Ok(())
    }
}

/// Parses UTF-16LE (with or without a BOM), preserving spaces and quotes in names.
///
/// # Errors
/// Truncated UTF-16, invalid Unicode, or a malformed table row.
pub fn parse_distros(bytes: &[u8]) -> Result<Vec<WslDistro>, SshError> {
    let invalid = || SshError::InvalidArgument("malformed WSL distribution list".into());
    let text = decode(bytes)?;
    let mut rows = Vec::new();
    for line in text
        .trim_start_matches('\u{feff}')
        .lines()
        .filter(|line| !line.trim().is_empty())
        .skip(1)
    {
        let line = line.trim();
        let default = line.starts_with('*');
        let line = line.strip_prefix('*').unwrap_or(line).trim_start();
        let version_at = line.rfind(char::is_whitespace).ok_or_else(invalid)?;
        let version = line[version_at..]
            .trim()
            .parse::<u32>()
            .map_err(|_| invalid())?;
        let rest = line[..version_at].trim_end();
        let state_at = rest
            .rfind("  ")
            .or_else(|| rest.rfind('\t'))
            .ok_or_else(invalid)?;
        let state = rest[state_at..].trim();
        let name = rest[..state_at].trim_end();
        validate_distro(name)?;
        rows.push(WslDistro {
            name: name.to_owned(),
            default,
            running: state.eq_ignore_ascii_case("running"),
            version,
        });
    }
    Ok(rows)
}

fn decode(bytes: &[u8]) -> Result<String, SshError> {
    let invalid = || SshError::InvalidArgument("malformed WSL distribution list".into());
    if !bytes.len().is_multiple_of(2) {
        return Err(invalid());
    }
    let words: Vec<u16> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
        .collect();
    String::from_utf16(&words).map_err(|_| invalid())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf16_names_default_stopped_and_bom() {
        let text = "\u{feff}  NAME                 STATE           VERSION\r\n* Lab 'quoted' distro   Running         2\r\n  Other distro          Stopped         1\r\n";
        let bytes: Vec<u8> = text.encode_utf16().flat_map(u16::to_le_bytes).collect();
        let rows = parse_distros(&bytes).unwrap();
        assert_eq!(
            rows[0],
            WslDistro {
                name: "Lab 'quoted' distro".into(),
                default: true,
                running: true,
                version: 2
            }
        );
        assert_eq!(
            rows[1],
            WslDistro {
                name: "Other distro".into(),
                default: false,
                running: false,
                version: 1
            }
        );
        assert!(parse_distros(&bytes[1..]).is_err());
        assert!(validate_distro("-option").is_err());
    }

    #[test]
    fn localized_multiword_states_and_invalid_utf16() {
        let text = "NOM                    ETAT                    VERSION\n  Lab  distro          En cours d’exécution    2\n";
        let bytes: Vec<u8> = text.encode_utf16().flat_map(u16::to_le_bytes).collect();
        assert_eq!(parse_distros(&bytes).unwrap()[0].name, "Lab  distro");
        assert!(parse_distros(&[0, 0xd8]).is_err());
        let text = "NAME STATE VERSION\n  Lab  Running  broken\n";
        let bytes: Vec<u8> = text.encode_utf16().flat_map(u16::to_le_bytes).collect();
        assert!(parse_distros(&bytes).is_err());
    }
}
