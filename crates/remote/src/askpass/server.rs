//! The listener side: one per ssh call, alive until the call ends.

use super::{
    ADDR_ENV, Ask, CancelTrigger, Hello, KEY_ENV, KEY_LEN, MAX_LINE, NONCE_LEN, PromptCancel,
    PromptHandler, PromptRequest, ServerHello, WireReply, client_proof, from_hex, random, same,
    server_proof, to_hex,
};
use std::io;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _};
use tokio::sync::watch;
use tokio::task::{JoinHandle, JoinSet};

/// How long a client has to finish the handshake. The user's answer has no limit.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// A running listener. Dropping it stops listening, ends every connection (which fires their
/// prompts' [`PromptCancel`]s) and removes the socket.
pub(crate) struct AskpassServer {
    addr: String,
    shared: Arc<Shared>,
    /// The accept loop. It owns the connections' tasks, so aborting it ends them too.
    task: JoinHandle<()>,
}

/// Why the server stopped a call. ssh must then be killed before anything else happens, and no
/// prompt is shown or answered from then on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Stop {
    /// The user cancelled a prompt, or its answer was refused.
    Cancelled,
    /// A client that spoke the protocol failed the handshake. Our askpass stops ssh when it
    /// gets no answer (on Unix by killing it, on Windows by waiting for the job to end), so the
    /// call cannot go on.
    BridgeFailed,
}

impl Stop {
    /// The error the call ends with.
    pub(crate) fn error(self) -> crate::SshError {
        match self {
            Self::Cancelled => crate::SshError::Cancelled,
            Self::BridgeFailed => crate::SshError::Bridge(
                "an askpass program failed the handshake; ssh was stopped".to_owned(),
            ),
        }
    }
}

struct Shared {
    key: [u8; KEY_LEN],
    host: String,
    handler: Arc<dyn PromptHandler>,
    /// Set once, when the call must stop.
    stopped: watch::Sender<Option<Stop>>,
    /// How many prompts are waiting for the user, so time limits can pause.
    open: watch::Sender<usize>,
}

impl Shared {
    /// Stops the call, unless it is already stopped (the first reason wins).
    fn stop(&self, why: Stop) {
        self.stopped.send_if_modified(|stopped| {
            let first = stopped.is_none();
            if first {
                *stopped = Some(why);
            }
            first
        });
    }

    fn is_stopped(&self) -> bool {
        self.stopped.borrow().is_some()
    }
}

impl Drop for Shared {
    fn drop(&mut self) {
        self.key.fill(0);
        std::hint::black_box(&self.key);
    }
}

impl AskpassServer {
    /// Starts listening. `dir` must be a private directory (Unix; unused on Windows). Must be
    /// called inside a tokio runtime.
    pub(crate) fn start(
        dir: &Path,
        host: &str,
        handler: Arc<dyn PromptHandler>,
    ) -> io::Result<Self> {
        let shared = Arc::new(Shared {
            key: random::<KEY_LEN>()?,
            host: host.to_owned(),
            handler,
            stopped: watch::Sender::new(None),
            open: watch::Sender::new(0),
        });
        let name = to_hex(&random::<8>()?);
        let (addr, task) = listen(dir, &name, shared.clone())?;
        Ok(Self { addr, shared, task })
    }

    /// The variables `pitcrew-askpass` needs, besides `SSH_ASKPASS` itself.
    pub(crate) fn env(&self) -> [(&'static str, String); 2] {
        [
            (ADDR_ENV, self.addr.clone()),
            (KEY_ENV, to_hex(&self.shared.key)),
        ]
    }

    /// Whether, and why, this call was stopped.
    pub(crate) fn stopped(&self) -> Option<Stop> {
        *self.shared.stopped.borrow()
    }

    /// Completes when the call must stop (see [`Stop`]). The caller must then kill ssh; until
    /// the server is dropped, nothing is answered.
    pub(crate) async fn wait_stopped(&self) -> Stop {
        let mut rx = self.shared.stopped.subscribe();
        match rx.wait_for(Option::is_some).await.map(|stopped| *stopped) {
            Ok(Some(why)) => why,
            _ => std::future::pending().await,
        }
    }

    /// The number of prompts waiting for the user, as it changes.
    pub(crate) fn open_prompts(&self) -> watch::Receiver<usize> {
        self.shared.open.subscribe()
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
    }
}

/// Starts a connection's task, first dropping the finished ones.
fn track<S>(conns: &mut JoinSet<()>, stream: S, shared: &Arc<Shared>)
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    while conns.try_join_next().is_some() {}
    conns.spawn(serve(stream, shared.clone()));
}

