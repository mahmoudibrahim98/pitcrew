//! The tracker sync's transports (`pitcrew_sync_github::Transport`, which Jira's crate shares):
//!
//! - [`HttpsTransport`]: the real one. HTTP/1.1 over TLS (rustls, with the `ring` provider and
//!   this machine's own trusted certificates), one connection per request, `https://` only, with
//!   time limits and a cap on the response body. It adds `User-Agent` (GitHub refuses requests
//!   without one) and `Host`, and sends the crates' method, headers and body as they are: `GET`
//!   for the sync, and an approved outward write's one `POST`, `PATCH`, `PUT` or `DELETE`
//!   (`writes.rs`). It never logs a header or a body, and never follows a redirect or retries.
//!   Behind a corporate proxy it tunnels through `HTTPS_PROXY` with `CONNECT`, except for the
//!   hosts `NO_PROXY` names (`proxy.rs`); TLS stays end to end. Writes take the same way.
//! - [`FixtureTransport`]: recorded exchanges from a folder of `*.fixture` files (the sync crates'
//!   format), answered by method and URL, as often as asked; for tests only (`serve
//!   --integration-fixtures`). The folder is read again for each request, so a test changes
//!   "upstream" between two syncs by adding a file whose name sorts first. It never reaches the
//!   network. In unit tests it keeps every request it was sent (`FixtureTransport::sent`), so they
//!   can show what was, and was not, written.

use super::proxy::Proxy;
use bytes::Bytes;
use http_body_util::{BodyExt as _, Full, Limited};
use hyper_util::rt::TokioIo;
use pitcrew_sync_github::fixture::{RecordedExchange, parse_fixture};
use pitcrew_sync_github::transport::{Method, Request, Response, Transport, TransportError};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
#[cfg(test)]
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

/// The largest response body read, in bytes: above the sync crates' own page cap (5 MiB), which
/// they enforce themselves.
const MAX_BODY: usize = 6 * 1024 * 1024;
/// How long connecting (TCP and TLS) may take.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
/// How long a whole request may take, the body included.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

fn failed(url: &str, reason: impl Into<String>) -> TransportError {
    // The URL never holds a credential (ADR-0006: never in a query string); the query is dropped
    // anyway, so a JQL cursor or a page token does not reach a person's screen.
    let shown = url.split('?').next().unwrap_or_default().to_owned();
    TransportError::Failed {
        url: shown,
        reason: reason.into(),
    }
}

/// The real HTTPS transport. See the [module docs](self).
#[derive(Clone)]
pub struct HttpsTransport {
    tls: tokio_rustls::TlsConnector,
    user_agent: String,
    proxy: Proxy,
}

impl std::fmt::Debug for HttpsTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpsTransport")
            .field("user_agent", &self.user_agent)
            .field("proxy", &self.proxy)
            .finish_non_exhaustive()
    }
}

impl HttpsTransport {
    /// A transport trusting this machine's certificates, through the proxy this process's
    /// environment names (`HTTPS_PROXY`, `NO_PROXY`; see `proxy.rs`).
    ///
    /// # Errors
    /// No certificate could be loaded, TLS cannot be set up, or `HTTPS_PROXY` is not an `http://`
    /// proxy.
    pub fn new() -> anyhow::Result<Self> {
        let proxy = Proxy::from_env()?;
        let mut roots = rustls::RootCertStore::empty();
        let found = rustls_native_certs::load_native_certs();
        let (added, _ignored) = roots.add_parsable_certificates(found.certs);
        if added == 0 {
            anyhow::bail!("no trusted certificate could be loaded from this machine");
        }
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let config = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()?
            .with_root_certificates(roots)
            .with_no_client_auth();
        Ok(Self {
            tls: tokio_rustls::TlsConnector::from(Arc::new(config)),
            user_agent: format!("pitcrewd/{}", env!("CARGO_PKG_VERSION")),
            proxy,
        })
    }

