//! Calls to the daemon's API: one checked connection per request.

use crate::config::{Endpoint, Env, token_from_env};
use crate::error::{Error, Kind, Result};
use crate::http::{self, Request, Response};
use crate::transport::Timeouts;
use pitcrew_protocol::{PROTOCOL_VERSION, api::API_PREFIX};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

/// Largest response body accepted.
const MAX_BODY: usize = 64 << 20;

/// The part of `HostInfo` the CLI needs; the rest may change without breaking it.
#[derive(Clone, Debug, Deserialize)]
pub struct Host {
    /// `pitcrewd`.
    pub name: String,
    /// Daemon version.
    pub version: String,
    /// Newest protocol it speaks.
    pub protocol: u32,
    /// Oldest protocol it accepts.
    pub protocol_min: u32,
}

/// A daemon to call, with the token to call it with.
#[derive(Debug)]
pub struct Client {
    endpoint: Endpoint,
    token: String,
    timeouts: Timeouts,
}

impl Client {
    /// From the environment (see [`crate::config`]).
    ///
    /// # Errors
    /// No usable endpoint or token.
    pub fn from_env(env: Env<'_>, timeouts: Timeouts) -> Result<Self> {
        let endpoint = Endpoint::from_env(env)?;
        let token = token_from_env(env)?;
        Ok(Self {
            endpoint,
            token,
            timeouts,
        })
    }

    /// Checks that the daemon speaks our protocol (`GET /v1/host/info`, without the token).
    ///
    /// # Errors
    /// `incompatible` when it does not; any transport or response error.
    pub fn check_version(&self) -> Result<Host> {
        let response = self.send("GET", "/host/info", None, false)?;
        if !response.is_success() {
            return Err(Error::from_response(response.status, &response.body));
        }
        let host: Host = decode(&response.body)?;
        if host.protocol_min <= PROTOCOL_VERSION && PROTOCOL_VERSION <= host.protocol {
            Ok(host)
        } else {
            Err(Error::new(
                Kind::Incompatible,
                format!(
                    "the daemon ({} {}) speaks protocol {}–{}, and this pitcrew speaks {}; \
                     update the older one",
                    host.name, host.version, host.protocol_min, host.protocol, PROTOCOL_VERSION
                ),
            ))
        }
    }

    /// `GET /v1{path}`, parsed.
    ///
    /// # Errors
    /// Transport errors, or the daemon's error.
    pub fn get(&self, path: &str) -> Result<Value> {
        self.call("GET", path, None)
    }

    /// `POST /v1{path}` with a JSON body.
    ///
    /// # Errors
    /// Transport errors, or the daemon's error.
    pub fn post(&self, path: &str, body: &Value) -> Result<Value> {
        self.call("POST", path, Some(body))
    }

    /// `PUT /v1{path}` with a JSON body.
    ///
    /// # Errors
    /// Transport errors, or the daemon's error.
    pub fn put(&self, path: &str, body: &Value) -> Result<Value> {
        self.call("PUT", path, Some(body))
    }

    fn call(&self, method: &str, path: &str, body: Option<&Value>) -> Result<Value> {
        let bytes = body
            .map(serde_json::to_vec)
            .transpose()
            .map_err(|e| Error::internal(format!("cannot encode the request: {e}")))?;
        let response = self.send(method, path, bytes.as_deref(), true)?;
        if !response.is_success() {
            return Err(Error::from_response(response.status, &response.body));
        }
        if response.body.is_empty() {
            return Ok(Value::Null);
        }
        decode(&response.body)
    }

    /// Sends raw bytes and returns the raw response. The token goes only to a server that passed
    /// the transport's identity check.
    ///
    /// # Errors
    /// `unavailable` for transport failures; the transport's own errors.
    pub fn send(
        &self,
        method: &str,
        path: &str,
        body: Option<&[u8]>,
        with_token: bool,
    ) -> Result<Response> {
        let mut conn = self.endpoint.connect(self.timeouts)?;
        let target = format!("{API_PREFIX}{path}");
        let request = Request {
            method,
            target: &target,
            host: self.endpoint.host_header(),
            token: with_token.then_some(self.token.as_str()),
            body,
        };
        http::exchange(&mut conn, &request, MAX_BODY).map_err(|e| {
            let kind = match e.kind() {
                std::io::ErrorKind::InvalidData => Kind::Internal,
                _ => Kind::Unavailable,
            };
            Error::new(
                kind,
                format!(
                    "{method} {target} to {} failed: {e}",
                    self.endpoint.describe()
                ),
            )
        })
    }
}

/// Decodes a JSON body into `T`.
///
/// # Errors
/// `internal`: the daemon answered something unexpected.
pub fn decode<T: DeserializeOwned>(body: &[u8]) -> Result<T> {
    serde_json::from_slice(body)
        .map_err(|e| Error::internal(format!("unexpected answer from the daemon: {e}")))
}

/// Converts an already parsed value into `T`.
///
/// # Errors
/// `internal`: the daemon answered something unexpected.
pub fn from_value<T: DeserializeOwned>(value: Value) -> Result<T> {
    serde_json::from_value(value)
        .map_err(|e| Error::internal(format!("unexpected answer from the daemon: {e}")))
}