#[cfg(unix)]
fn listen(dir: &Path, name: &str, shared: Arc<Shared>) -> io::Result<(String, JoinHandle<()>)> {
    crate::private::ensure_private_dir(dir)?;
    let path = dir.join(format!("ask-{name}"));
    let addr = path
        .to_str()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "runtime dir is not UTF-8"))?
        .to_owned();
    let listener = tokio::net::UnixListener::bind(&path)?;
    let task = tokio::spawn(async move {
        let uid = crate::private::euid();
        let mut conns = JoinSet::new();
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                tokio::time::sleep(Duration::from_millis(50)).await;
                continue;
            };
            if stream.peer_cred().is_ok_and(|c| c.uid() == uid) {
                track(&mut conns, stream, &shared);
            }
        }
    });
    Ok((addr, task))
}

#[cfg(windows)]
fn listen(_dir: &Path, name: &str, shared: Arc<Shared>) -> io::Result<(String, JoinHandle<()>)> {
    use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
    let addr = format!(r"\\.\pipe\pitcrew-askpass-{name}");
    // The current user owns every instance and alone may open it: the default descriptor would
    // let everyone read it.
    let security = crate::pipe_security::PipeSecurity::current_user_only()?;
    let create = move |addr: &str, first: bool| -> io::Result<NamedPipeServer> {
        let mut options = ServerOptions::new();
        options
            .first_pipe_instance(first)
            .reject_remote_clients(true);
        security.create(&options, addr)
    };
    // `first_pipe_instance` fails if the name exists, so nobody can have squatted it. From then
    // on an instance of ours always exists: the replacement is created before the previous one
    // is handed off or dropped, so the name never lapses for another user to take.
    let first = create(&addr, true)?;
    let task = {
        let addr = addr.clone();
        tokio::spawn(async move {
            let mut conns = JoinSet::new();
            let mut server = first;
            loop {
                let connected = server.connect().await.is_ok();
                // Creating can fail for a moment (e.g. under load): retry with back-off,
                // holding on to the current instance meanwhile.
                let mut delay = Duration::from_millis(50);
                let next = loop {
                    match create(&addr, false) {
                        Ok(next) => break next,
                        Err(_) => {
                            tokio::time::sleep(delay).await;
                            delay = (delay * 2).min(Duration::from_secs(2));
                        }
                    }
                };
                let current = std::mem::replace(&mut server, next);
                if connected {
                    track(&mut conns, current, &shared);
                }
            }
        })
    };
    Ok((addr, task))
}

#[cfg(not(any(unix, windows)))]
fn listen(_dir: &Path, _name: &str, _shared: Arc<Shared>) -> io::Result<(String, JoinHandle<()>)> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "no local sockets on this platform",
    ))
}

/// Counts a prompt as open while it lives.
struct OpenPrompt<'a>(&'a watch::Sender<usize>);

impl<'a> OpenPrompt<'a> {
    fn new(open: &'a watch::Sender<usize>) -> Self {
        open.send_modify(|n| *n += 1);
        Self(open)
    }
}

impl Drop for OpenPrompt<'_> {
    fn drop(&mut self) {
        self.0.send_modify(|n| *n = n.saturating_sub(1));
    }
}