    async fn exchange(&self, request: Request) -> Result<Response, TransportError> {
        let url = url::Url::parse(&request.url).map_err(|_| failed(&request.url, "a bad URL"))?;
        if url.scheme() != "https" || !url.username().is_empty() || url.password().is_some() {
            return Err(failed(&request.url, "only https:// URLs are fetched"));
        }
        let host = url
            .host_str()
            .ok_or_else(|| failed(&request.url, "a URL without a host"))?
            .to_owned();
        let port = url.port_or_known_default().unwrap_or(443);
        let server = rustls::pki_types::ServerName::try_from(
            host.trim_start_matches('[')
                .trim_end_matches(']')
                .to_owned(),
        )
        .map_err(|_| failed(&request.url, "a bad host name"))?;
        let connect = async {
            let tcp = self
                .proxy
                .connect(&host, port)
                .await
                .map_err(|why| failed(&request.url, why))?;
            self.tls
                .connect(server, tcp)
                .await
                .map_err(|e| failed(&request.url, format!("TLS failed: {}", e.kind())))
        };
        let tls = tokio::time::timeout(CONNECT_TIMEOUT, connect)
            .await
            .map_err(|_| failed(&request.url, "connecting timed out"))??;
        let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(tls))
            .await
            .map_err(|_| failed(&request.url, "the HTTP handshake failed"))?;
        // Driven until the response is read; dropped with this request.
        let driver = tokio::spawn(async move {
            let _ = connection.await;
        });
        let path = match url.query() {
            Some(query) => format!("{}?{query}", url.path()),
            None => url.path().to_owned(),
        };
        let authority = match url.port() {
            Some(port) => format!("{host}:{port}"),
            None => host.clone(),
        };
        let method = match request.method {
            Method::Get => hyper::Method::GET,
            Method::Post => hyper::Method::POST,
            Method::Patch => hyper::Method::PATCH,
            Method::Put => hyper::Method::PUT,
        };
        let mut builder = hyper::Request::builder()
            .method(method)
            .uri(path)
            .header(hyper::header::HOST, authority)
            .header(hyper::header::USER_AGENT, &self.user_agent);
        for (name, value) in &request.headers {
            builder = builder.header(name.as_str(), value.as_str());
        }
        let outgoing = builder
            .body(Full::new(Bytes::from(request.body)))
            .map_err(|_| failed(&request.url, "a bad request header"))?;
        let response = sender
            .send_request(outgoing)
            .await
            .map_err(|_| failed(&request.url, "the request failed"))?;
        let status = response.status().as_u16();
        let headers = response
            .headers()
            .iter()
            .filter_map(|(name, value)| {
                value
                    .to_str()
                    .ok()
                    .map(|v| (name.as_str().to_owned(), v.to_owned()))
            })
            .collect();
        let body = Limited::new(response.into_body(), MAX_BODY)
            .collect()
            .await
            .map_err(|_| failed(&request.url, "the response was too large or broken"))?
            .to_bytes()
            .to_vec();
        driver.abort();
        Ok(Response {
            status,
            headers,
            body,
        })
    }
}

impl Transport for HttpsTransport {
    async fn send(&self, request: Request) -> Result<Response, TransportError> {
        let url = request.url.clone();
        tokio::time::timeout(REQUEST_TIMEOUT, self.exchange(request))
            .await
            .map_err(|_| failed(&url, "the request timed out"))?
    }
}

type Exchanges = HashMap<(String, String), RecordedExchange>;

/// Recorded exchanges, answered by method and URL (tests only). See the [module docs](self).
#[derive(Debug, Clone)]
pub struct FixtureTransport {
    dir: Arc<std::path::PathBuf>,
    /// Every request sent, in order (clones share it). Kept for unit tests only.
    #[cfg(test)]
    sent: Arc<Mutex<Vec<Request>>>,
}

impl FixtureTransport {
    /// The exchanges in the `*.fixture` files of `dir`, read now (to fail early) and again for
    /// each request.
    ///
    /// # Errors
    /// The folder or a file cannot be read, or a file is not in the fixture format.
    pub fn load(dir: &Path) -> anyhow::Result<Self> {
        read_exchanges(dir)?;
        Ok(Self {
            dir: Arc::new(dir.to_path_buf()),
            #[cfg(test)]
            sent: Arc::default(),
        })
    }

    /// Every request sent through this transport or a clone of it, in order.
    #[cfg(test)]
    #[must_use]
    pub fn sent(&self) -> Vec<Request> {
        self.sent
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

/// Every exchange in the `*.fixture` files of `dir`, by name; the first of two for one method and
/// URL wins.
fn read_exchanges(dir: &Path) -> anyhow::Result<Exchanges> {
    let mut names: Vec<_> = std::fs::read_dir(dir)?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "fixture"))
        .collect();
    names.sort();
    let mut exchanges = HashMap::new();
    for path in names {
        let text = std::fs::read_to_string(&path)?;
        for exchange in parse_fixture(&text)? {
            exchanges
                .entry((exchange.method.clone(), exchange.url.clone()))
                .or_insert(exchange);
        }
    }
    Ok(exchanges)
}

