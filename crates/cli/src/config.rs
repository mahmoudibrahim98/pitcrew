//! Where the daemon is and which token to send, from the environment.
//!
//! - `PITCREW_SOCKET` (Unix): the daemon's socket (`…/pitcrewd.sock`) or its directory.
//! - `PITCREW_PIPE` (Windows): the daemon's pipe, `\\.\pipe\…`. Without it (and without
//!   `PITCREW_URL`), the default pipe `\\.\pipe\pitcrewd-<user SID>`.
//! - `PITCREW_URL`: loopback TCP, for development only: `http://127.0.0.1:<port>`,
//!   `http://[::1]:<port>` or `http://localhost:<port>`. Any other host is refused.
//! - `PITCREW_TOKEN`, or `PITCREW_TOKEN_FILE`: a file holding the token, which must be private
//!   (on Unix: ours, mode 0600 or stricter, not a symlink).

use crate::error::{Error, Kind, Result};
use std::ffi::OsString;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::Path;
#[cfg(unix)]
use std::path::PathBuf;

/// Reads an environment variable. `main` passes `std::env::var_os`; tests pass their own.
pub type Env<'a> = &'a dyn Fn(&str) -> Option<OsString>;

/// Longest token accepted, in bytes.
const MAX_TOKEN_LEN: usize = 4096;

/// A variable that is set and not empty.
fn var(env: Env<'_>, name: &str) -> Option<OsString> {
    env(name).filter(|v| !v.is_empty())
}

/// A variable that must be UTF-8.
fn var_str(env: Env<'_>, name: &str) -> Result<Option<String>> {
    var(env, name)
        .map(|v| {
            v.into_string()
                .map_err(|_| Error::invalid(format!("{name} is not valid UTF-8")))
        })
        .transpose()
}

/// How to reach the daemon.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Endpoint {
    /// The daemon's socket directory. The socket is `pitcrewd.sock` inside it.
    #[cfg(unix)]
    Unix {
        /// The directory.
        dir: PathBuf,
    },
    /// A local named pipe.
    #[cfg(windows)]
    Pipe {
        /// The full name, `\\.\pipe\…`.
        name: String,
    },
    /// Loopback TCP, for development only.
    Tcp {
        /// Addresses to try, in order.
        addrs: Vec<SocketAddr>,
        /// The `Host` header: the URL's host and port.
        host: String,
    },
}

impl Endpoint {
    /// Reads the endpoint from the environment. The platform's private transport wins over
    /// `PITCREW_URL` when both are set.
    ///
    /// # Errors
    /// `invalid` when nothing usable is set, or `PITCREW_URL` is malformed or not loopback.
    pub fn from_env(env: Env<'_>) -> Result<Self> {
        #[cfg(unix)]
        if let Some(socket) = var(env, "PITCREW_SOCKET") {
            return Ok(Self::unix(Path::new(&socket)));
        }
        #[cfg(windows)]
        if let Some(pipe) = var_str(env, "PITCREW_PIPE")? {
            return Self::pipe(pipe);
        }
        if let Some(url) = var_str(env, "PITCREW_URL")? {
            return Self::dev_url(&url);
        }
        #[cfg(windows)]
        {
            let name = pitcrew_api::default_pipe_name().map_err(|e| {
                Error::internal(format!("cannot work out the default pipe name: {e}"))
            })?;
            Ok(Self::Pipe { name })
        }
        #[cfg(not(windows))]
        {
            Err(Error::invalid(
                "no daemon to talk to: set PITCREW_SOCKET to the daemon's socket (or, for \
                 development, PITCREW_URL to a loopback URL)",
            ))
        }
    }

    /// The socket's directory: the parent of a path ending in `pitcrewd.sock`, else the path.
    #[cfg(unix)]
    #[must_use]
    pub fn unix(path: &Path) -> Self {
        let named_socket = path
            .file_name()
            .is_some_and(|n| n == pitcrew_api::SOCKET_NAME);
        let dir = match path.parent() {
            Some(parent) if named_socket => {
                if parent.as_os_str().is_empty() {
                    PathBuf::from(".")
                } else {
                    parent.to_path_buf()
                }
            }
            _ => path.to_path_buf(),
        };
        Self::Unix { dir }
    }

