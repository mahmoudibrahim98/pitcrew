//! One HTTP/1.1 request over a daemon's stream (hyper 1). Only the status, the `Content-Type` and
//! the body come back: no other daemon header is read.

use crate::daemon::endpoint::BoxIo;
use crate::token::DeviceToken;
use bytes::Bytes;
use http::header::{AUTHORIZATION, CONTENT_TYPE, HOST};
use http::{HeaderValue, Method, Request};
use http_body_util::{BodyExt as _, Full, LengthLimitError, Limited};
use hyper_util::rt::TokioIo;
use std::time::Duration;

/// What the daemon answered.
#[derive(Debug)]
pub(crate) struct Reply {
    pub status: u16,
    pub content_type: Option<String>,
    pub body: Bytes,
}

/// Why no answer came back.
#[derive(Debug, thiserror::Error)]
pub(crate) enum HttpError {
    /// The connection failed or broke before a whole answer arrived.
    #[error("the connection to the daemon broke: {0}")]
    Broken(String),
    /// The answer's body is over the limit.
    #[error("the daemon's answer is over {0} bytes")]
    TooLarge(usize),
    /// No answer in time.
    #[error("the daemon did not answer within {0} s")]
    Timeout(u64),
    /// The request could not be built.
    #[error("{0}")]
    Bad(String),
}

/// Sends one request on `io` and reads the whole answer, at most `max_body` bytes of body.
///
/// The gateway sets `Host`, `Content-Type` (with a body) and, when given a token,
/// `Authorization`, which is marked sensitive so it is never printed.
pub(crate) async fn send(
    io: BoxIo,
    token: Option<&DeviceToken>,
    method: Method,
    path: &str,
    body: Option<Bytes>,
    max_body: usize,
    timeout: Duration,
) -> Result<Reply, HttpError> {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header(HOST, "localhost");
    if let Some(token) = token {
        let mut value = HeaderValue::try_from(format!("Bearer {}", token.expose()))
            .map_err(|_| HttpError::Bad("the device token cannot go in a header".into()))?;
        value.set_sensitive(true);
        request = request.header(AUTHORIZATION, value);
    }
    if body.is_some() {
        request = request.header(CONTENT_TYPE, "application/json");
    }
    let request = request
        .body(Full::new(body.unwrap_or_default()))
        .map_err(|e| HttpError::Bad(format!("cannot build the request: {e}")))?;

    let exchange = async move {
        let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(io))
            .await
            .map_err(|e| HttpError::Broken(e.to_string()))?;
        let _driver = AbortOnDrop(tokio::spawn(async move {
            let _ = connection.await;
        }));
        let response = sender
            .send_request(request)
            .await
            .map_err(|e| HttpError::Broken(e.to_string()))?;
        let status = response.status().as_u16();
        let content_type = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let body = Limited::new(response.into_body(), max_body)
            .collect()
            .await
            .map_err(|e| {
                if e.downcast_ref::<LengthLimitError>().is_some() {
                    HttpError::TooLarge(max_body)
                } else {
                    HttpError::Broken(e.to_string())
                }
            })?
            .to_bytes();
        Ok(Reply {
            status,
            content_type,
            body,
        })
    };
    tokio::time::timeout(timeout, exchange)
        .await
        .map_err(|_| HttpError::Timeout(timeout.as_secs()))?
}

/// Aborts a task when dropped, so a connection never outlives its request.
struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}