/// The exchange for `request`: its URL, else its URL without a `since=` parameter (a fixture for
/// a first read answers later incremental ones too, as if nothing changed upstream).
fn find<'a>(exchanges: &'a Exchanges, request: &Request) -> Option<&'a RecordedExchange> {
    let method = request.method.as_str().to_owned();
    exchanges
        .get(&(method.clone(), request.url.clone()))
        .or_else(|| {
            let (base, query) = request.url.split_once('?')?;
            let kept: Vec<&str> = query
                .split('&')
                .filter(|pair| !pair.starts_with("since="))
                .collect();
            let url = if kept.is_empty() {
                base.to_owned()
            } else {
                format!("{base}?{}", kept.join("&"))
            };
            exchanges.get(&(method, url))
        })
}

impl Transport for FixtureTransport {
    async fn send(&self, request: Request) -> Result<Response, TransportError> {
        #[cfg(test)]
        self.sent
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(request.clone());
        let exchanges = read_exchanges(&self.dir)
            .map_err(|_| failed(&request.url, "the recorded fixtures cannot be read"))?;
        match find(&exchanges, &request) {
            Some(exchange) => Ok(Response {
                status: exchange.status,
                headers: exchange.response_headers.clone(),
                body: exchange.body.clone(),
            }),
            None => Err(failed(
                &request.url,
                "no recorded fixture matches this request",
            )),
        }
    }
}

/// The transport a hub syncs through: the real one, or recorded fixtures in tests.
#[derive(Debug, Clone)]
pub enum Upstream {
    /// HTTPS.
    Https(HttpsTransport),
    /// Recorded fixtures.
    Fixtures(FixtureTransport),
}

impl Transport for Upstream {
    async fn send(&self, request: Request) -> Result<Response, TransportError> {
        match self {
            Self::Https(t) => t.send(request).await,
            Self::Fixtures(t) => t.send(request).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pitcrew_sync_github::transport::Method;
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    fn get(url: &str) -> Request {
        Request {
            method: Method::Get,
            url: url.into(),
            headers: vec![("Authorization".into(), "Bearer synthetic".into())],
            body: Vec::new(),
        }
    }

    #[tokio::test]
    async fn fixtures_answer_by_url_and_ignore_a_since_cursor() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("a.fixture"),
            "GET https://api.github.com/repos/example-org/demo-repo/issues?state=all&per_page=100 HTTP/1.1\n\n\
             HTTP/1.1 200\nETag: \"e1\"\n\n[]\n",
        )
        .unwrap();
        std::fs::write(dir.path().join("ignored.txt"), "not a fixture").unwrap();
        let fixtures = FixtureTransport::load(dir.path()).unwrap();
        let first = fixtures
            .send(get(
                "https://api.github.com/repos/example-org/demo-repo/issues?state=all&per_page=100",
            ))
            .await
            .unwrap();
        assert_eq!(first.status, 200);
        assert_eq!(first.header("etag"), Some("\"e1\""));
        let again = fixtures
            .send(get("https://api.github.com/repos/example-org/demo-repo/issues?state=all&per_page=100&since=2026-01-01T00%3A00%3A00Z"))
            .await
            .unwrap();
        assert_eq!(again.body, b"[]");
        let missing = fixtures
            .send(get(
                "https://api.github.com/repos/example-org/other?token=x",
            ))
            .await
            .unwrap_err();
        assert!(!missing.to_string().contains("token=x"));

        // The folder is read again for each request: a file whose name sorts first changes what
        // "upstream" says from then on.
        std::fs::write(
            dir.path().join("0-later.fixture"),
            "GET https://api.github.com/repos/example-org/demo-repo/issues?state=all&per_page=100 HTTP/1.1\n\n\
             HTTP/1.1 200\nETag: \"e2\"\n\n[{}]\n",
        )
        .unwrap();
        let later = fixtures
            .send(get(
                "https://api.github.com/repos/example-org/demo-repo/issues?state=all&per_page=100",
            ))
            .await
            .unwrap();
        assert_eq!(
            (later.header("etag"), later.body.as_slice()),
            (Some("\"e2\""), &b"[{}]"[..])
        );
    }

    /// A transport that trusts no certificate: TLS never completes, which these tests do not need.
    fn untrusting(proxy: Proxy) -> HttpsTransport {
        HttpsTransport {
            tls: tokio_rustls::TlsConnector::from(Arc::new(
                rustls::ClientConfig::builder_with_provider(Arc::new(
                    rustls::crypto::ring::default_provider(),
                ))
                .with_safe_default_protocol_versions()
                .unwrap()
                .with_root_certificates(rustls::RootCertStore::empty())
                .with_no_client_auth(),
            )),
            user_agent: "pitcrewd/test".into(),
            proxy,
        }
    }