    /// A local pipe. Remote pipes (`\\server\pipe\…`) are refused: opening one would talk to
    /// another machine.
    ///
    /// # Errors
    /// `invalid` if the name is not `\\.\pipe\…`.
    #[cfg(windows)]
    pub fn pipe(name: String) -> Result<Self> {
        let prefix = r"\\.\pipe\";
        let local = name
            .get(..prefix.len())
            .is_some_and(|p| p.eq_ignore_ascii_case(prefix));
        if local && name.len() > prefix.len() {
            Ok(Self::Pipe { name })
        } else {
            Err(Error::invalid(format!(
                r"PITCREW_PIPE must be a local pipe, \\.\pipe\<name>, not {name}"
            )))
        }
    }

    /// Parses a development URL: `http://` and a loopback host (`127.0.0.1`, `[::1]` or
    /// `localhost`), an optional port, and no path. Host names are never looked up.
    ///
    /// # Errors
    /// `invalid` for anything else.
    pub fn dev_url(url: &str) -> Result<Self> {
        let bad = |why: &str| Error::invalid(format!("PITCREW_URL {url:?}: {why}"));
        let rest = strip_prefix_ignore_case(url, "http://")
            .ok_or_else(|| bad("only http:// is supported; it is for loopback development only"))?;
        let authority = match rest.find('/') {
            Some(i) if &rest[i..] == "/" => &rest[..i],
            Some(_) => return Err(bad("a path is not allowed")),
            None => rest,
        };
        if authority.contains(['@', '?', '#']) {
            return Err(bad("user names, queries and fragments are not allowed"));
        }
        let (host, port) =
            split_host_port(authority).ok_or_else(|| bad("malformed host or port"))?;
        let host_lower = host.to_ascii_lowercase();
        let addrs = match host_lower.as_str() {
            "127.0.0.1" => vec![SocketAddr::from((Ipv4Addr::LOCALHOST, port))],
            "[::1]" => vec![SocketAddr::from((Ipv6Addr::LOCALHOST, port))],
            "localhost" => vec![
                SocketAddr::from((Ipv4Addr::LOCALHOST, port)),
                SocketAddr::from((Ipv6Addr::LOCALHOST, port)),
            ],
            _ => {
                return Err(bad(
                    "the host must be this machine: 127.0.0.1, [::1] or localhost",
                ));
            }
        };
        Ok(Self::Tcp {
            addrs,
            host: authority.to_owned(),
        })
    }

    /// Where it is, for messages.
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            #[cfg(unix)]
            Self::Unix { dir } => dir.join(pitcrew_api::SOCKET_NAME).display().to_string(),
            #[cfg(windows)]
            Self::Pipe { name } => name.clone(),
            Self::Tcp { host, .. } => format!("http://{host}"),
        }
    }

    /// The `Host` header to send.
    #[must_use]
    pub fn host_header(&self) -> &str {
        match self {
            Self::Tcp { host, .. } => host,
            #[allow(unreachable_patterns)]
            _ => "localhost",
        }
    }
}

fn strip_prefix_ignore_case<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    let head = s.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix)
        .then(|| &s[prefix.len()..])
}

/// `host[:port]` or `[v6][:port]`; the port defaults to 80.
fn split_host_port(authority: &str) -> Option<(&str, u16)> {
    let (host, port) = if authority.starts_with('[') {
        let end = authority.find(']')?;
        let (host, rest) = authority.split_at(end + 1);
        if rest.is_empty() {
            (host, None)
        } else {
            (host, Some(rest.strip_prefix(':')?))
        }
    } else {
        match authority.split_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (authority, None),
        }
    };
    if host.is_empty() {
        return None;
    }
    let port = match port {
        None => 80,
        Some(p) if !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) => p.parse().ok()?,
        Some(_) => return None,
    };
    Some((host, port))
}

/// The token: `PITCREW_TOKEN`, else the contents of `PITCREW_TOKEN_FILE`.
///
/// # Errors
/// `invalid` when neither is set or the token is malformed; `untrusted` when the file is not
/// private; `internal` when it cannot be read.
pub fn token_from_env(env: Env<'_>) -> Result<String> {
    if let Some(token) = var(env, "PITCREW_TOKEN") {
        let token = token
            .into_string()
            .map_err(|_| Error::invalid("PITCREW_TOKEN is not valid UTF-8"))?;
        return check_token(&token, "PITCREW_TOKEN");
    }
    if let Some(path) = var(env, "PITCREW_TOKEN_FILE") {
        let text = read_private_file(Path::new(&path))?;
        return check_token(&text, "PITCREW_TOKEN_FILE");
    }
    Err(Error::invalid(
        "no token: set PITCREW_TOKEN, or PITCREW_TOKEN_FILE to a private file holding it",
    ))
}

