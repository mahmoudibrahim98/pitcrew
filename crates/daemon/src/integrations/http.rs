//! The tracker sync's transports (`pitcrew_sync_github::Transport`, which Jira's crate shares):
//!
//! - [`HttpsTransport`]: the real one. HTTP/1.1 over TLS (rustls, with the `ring` provider and
//!   this machine's own trusted certificates), one connection per request, `https://` only, with
//!   time limits and a cap on the response body. It adds `User-Agent` (GitHub refuses requests
//!   without one) and `Host`, and sends the crates' headers as they are. It never logs a header or
//!   a body.
//! - [`FixtureTransport`]: recorded exchanges from a folder of `*.fixture` files (the sync crates'
//!   format), answered by method and URL, as often as asked; for tests only (`serve
//!   --integration-fixtures`). It never reaches the network.

use bytes::Bytes;
use http_body_util::{BodyExt as _, Empty, Limited};
use hyper_util::rt::TokioIo;
use pitcrew_sync_github::fixture::{RecordedExchange, parse_fixture};
use pitcrew_sync_github::transport::{Request, Response, Transport, TransportError};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
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
}

impl std::fmt::Debug for HttpsTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpsTransport")
            .field("user_agent", &self.user_agent)
            .finish_non_exhaustive()
    }
}

impl HttpsTransport {
    /// A transport trusting this machine's certificates.
    ///
    /// # Errors
    /// No certificate could be loaded, or TLS cannot be set up.
    pub fn new() -> anyhow::Result<Self> {
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
            let tcp = tokio::net::TcpStream::connect((host.as_str(), port))
                .await
                .map_err(|e| failed(&request.url, format!("cannot connect: {}", e.kind())))?;
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
        let mut builder = hyper::Request::builder()
            .method(hyper::Method::GET)
            .uri(path)
            .header(hyper::header::HOST, authority)
            .header(hyper::header::USER_AGENT, &self.user_agent);
        for (name, value) in &request.headers {
            builder = builder.header(name.as_str(), value.as_str());
        }
        let outgoing = builder
            .body(Empty::<Bytes>::new())
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

/// Recorded exchanges, answered by method and URL (tests only). See the [module docs](self).
#[derive(Debug, Clone, Default)]
pub struct FixtureTransport {
    exchanges: Arc<HashMap<(String, String), RecordedExchange>>,
}

impl FixtureTransport {
    /// Every exchange in the `*.fixture` files of `dir`; the first of two for one method and URL
    /// wins.
    ///
    /// # Errors
    /// The folder or a file cannot be read, or a file is not in the fixture format.
    pub fn load(dir: &Path) -> anyhow::Result<Self> {
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
        Ok(Self {
            exchanges: Arc::new(exchanges),
        })
    }

    /// The exchange for `request`: its URL, else its URL without a `since=` parameter (a fixture
    /// for a first read answers later incremental ones too, as if nothing changed upstream).
    fn find(&self, request: &Request) -> Option<&RecordedExchange> {
        let method = request.method.as_str().to_owned();
        self.exchanges
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
                self.exchanges.get(&(method, url))
            })
    }
}

impl Transport for FixtureTransport {
    async fn send(&self, request: Request) -> Result<Response, TransportError> {
        match self.find(&request) {
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
    }

    #[tokio::test]
    async fn https_refuses_other_schemes_and_user_info_before_connecting() {
        let https = HttpsTransport {
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
        };
        for url in [
            "http://api.github.com/repos/example-org/demo-repo",
            "https://user:pass@api.github.com/repos/example-org/demo-repo",
            "file:///etc/hosts",
            "not a url",
        ] {
            assert!(https.send(get(url)).await.is_err(), "{url}");
        }
    }
}
