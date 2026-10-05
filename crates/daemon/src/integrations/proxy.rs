//! The HTTPS transport's proxy (`http.rs`): an `http://` proxy from `HTTPS_PROXY` (else
//! `https_proxy`), reached with `CONNECT`, except for the hosts `NO_PROXY` (else `no_proxy`)
//! names.
//!
//! - TLS runs end to end through the tunnel, so the proxy sees the host and port it is asked for,
//!   never a request header, a body or the tracker credential.
//! - A proxy's own user name and password (`http://user:pass@proxy:3128`) go in
//!   `Proxy-Authorization` (Basic); they are never shown, logged or put in an error.
//! - `NO_PROXY` takes `*`, host names (each also covers its subdomains; a leading `.` or `*.` is
//!   the same), an optional `:port`, IP addresses and CIDR blocks (`10.0.0.0/8`), separated by
//!   commas or spaces, as curl reads it.
//! - `https://` and SOCKS proxies, and `ALL_PROXY`, are not supported: an `HTTPS_PROXY` of
//!   another scheme stops the transport from being set up, with a reason that does not repeat the
//!   setting.

use base64::Engine as _;
use std::fmt;
use std::net::IpAddr;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpStream;

/// The longest answer head to `CONNECT` read from a proxy.
const MAX_HEAD: usize = 8 * 1024;

/// Where the transport's connections go. See the [module docs](self).
#[derive(Clone, Default)]
pub struct Proxy {
    server: Option<Server>,
    bypass: Vec<Bypass>,
}

/// An `http://` proxy.
#[derive(Clone)]
struct Server {
    host: String,
    port: u16,
    /// `Basic …`, when the proxy URL has a user name or password. Never shown.
    authorization: Option<String>,
}

/// One `NO_PROXY` entry.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Bypass {
    All,
    Host { name: String, port: Option<u16> },
    Network { addr: IpAddr, prefix: u8 },
}

impl fmt::Debug for Proxy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Proxy")
            .field(
                "server",
                &self
                    .server
                    .as_ref()
                    .map(|s| format!("{}:{}", s.host, s.port)),
            )
            .field("bypass", &self.bypass)
            .finish()
    }
}

/// Why `HTTPS_PROXY` cannot be used. Never holds the setting (it may hold a password).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "HTTPS_PROXY must be an http:// proxy URL with a host; https:// and SOCKS proxies are not \
     supported"
)]
pub struct ProxyError;

/// A non-empty environment variable, the first of `names` that is set.
fn env(names: [&str; 2]) -> Option<String> {
    names
        .iter()
        .find_map(|name| std::env::var(name).ok().filter(|v| !v.trim().is_empty()))
}

/// `%XX` decoded, for a proxy URL's user name and password.
fn decoded(text: &str) -> String {
    percent_encoding::percent_decode_str(text)
        .decode_utf8_lossy()
        .into_owned()
}

impl Proxy {
    /// From this process's environment. See the [module docs](self).
    ///
    /// # Errors
    /// `HTTPS_PROXY` is set but is not an `http://` proxy URL.
    pub fn from_env() -> Result<Self, ProxyError> {
        Self::parse(
            env(["HTTPS_PROXY", "https_proxy"]).as_deref(),
            env(["NO_PROXY", "no_proxy"]).as_deref(),
        )
    }

    /// From the values of `HTTPS_PROXY` and `NO_PROXY`; `None` or blank is unset.
    ///
    /// # Errors
    /// `https_proxy` is set but is not an `http://` proxy URL.
    pub fn parse(https_proxy: Option<&str>, no_proxy: Option<&str>) -> Result<Self, ProxyError> {
        let server = match https_proxy.map(str::trim).filter(|v| !v.is_empty()) {
            None => None,
            Some(value) => Some(server(value)?),
        };
        Ok(Self {
            server,
            bypass: no_proxy.map(bypass).unwrap_or_default(),
        })
    }

    /// Whether connections to `host:port` go through the proxy.
    #[must_use]
    pub fn proxies(&self, host: &str, port: u16) -> bool {
        self.server.is_some() && !self.bypassed(host, port)
    }