/// One askpass connection.
///
/// A handshake that fails after a well-formed hello stops the call ([`Stop::BridgeFailed`]):
/// only a client that knows the protocol gets that far, and if it is ours it is now stopping
/// ssh for want of an answer. Anything that fails earlier (garbage, or a connection that never
/// says hello) is just dropped, so that strays cannot end a call.
async fn serve<S: AsyncRead + AsyncWrite + Unpin>(mut stream: S, shared: Arc<Shared>) {
    let mut greeted = false;
    let shook = tokio::time::timeout(
        HANDSHAKE_TIMEOUT,
        handshake(&mut stream, &shared.key, &mut greeted),
    )
    .await;
    let Ok(Ok(ask)) = shook else {
        if greeted {
            shared.stop(Stop::BridgeFailed);
        }
        return;
    };
    if shared.is_stopped() {
        // The user already said no in this call, and ssh is being killed. Never ask again, and
        // never answer: any reply, even a failure, lets ssh send an empty credential.
        return hold(stream).await;
    }
    let kind = ask.kind;
    let request = PromptRequest {
        host: shared.host.clone(),
        kind,
        prompt: ask.prompt,
    };
    // Dropped with this task, which fires the handler's token when the call ends.
    let (stale, cancel): (CancelTrigger, PromptCancel) = PromptCancel::pair();
    let reply = {
        let _open = OpenPrompt::new(&shared.open);
        let answer = shared.handler.prompt(request, cancel);
        let mut extra = [0u8; 1];
        tokio::select! {
            reply = answer => Some(reply),
            // The client sends nothing more until it has its answer. End of stream (ssh closed
            // a notice, or died) or anything else means nobody waits for this prompt.
            _ = stream.read(&mut extra) => None,
        }
    };
    let Some(reply) = reply else {
        stale.cancel();
        return;
    };
    let Some(wire) = WireReply::for_prompt(kind, reply) else {
        shared.stop(Stop::Cancelled);
        return hold(stream).await;
    };
    if shared.is_stopped() {
        // The call was stopped meanwhile; ssh is being killed.
        return hold(stream).await;
    }
    if let Ok(mut line) = serde_json::to_vec(&wire) {
        line.push(b'\n');
        let _ = stream.write_all(&line).await;
        let _ = stream.flush().await;
        line.fill(0);
        std::hint::black_box(&line);
    }
}

/// Keeps a connection open, unanswered, until the call ends and the task is aborted.
async fn hold<S>(stream: S) {
    let _stream = stream;
    std::future::pending::<()>().await;
}

