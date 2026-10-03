//! Where the API listens: a unix socket (Unix), a named pipe (Windows), or, for development only
//! and only when asked, loopback TCP.

#[cfg(windows)]
mod pipe;
#[cfg(windows)]
pub(crate) mod pipe_security;
#[cfg(unix)]
mod unix;

#[cfg(windows)]
pub use pipe::{NamedPipe, PipeAddr};
#[cfg(unix)]
pub use unix::{SOCKET_NAME, UnixSocket};

use crate::util::HubShutdown;
use axum::extract::Request;
use axum::http::header::HOST;
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Router};
use pitcrew_auth::ErrorResponse;
use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// Where to listen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Listen {
    /// A unix socket named [`SOCKET_NAME`] in this directory, which is made 0700. Unix only.
    Unix {
        /// The socket's directory.
        dir: PathBuf,
    },
    /// A named pipe, e.g. `\\.\pipe\pitcrewd-<user SID>` (`default_pipe_name`). Windows only.
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
    /// pipe `\\.\pipe\pitcrewd-<user SID>` (`run_dir` is unused). The SID, unlike the user name,
    /// cannot be chosen by another user.
    ///
    /// # Errors
    /// On Windows, the current user's SID cannot be read.
    pub fn private_default(run_dir: PathBuf) -> io::Result<Self> {
        #[cfg(windows)]
        {
            let _ = run_dir;
            Ok(Self::Pipe {
                name: default_pipe_name()?,
            })
        }
        #[cfg(not(windows))]
        {
            Ok(Self::Unix { dir: run_dir })
        }
    }
}

/// `\\.\pipe\pitcrewd-<current user's SID>`.
///
/// # Errors
/// The SID cannot be read.
#[cfg(windows)]
pub fn default_pipe_name() -> io::Result<String> {
    Ok(format!(
        r"\\.\pipe\pitcrewd-{}",
        pipe_security::current_user_sid()?
    ))
}

/// A bound listener, ready to serve. Only [`Bound::bind`] makes one, so its checks (such as
/// loopback-only TCP) cannot be skipped.
#[derive(Debug)]
pub struct Bound(Inner);

#[derive(Debug)]
enum Inner {
    #[cfg(unix)]
    Unix(UnixSocket),
    #[cfg(windows)]
    Pipe(NamedPipe),
    DevTcp(tokio::net::TcpListener),
}

impl Bound {
    /// Binds `listen`.
    ///
    /// # Errors
    /// The transport is not available on this platform, a TCP address is not loopback, or
    /// binding fails (see `UnixSocket::bind` and `NamedPipe::bind`).
    pub async fn bind(listen: &Listen) -> io::Result<Self> {
        let inner = match listen {
            #[cfg(unix)]
            Listen::Unix { dir } => Inner::Unix(UnixSocket::bind(dir)?),
            #[cfg(windows)]
            Listen::Pipe { name } => Inner::Pipe(NamedPipe::bind(name)?),
            Listen::DevTcp { addr } => {
                if !addr.ip().is_loopback() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("refusing to listen on {addr}: only loopback is allowed"),
                    ));
                }
                tracing::warn!(%addr, "listening on loopback TCP: for development only");
                Inner::DevTcp(tokio::net::TcpListener::bind(addr).await?)
            }
            #[allow(unreachable_patterns)]
            other => {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    format!("{other:?} is not available on this platform"),
                ));
            }
        };
        Ok(Self(inner))
    }

    /// Where it listens, for people and logs.
    #[must_use]
    pub fn describe(&self) -> String {
        match &self.0 {
            #[cfg(unix)]
            Inner::Unix(socket) => socket.path().display().to_string(),
            #[cfg(windows)]
            Inner::Pipe(pipe) => pipe.name().to_owned(),
            Inner::DevTcp(tcp) => tcp
                .local_addr()
                .map_or_else(|e| format!("tcp (unknown: {e})"), |a| format!("http://{a}")),
        }
    }

    /// The TCP address, when listening on development TCP.
    #[must_use]
    pub fn tcp_addr(&self) -> Option<SocketAddr> {
        match &self.0 {
            Inner::DevTcp(tcp) => tcp.local_addr().ok(),
            #[allow(unreachable_patterns)]
            _ => None,
        }
    }

    /// Serves `app` until `shutdown` completes, then finishes in-flight requests.
    ///
    /// WebSockets (the stream, terminals) are told when `shutdown` completes and close with 1001;
    /// this waits up to two seconds for them before returning.
    ///
    /// # Errors
    /// Only as `axum::serve` reports them; accept errors are logged and retried.
    pub async fn serve<F>(self, app: Router, shutdown: F) -> io::Result<()>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let (going, signal) = HubShutdown::channel();
        let going = Arc::new(going);
        let app = app.layer(Extension(signal));
        let announce = Arc::clone(&going);
        let shutdown = async move {
            shutdown.await;
            announce.send_replace(true);
        };
        let served = match self.0 {
            #[cfg(unix)]
            Inner::Unix(socket) => {
                axum::serve(socket, app)
                    .with_graceful_shutdown(shutdown)
                    .await
            }
            #[cfg(windows)]
            Inner::Pipe(pipe) => {
                axum::serve(pipe, app)
                    .with_graceful_shutdown(shutdown)
                    .await
            }
            Inner::DevTcp(tcp) => {
                let app = app.layer(middleware::from_fn(local_host_only));
                axum::serve(tcp, app).with_graceful_shutdown(shutdown).await
            }
        };
        // Upgraded connections left hyper, so its graceful shutdown does not wait for them. Each
        // open socket holds a receiver: wait (briefly) until they have all said goodbye.
        going.send_replace(true);
        let _ = tokio::time::timeout(SOCKETS_GRACE, going.closed()).await;
        served
    }
}

/// How long [`Bound::serve`] waits for open WebSockets to close after shutdown.
const SOCKETS_GRACE: Duration = Duration::from_secs(2);

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