    fn bypassed(&self, host: &str, port: u16) -> bool {
        let host = host
            .trim_start_matches('[')
            .trim_end_matches(']')
            .trim_end_matches('.')
            .to_ascii_lowercase();
        let ip: Option<IpAddr> = host.parse().ok();
        self.bypass.iter().any(|rule| match rule {
            Bypass::All => true,
            Bypass::Host { name, port: wanted } => {
                wanted.is_none_or(|p| p == port)
                    && (host == *name
                        || (ip.is_none()
                            && host
                                .strip_suffix(name.as_str())
                                .is_some_and(|rest| rest.ends_with('.'))))
            }
            Bypass::Network { addr, prefix } => ip.is_some_and(|ip| in_network(ip, *addr, *prefix)),
        })
    }

    /// A TCP connection that reaches `host:port`: through the proxy's tunnel, or directly.
    ///
    /// # Errors
    /// Why not, for a person to read; never the proxy's credentials.
    pub async fn connect(&self, host: &str, port: u16) -> Result<TcpStream, String> {
        let bare = host.trim_start_matches('[').trim_end_matches(']');
        match &self.server {
            Some(server) if self.proxies(host, port) => tunnel(server, host, port).await,
            _ => TcpStream::connect((bare, port))
                .await
                .map_err(|e| format!("cannot connect: {}", e.kind())),
        }
    }
}

fn server(value: &str) -> Result<Server, ProxyError> {
    let with_scheme = if value.contains("://") {
        value.to_owned()
    } else {
        format!("http://{value}")
    };
    let url = url::Url::parse(&with_scheme).map_err(|_| ProxyError)?;
    if url.scheme() != "http" {
        return Err(ProxyError);
    }
    let host = url
        .host_str()
        .filter(|h| !h.is_empty())
        .ok_or(ProxyError)?
        .to_owned();
    let authorization = (!url.username().is_empty() || url.password().is_some()).then(|| {
        let pair = format!(
            "{}:{}",
            decoded(url.username()),
            decoded(url.password().unwrap_or_default())
        );
        format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode(pair)
        )
    });
    Ok(Server {
        port: url.port_or_known_default().unwrap_or(80),
        host,
        authorization,
    })
}

/// `entry` without its port: `host:port`, `[v6]:port`; a bare IPv6 address keeps its colons.
fn split_port(entry: &str) -> (&str, Option<u16>) {
    if let Some(rest) = entry.strip_prefix('[') {
        return match rest.split_once(']') {
            Some((addr, after)) => (addr, after.strip_prefix(':').and_then(|p| p.parse().ok())),
            None => (entry, None),
        };
    }
    match entry.rsplit_once(':') {
        Some((name, port)) if !name.contains(':') => match port.parse() {
            Ok(port) => (name, Some(port)),
            Err(_) => (entry, None),
        },
        _ => (entry, None),
    }
}

fn bypass(no_proxy: &str) -> Vec<Bypass> {
    no_proxy
        .split([',', ' ', '\t'])
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .filter_map(|entry| {
            if entry == "*" {
                return Some(Bypass::All);
            }
            if let Some((addr, prefix)) = entry.split_once('/') {
                let addr: IpAddr = addr
                    .trim_start_matches('[')
                    .trim_end_matches(']')
                    .parse()
                    .ok()?;
                let prefix: u8 = prefix.parse().ok()?;
                let most = if addr.is_ipv4() { 32 } else { 128 };
                return (prefix <= most).then_some(Bypass::Network { addr, prefix });
            }
            let entry = entry.to_ascii_lowercase();
            let (name, port) = split_port(&entry);
            let name = name
                .trim_start_matches("*.")
                .trim_start_matches('.')
                .trim_end_matches('.');
            (!name.is_empty()).then(|| Bypass::Host {
                name: name.to_owned(),
                port,
            })
        })
        .collect()
}

fn in_network(ip: IpAddr, network: IpAddr, prefix: u8) -> bool {
    match (ip, network) {
        (IpAddr::V4(ip), IpAddr::V4(network)) => {
            let mask = u32::MAX.checked_shl(32 - u32::from(prefix)).unwrap_or(0);
            u32::from(ip) & mask == u32::from(network) & mask
        }
        (IpAddr::V6(ip), IpAddr::V6(network)) => {
            let mask = u128::MAX.checked_shl(128 - u32::from(prefix)).unwrap_or(0);
            u128::from(ip) & mask == u128::from(network) & mask
        }
        _ => false,
    }
}

