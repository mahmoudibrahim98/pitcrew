//! The gateway's Tauri commands, as `docs/build/contracts/desktop-gateway.md` names them. Their
//! arguments are top-level keys of `invoke`'s second argument:
//!
//! ```js
//! invoke('gateway_workspaces')
//! invoke('gateway_request', { req: { workspace, method, path, body } })
//! invoke('gateway_socket_open', { workspace, path, events: new Channel() })  // → { socket }
//! invoke('gateway_socket_send', { socket, text })   // or { socket, binary }
//! invoke('gateway_socket_close', { socket, code, reason })
//! // Remote workspaces:
//! invoke('gateway_ssh_hosts')                                    // → { hosts }
//! invoke('gateway_remote_probe', { host })                       // → RemoteProbe
//! invoke('gateway_remote_plan', { req: { host, launcher, site, job } })  // → RemotePlan
//! invoke('gateway_remote_add', { plan, events: new Channel() })  // → GatewayWorkspace
//! invoke('gateway_remote_cancel', { plan })
//! invoke('gateway_workspace_retry', { workspace })
//! invoke('gateway_workspace_remove', { workspace, stopHelper })
//! invoke('gateway_prompt_reply', { id, answer })                 // or { id, accept }, or { id }
//! ```
//!
//! Every argument is taken as raw JSON and checked here, so a malformed call fails with a
//! `GatewayError` (`invalid`) like any other gateway failure, not with Tauri's own message.

use crate::app::MAIN;
use crate::gateway::{
    Delivery, Gateway, GatewayError, GatewayRequest, GatewayResponse, Payload, Sink, SinkClosed,
};
use crate::navigate::Navigator;
use crate::registry::GatewayWorkspace;
use crate::remote::{
    AddProgress, Progress, RemotePlan, RemotePlanRequest, RemoteProbe, Remotes, SshHosts,
};
use pitcrew_remote::Secret;
use serde::Serialize;
use serde_json::Value;
use std::str::FromStr as _;
use std::sync::{Arc, Mutex};
use tauri::ipc::{Channel, InvokeResponseBody, JavaScriptChannelId};
use tauri::{Manager as _, Runtime, State, Webview};

/// `gateway_workspaces() → GatewayWorkspace[]`.
///
/// The UI asks for the list once it listens to the gateway's events, so from the main window
/// this also releases a navigation held for the page ([`Navigator::page_listening`]), and emits
/// again the SSH prompts still open since the page last loaded.
#[tauri::command]
pub fn gateway_workspaces<R: Runtime>(
    gateway: State<'_, Gateway>,
    webview: Webview<R>,
) -> Vec<GatewayWorkspace> {
    let list = gateway.workspaces();
    if webview.label() == MAIN {
        if let Some(navigator) = webview.try_state::<Navigator>() {
            navigator.page_listening(webview.app_handle());
        }
        if let Some(remotes) = webview.try_state::<Remotes>() {
            remotes.prompts().page_listening();
        }
    }
    list
}

/// `gateway_ssh_hosts() → { hosts: string[] }`. Async, so the config is never read on the main
/// thread.
#[tauri::command]
pub async fn gateway_ssh_hosts<R: Runtime>(webview: Webview<R>) -> Result<SshHosts, GatewayError> {
    Ok(remotes(&webview)?.ssh_hosts().await)
}

/// `gateway_workspace_retry({ workspace })`: tries a remote workspace's connection again now.
#[tauri::command]
pub async fn gateway_workspace_retry<R: Runtime>(
    webview: Webview<R>,
    workspace: Option<Value>,
) -> Result<(), GatewayError> {
    let workspace = string(workspace, "workspace")?;
    remotes(&webview)?.retry(&workspace)
}

/// `gateway_remote_probe({ host }) → RemoteProbe`.
#[tauri::command]
pub async fn gateway_remote_probe<R: Runtime>(
    webview: Webview<R>,
    host: Option<Value>,
) -> Result<RemoteProbe, GatewayError> {
    let host = string(host, "host")?;
    remotes(&webview)?.probe(&host).await
}

/// `gateway_remote_plan(req) → RemotePlan`.
#[tauri::command]
pub async fn gateway_remote_plan<R: Runtime>(
    webview: Webview<R>,
    req: Option<Value>,
) -> Result<RemotePlan, GatewayError> {
    let req: RemotePlanRequest = serde_json::from_value(required(req, "req")?)
        .map_err(|e| GatewayError::invalid(format!("req is not a RemotePlanRequest: {e}")))?;
    remotes(&webview)?.plan(req).await
}