/// A bearer token is visible ASCII only, so it can never break a header.
fn check_token(raw: &str, source: &str) -> Result<String> {
    let token = raw.trim();
    if token.is_empty() {
        return Err(Error::invalid(format!("{source} is empty")));
    }
    if token.len() > MAX_TOKEN_LEN || !token.bytes().all(|b| (0x21..=0x7e).contains(&b)) {
        return Err(Error::invalid(format!(
            "{source} does not look like a token (it must be one line of visible ASCII)"
        )));
    }
    Ok(token.to_owned())
}

/// Reads a small file that only we may read. On Unix it must be a regular file (not a symlink),
/// owned by us, with no group or other permissions. On Windows, files inherit their folder's
/// ACL, so keep it under the user's profile.
fn read_private_file(path: &Path) -> Result<String> {
    use std::io::Read as _;
    let shown = path.display();
    let file = open_private(path)?;
    let mut text = String::new();
    file.take(MAX_TOKEN_LEN as u64 + 2)
        .read_to_string(&mut text)
        .map_err(|e| Error::invalid(format!("cannot read PITCREW_TOKEN_FILE {shown}: {e}")))?;
    Ok(text)
}

#[cfg(unix)]
fn open_private(path: &Path) -> Result<std::fs::File> {
    use rustix::fs::{Mode, OFlags};
    use std::os::unix::fs::MetadataExt as _;
    let shown = path.display();
    let fd = rustix::fs::open(
        path,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK,
        Mode::empty(),
    )
    .map_err(|e| match e {
        rustix::io::Errno::LOOP => Error::new(
            Kind::Untrusted,
            format!("PITCREW_TOKEN_FILE {shown} is a symbolic link; point it at the file itself"),
        ),
        e => Error::invalid(format!(
            "cannot open PITCREW_TOKEN_FILE {shown}: {}",
            std::io::Error::from(e)
        )),
    })?;
    let file = std::fs::File::from(fd);
    let meta = file
        .metadata()
        .map_err(|e| Error::invalid(format!("cannot read PITCREW_TOKEN_FILE {shown}: {e}")))?;
    if !meta.is_file() {
        return Err(Error::invalid(format!(
            "PITCREW_TOKEN_FILE {shown} is not a regular file"
        )));
    }
    if meta.uid() != pitcrew_auth::euid() {
        return Err(Error::new(
            Kind::Untrusted,
            format!("PITCREW_TOKEN_FILE {shown} is owned by another user"),
        ));
    }
    let mode = meta.mode() & 0o777;
    if mode & 0o077 != 0 {
        return Err(Error::new(
            Kind::Untrusted,
            format!(
                "PITCREW_TOKEN_FILE {shown} is open to other users (mode {mode:o}); make it \
                 private with: chmod 600 {shown}"
            ),
        ));
    }
    Ok(file)
}