/// The handshake. `greeted` is set once the client has sent a well-formed hello.
async fn handshake<S: AsyncRead + AsyncWrite + Unpin>(
    stream: &mut S,
    key: &[u8; KEY_LEN],
    greeted: &mut bool,
) -> io::Result<Ask> {
    let hello: Hello = receive(stream).await?;
    let nc = from_hex(&hello.nonce)
        .filter(|n| hello.v == 1 && n.len() == NONCE_LEN)
        .ok_or_else(|| bad("bad hello"))?;
    *greeted = true;
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
    use super::super::{PromptFuture, PromptKind, Reply, Secret, client};
    use super::*;
    use std::sync::Mutex;

    struct Scripted {
        seen: Mutex<Vec<(PromptRequest, PromptCancel)>>,
        answer: fn(&PromptRequest) -> Option<Reply>,
    }

    impl Scripted {
        fn new(answer: fn(&PromptRequest) -> Option<Reply>) -> Arc<Self> {
            Arc::new(Self {
                seen: Mutex::new(Vec::new()),
                answer,
            })
        }

        fn kinds(&self) -> Vec<(String, PromptKind)> {
            let seen = self.seen.lock().unwrap();
            seen.iter().map(|(r, _)| (r.host.clone(), r.kind)).collect()
        }

        fn token(&self, i: usize) -> PromptCancel {
            self.seen.lock().unwrap()[i].1.clone()
        }
    }

    impl PromptHandler for Scripted {
        /// `None` from the script means the user never answers.
        fn prompt(&self, request: PromptRequest, cancel: PromptCancel) -> PromptFuture<'_> {
            let answer = (self.answer)(&request);
            self.seen.lock().unwrap().push((request, cancel));
            Box::pin(async move {
                match answer {
                    Some(reply) => reply,
                    None => std::future::pending().await,
                }
            })
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

    fn spawn_client(
        env: &[(&'static str, String); 2],
        prompt: &str,
    ) -> tokio::task::JoinHandle<(i32, String)> {
        let env = env.clone();
        let prompt = prompt.to_owned();
        tokio::task::spawn_blocking(move || run_client(env, &prompt))
    }

    async fn eventually(what: &str, check: impl Fn() -> bool) {
        for _ in 0..500 {
            if check() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("timed out waiting for {what}");
    }

    /// Windows: every instance of the pipe is the current user's alone (the default descriptor
    /// would let everyone read it): the first, and the one made when it is taken.
    #[cfg(windows)]
    #[tokio::test(flavor = "multi_thread")]
    async fn the_pipe_is_the_current_users_alone() {
        use crate::pipe_security::{assert_current_user_only, owner_and_dacl};
        let dir = tempfile::tempdir().unwrap();
        let handler = Scripted::new(|_| Some(Reply::Cancel));
        let server = AskpassServer::start(&dir.path().join("rt"), "cluster", handler).unwrap();
        let open = |addr: &str| {
            for _ in 0..100 {
                if let Ok(pipe) = std::fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(addr)
                {
                    return pipe;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            panic!("cannot open {addr}");
        };
        let first = open(&server.addr);
        assert_current_user_only(&owner_and_dacl(&first).unwrap());
        let next = open(&server.addr);
        assert_current_user_only(&owner_and_dacl(&next).unwrap());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn answers_and_wrong_keys() {
        let dir = tempfile::tempdir().unwrap();
        let handler = Scripted::new(|r| {
            Some(match r.kind {
                PromptKind::Password => Reply::Text(Secret::new("s3cr3t")),
                PromptKind::HostKey => Reply::Accept,
                _ => Reply::Cancel,
            })
        });
        let server =
            AskpassServer::start(&dir.path().join("rt"), "cluster", handler.clone()).unwrap();
        let env = server.env();

        let (code, out) = spawn_client(&env, "u@cluster's password: ").await.unwrap();
        assert_eq!((code, out.as_str()), (client::EXIT_ANSWERED, "s3cr3t\n"));
        let (code, out) = spawn_client(
            &env,
            "Are you sure you want to continue connecting (yes/no)? ",
        )
        .await
        .unwrap();
        assert_eq!((code, out.as_str()), (client::EXIT_ANSWERED, "yes\n"));
        // "No" to a yes/no question is answered, and the call goes on.
        let (code, out) = spawn_client(&env, "Accept updated hostkeys? (yes/no): ")
            .await
            .unwrap();
        assert_eq!((code, out.as_str()), (client::EXIT_ANSWERED, "no\n"));
        assert_eq!(server.stopped(), None);

        // Without the key: no answer, which the program turns into stopping ssh.
        let mut broken = env.clone();
        broken[1].1 = "not hex".to_owned();
        let (code, _) = spawn_client(&broken, "Password: ").await.unwrap();
        assert_eq!(code, client::EXIT_NO_ANSWER);
        let mut wrong = env.clone();
        wrong[1].1 = to_hex(&[7u8; KEY_LEN]);
        let (code, out) = spawn_client(&wrong, "Password: ").await.unwrap();
        assert_eq!((code, out.as_str()), (client::EXIT_NO_ANSWER, ""));
        // That client said hello and then gave up on our proof: the call stops.
        eventually("the call to stop", || server.stopped().is_some()).await;
        assert_eq!(server.stopped(), Some(Stop::BridgeFailed));

        assert_eq!(
            handler.kinds(),
            [
                ("cluster".to_owned(), PromptKind::Password),
                ("cluster".to_owned(), PromptKind::HostKey),
                ("cluster".to_owned(), PromptKind::Confirm),
            ]
        );
    }

    /// Connections that never say a proper hello (strays, garbage) are dropped without
    /// stopping the call; one that says hello and then fails does stop it.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    async fn only_a_failed_handshake_after_hello_stops_the_call() {
        use std::io::Write as _;
        let dir = tempfile::tempdir().unwrap();
        let handler = Scripted::new(|_| Some(Reply::Cancel));
        let server =
            AskpassServer::start(&dir.path().join("rt"), "cluster", handler.clone()).unwrap();
        let addr = server.env()[0].1.clone();
        let connect = || std::os::unix::net::UnixStream::connect(&addr).unwrap();

        drop(connect());
        connect().write_all(b"garbage\n").unwrap();
        connect()
            .write_all(b"{\"v\":2,\"nonce\":\"00\"}\n")
            .unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(server.stopped(), None);

        // A well-formed hello, then a bad proof.
        let mut stream = connect();
        stream
            .write_all(b"{\"v\":1,\"nonce\":\"000102030405060708090a0b0c0d0e0f\"}\n")
            .unwrap();
        stream
            .write_all(b"{\"proof\":\"00\",\"kind\":\"password\",\"prompt\":\"x\"}\n")
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), server.wait_stopped())
            .await
            .unwrap();
        assert_eq!(server.stopped(), Some(Stop::BridgeFailed));
        assert!(handler.kinds().is_empty());
    }

    /// A cancel is never answered, later prompts are not shown, and nothing reaches the client
    /// until the server goes away.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_cancel_is_held_until_the_call_ends() {
        let dir = tempfile::tempdir().unwrap();
        let handler = Scripted::new(|r| {
            Some(match r.kind {
                // Accept for a password is refused like a cancel.
                PromptKind::Password => Reply::Accept,
                _ => Reply::Cancel,
            })
        });
        let server =
            AskpassServer::start(&dir.path().join("rt"), "cluster", handler.clone()).unwrap();
        let env = server.env();

        let first = spawn_client(&env, "u@cluster's password: ");
        let why = tokio::time::timeout(Duration::from_secs(5), server.wait_stopped())
            .await
            .unwrap();
        assert_eq!(why, Stop::Cancelled);
        let second = spawn_client(&env, "Verification code: ");
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(!first.is_finished() && !second.is_finished());
        assert_eq!(
            handler.kinds().len(),
            1,
            "a prompt was shown after a cancel"
        );

        drop(server);
        assert_eq!(
            first.await.unwrap(),
            (client::EXIT_NO_ANSWER, String::new())
        );
        assert_eq!(
            second.await.unwrap(),
            (client::EXIT_NO_ANSWER, String::new())
        );
    }

    /// ssh closes a notice by killing askpass: the handler's token fires, and the prompt no
    /// longer counts as open.
    #[cfg(unix)]
    #[tokio::test(flavor = "multi_thread")]
    async fn a_prompt_goes_stale_when_askpass_leaves() {
        let dir = tempfile::tempdir().unwrap();
        let handler = Scripted::new(|_| None);
        let server =
            AskpassServer::start(&dir.path().join("rt"), "cluster", handler.clone()).unwrap();
        let open = server.open_prompts();
        let env = server.env();
        let key = super::super::key_from_hex(&env[1].1).unwrap();

        let stream = std::os::unix::net::UnixStream::connect(&env[0].1).unwrap();
        let closer = stream.try_clone().unwrap();
        let asking = std::thread::spawn(move || {
            let mut stream = stream;
            client::ask(&mut stream, &key, PromptKind::Notice, "Touch your key").is_ok()
        });
        eventually("the notice", || handler.kinds().len() == 1).await;
        let notice = handler.token(0);
        assert!(!notice.is_cancelled());
        assert_eq!(*open.borrow(), 1);

        closer.shutdown(std::net::Shutdown::Both).unwrap();
        tokio::time::timeout(Duration::from_secs(5), notice.cancelled())
            .await
            .unwrap();
        eventually("the prompt to close", || *open.borrow() == 0).await;
        assert!(!asking.join().unwrap());
        // A stale prompt is not a failed handshake: the call goes on.
        assert_eq!(server.stopped(), None);
    }

    /// Dropping the call (here, the server) makes a pending prompt see cancellation.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_pending_prompt_sees_the_call_end() {
        let dir = tempfile::tempdir().unwrap();
        let handler = Scripted::new(|_| None);
        let server =
            AskpassServer::start(&dir.path().join("rt"), "cluster", handler.clone()).unwrap();
        let pending = spawn_client(&server.env(), "Password: ");
        eventually("the password prompt", || handler.kinds().len() == 1).await;
        let password = handler.token(0);
        assert!(!password.is_cancelled());

        drop(server);
        tokio::time::timeout(Duration::from_secs(5), password.cancelled())
            .await
            .unwrap();
        assert!(password.is_cancelled());
        assert_eq!(
            pending.await.unwrap(),
            (client::EXIT_NO_ANSWER, String::new())
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