/// `gateway_remote_add({ plan, events }) → GatewayWorkspace`. Progress goes on `events` as
/// `{ step, state, detail? }`, ending with one `{ step: "add", state: "done" | "failed" }`.
#[tauri::command]
pub async fn gateway_remote_add<R: Runtime>(
    webview: Webview<R>,
    plan: Option<Value>,
    events: Option<Value>,
) -> Result<GatewayWorkspace, GatewayError> {
    let plan = string(plan, "plan")?;
    let events = string(events, "events")?;
    let channel = JavaScriptChannelId::from_str(&events)
        .map_err(|_| GatewayError::invalid("events must be a Channel"))?
        .channel_on::<R, InvokeResponseBody>(webview.clone());
    let progress: Progress = Arc::new(move |message: &AddProgress| {
        if let Ok(json) = serde_json::to_string(message) {
            let _ = channel.send(InvokeResponseBody::Json(json));
        }
    });
    remotes(&webview)?.add(&plan, progress).await
}

/// `gateway_remote_cancel({ plan })`: stops the add carrying out `plan`, if one is running, and
/// undoes what it started; that add then ends `failed`. A plan not used yet is dropped. An add
/// that has finished, or an unknown plan, is left alone.
#[tauri::command]
pub fn gateway_remote_cancel<R: Runtime>(
    webview: Webview<R>,
    plan: Option<Value>,
) -> Result<(), GatewayError> {
    let plan = string(plan, "plan")?;
    remotes(&webview)?.cancel(&plan);
    Ok(())
}

/// `gateway_workspace_remove({ workspace, stopHelper })`.
#[tauri::command]
pub async fn gateway_workspace_remove<R: Runtime>(
    webview: Webview<R>,
    workspace: Option<Value>,
    stop_helper: Option<Value>,
) -> Result<(), GatewayError> {
    let workspace = string(workspace, "workspace")?;
    let stop_helper = match required(stop_helper, "stopHelper")? {
        Value::Bool(stop) => stop,
        _ => return Err(GatewayError::invalid("stopHelper must be true or false")),
    };
    remotes(&webview)?.remove(&workspace, stop_helper).await
}

/// `gateway_prompt_reply({ id, answer?, accept? })`: `answer` for a password, passphrase or
/// one-time code, `accept` for a host key or a `confirm`, neither for a `notice`; neither cancels
/// any prompt. The answer is passed to ssh once, and never kept or logged.
#[tauri::command]
pub fn gateway_prompt_reply<R: Runtime>(
    webview: Webview<R>,
    id: Option<Value>,
    answer: Option<Value>,
    accept: Option<Value>,
) -> Result<(), GatewayError> {
    let id = string(id, "id")?;
    let answer = match present(answer) {
        None => None,
        Some(Value::String(text)) => Some(Secret::new(text)),
        Some(_) => return Err(GatewayError::invalid("answer must be a string")),
    };
    let accept = match present(accept) {
        None => None,
        Some(Value::Bool(accept)) => Some(accept),
        Some(_) => return Err(GatewayError::invalid("accept must be true or false")),
    };
    remotes(&webview)?.prompts().reply(&id, answer, accept)
}

/// The remote workspaces, which the app manages (tests may not).
fn remotes<'a, R: Runtime>(webview: &'a Webview<R>) -> Result<State<'a, Remotes>, GatewayError> {
    webview
        .try_state::<Remotes>()
        .ok_or_else(|| GatewayError::internal("remote workspaces are not set up"))
}

/// `gateway_request(req) → GatewayResponse`.
#[tauri::command]
pub async fn gateway_request(
    gateway: State<'_, Gateway>,
    req: Option<Value>,
) -> Result<GatewayResponse, GatewayError> {
    let req: GatewayRequest = serde_json::from_value(required(req, "req")?)
        .map_err(|e| GatewayError::invalid(format!("req is not a GatewayRequest: {e}")))?;
    gateway.request(req).await
}

/// What `gateway_socket_open` resolves with.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct SocketOpened {
    /// The socket's number, for `gateway_socket_send` and `gateway_socket_close`.
    pub socket: u32,
}

/// `gateway_socket_open({ workspace, path, events }) → { socket }`.
#[tauri::command]
pub async fn gateway_socket_open<R: Runtime>(
    gateway: State<'_, Gateway>,
    webview: Webview<R>,
    workspace: Option<Value>,
    path: Option<Value>,
    events: Option<Value>,
) -> Result<SocketOpened, GatewayError> {
    let workspace = string(workspace, "workspace")?;
    let path = string(path, "path")?;
    let events = string(events, "events")?;
    let channel = JavaScriptChannelId::from_str(&events)
        .map_err(|_| GatewayError::invalid("events must be a Channel"))?
        .channel_on::<R, InvokeResponseBody>(webview.clone());
    let sink = Arc::new(ChannelSink {
        channel,
        webview: webview.clone(),
    });
    let socket = gateway
        .socket_open(webview.label(), &workspace, &path, sink)
        .await?;
    Ok(SocketOpened { socket })
}

