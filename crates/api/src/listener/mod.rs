//! Where the API listens: a unix socket (Unix), a named pipe (Windows), or, for development only
//! and only when asked, loopback TCP.

#[cfg(windows)]
mod pipe;
#[cfg(windows)]
#[allow(unsafe_code)]
mod pipe_security;
#[cfg(unix)]
mod unix;

#[cfg(windows)]
pub use pipe::{NamedPipe, PipeAddr};
#[cfg(unix)]
pub use unix::{SOCKET_NAME, UnixSocket};

use axum::Router;
use axum::extract::Request;
use axum::http::header::HOST;
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use pitcrew_auth::ErrorResponse;
use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::path::PathBuf;

/// Where to listen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Listen {
    /// A unix socket named [`SOCKET_NAME`] in this directory, which is made 0700. Unix only.
    Unix {
        /// The socket's directory.
        dir: PathBuf,
    },
    /// A named pipe, e.g. `\\.\pipe\pitcrewd-<user>`. Windows only.
    Pipe {
        /// The full pipe name.
        name: String,
    },
    /// Loopback TCP, **for development only**. Any local user can connect, so bearer tokens are
    /// the only protection. Refused for non-loopback addresses. Requests must be addressed to
    /// `localhost`, `127.0.0.1`, `[::1]` or `*.localhost` (a DNS-rebinding guard).
    DevTcp {
        /// A loopback address; port 0 picks a free port.
        addr: SocketAddr,
    },
}

impl Listen {
    /// The platform's private transport: a unix socket in `run_dir` on Unix, or on Windows the
    /// pipe `\\.\pipe\pitcrewd-<user>` (`run_dir` is unused).
    #[must_use]
    pub fn private_default(run_dir: PathBuf) -> Self {
        if cfg!(windows) {
            let user = std::env::var("USERNAME").unwrap_or_else(|_| "user".to_owned());
            let _ = run_dir;
            Self::Pipe {
                name: format!(r"\\.\pipe\pitcrewd-{user}"),
            }
        } else {
            Self::Unix { dir: run_dir }
        }
    }
}

/// A bound listener, ready to serve.
#[derive(Debug)]
pub enum Bound {
    /// A unix socket.
    #[cfg(unix)]
    Unix(UnixSocket),
    /// A named pipe.
    #[cfg(windows)]
    Pipe(NamedPipe),
    /// Loopback TCP (development).
    DevTcp(tokio::net::TcpListener),
}

impl Bound {
    /// Binds `listen`.
    ///
    /// # Errors
    /// The transport is not available on this platform, a TCP address is not loopback, or
    /// binding fails (see [`UnixSocket::bind`] and [`NamedPipe::bind`]).
    pub async fn bind(listen: &Listen) -> io::Result<Self> {
        match listen {
            #[cfg(unix)]
            Listen::Unix { dir } => UnixSocket::bind(dir).map(Self::Unix),
            #[cfg(windows)]
            Listen::Pipe { name } => NamedPipe::bind(name).map(Self::Pipe),
            Listen::DevTcp { addr } => {
                if !addr.ip().is_loopback() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("refusing to listen on {addr}: only loopback is allowed"),
                    ));
                }
                tracing::warn!(%addr, "listening on loopback TCP: for development only");
                tokio::net::TcpListener::bind(addr).await.map(Self::DevTcp)
            }
            #[allow(unreachable_patterns)]
            other => Err(io::Error::new(
                io::ErrorKind::Unsupported,
                format!("{other:?} is not available on this platform"),
            )),
        }
    }

    /// Where it listens, for people and logs.
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            #[cfg(unix)]
            Self::Unix(socket) => socket.path().display().to_string(),
            #[cfg(windows)]
            Self::Pipe(pipe) => pipe.name().to_owned(),
            Self::DevTcp(tcp) => tcp
                .local_addr()
                .map_or_else(|e| format!("tcp (unknown: {e})"), |a| format!("http://{a}")),
        }
    }

    /// The TCP address, for [`Bound::DevTcp`].
    #[must_use]
    pub fn tcp_addr(&self) -> Option<SocketAddr> {
        match self {
            Self::DevTcp(tcp) => tcp.local_addr().ok(),
            #[allow(unreachable_patterns)]
            _ => None,
        }
    }

    /// Serves `app` until `shutdown` completes, then finishes in-flight requests.
    ///
    /// # Errors
    /// Only as `axum::serve` reports them; accept errors are logged and retried.
    pub async fn serve<F>(self, app: Router, shutdown: F) -> io::Result<()>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        match self {
            #[cfg(unix)]
            Self::Unix(socket) => {
                axum::serve(socket, app)
                    .with_graceful_shutdown(shutdown)
                    .await
            }
            #[cfg(windows)]
            Self::Pipe(pipe) => {
                axum::serve(pipe, app)
                    .with_graceful_shutdown(shutdown)
                    .await
            }
            Self::DevTcp(tcp) => {
                let app = app.layer(middleware::from_fn(local_host_only));
                axum::serve(tcp, app).with_graceful_shutdown(shutdown).await
            }
        }
    }
}

/// The DNS-rebinding guard for development TCP.
async fn local_host_only(request: Request, next: Next) -> Response {
    let host = request
        .headers()
        .get(HOST)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");
    if is_local_host(host) {
        next.run(request).await
    } else {
        ErrorResponse::forbidden("Requests must be addressed to localhost.").into_response()
    }
}

fn is_local_host(host: &str) -> bool {
    let name = if let Some(rest) = host.strip_prefix('[') {
        rest.split_once(']').map_or("", |(ip, _)| ip)
    } else {
        host.rsplit_once(':').map_or(host, |(name, _)| name)
    };
    let name = name.to_ascii_lowercase();
    name == "localhost" || name == "127.0.0.1" || name == "::1" || name.ends_with(".localhost")
}

/// As axum's own listeners do: skip per-connection errors, back off on others (e.g. out of file
/// descriptors).
#[cfg(unix)]
async fn accept_error(e: io::Error) {
    if matches!(
        e.kind(),
        io::ErrorKind::ConnectionRefused
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::ConnectionReset
    ) {
        return;
    }
    tracing::error!(error = %e, "accept failed; retrying in 1 s");
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_hosts() {
        for host in [
            "localhost",
            "localhost:47317",
            "127.0.0.1:1",
            "[::1]:80",
            "app.localhost",
            "LOCALHOST",
        ] {
            assert!(is_local_host(host), "{host}");
        }
        for host in ["", "example.com", "127.0.0.1.example.com:80", "[::2]:80"] {
            assert!(!is_local_host(host), "{host}");
        }
    }

    #[tokio::test]
    async fn tcp_must_be_loopback() {
        let err = Bound::bind(&Listen::DevTcp {
            addr: "0.0.0.0:0".parse().unwrap(),
        })
        .await
        .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }
}
