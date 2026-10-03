use super::*;
use crate::gateway::{BoxFuture, Connected, ErrorCode};
use crate::keychain::MemoryStore;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc;

#[test]
fn ids_are_random_hex() {
    let a = new_id();
    let b = new_id();
    assert_eq!(a.len(), 32);
    assert!(a.bytes().all(|c| c.is_ascii_hexdigit()));
    assert_ne!(a, b);
}

#[test]
fn messages_are_tidied() {
    assert_eq!(
        tidy("auth failed\nfor pcd_SECRETSECRETSECRET \u{1b}[0m"),
        "auth failed for pcd_…  [0m"
    );
    assert_eq!(tidy(&"word ".repeat(400)).chars().count(), 1001);
    // A long run of token characters is taken for a secret.
    assert_eq!(tidy(&"x".repeat(2000)), "…");
}

#[test]
fn failures_get_the_contracts_codes() {
    assert_eq!(
        ssh_error("hpc-login", &SshError::Cancelled).message,
        "signing in to hpc-login was cancelled"
    );
    assert_eq!(
        ssh_error("hpc-login", &SshError::Cancelled).code,
        ErrorCode::Unreachable
    );
    assert_eq!(
        ssh_error(
            "hpc-login",
            &SshError::AuthFailed {
                stderr: "Permission denied".into()
            }
        )
        .code,
        ErrorCode::Unreachable
    );
    assert_eq!(
        helper_error("hpc-login", &HelperError::NoHashTool).code,
        ErrorCode::Invalid
    );
    assert_eq!(
        helper_error("hpc-login", &HelperError::SubmitFailed("no account".into())).code,
        ErrorCode::Invalid
    );
    assert_eq!(
        helper_error("hpc-login", &HelperError::Slurm("squeue failed".into())).code,
        ErrorCode::Unreachable
    );
    assert_eq!(
        helper_error("hpc-login", &HelperError::Ssh(SshError::Cancelled)).code,
        ErrorCode::Unreachable
    );
    assert_eq!(
        check_host("-oProxyCommand=x").unwrap_err().code,
        ErrorCode::Invalid
    );
    assert!(check_host("hpc-login").is_ok());
}

#[test]
fn progress_is_the_contracts_shape() {
    assert_eq!(
        serde_json::to_value(AddProgress::new("Copy", StepState::Running, None)).unwrap(),
        serde_json::json!({ "step": "Copy", "state": "running" })
    );
    assert_eq!(
        serde_json::to_value(AddProgress::new(
            ADD_STEP,
            StepState::Failed,
            Some("why".into())
        ))
        .unwrap(),
        serde_json::json!({ "step": "add", "state": "failed", "detail": "why" })
    );
    let probe = RemoteProbe {
        host: "hpc-login".into(),
        os: "linux".into(),
        arch: "x86_64".into(),
        helper: Some(HelperFound {
            version: "0.4.0".into(),
            running: true,
        }),
        slurm: Some(SlurmFound {
            version: "slurm 23.02.7".into(),
            default_partition: Some("batch".into()),
            srun_overlap: true,
        }),
        tmux: Some(TmuxFound {
            version: "3.3a".into(),
        }),
    };
    assert_eq!(
        serde_json::to_value(probe).unwrap(),
        serde_json::json!({
            "host": "hpc-login", "os": "linux", "arch": "x86_64",
            "helper": { "version": "0.4.0", "running": true },
            "slurm": { "version": "slurm 23.02.7", "defaultPartition": "batch", "srunOverlap": true },
            "tmux": { "version": "3.3a" }
        })
    );
}

#[test]
fn the_token_is_read_between_its_markers() {
    let (begin, end) = ("@@pitcrew-token-begin-ab", "@@pitcrew-token-end-ab");
    let out =
        format!("Welcome to hpc-login!\nlast login yesterday\n{begin}\npcd_secret\n\n{end}\nbye\n");
    assert_eq!(between(&out, begin, end), Some("pcd_secret"));
    assert_eq!(between("pcd_secret\n", begin, end), None);
    assert_eq!(between(&format!("{begin}\npcd_secret"), begin, end), None);
}