/// `gateway_socket_send({ socket, text? , binary? })`.
#[tauri::command]
pub async fn gateway_socket_send<R: Runtime>(
    gateway: State<'_, Gateway>,
    webview: Webview<R>,
    socket: Option<Value>,
    text: Option<Value>,
    binary: Option<Value>,
) -> Result<(), GatewayError> {
    let socket = socket_number(socket)?;
    let payload = match (present(text), present(binary)) {
        (Some(Value::String(text)), None) => Payload::Text(text),
        (None, Some(binary)) => Payload::Binary(bytes(binary)?),
        (Some(_), None) => return Err(GatewayError::invalid("text must be a string")),
        _ => {
            return Err(GatewayError::invalid("send exactly one of text and binary"));
        }
    };
    gateway.socket_send(webview.label(), socket, payload).await
}

/// `gateway_socket_close({ socket, code?, reason? })`.
#[tauri::command]
pub async fn gateway_socket_close<R: Runtime>(
    gateway: State<'_, Gateway>,
    webview: Webview<R>,
    socket: Option<Value>,
    code: Option<Value>,
    reason: Option<Value>,
) -> Result<(), GatewayError> {
    let socket = socket_number(socket)?;
    let code = match present(code) {
        None => None,
        Some(code) => Some(
            code.as_u64()
                .and_then(|c| u16::try_from(c).ok())
                .ok_or_else(|| GatewayError::invalid("code must be a close code"))?,
        ),
    };
    let reason = match present(reason) {
        None => None,
        Some(Value::String(reason)) => Some(reason),
        Some(_) => return Err(GatewayError::invalid("reason must be a string")),
    };
    gateway.socket_close(webview.label(), socket, code, reason)
}

/// A socket's messages, on the webview's `events` channel: text and close as JSON, binary as an
/// `ArrayBuffer`. A probe is a no-op script evaluated after them.
struct ChannelSink<R: Runtime> {
    channel: Channel<InvokeResponseBody>,
    webview: Webview<R>,
}

impl<R: Runtime> Sink for ChannelSink<R> {
    fn deliver(&self, message: Delivery) -> Result<(), SinkClosed> {
        let body = match message {
            Delivery::Binary(bytes) => InvokeResponseBody::Raw(bytes),
            other => InvokeResponseBody::Json(other.json().unwrap_or_default()),
        };
        self.channel.send(body).map_err(|_| SinkClosed)
    }

    fn probe(&self, done: Box<dyn FnOnce() + Send>) -> Result<(), SinkClosed> {
        let done = Mutex::new(Some(done));
        self.webview
            .eval_with_callback("0", move |_| {
                if let Some(done) = done.lock().ok().and_then(|mut d| d.take()) {
                    done();
                }
            })
            .map_err(|_| SinkClosed)
    }
}

fn present(value: Option<Value>) -> Option<Value> {
    value.filter(|v| !v.is_null())
}

fn required(value: Option<Value>, name: &str) -> Result<Value, GatewayError> {
    present(value).ok_or_else(|| GatewayError::invalid(format!("{name} is missing")))
}

fn string(value: Option<Value>, name: &str) -> Result<String, GatewayError> {
    match required(value, name)? {
        Value::String(s) => Ok(s),
        _ => Err(GatewayError::invalid(format!("{name} must be a string"))),
    }
}

fn socket_number(value: Option<Value>) -> Result<u32, GatewayError> {
    required(value, "socket")?
        .as_u64()
        .and_then(|n| u32::try_from(n).ok())
        .filter(|n| *n > 0)
        .ok_or_else(|| GatewayError::invalid("socket must be a socket number"))
}

/// `number[]` (a `Uint8Array` arrives as one) into bytes.
fn bytes(value: Value) -> Result<Vec<u8>, GatewayError> {
    let Value::Array(items) = value else {
        return Err(GatewayError::invalid(
            "binary must be an array of bytes or a Uint8Array",
        ));
    };
    items
        .into_iter()
        .map(|item| {
            item.as_u64()
                .and_then(|b| u8::try_from(b).ok())
                .ok_or_else(|| GatewayError::invalid("binary must hold bytes (0 to 255)"))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gateway::ErrorCode;
    use serde_json::json;

    #[test]
    fn arguments_are_checked() {
        assert_eq!(socket_number(Some(json!(3))), Ok(3));
        for bad in [
            json!(0),
            json!(-1),
            json!(1.5),
            json!("3"),
            json!(u64::MAX),
            Value::Null,
        ] {
            assert_eq!(
                socket_number(Some(bad.clone())).unwrap_err().code,
                ErrorCode::Invalid,
                "{bad}"
            );
        }
        assert_eq!(socket_number(None).unwrap_err().code, ErrorCode::Invalid);
        assert_eq!(bytes(json!([0, 1, 255])), Ok(vec![0, 1, 255]));
        assert!(bytes(json!([256])).is_err());
        assert!(bytes(json!([-1])).is_err());
        assert!(bytes(json!({ "0": 1 })).is_err());
        assert_eq!(string(Some(json!("x")), "w"), Ok("x".into()));
        assert!(string(Some(json!(1)), "w").is_err());
        assert!(string(None, "w").is_err());
    }
}