    #[tokio::test]
    async fn https_refuses_other_schemes_and_user_info_before_connecting() {
        let https = untrusting(Proxy::default());
        for url in [
            "http://api.github.com/repos/example-org/demo-repo",
            "https://user:pass@api.github.com/repos/example-org/demo-repo",
            "file:///etc/hosts",
            "not a url",
        ] {
            assert!(https.send(get(url)).await.is_err(), "{url}");
        }
    }

    /// What a stand-in on 127.0.0.1 received: as a proxy, the head up to its blank line, then,
    /// after it answers `reply`, the first bytes sent through; as a server (`reply` `None`), only
    /// the first bytes.
    struct Seen {
        head: String,
        after: Vec<u8>,
    }

    async fn stand_in(reply: Option<&'static str>) -> (u16, tokio::task::JoinHandle<Seen>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut head = Vec::new();
            if let Some(reply) = reply {
                while !head.ends_with(b"\r\n\r\n") && head.len() < 8192 {
                    match socket.read_u8().await {
                        Ok(byte) => head.push(byte),
                        Err(_) => break,
                    }
                }
                socket.write_all(reply.as_bytes()).await.unwrap();
            }
            let mut after = vec![0_u8; 5];
            let read = socket.read(&mut after).await.unwrap_or(0);
            after.truncate(read);
            Seen {
                head: String::from_utf8_lossy(&head).into_owned(),
                after,
            }
        });
        (port, task)
    }

    #[tokio::test]
    async fn https_tunnels_through_the_proxy_and_the_token_never_reaches_it() {
        // The proxy opens the tunnel: what follows it is the TLS handshake, end to end.
        let (port, proxy) = stand_in(Some("HTTP/1.1 200 Connection established\r\n\r\n")).await;
        let https = untrusting(
            Proxy::parse(
                Some(&format!(
                    "http://synthetic-user:synthetic-pass@127.0.0.1:{port}"
                )),
                Some("localhost"),
            )
            .unwrap(),
        );
        let err = https
            .send(get("https://api.github.com/repos/example-org/demo-repo"))
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("TLS failed"), "{err}");
        assert!(!err.contains("synthetic-pass"), "{err}");
        let seen = proxy.await.unwrap();
        assert!(
            seen.head
                .starts_with("CONNECT api.github.com:443 HTTP/1.1\r\nHost: api.github.com:443\r\n"),
            "{}",
            seen.head
        );
        let credentials = base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            "synthetic-user:synthetic-pass",
        );
        assert!(
            seen.head
                .contains(&format!("Proxy-Authorization: Basic {credentials}\r\n")),
            "{}",
            seen.head
        );
        assert!(
            !seen.head.contains("Bearer"),
            "the token went to the proxy in clear"
        );
        assert_eq!(
            seen.after.first(),
            Some(&0x16),
            "a TLS handshake goes through"
        );
    }

    #[tokio::test]
    async fn a_proxy_that_refuses_the_tunnel_is_reported() {
        let (port, proxy) =
            stand_in(Some("HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n")).await;
        let https = untrusting(Proxy::parse(Some(&format!("127.0.0.1:{port}")), None).unwrap());
        let err = https
            .send(get("https://jira.example.com/rest/api/3/myself"))
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("the proxy refused the tunnel (403)"), "{err}");
        let seen = proxy.await.unwrap();
        assert!(
            seen.head
                .starts_with("CONNECT jira.example.com:443 HTTP/1.1\r\n")
        );
        assert!(!seen.head.contains("Proxy-Authorization"));
    }

    #[tokio::test]
    async fn no_proxy_hosts_are_reached_directly() {
        // The proxy would refuse; the server, named in NO_PROXY, is reached without it.
        let (proxy_port, proxy) = stand_in(Some("HTTP/1.1 403 Forbidden\r\n\r\n")).await;
        let (server_port, server) = stand_in(None).await;
        let https = untrusting(
            Proxy::parse(
                Some(&format!("http://127.0.0.1:{proxy_port}")),
                Some("example.org, 127.0.0.1"),
            )
            .unwrap(),
        );
        let err = https
            .send(get(&format!(
                "https://127.0.0.1:{server_port}/rest/api/2/myself"
            )))
            .await
            .unwrap_err()
            .to_string();
        assert!(!err.contains("proxy"), "{err}");
        let seen = server.await.unwrap();
        // No CONNECT: the first bytes are the TLS handshake itself.
        assert_eq!(seen.after.first(), Some(&0x16), "{:?}", seen.after);
        proxy.abort();
    }
}