#[test]
fn a_hubs_name_is_cleaned_and_cut() {
    assert_eq!(
        workspace_name("Demo\u{202e}  Lab\n\u{1b}[31m"),
        "Demo Lab [31m"
    );
    assert_eq!(workspace_name("x".repeat(500).as_str()).chars().count(), 80);
    assert_eq!(workspace_name("\u{200b}\n"), "Remote workspace");
}

#[test]
fn a_failed_undo_is_in_the_error() {
    let e = GatewayError::unreachable("cannot reach the helper on hpc-login");
    assert_eq!(with_note(e.clone(), None), e);
    let e = with_note(
        e,
        Some("job 4242 may still be queued on hpc-login; cancel it with scancel 4242".into()),
    );
    assert_eq!(e.code, ErrorCode::Unreachable);
    assert!(
        e.message.ends_with("cancel it with scancel 4242"),
        "{}",
        e.message
    );
}

// ─── Races, ordered with the seams ──────────────────────────────────────────────────────────
//
// The machine is never reached here: ssh and pitcrew-askpass do not exist, so a tunnel only
// tries, and fails. What matters is what is left: links, tunnels, tokens and claims.

const ID: &str = "01JR0000000000000000000000";
const HOST: &str = "hpc-login";

struct Nowhere;

impl Connector for Nowhere {
    fn connect(&self) -> BoxFuture<'_, Result<Connected, GatewayError>> {
        Box::pin(async { Err(GatewayError::unreachable("nowhere")) })
    }
}

struct Rig {
    rt: tokio::runtime::Runtime,
    remotes: Arc<Remotes>,
    registry: Arc<Registry>,
    tokens: Arc<MemoryStore>,
    dir: tempfile::TempDir,
}

fn rig() -> Rig {
    // Short: the runtime directory, with ssh's and the askpass sockets, is under it.
    let dir = pitcrew_fixtures::temp::short_tempdir().unwrap();
    let runtime_dir = dir.path().join("rt");
    std::fs::create_dir(&runtime_dir).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&runtime_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let registry = Arc::new(Registry::in_memory());
    let tokens = Arc::new(MemoryStore::default());
    let mut options = RemoteOptions::new(
        Ok(dir.path().join("no-ssh")),
        Ok(dir.path().join("no-askpass")),
        Helpers::in_dir(dir.path().join("helpers")),
    );
    options.runtime_dir = Some(runtime_dir);
    options.multiplex = Some(false);
    let remotes = Arc::new(Remotes::new(
        options,
        Arc::clone(&registry),
        Arc::clone(&tokens) as Arc<dyn TokenStore>,
        Arc::new(PromptHub::new(|_| {})),
        rt.handle().clone(),
    ));
    Rig {
        rt,
        remotes,
        registry,
        tokens,
        dir,
    }
}

fn connection() -> RemoteConnection {
    RemoteConnection {
        target: None,
        host: HOST.into(),
        launcher: LauncherKind::Direct,
        root: "/home/sam/.pitcrew".into(),
        platform: Platform::LinuxX86_64.target().into(),
        site: None,
        job: None,
        last_hop: None,
        transport: None,
    }
}

fn token() -> DeviceToken {
    DeviceToken::new("pcd_a-token-for-the-race-tests-0123456789").unwrap()
}

impl Rig {
    /// A remote workspace saved, with its token in the keychain (no link yet).
    fn saved_remote(&self) {
        self.registry
            .claim_remote(
                WorkspaceRecord {
                    id: ID.into(),
                    name: "Cluster".into(),
                    kind: WorkspaceKind::Remote,
                    connection: Connection::Remote(Box::new(connection())),
                },
                Arc::new(Nowhere),
                WorkspaceState::Unreachable,
            )
            .unwrap();
        self.tokens.set(ID, &token()).unwrap();
    }

    /// From now on the code pauses once at `point`: `paused` hears when it got there, and it
    /// goes on once `go` is sent.
    fn pause_at(&self, point: &'static str) -> (mpsc::Receiver<()>, mpsc::Sender<()>) {
        let (paused_tx, paused) = mpsc::channel();
        let (go, go_rx) = mpsc::channel::<()>();
        let go_rx = Mutex::new(go_rx);
        let fired = AtomicBool::new(false);
        self.hook(move |at| {
            if at == point && !fired.swap(true, Ordering::SeqCst) {
                paused_tx.send(()).unwrap();
                let _ = go_rx.lock().unwrap().recv_timeout(Duration::from_secs(30));
            }
        });
        (paused, go)
    }