#[cfg(not(unix))]
fn open_private(path: &Path) -> Result<std::fs::File> {
    let shown = path.display();
    let cannot =
        |e: std::io::Error| Error::invalid(format!("cannot open PITCREW_TOKEN_FILE {shown}: {e}"));
    let meta = std::fs::symlink_metadata(path).map_err(cannot)?;
    if !meta.is_file() {
        return Err(Error::new(
            Kind::Untrusted,
            format!("PITCREW_TOKEN_FILE {shown} is not a regular file"),
        ));
    }
    std::fs::File::open(path).map_err(cannot)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> + use<> {
        let map: HashMap<String, OsString> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), OsString::from(v)))
            .collect();
        move |name| map.get(name).cloned()
    }

    #[test]
    fn loopback_urls_are_accepted() {
        let e = Endpoint::dev_url("http://127.0.0.1:47317").unwrap();
        assert_eq!(
            e,
            Endpoint::Tcp {
                addrs: vec!["127.0.0.1:47317".parse().unwrap()],
                host: "127.0.0.1:47317".into()
            }
        );
        let e = Endpoint::dev_url("HTTP://[::1]:9/").unwrap();
        assert_eq!(e.host_header(), "[::1]:9");
        let Endpoint::Tcp { addrs, .. } = Endpoint::dev_url("http://LocalHost").unwrap() else {
            panic!("not tcp");
        };
        assert_eq!(
            addrs,
            vec![
                "127.0.0.1:80".parse::<SocketAddr>().unwrap(),
                "[::1]:80".parse().unwrap()
            ]
        );
    }

    #[test]
    fn other_urls_are_refused() {
        for url in [
            "http://10.0.0.1:47317",
            "http://example.com:47317",
            "http://127.0.0.2:47317",
            "http://0.0.0.0:47317",
            "http://localhost.example.com",
            "http://[::2]:1",
            "https://127.0.0.1:47317",
            "127.0.0.1:47317",
            "http://user@127.0.0.1:1",
            "http://127.0.0.1:1/v1",
            "http://127.0.0.1:1?x",
            "http://127.0.0.1:99999",
            "http://127.0.0.1:",
            "http://:80",
        ] {
            let err = Endpoint::dev_url(url).unwrap_err();
            assert_eq!(err.kind, Kind::Invalid, "{url}");
        }
    }

    #[test]
    fn a_non_loopback_url_from_the_environment_is_refused() {
        let env = env_of(&[("PITCREW_URL", "http://192.0.2.1:47317")]);
        let err = Endpoint::from_env(&env).unwrap_err();
        assert_eq!(err.kind, Kind::Invalid);
        assert!(err.message.contains("127.0.0.1"), "{}", err.message);
    }

    #[cfg(unix)]
    #[test]
    fn the_socket_wins_and_names_its_directory() {
        let env = env_of(&[
            ("PITCREW_SOCKET", "/run/x/pitcrewd.sock"),
            ("PITCREW_URL", "http://127.0.0.1:1"),
        ]);
        assert_eq!(
            Endpoint::from_env(&env).unwrap(),
            Endpoint::Unix {
                dir: "/run/x".into()
            }
        );
        assert_eq!(
            Endpoint::unix(Path::new("/run/x")),
            Endpoint::Unix {
                dir: "/run/x".into()
            }
        );
        assert_eq!(
            Endpoint::unix(Path::new("pitcrewd.sock")),
            Endpoint::Unix { dir: ".".into() }
        );
    }

    #[cfg(unix)]
    #[test]
    fn nothing_set_is_a_clear_error() {
        let err = Endpoint::from_env(&env_of(&[])).unwrap_err();
        assert_eq!(err.kind, Kind::Invalid);
        assert!(err.message.contains("PITCREW_SOCKET"));
    }

    #[cfg(windows)]
    #[test]
    fn only_local_pipes() {
        assert!(Endpoint::pipe(r"\\.\pipe\pitcrewd-x".into()).is_ok());
        assert!(Endpoint::pipe(r"\\.\PIPE\pitcrewd-x".into()).is_ok());
        for name in [r"\\server\pipe\x", r"\\.\pipe\", "pitcrewd", r"C:\x"] {
            assert!(Endpoint::pipe(name.into()).is_err(), "{name}");
        }
    }

    #[test]
    fn tokens_come_from_the_variable_first() {
        let env = env_of(&[
            ("PITCREW_TOKEN", " pca_abc \n"),
            ("PITCREW_TOKEN_FILE", "/x"),
        ]);
        assert_eq!(token_from_env(&env).unwrap(), "pca_abc");
        let err = token_from_env(&env_of(&[])).unwrap_err();
        assert_eq!(err.kind, Kind::Invalid);
        for bad in ["a b", "a\r\nX-Evil: 1", "tok\u{e9}"] {
            let env = env_of(&[("PITCREW_TOKEN", bad)]);
            assert_eq!(
                token_from_env(&env).unwrap_err().kind,
                Kind::Invalid,
                "{bad:?}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_token_file_must_be_private() {
        use std::os::unix::fs::PermissionsExt as _;
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("token");
        std::fs::write(&file, "pca_secret\n").unwrap();
        let path = file.to_str().unwrap();
        let env = env_of(&[("PITCREW_TOKEN_FILE", path)]);

        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(token_from_env(&env).unwrap(), "pca_secret");
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o400)).unwrap();
        assert_eq!(token_from_env(&env).unwrap(), "pca_secret");

        for mode in [0o644, 0o640, 0o604, 0o660] {
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(mode)).unwrap();
            let err = token_from_env(&env).unwrap_err();
            assert_eq!(err.kind, Kind::Untrusted, "mode {mode:o}");
            assert!(err.message.contains("chmod 600"), "{}", err.message);
        }

        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
        let link = tmp.path().join("link");
        std::os::unix::fs::symlink(&file, &link).unwrap();
        let env = env_of(&[("PITCREW_TOKEN_FILE", link.to_str().unwrap())]);
        assert_eq!(token_from_env(&env).unwrap_err().kind, Kind::Untrusted);

        let env = env_of(&[("PITCREW_TOKEN_FILE", tmp.path().to_str().unwrap())]);
        assert_eq!(token_from_env(&env).unwrap_err().kind, Kind::Invalid);
    }
}
