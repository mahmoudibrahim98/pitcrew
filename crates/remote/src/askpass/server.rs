//! The listener side: one per ssh call, alive until the call ends.

use super::{
    ADDR_ENV, Ask, Hello, KEY_ENV, KEY_LEN, MAX_LINE, NONCE_LEN, PromptHandler, PromptRequest,
    Reply, ServerHello, WireReply, client_proof, from_hex, random, same, server_proof, to_hex,
};
use std::io;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _};
use tokio::task::JoinHandle;

/// How long a client has to finish the handshake. The user's answer has no limit.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// A running listener. Dropping it stops listening and removes the socket.
pub(crate) struct AskpassServer {
    addr: String,
    key: [u8; KEY_LEN],
    cancelled: Arc<AtomicBool>,
    task: JoinHandle<()>,
}

#[derive(Clone)]
struct Shared {
    key: [u8; KEY_LEN],
    host: String,
    handler: Arc<dyn PromptHandler>,
    cancelled: Arc<AtomicBool>,
}

impl AskpassServer {
    /// Starts listening. `dir` must be a private directory (Unix; unused on Windows). Must be
    /// called inside a tokio runtime.
    pub(crate) fn start(
        dir: &Path,
        host: &str,
        handler: Arc<dyn PromptHandler>,
    ) -> io::Result<Self> {
        let key = random::<KEY_LEN>()?;
        let cancelled = Arc::new(AtomicBool::new(false));
        let shared = Shared {
            key,
            host: host.to_owned(),
            handler,
            cancelled: cancelled.clone(),
        };
        let name = to_hex(&random::<8>()?);
        let (addr, task) = listen(dir, &name, shared)?;
        Ok(Self {
            addr,
            key,
            cancelled,
            task,
        })
    }

    /// The variables `pitcrew-askpass` needs, besides `SSH_ASKPASS` itself.
    pub(crate) fn env(&self) -> [(&'static str, String); 2] {
        [(ADDR_ENV, self.addr.clone()), (KEY_ENV, to_hex(&self.key))]
    }

    /// Whether the user cancelled any prompt during this call.
    pub(crate) fn cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }
}

impl std::fmt::Debug for AskpassServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AskpassServer")
            .field("addr", &self.addr)
            .finish_non_exhaustive()
    }
}

impl Drop for AskpassServer {
    fn drop(&mut self) {
        self.task.abort();
        #[cfg(unix)]
        let _ = std::fs::remove_file(&self.addr);
        self.key.fill(0);
    }
}

#[cfg(unix)]
fn listen(dir: &Path, name: &str, shared: Shared) -> io::Result<(String, JoinHandle<()>)> {
    crate::private::ensure_private_dir(dir)?;
    let path = dir.join(format!("ask-{name}"));
    let addr = path
        .to_str()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "runtime dir is not UTF-8"))?
        .to_owned();
    let listener = tokio::net::UnixListener::bind(&path)?;
    let task = tokio::spawn(async move {
        let uid = crate::private::euid();
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                tokio::time::sleep(Duration::from_millis(50)).await;
                continue;
            };
            if !stream.peer_cred().is_ok_and(|c| c.uid() == uid) {
                continue;
            }
            tokio::spawn(serve(stream, shared.clone()));
        }
    });
    Ok((addr, task))
}

#[cfg(windows)]
fn listen(_dir: &Path, name: &str, shared: Shared) -> io::Result<(String, JoinHandle<()>)> {
    use tokio::net::windows::named_pipe::ServerOptions;
    let addr = format!(r"\\.\pipe\pitcrew-askpass-{name}");
    let create = |first: bool| {
        ServerOptions::new()
            .first_pipe_instance(first)
            .reject_remote_clients(true)
            .create(&addr)
    };
    // `first_pipe_instance` fails if the name exists, so nobody can have squatted it.
    let mut next = create(true)?;
    let task = {
        let addr = addr.clone();
        tokio::spawn(async move {
            loop {
                if next.connect().await.is_err() {
                    match ServerOptions::new()
                        .reject_remote_clients(true)
                        .create(&addr)
                    {
                        Ok(fresh) => next = fresh,
                        Err(_) => return,
                    }
                    continue;
                }
                let Ok(fresh) = ServerOptions::new()
                    .reject_remote_clients(true)
                    .create(&addr)
                else {
                    return;
                };
                let connected = std::mem::replace(&mut next, fresh);
                tokio::spawn(serve(connected, shared.clone()));
            }
        })
    };
    Ok((addr, task))
}

#[cfg(not(any(unix, windows)))]
fn listen(_dir: &Path, _name: &str, _shared: Shared) -> io::Result<(String, JoinHandle<()>)> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "no local sockets on this platform",
    ))
}

/// One askpass connection. Any protocol error just drops it; the askpass program then exits
/// non-zero and ssh treats that as no answer.
async fn serve<S: AsyncRead + AsyncWrite + Unpin>(mut stream: S, shared: Shared) {
    let Ok(Ok(ask)) =
        tokio::time::timeout(HANDSHAKE_TIMEOUT, handshake(&mut stream, &shared.key)).await
    else {
        return;
    };
    let request = PromptRequest {
        host: shared.host.clone(),
        kind: ask.kind,
        prompt: ask.prompt,
    };
    let handler = shared.handler.clone();
    let reply = tokio::task::spawn_blocking(move || handler.prompt(&request))
        .await
        .unwrap_or(Reply::Cancel);
    let wire = match reply {
        Reply::Text(secret) => WireReply::Text {
            text: secret.expose().to_owned(),
        },
        Reply::Accept => WireReply::Accept,
        Reply::Cancel => {
            shared.cancelled.store(true, Ordering::SeqCst);
            WireReply::Cancel
        }
    };
    if let Ok(mut line) = serde_json::to_vec(&wire) {
        line.push(b'\n');
        let _ = stream.write_all(&line).await;
        let _ = stream.flush().await;
        line.fill(0);
    }
    if let WireReply::Text { text } = wire {
        let mut bytes = text.into_bytes();
        bytes.fill(0);
    }
}