    fn hook(&self, hook: impl Fn(&'static str) + Send + Sync + 'static) {
        *self.remotes.core.seams.hook.lock().unwrap() = Some(Arc::new(hook));
    }

    fn links(&self) -> usize {
        self.remotes.core.lock_links().len()
    }

    /// Waits (10 s at most) until `check` holds.
    fn wait(&self, what: &str, check: impl Fn(&Self) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !check(self) {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// The saved remote, resumed: its link is in (resuming asks `ssh -V` first, in a task).
    fn resumed_remote(&self) {
        self.saved_remote();
        self.remotes.resume();
        self.wait("the resumed link", |rig| rig.links() == 1);
    }

    /// No link is left, and every tunnel made so far ends closed.
    fn nothing_left_open(&self) {
        assert_eq!(self.links(), 0);
        let made = self.remotes.core.seams.tunnels.lock().unwrap().clone();
        assert!(!made.is_empty(), "no tunnel was made");
        let deadline = Instant::now() + Duration::from_secs(20);
        for tunnel in made {
            while tunnel.state() != LinkState::Closed {
                assert!(
                    Instant::now() < deadline,
                    "a tunnel is still open: {tunnel:?}"
                );
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        self.rt.block_on(self.remotes.shutdown());
        for tunnel in self.remotes.core.seams.tunnels.lock().unwrap().iter() {
            self.rt.block_on(tunnel.close());
        }
    }
}

/// A cancel that comes after the add took its plan, but before it recorded its cancel: the
/// add still ends cancelled (taking the plan and recording the cancel are one step).
#[test]
fn a_cancel_racing_the_start_of_an_add_is_not_lost() {
    let rig = rig();
    let ssh = rig.remotes.core.ssh().unwrap();
    let target = Target::with_layout(
        ssh,
        HOST,
        Layout::at("/home/sam/.pitcrew").unwrap(),
        Platform::LinuxX86_64,
    )
    .unwrap();
    let steps = Steps {
        deploy: "deploy".into(),
        launch: "launch".into(),
        connect: "connect".into(),
        pair: "pair".into(),
    };
    rig.remotes.lock_plans().insert(
        "p".into(),
        Plan {
            wsl: None,
            host: HOST.into(),
            launcher: LauncherKind::Direct,
            target,
            helper: HelperRef {
                platform: Platform::LinuxX86_64,
                version: "0.0.0".into(),
                sha256: "0".repeat(64),
                path: rig.dir.path().join("helpers").join("nowhere"),
            },
            script: None,
            site: None,
            job: None,
            last_hop: None,
            steps,
        },
        Instant::now(),
    );
    // The add pauses with its plan taken, before its cancel is recorded; once running, it waits
    // for the cancel to have returned.
    let (paused_tx, paused) = mpsc::channel();
    let (go, go_rx) = mpsc::channel::<()>();
    let (cancelled_tx, cancelled) = mpsc::channel::<()>();
    let waits = Mutex::new((go_rx, cancelled));
    rig.hook(move |at| {
        let waits = waits.lock().unwrap();
        match at {
            "add: plan taken" => {
                paused_tx.send(()).unwrap();
                let _ = waits.0.recv_timeout(Duration::from_secs(30));
            }
            "add: running" => {
                let _ = waits.1.recv_timeout(Duration::from_secs(30));
            }
            _ => {}
        }
    });
    let adding = {
        let remotes = Arc::clone(&rig.remotes);
        let handle = rig.rt.handle().clone();
        std::thread::spawn(move || {
            handle.block_on(remotes.add("p", Arc::new(|_: &AddProgress| {})))
        })
    };
    paused.recv_timeout(Duration::from_secs(10)).unwrap();
    let cancelling = {
        let remotes = Arc::clone(&rig.remotes);
        std::thread::spawn(move || {
            remotes.cancel("p");
            cancelled_tx.send(()).unwrap();
        })
    };
    // Time for the cancel to come in; it waits for the add's first step.
    std::thread::sleep(Duration::from_millis(300));
    go.send(()).unwrap();
    cancelling.join().unwrap();
    let e = adding.join().unwrap().unwrap_err();
    assert_eq!(e.code, ErrorCode::Unreachable, "{e:?}");
    assert_eq!(e.message, format!("adding {HOST} was cancelled"));
    assert!(rig.remotes.lock_adds().is_empty());
}

/// A retry while a remove runs (between taking the workspace out and closing its link): the
/// retry finds no workspace, and nothing is left: no link, no open tunnel, no token.
#[test]
fn a_retry_during_a_remove_leaves_no_link_tunnel_or_token() {
    let rig = rig();
    rig.resumed_remote();
    let (paused, go) = rig.pause_at("remove: forgotten");
    let removing = {
        let remotes = Arc::clone(&rig.remotes);
        let handle = rig.rt.handle().clone();
        std::thread::spawn(move || handle.block_on(remotes.remove(ID, false)))
    };
    paused.recv_timeout(Duration::from_secs(10)).unwrap();
    let retried = rig.remotes.retry(ID);
    go.send(()).unwrap();
    removing.join().unwrap().unwrap();
    assert_eq!(
        retried.unwrap_err().code,
        ErrorCode::UnknownWorkspace,
        "the workspace was already out"
    );
    assert_eq!(rig.links(), 0);
    assert!(rig.registry.record(ID).is_none());
    assert_eq!(rig.tokens.get(ID).unwrap(), None);
    assert!(rig.remotes.core.lock_retries().is_empty());
    rig.nothing_left_open();
}

/// A retry whose tunnel is made while a remove runs to its end: the retry puts no link in, and
/// closes its tunnel.
#[test]
fn a_retry_racing_a_remove_puts_no_link_in() {
    let rig = rig();
    // Saved, with no link (its tunnel could not be made at start).
    rig.saved_remote();
    let (paused, go) = rig.pause_at("reconnect: tunnel made");
    let retrying = {
        let remotes = Arc::clone(&rig.remotes);
        std::thread::spawn(move || remotes.retry(ID))
    };
    paused.recv_timeout(Duration::from_secs(10)).unwrap();
    rig.rt.block_on(rig.remotes.remove(ID, false)).unwrap();
    go.send(()).unwrap();
    retrying.join().unwrap().unwrap();
    assert_eq!(rig.links(), 0);
    assert!(rig.registry.record(ID).is_none());
    assert_eq!(rig.tokens.get(ID).unwrap(), None);
    assert!(rig.remotes.core.lock_retries().is_empty());
    rig.nothing_left_open();
}

/// Pairs the machine again from another thread (pairing's synchronous last part), with a fresh
/// tunnel; its outcome comes from the thread, with the tunnel (which the add closes when
/// pairing fails).
fn pair_again(
    rig: &Rig,
) -> std::thread::JoinHandle<(Result<GatewayWorkspace, GatewayError>, Tunnel)> {
    let tunnel = rig.remotes.core.tunnel_for(&connection()).unwrap();
    let remotes = Arc::clone(&rig.remotes);
    std::thread::spawn(move || {
        let kept = remotes
            .core
            .keep(HOST, tunnel.clone(), connection(), ID, "Cluster", token());
        (kept, tunnel)
    })
}

/// A pairing of the same machine that comes while a remove is taking the workspace out: it
/// waits for the remove's step to end, then stays whole, with its entry, its token and its
/// link (the remove does not take them).
#[test]
fn a_pairing_during_a_remove_keeps_its_token_and_link() {
    let rig = rig();
    rig.resumed_remote();
    let (paused, go) = rig.pause_at("remove: forgotten");
    let removing = {
        let remotes = Arc::clone(&rig.remotes);
        let handle = rig.rt.handle().clone();
        std::thread::spawn(move || handle.block_on(remotes.remove(ID, false)))
    };
    paused.recv_timeout(Duration::from_secs(10)).unwrap();
    // The entry is out; the remove has not yet taken its link out.
    assert!(rig.registry.record(ID).is_none());
    let pairing = pair_again(&rig);
    // Time for the pairing to run, if nothing holds it back.
    std::thread::sleep(Duration::from_millis(300));
    go.send(()).unwrap();
    removing.join().unwrap().unwrap();
    pairing.join().unwrap().0.unwrap();
    assert!(rig.registry.record(ID).is_some(), "the new pairing's entry");
    assert_eq!(rig.tokens.get(ID).unwrap(), Some(token()), "its token");
    let link = rig.remotes.core.lock_links().get(ID).cloned();
    let link = link.expect("its link");
    assert!(link.following(), "its link is live");
    let list = rig.registry.list();
    assert_eq!(list.len(), 1, "{list:?}");
}

/// A remove that comes while a pairing is between its claim and its token waits for the pairing
/// to end, then removes it all: no entry, no token, no link, no open tunnel.
#[test]
fn a_remove_during_a_pairing_waits_and_removes_it_all() {
    let rig = rig();
    let (paused, go) = rig.pause_at("pair: claimed");
    let pairing = pair_again(&rig);
    paused.recv_timeout(Duration::from_secs(10)).unwrap();
    assert!(rig.registry.record(ID).is_some(), "claimed");
    let removing = {
        let remotes = Arc::clone(&rig.remotes);
        let handle = rig.rt.handle().clone();
        std::thread::spawn(move || handle.block_on(remotes.remove(ID, false)))
    };
    std::thread::sleep(Duration::from_millis(300));
    go.send(()).unwrap();
    pairing.join().unwrap().0.unwrap();
    removing.join().unwrap().unwrap();
    assert!(rig.registry.record(ID).is_none());
    assert_eq!(rig.tokens.get(ID).unwrap(), None);
    rig.nothing_left_open();
}

/// Once the app quits, nothing is put in: not a retry's attempt, not a pairing.
#[test]
fn nothing_is_put_in_after_shutdown() {
    let rig = rig();
    rig.saved_remote();
    rig.rt.block_on(rig.remotes.shutdown());
    rig.remotes.retry(ID).unwrap();
    let (kept, tunnel) = pair_again(&rig).join().unwrap();
    let e = kept.unwrap_err();
    assert!(e.message.contains("PitCrew is quitting"), "{e:?}");
    // The add closes the tunnel of a pairing that failed.
    rig.rt.block_on(tunnel.close());
    assert!(rig.remotes.core.lock_retries().is_empty());
    assert_eq!(
        rig.tokens.get(ID).unwrap(),
        Some(token()),
        "the saved token stays"
    );
    rig.nothing_left_open();
}

/// A link stops following its tunnel before the tunnel closes, so nothing the tunnel reports
/// while closing reaches the workspace.
#[test]
fn a_link_stops_following_before_its_tunnel_closes() {
    use std::future::Future as _;
    let rig = rig();
    rig.resumed_remote();
    let link = rig.remotes.core.lock_links().get(ID).cloned().unwrap();
    assert!(link.following());
    let closing = link.close();
    let mut closing = std::pin::pin!(closing);
    let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
    // The first poll stops the follower; the tunnel's close then waits for its task.
    let _ = closing.as_mut().poll(&mut cx);
    assert!(!link.following(), "the follower stopped first");
    rig.rt.block_on(closing);
}

/// A remote's link never sets the state of the local workspace, even with the same id (a link
/// that outlived its remote, whose id the local workspace took back).
#[test]
fn a_remote_link_never_sets_the_local_workspaces_state() {
    let rig = rig();
    rig.registry.attach_local(Arc::new(Nowhere));
    rig.registry.set_local(ID, "Here").unwrap();
    let tunnel = rig.remotes.core.tunnel_for(&connection()).unwrap();
    let link = {
        let _runtime = rig.rt.enter();
        Link::start(
            ID.into(),
            tunnel.clone(),
            rig.remotes.core.follow(),
            None,
            rig.rt.handle(),
        )
    };
    // Its follower reports the tunnel's states at once (`connecting`), then its failure (there is
    // no ssh), for a remote.
    let deadline = Instant::now() + Duration::from_secs(10);
    while tunnel.state() == LinkState::Connecting && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    std::thread::sleep(Duration::from_millis(200));
    let local = rig.registry.list().remove(0);
    assert_eq!(local.kind, WorkspaceKind::Local);
    assert_eq!((local.state, local.detail), (WorkspaceState::Ready, None));
    rig.rt.block_on(link.close());
}