/// A tunnel to `host:port` through `server`: `CONNECT`, then the answer's head, read byte by byte
/// so nothing after it (the server's TLS bytes) is taken.
async fn tunnel(server: &Server, host: &str, port: u16) -> Result<TcpStream, String> {
    let proxy_host = server.host.trim_start_matches('[').trim_end_matches(']');
    let mut tcp = TcpStream::connect((proxy_host, server.port))
        .await
        .map_err(|e| format!("cannot connect to the proxy: {}", e.kind()))?;
    let authority = format!("{host}:{port}");
    let mut head = format!("CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\n");
    if let Some(authorization) = &server.authorization {
        head.push_str("Proxy-Authorization: ");
        head.push_str(authorization);
        head.push_str("\r\n");
    }
    head.push_str("\r\n");
    tcp.write_all(head.as_bytes())
        .await
        .map_err(|e| format!("cannot reach the proxy: {}", e.kind()))?;
    let mut answer = Vec::new();
    while !answer.ends_with(b"\r\n\r\n") {
        if answer.len() >= MAX_HEAD {
            return Err("the proxy's answer is too long".into());
        }
        let byte = tcp
            .read_u8()
            .await
            .map_err(|_| "the proxy closed the connection".to_owned())?;
        answer.push(byte);
    }
    let status = std::str::from_utf8(&answer)
        .ok()
        .and_then(|text| text.split_whitespace().nth(1))
        .and_then(|code| code.parse::<u16>().ok());
    match status {
        Some(code) if (200..300).contains(&code) => Ok(tcp),
        Some(407) => Err("the proxy wants credentials (407); put them in HTTPS_PROXY".into()),
        Some(code) => Err(format!("the proxy refused the tunnel ({code})")),
        None => Err("the proxy's answer is not HTTP".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn https_proxy_is_read_with_its_credentials_kept_out_of_sight() {
        let proxy = Proxy::parse(
            Some(" http://synthetic-user:synthetic%40pass@proxy.example.com:3128/ "),
            None,
        )
        .unwrap();
        let server = proxy.server.as_ref().unwrap();
        assert_eq!(
            (server.host.as_str(), server.port),
            ("proxy.example.com", 3128)
        );
        let expected = format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode("synthetic-user:synthetic@pass")
        );
        assert_eq!(server.authorization.as_deref(), Some(expected.as_str()));
        let shown = format!("{proxy:?}");
        assert!(!shown.contains("synthetic"), "{shown}");
        assert!(proxy.proxies("api.github.com", 443));

        // No scheme means http://; no port means 80; unset or blank means no proxy.
        let bare = Proxy::parse(Some("proxy.example.com"), None).unwrap();
        let server = bare.server.as_ref().unwrap();
        assert_eq!((server.port, server.authorization.is_none()), (80, true));
        for unset in [None, Some(""), Some("   ")] {
            assert!(
                !Proxy::parse(unset, None)
                    .unwrap()
                    .proxies("api.github.com", 443)
            );
        }
        // Other schemes are refused, without repeating the setting.
        for bad in [
            "https://synthetic-user:synthetic-pass@proxy.example.com",
            "socks5://proxy.example.com:1080",
            "http://",
        ] {
            let err = Proxy::parse(Some(bad), None).unwrap_err();
            assert!(!err.to_string().contains("synthetic"), "{bad}");
        }
    }

    #[test]
    fn no_proxy_names_hosts_domains_ports_addresses_and_blocks() {
        let proxy = Proxy::parse(
            Some("http://proxy.example.com:3128"),
            Some(
                "localhost, .corp.example.com,*.internal.example.org jira.example.com:8443,\
                 10.0.0.0/8,192.168.1.7,[::1],fd00::/8",
            ),
        )
        .unwrap();
        for (host, port) in [
            ("localhost", 443),
            ("corp.example.com", 443),
            ("ghe.corp.example.com", 443),
            ("GHE.Corp.Example.com.", 443),
            ("jira.internal.example.org", 443),
            ("jira.example.com", 8443),
            ("10.1.2.3", 443),
            ("192.168.1.7", 443),
            ("[::1]", 443),
            ("[fd12::7]", 443),
        ] {
            assert!(!proxy.proxies(host, port), "{host}:{port} should go direct");
        }
        for (host, port) in [
            ("api.github.com", 443),
            ("notcorp.example.com", 443),
            ("jira.example.com", 443),
            ("11.0.0.1", 443),
            ("192.168.1.8", 443),
            ("[fe80::1]", 443),
        ] {
            assert!(
                proxy.proxies(host, port),
                "{host}:{port} should use the proxy"
            );
        }
        let everything = Proxy::parse(Some("http://proxy.example.com"), Some("*")).unwrap();
        assert!(!everything.proxies("api.github.com", 443));
    }
}