async fn handshake<S: AsyncRead + AsyncWrite + Unpin>(
    stream: &mut S,
    key: &[u8; KEY_LEN],
) -> io::Result<Ask> {
    let hello: Hello = receive(stream).await?;
    let nc = from_hex(&hello.nonce)
        .filter(|n| hello.v == 1 && n.len() == NONCE_LEN)
        .ok_or_else(|| bad("bad hello"))?;
    let ns = random::<NONCE_LEN>()?;
    send(
        stream,
        &ServerHello {
            nonce: to_hex(&ns),
            proof: to_hex(&server_proof(key, &nc, &ns)),
        },
    )
    .await?;
    let ask: Ask = receive(stream).await?;
    let proof = from_hex(&ask.proof).ok_or_else(|| bad("bad proof"))?;
    if !same(&proof, &client_proof(key, &nc, &ns, ask.kind, &ask.prompt)) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "client proof mismatch",
        ));
    }
    Ok(ask)
}

fn bad(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, what.to_owned())
}

async fn send<S: AsyncWrite + Unpin>(
    stream: &mut S,
    message: &impl serde::Serialize,
) -> io::Result<()> {
    let mut line = serde_json::to_vec(message)?;
    line.push(b'\n');
    stream.write_all(&line).await?;
    stream.flush().await
}

async fn receive<S: AsyncRead + Unpin, T: serde::de::DeserializeOwned>(
    stream: &mut S,
) -> io::Result<T> {
    let mut line = Vec::new();
    loop {
        let byte = stream.read_u8().await?;
        if byte == b'\n' {
            break;
        }
        if line.len() >= MAX_LINE {
            return Err(bad("line too long"));
        }
        line.push(byte);
    }
    serde_json::from_slice(&line).map_err(|e| bad(&e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::super::client;
    use super::super::{PromptKind, Secret};
    use super::*;
    use std::sync::Mutex;

    struct Scripted {
        seen: Mutex<Vec<PromptRequest>>,
        answer: fn(&PromptRequest) -> Reply,
    }

    impl PromptHandler for Scripted {
        fn prompt(&self, request: &PromptRequest) -> Reply {
            self.seen.lock().unwrap().push(request.clone());
            (self.answer)(request)
        }
    }

    fn run_client(env: [(&'static str, String); 2], prompt: &str) -> (i32, String) {
        let args = vec![prompt.to_owned()];
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = client::main_with(
            &args,
            |name| env.iter().find(|(k, _)| *k == name).map(|(_, v)| v.clone()),
            &mut out,
            &mut err,
        );
        (code, String::from_utf8(out).unwrap())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn answers_cancels_and_wrong_keys() {
        let dir = tempfile::tempdir().unwrap();
        let rt_dir = dir.path().join("rt");
        let handler = Arc::new(Scripted {
            seen: Mutex::new(Vec::new()),
            answer: |r| match r.kind {
                PromptKind::Password => Reply::Text(Secret::new("s3cr3t")),
                PromptKind::HostKey => Reply::Accept,
                _ => Reply::Cancel,
            },
        });
        let server = AskpassServer::start(&rt_dir, "cluster", handler.clone()).unwrap();
        let env = server.env();

        let (code, out) = {
            let env = env.clone();
            tokio::task::spawn_blocking(move || run_client(env, "u@cluster's password: "))
                .await
                .unwrap()
        };
        assert_eq!((code, out.as_str()), (client::EXIT_ANSWERED, "s3cr3t\n"));
        assert!(!server.cancelled());

        let (code, out) = {
            let env = env.clone();
            tokio::task::spawn_blocking(move || {
                run_client(
                    env,
                    "Are you sure you want to continue connecting (yes/no)? ",
                )
            })
            .await
            .unwrap()
        };
        assert_eq!((code, out.as_str()), (client::EXIT_ANSWERED, "yes\n"));

        let (code, out) = {
            let env = env.clone();
            tokio::task::spawn_blocking(move || run_client(env, "Verification code: "))
                .await
                .unwrap()
        };
        assert_eq!((code, out.as_str()), (client::EXIT_CANCELLED, ""));
        assert!(server.cancelled());

        let mut wrong = env.clone();
        wrong[1].1 = to_hex(&[7u8; KEY_LEN]);
        let (code, out) = tokio::task::spawn_blocking(move || run_client(wrong, "Password: "))
            .await
            .unwrap();
        assert_eq!((code, out.as_str()), (client::EXIT_FAILED, ""));

        let seen = handler.seen.lock().unwrap();
        let kinds: Vec<_> = seen.iter().map(|r| (r.host.as_str(), r.kind)).collect();
        assert_eq!(
            kinds,
            [
                ("cluster", PromptKind::Password),
                ("cluster", PromptKind::HostKey),
                ("cluster", PromptKind::Otp),
            ]
        );
    }

    #[test]
    fn without_the_environment_it_fails_cleanly() {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let code = client::main_with(&["Password:".into()], |_| None, &mut out, &mut err);
        assert_eq!(code, client::EXIT_FAILED);
        assert!(out.is_empty());
    }
}
