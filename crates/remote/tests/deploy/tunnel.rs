//! The tunnel ([`Connector`]) and the stdio bridge, against `deploy.rs`'s fake machine (with
//! `slurm.rs`'s fake SLURM for jobs). How the fake `ssh` plays the tunnel's calls, and the
//! machine's state files, are in `tunnel_fake.rs`.

use crate::slurm::{self as fake_slurm, Config};
use crate::tunnel_fake::{
    APP_ENV, ASKED, AppSpec, CLOSE_WRITE, DROP_AFTER, FORWARD_FAIL_ONCE, FORWARD_SILENT,
    HOLD_CHECKS, MAX_SESSIONS, NET, NO_FORWARDING, Net, PASSWORD, REFUSED, TUNNEL_LOG, TunnelCall,
};
use crate::unix::{
    Machine, RUN_ENV, Remote, alive, daemon, deploy_and_start, launch_options, me, mode,
    private_dir, quick, run_mark, runtime, stop_helper,
};
use pitcrew_remote::helper::slurm::{LastHop, Site, SocketPlace};
use pitcrew_remote::{
    Connector, ConnectorOptions, Daemon, DirectLauncher, JobOptions, Launcher as _, LinkState,
    Platform, PromptCancel, PromptFuture, PromptHandler, PromptKind, PromptRequest, Reply, Secret,
    SlurmLauncher, Ssh, SshError, Target, Transport, TunnelError, Unreachable, WallClock, deploy,
};
use std::collections::VecDeque;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
// ─── Fixtures ──────────────────────────────────────────────────────────────────────────────

/// The askpass binary.
fn askpass() -> String {
    env!("CARGO_BIN_EXE_pitcrew-askpass").to_owned()
}

/// Options that keep the cases short.
fn options() -> ConnectorOptions {
    ConnectorOptions {
        link_wait: Duration::from_secs(20),
        connect_wait: Duration::from_secs(15),
        bridge_wait: Duration::from_secs(15),
        check_every: Duration::from_secs(2),
        probe_every: Duration::from_secs(2),
        probe_timeout: Duration::from_secs(2),
        backoff_min: Duration::from_millis(200),
        backoff_max: Duration::from_secs(2),
        give_up_after: Duration::from_secs(60),
        retry_every: Duration::from_secs(2),
        // Never the developer's own ~/.ssh/config.
        ssh_config: Some(PathBuf::from("/nonexistent/pitcrew-test-ssh-config")),
        ..ConnectorOptions::default()
    }
}

/// A connector started on `rt`.
fn start(rt: &tokio::runtime::Runtime, daemon: Daemon, options: ConnectorOptions) -> Connector {
    let _entered = rt.enter();
    Connector::start(daemon, options).unwrap()
}

/// Waits up to `within` for a state `pred` accepts, and returns it.
fn wait_for(
    rt: &tokio::runtime::Runtime,
    connector: &Connector,
    what: &str,
    within: Duration,
    pred: impl Fn(&LinkState) -> bool,
) -> LinkState {
    let mut rx = connector.watch();
    let found = rt.block_on(async {
        tokio::time::timeout(within, async {
            loop {
                let now = rx.borrow_and_update().clone();
                if pred(&now) {
                    return now;
                }
                if rx.changed().await.is_err() {
                    return LinkState::Closed;
                }
            }
        })
        .await
    });
    match found {
        Ok(state) if pred(&state) => state,
        _ => panic!(
            "timed out waiting for {what}: the state is {}",
            connector.state()
        ),
    }
}

fn connected(transport: Transport) -> impl Fn(&LinkState) -> bool {
    move |s| *s == LinkState::Connected { transport }
}

fn unverifiable(s: &LinkState) -> bool {
    matches!(s, LinkState::Unverifiable { .. })
}

/// Sends `data` through one connection, half-closes, and reads to the end.
fn echo(rt: &tokio::runtime::Runtime, connector: &Connector, data: &[u8]) -> Vec<u8> {
    rt.block_on(within(STEP, "the echo through the tunnel", async {
        let stream = connector.connect().await.unwrap();
        exchange(stream, data.to_vec()).await
    }))
}

/// The longest one step of a case may take before the case fails, naming it: a stalled link
/// then fails the case at once instead of the whole run at CI's job limit.
const STEP: Duration = Duration::from_secs(90);

/// `future`, or a panic naming `what` once `limit` has passed.
async fn within<T>(limit: Duration, what: &str, future: impl std::future::Future<Output = T>) -> T {
    match tokio::time::timeout(limit, future).await {
        Ok(out) => out,
        Err(_) => panic!("{what} did not finish within {} s", limit.as_secs()),
    }
}

async fn exchange(stream: pitcrew_remote::TunnelStream, data: Vec<u8>) -> Vec<u8> {
    let (mut from, mut to) = tokio::io::split(stream);
    let writing = async move {
        to.write_all(&data).await.unwrap();
        to.shutdown().await.unwrap();
    };
    let reading = async move {
        let mut got = Vec::new();
        from.read_to_end(&mut got).await.unwrap();
        got
    };
    let ((), got) = tokio::join!(writing, reading);
    got
}

/// Bytes that are not text, nor a request the fake daemon reads specially.
fn pattern(len: usize, seed: u8) -> Vec<u8> {
    let mut x = u32::from(seed).wrapping_mul(2_654_435_761).wrapping_add(1);
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            x.to_le_bytes()[0]
        })
        .map(|b| if b == b'G' { 0 } else { b })
        .collect()
}

fn net_set(m: &Machine, state: Net) {
    let path = m.dir.path().join(NET);
    match state {
        Net::Up => {
            let _ = std::fs::remove_file(path);
        }
        Net::Down => std::fs::write(path, "down").unwrap(),
        Net::Frozen => std::fs::write(path, "frozen").unwrap(),
    }
}

/// The process of the newest link to `host`.
fn newest_link(m: &Machine, host: &str) -> u32 {
    tunnel_calls(m)
        .iter()
        .rev()
        .find(|c| c.kind == "link" && c.host == host)
        .unwrap()
        .pid
}

/// Kills the newest link to `host`, as a connection reset would end it.
fn kill_link(m: &Machine, host: &str) {
    let pid = rustix::process::Pid::from_raw(i32::try_from(newest_link(m, host)).unwrap());
    rustix::process::kill_process(pid.unwrap(), rustix::process::Signal::KILL).unwrap();
}

/// How many links the machine's fake ssh started.
fn links(m: &Machine) -> usize {
    tunnel_calls(m).iter().filter(|c| c.kind == "link").count()
}

/// Opens a connection, trying again while the server refuses another session (one just
/// closed may take a moment to be freed).
fn open_when_free(
    rt: &tokio::runtime::Runtime,
    connector: &Connector,
) -> pitcrew_remote::TunnelStream {
    let start = Instant::now();
    loop {
        match rt.block_on(connector.connect()) {
            Ok(stream) => return stream,
            Err(TunnelError::Ssh(SshError::SessionRefused { .. }))
                if start.elapsed() < Duration::from_secs(10) =>
            {
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => panic!("{e:?}"),
        }
    }
}

/// The tunnel calls the machine's fake ssh saw. A last line without its newline is still being
/// written, and is left for the next look.
fn tunnel_calls(m: &Machine) -> Vec<TunnelCall> {
    let mut log = std::fs::read(m.dir.path().join(TUNNEL_LOG)).unwrap_or_default();
    log.truncate(
        log.iter()
            .rposition(|&b| b == b'\n')
            .map_or(0, |end| end + 1),
    );
    String::from_utf8(log)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("{e}: {l:?}")))
        .collect()
}

/// A target whose ssh is a fresh fake of `m`, with connection reuse.
fn target(m: &Machine) -> Target {
    reuse(m, &m.fake(Remote::default()))
}

/// `m` through `fake`, with connection reuse (a ControlMaster), as on Unix.
fn reuse(m: &Machine, fake: &crate::unix::Fake) -> Target {
    on(m, fake.ssh.clone().with_multiplex(true))
}

/// `m` through `ssh`.
fn on(m: &Machine, ssh: Ssh) -> Target {
    Target::with_layout(ssh, "cluster", m.layout(), Platform::LinuxX86_64)
        .unwrap()
        .with_tool_path(m.bin.to_str().unwrap())
        .unwrap()
}

/// The connectors' private directories under a fake's runtime directory.
fn private_dirs(fake: &crate::unix::Fake) -> Vec<PathBuf> {
    std::fs::read_dir(fake.dir.join("rt"))
        .map(|entries| {
            entries
                .map(|e| e.unwrap().path())
                .filter(|p| {
                    p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.starts_with('t'))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// A SLURM machine with the helper deployed and a job running it, reached as `last_hop` says.
fn job(site: &Site, config: Config) -> (Machine, fake_slurm::Sim, SlurmLauncher, u64) {
    let (m, sim) = fake_slurm::machine(config);
    let target = m.plain();
    pitcrew_remote_deploy(&target);
    let script = fake_slurm::render(&target, site, &JobOptions::default());
    let launcher = fake_slurm::launcher(&script);
    let started = crate::unix::block_on(launcher.start(&target)).unwrap();
    let id = started.endpoint.job.unwrap();
    (m, sim, launcher, id)
}

fn pitcrew_remote_deploy(target: &Target) {
    crate::unix::block_on(deploy(target, &crate::unix::helper("1.0.0"), &quick())).unwrap();
}

// ─── Cases ─────────────────────────────────────────────────────────────────────────────────

/// A login node whose sshd forwards unix sockets: one forwarded socket in a private directory,
/// shared by many connections at once, byte for byte; gone on close.
fn tunnel_forwarded_socket_with_many_connections() {
    let m = Machine::new();
    deploy_and_start(&m);
    let fake = m.fake(Remote::default());
    let rt = runtime();
    let launcher = Arc::new(DirectLauncher::new(launch_options()));
    let connector = start(&rt, Daemon::new(reuse(&m, &fake), launcher), options());
    wait_for(
        &rt,
        &connector,
        "connected",
        Duration::from_secs(30),
        connected(Transport::Forwarded),
    );
    assert_eq!(connector.transport(), Some(Transport::Forwarded));

    let got = rt.block_on(async {
        let mut set = tokio::task::JoinSet::new();
        for i in 0..8u8 {
            let stream = connector.connect().await.unwrap();
            assert_eq!(stream.transport(), Transport::Forwarded);
            set.spawn(async move {
                let data = pattern(256 * 1024, i);
                (exchange(stream, data.clone()).await, data)
            });
        }
        set.join_all().await
    });
    for (back, sent) in got {
        assert_eq!(back.len(), sent.len());
        assert!(back == sent, "the echo differs");
    }

    // One link and one forward did it all; the private directory is the user's alone.
    let calls = tunnel_calls(&m);
    assert_eq!(calls.iter().filter(|c| c.kind == "link").count(), 1);
    assert_eq!(
        calls
            .iter()
            .filter(|c| c.op.as_deref() == Some("forward"))
            .count(),
        1
    );
    let dirs = private_dirs(&fake);
    assert_eq!(dirs.len(), 1, "{dirs:?}");
    assert_eq!(mode(&dirs[0]), 0o700);
    let forwarded: Vec<PathBuf> = std::fs::read_dir(&dirs[0])
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.file_name().unwrap().to_str().unwrap().starts_with('f'))
        .collect();
    assert_eq!(forwarded.len(), 1);
    assert_eq!(mode(&forwarded[0]) & 0o077, 0);

    rt.block_on(connector.close());
    assert_eq!(connector.state(), LinkState::Closed);
    assert!(!dirs[0].exists(), "the private directory is left");
    let err = rt.block_on(connector.connect()).unwrap_err();
    assert!(err.to_string().contains("closed"), "{err}");
    stop_helper(&m);
}

/// `AllowStreamLocalForwarding no`: ssh's refusal is read, remembered, and the stdio bridge
/// carries the connections instead.
fn tunnel_forwarding_refused_then_the_stdio_bridge() {
    let m = Machine::new();
    deploy_and_start(&m);
    std::fs::write(m.dir.path().join(NO_FORWARDING), "").unwrap();
    let rt = runtime();
    let launcher = Arc::new(DirectLauncher::new(launch_options()));
    let connector = start(&rt, Daemon::new(target(&m), launcher.clone()), options());
    wait_for(
        &rt,
        &connector,
        "connected through the bridge",
        Duration::from_secs(30),
        connected(Transport::Stdio),
    );
    assert_eq!(connector.transport(), Some(Transport::Stdio));
    let data = pattern(100_000, 7);
    assert!(echo(&rt, &connector, &data) == data);
    // The bridge ran in sessions of the link, as `exec <helper> connect --socket <socket>
    // --nonce <hex>`, a fresh nonce each time.
    let calls = tunnel_calls(&m);
    let socket = m.layout().socket();
    let bridge = pitcrew_remote::quote::posix_command(&[
        "exec",
        &m.layout().binary("1.0.0"),
        "connect",
        "--socket",
        &socket,
        "--nonce",
    ])
    .unwrap();
    let nonces: Vec<&str> = calls
        .iter()
        .filter(|c| c.kind == "session")
        .filter_map(|c| c.line.as_deref()?.strip_prefix(&format!("{bridge} ")))
        .collect();
    assert!(nonces.len() >= 2, "{calls:?}");
    for nonce in &nonces {
        assert_eq!(nonce.len(), 16, "{nonce}");
        assert!(nonce.bytes().all(|b| b.is_ascii_hexdigit()), "{nonce}");
    }
    assert_ne!(nonces[0], nonces[1]);
    let forwards = |calls: &[TunnelCall]| {
        calls
            .iter()
            .filter(|c| c.op.as_deref() == Some("forward"))
            .count()
    };
    assert_eq!(forwards(&calls), 1);

    // Reconnected, the refusal is remembered: no forward is tried again.
    net_set(&m, Net::Down);
    wait_for(
        &rt,
        &connector,
        "unverifiable",
        Duration::from_secs(15),
        unverifiable,
    );
    net_set(&m, Net::Up);
    wait_for(
        &rt,
        &connector,
        "connected again",
        Duration::from_secs(30),
        connected(Transport::Stdio),
    );
    assert!(echo(&rt, &connector, &data) == data);
    assert_eq!(forwards(&tunnel_calls(&m)), 1);
    rt.block_on(connector.close());

    // A remembered choice is used at once.
    let remembered = ConnectorOptions {
        transport: Some(Transport::Stdio),
        ..options()
    };
    std::fs::remove_file(m.dir.path().join(NO_FORWARDING)).unwrap();
    let connector = start(&rt, Daemon::new(target(&m), launcher), remembered);
    wait_for(
        &rt,
        &connector,
        "connected through the bridge",
        Duration::from_secs(30),
        connected(Transport::Stdio),
    );
    assert_eq!(forwards(&tunnel_calls(&m)), 1);
    rt.block_on(connector.close());
    stop_helper(&m);
}

/// Forwarding refused while the bridge fails too (a helper whose `connect` finds no daemon): the
/// attempts that follow use the bridge alone, never the refused forward again.
fn tunnel_a_refused_forward_is_not_tried_again() {
    let m = Machine::new();
    std::fs::write(m.dir.path().join(NO_FORWARDING), "").unwrap();
    let script = String::from_utf8(crate::unix::helper_script("1.0.0", "serve", 0)).unwrap();
    let mut broken: String = script
        .lines()
        .map(|line| {
            if line.starts_with("connect)") {
                "connect) echo 'pitcrewd connect: no daemon listens on the socket: x' >&2; exit 4 ;;"
            } else {
                line
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    broken.push('\n');
    let plain = m.plain();
    crate::unix::block_on(deploy(
        &plain,
        &crate::unix::helper_from("1.0.0", broken.into_bytes()),
        &quick(),
    ))
    .unwrap();
    crate::unix::block_on(DirectLauncher::new(launch_options()).start(&plain)).unwrap();
    let rt = runtime();
    let launcher = Arc::new(DirectLauncher::new(launch_options()));
    let connector = start(&rt, Daemon::new(target(&m), launcher), options());
    let state = wait_for(
        &rt,
        &connector,
        "not running",
        Duration::from_secs(30),
        |s| {
            matches!(
                s,
                LinkState::Unreachable {
                    why: Unreachable::NotRunning,
                    ..
                }
            )
        },
    );
    assert!(state.to_string().contains("no daemon"), "{state}");
    // It tries again every `retry_every`, with the bridge alone.
    let bridges = |m: &Machine| {
        tunnel_calls(m)
            .iter()
            .filter(|c| {
                c.kind == "session" && c.line.as_deref().is_some_and(|l| l.contains(" connect "))
            })
            .count()
    };
    crate::unix::eventually("three tries of the bridge", || bridges(&m) >= 3);
    let refused = std::fs::read_to_string(m.dir.path().join(REFUSED))
        .unwrap_or_default()
        .lines()
        .count();
    assert_eq!(refused, 1, "the refused forward was tried again");
    rt.block_on(connector.close());
    stop_helper(&m);
}

/// A job on a node that takes no ssh, with its socket on the node's own disk: the bridge runs
/// in the job (`srun --jobid <id> --overlap`), started on the login node. A forwarded transport
/// remembered from elsewhere does not make the login link patient: nothing would watch it.
fn tunnel_bridge_through_srun_to_a_node_local_socket() {
    let site = Site {
        name: "node-local".to_owned(),
        socket: SocketPlace::NodeLocal,
        last_hop: LastHop::SrunOverlap,
        ..Site::default()
    };
    let (m, sim) = fake_slurm::machine(Config::default());
    let tmp = m.dir.path().join("node-tmp");
    private_dir(&tmp);
    sim.set(|c| c.tmpdir = Some(tmp.clone()));
    let plain = m.plain();
    pitcrew_remote_deploy(&plain);
    let script = fake_slurm::render(&plain, &site, &JobOptions::default());
    let launcher = fake_slurm::launcher(&script);
    let started = crate::unix::block_on(launcher.start(&plain)).unwrap();
    let id = started.endpoint.job.unwrap();
    let socket = started.endpoint.socket.clone();
    assert!(socket.starts_with(tmp.to_str().unwrap()), "{socket}");

    let rt = runtime();
    let daemon =
        Daemon::new(target(&m), Arc::new(launcher.clone())).with_last_hop(LastHop::SrunOverlap);
    let remembered = ConnectorOptions {
        transport: Some(Transport::Forwarded),
        ..options()
    };
    let connector = start(&rt, daemon, remembered);
    wait_for(
        &rt,
        &connector,
        "connected through srun",
        Duration::from_secs(30),
        connected(Transport::Stdio),
    );
    let data = pattern(200_000, 3);
    assert!(echo(&rt, &connector, &data) == data);
    let calls = tunnel_calls(&m);
    let links: Vec<&TunnelCall> = calls.iter().filter(|c| c.kind == "link").collect();
    assert_eq!(links.len(), 1);
    assert!(
        links[0].args.iter().any(|a| a == "ServerAliveCountMax=3"),
        "{:?}",
        links[0].args
    );

    let steps = sim.calls("srun");
    let want: Vec<String> = [
        format!("--jobid={id}"),
        "--overlap".to_owned(),
        "--nodes=1".to_owned(),
        "--ntasks=1".to_owned(),
        "--nodelist=node017".to_owned(),
        "--quiet".to_owned(),
        m.layout().binary("1.0.0"),
        "connect".to_owned(),
        "--socket".to_owned(),
        socket,
    ]
    .to_vec();
    // Then this call's `--nonce <hex>`, and `--framed`.
    assert!(
        steps
            .iter()
            .any(|s| s.starts_with(&want) && s.last().is_some_and(|a| a == "--framed")),
        "{steps:?}"
    );
    // No link to the node: the login node's link carried it all.
    assert!(
        tunnel_calls(&m)
            .iter()
            .all(|c| c.kind != "link" || c.host == "cluster")
    );
    rt.block_on(connector.close());
    crate::unix::block_on(launcher.cancel(&plain)).unwrap();
}

/// A job on a node that takes ssh: a link to the node, through the login node's link (its
/// `ProxyCommand` a channel of the login link, never a second login there).
fn tunnel_proxyjump_to_a_node() {
    let (m, _sim, launcher, _id) = job(&fake_slurm_generic(), Config::default());
    let rt = runtime();
    let connector = start(
        &rt,
        Daemon::new(target(&m), Arc::new(launcher.clone())),
        options(),
    );
    wait_for(
        &rt,
        &connector,
        "connected to the node",
        Duration::from_secs(30),
        connected(Transport::Forwarded),
    );
    let data = pattern(150_000, 5);
    assert!(echo(&rt, &connector, &data) == data);
    let calls = tunnel_calls(&m);
    let links: Vec<&TunnelCall> = calls.iter().filter(|c| c.kind == "link").collect();
    assert_eq!(
        links.iter().map(|c| c.host.as_str()).collect::<Vec<_>>(),
        ["cluster", "node017"]
    );
    let proxy = links[1].proxy.as_deref().unwrap();
    assert!(proxy.starts_with("exec "), "{proxy}");
    assert!(proxy.contains("ControlMaster=no") && proxy.contains("-W '[%h]:%p' -- cluster"));
    assert!(
        calls
            .iter()
            .any(|c| c.kind == "stdio" && c.host == "cluster"),
        "{calls:?}"
    );
    rt.block_on(connector.close());

    // The bridge to the node, through its link.
    let stdio = ConnectorOptions {
        transport: Some(Transport::Stdio),
        ..options()
    };
    let connector = start(
        &rt,
        Daemon::new(target(&m), Arc::new(launcher.clone())),
        stdio,
    );
    wait_for(
        &rt,
        &connector,
        "connected to the node",
        Duration::from_secs(30),
        connected(Transport::Stdio),
    );
    assert!(echo(&rt, &connector, &data) == data);
    assert!(
        tunnel_calls(&m)
            .iter()
            .any(|c| c.kind == "session" && c.host == "node017"),
    );
    rt.block_on(connector.close());
    crate::unix::block_on(launcher.cancel(&m.plain())).unwrap();
}

fn fake_slurm_generic() -> Site {
    pitcrew_remote::helper::slurm::generic()
}

/// The node squeue names must be the one the job recorded, and plain: otherwise nothing is
/// started towards it (no link to a node, no srun), and the machine is unreachable, refused.
fn tunnel_a_node_that_fails_the_check_is_refused() {
    let (m, sim, launcher, id) = job(&fake_slurm_generic(), Config::default());
    let rt = runtime();
    let calls_to_nodes = |m: &Machine| {
        tunnel_calls(m)
            .iter()
            .filter(|c| {
                c.host != "cluster" || c.line.as_deref().is_some_and(|l| l.contains("srun"))
            })
            .count()
    };

    // squeue runs the job on another node than its record names.
    let mut moved = sim.job(id);
    moved.node = "node018".to_owned();
    sim.save(&moved);
    let connector = start(
        &rt,
        Daemon::new(target(&m), Arc::new(launcher.clone())),
        options(),
    );
    let state = wait_for(&rt, &connector, "refused", Duration::from_secs(30), |s| {
        matches!(
            s,
            LinkState::Unreachable {
                why: Unreachable::Refused,
                ..
            }
        )
    });
    assert!(state.to_string().contains("node018"), "{state}");
    assert_eq!(calls_to_nodes(&m), 0);
    // Put right, it is picked up again from the record.
    moved.node = "node017".to_owned();
    sim.save(&moved);
    wait_for(
        &rt,
        &connector,
        "connected",
        Duration::from_secs(30),
        connected(Transport::Forwarded),
    );
    rt.block_on(connector.close());

    // A node named like one of the user's own Hosts: never connected to.
    let config = m.dir.path().join("ssh_config");
    std::fs::write(&config, "Host node017\n  HostName 192.0.2.17\n").unwrap();
    let before = calls_to_nodes(&m);
    let aliased = ConnectorOptions {
        ssh_config: Some(config),
        ..options()
    };
    let connector = start(
        &rt,
        Daemon::new(target(&m), Arc::new(launcher.clone())),
        aliased,
    );
    let state = wait_for(&rt, &connector, "refused", Duration::from_secs(30), |s| {
        matches!(
            s,
            LinkState::Unreachable {
                why: Unreachable::Refused,
                ..
            }
        )
    });
    assert!(state.to_string().contains("ssh config"), "{state}");
    assert_eq!(calls_to_nodes(&m), before);
    rt.block_on(connector.close());

    // A name that would be an option or a shell word, in both squeue and the record, with
    // either last hop.
    let endpoint = m.run_dir().join("endpoint.json");
    let record = std::fs::read_to_string(&endpoint).unwrap();
    for hostile in ["-oProxyCommand=touch", "node017;id"] {
        let before = calls_to_nodes(&m);
        let mut bad = sim.job(id);
        bad.node = hostile.to_owned();
        sim.save(&bad);
        std::fs::write(
            &endpoint,
            record.replace("\"host\":\"node017\"", &format!("\"host\":\"{hostile}\"")),
        )
        .unwrap();
        for last_hop in [LastHop::Ssh, LastHop::SrunOverlap] {
            let daemon =
                Daemon::new(target(&m), Arc::new(launcher.clone())).with_last_hop(last_hop);
            let connector = start(&rt, daemon, options());
            let state = wait_for(&rt, &connector, "refused", Duration::from_secs(30), |s| {
                matches!(
                    s,
                    LinkState::Unreachable {
                        why: Unreachable::Refused,
                        ..
                    }
                )
            });
            assert!(state.to_string().contains("node name"), "{state}");
            rt.block_on(connector.close());
        }
        assert_eq!(calls_to_nodes(&m), before, "{hostile}");
        assert!(sim.calls("srun").is_empty());
    }
    let mut restored = sim.job(id);
    restored.node = "node017".to_owned();
    sim.save(&restored);
    std::fs::write(&endpoint, record).unwrap();
    crate::unix::block_on(launcher.cancel(&m.plain())).unwrap();
}

/// The network goes: the link's keepalives notice within ten seconds, and the connector, after
/// backing off, recovers by itself once it is back.
fn tunnel_a_dropped_stream_is_unverifiable_within_ten_seconds_then_recovers() {
    let m = Machine::new();
    deploy_and_start(&m);
    let rt = runtime();
    let launcher = Arc::new(DirectLauncher::new(launch_options()));
    // Only the keepalives: no check or probe that might notice first.
    let quiet = ConnectorOptions {
        connect_wait: Duration::from_secs(3),
        check_every: Duration::from_secs(600),
        probe_every: Duration::from_secs(600),
        ..options()
    };
    let connector = start(&rt, Daemon::new(target(&m), launcher), quiet);
    wait_for(
        &rt,
        &connector,
        "connected",
        Duration::from_secs(30),
        connected(Transport::Forwarded),
    );
    let lost = Instant::now();
    net_set(&m, Net::Down);
    let state = wait_for(
        &rt,
        &connector,
        "unverifiable",
        Duration::from_secs(20),
        unverifiable,
    );
    let noticed = lost.elapsed();
    println!("the lost connection was noticed after {noticed:.1?}: {state}");
    assert!(noticed < Duration::from_secs(10), "{noticed:?}");
    assert!(state.to_string().contains("not responding"), "{state}");
    // It keeps trying: the state stays unverifiable, with the latest reason.
    std::thread::sleep(Duration::from_secs(3));
    assert!(unverifiable(&connector.state()), "{}", connector.state());
    let err = rt.block_on(async {
        tokio::time::timeout(Duration::from_secs(30), connector.connect()).await
    });
    assert!(matches!(err, Ok(Err(_))), "{err:?}");

    net_set(&m, Net::Up);
    wait_for(
        &rt,
        &connector,
        "connected again",
        Duration::from_secs(30),
        connected(Transport::Forwarded),
    );
    let data = pattern(50_000, 9);
    assert!(echo(&rt, &connector, &data) == data);
    rt.block_on(connector.close());
    stop_helper(&m);
}

/// The laptop slept: its wall clock jumped while the monotonic one stood still. The link has not
/// noticed (nothing answers, nothing times out), but the connector checks at once and finds no
/// answer; it checks again every second while the forward is silent, and is connected again
/// once the network answers.
fn tunnel_a_wall_clock_jump() {
    let m = Machine::new();
    deploy_and_start(&m);
    let rt = runtime();
    let launcher = Arc::new(DirectLauncher::new(launch_options()));
    let offset = Arc::new(AtomicU64::new(0));
    let clock = {
        let offset = offset.clone();
        WallClock::new(move || {
            SystemTime::now() + Duration::from_secs(offset.load(Ordering::SeqCst))
        })
    };
    let asleep = ConnectorOptions {
        check_every: Duration::from_secs(600),
        probe_every: Duration::from_secs(600),
        probe_timeout: Duration::from_secs(2),
        wall_clock: clock,
        ..options()
    };
    let connector = start(&rt, Daemon::new(target(&m), launcher), asleep);
    wait_for(
        &rt,
        &connector,
        "connected",
        Duration::from_secs(30),
        connected(Transport::Forwarded),
    );
    net_set(&m, Net::Frozen);
    std::thread::sleep(Duration::from_secs(3));
    assert!(connector.state().is_connected(), "{}", connector.state());

    let woke = Instant::now();
    offset.store(3600, Ordering::SeqCst);
    let state = wait_for(
        &rt,
        &connector,
        "unverifiable",
        Duration::from_secs(15),
        unverifiable,
    );
    let noticed = woke.elapsed();
    println!("the jump was acted on after {noticed:.1?}: {state}");
    assert!(noticed < Duration::from_secs(6), "{noticed:?}");

    net_set(&m, Net::Up);
    wait_for(
        &rt,
        &connector,
        "connected again",
        Duration::from_secs(30),
        connected(Transport::Forwarded),
    );
    let data = pattern(10_000, 11);
    assert!(echo(&rt, &connector, &data) == data);
    rt.block_on(connector.close());
    stop_helper(&m);
}

/// The job ends (its node fails, with no time to clean up): the machine is unreachable, saying
/// why; once a new job runs, on another node, its endpoint is picked up again; and a job the
/// launcher stopped is gone too.
fn tunnel_a_job_that_ended_then_moved() {
    let (m, sim, launcher, id) = job(&fake_slurm_generic(), Config::default());
    let rt = runtime();
    let watchful = ConnectorOptions {
        probe_every: Duration::from_secs(1),
        ..options()
    };
    let connector = start(
        &rt,
        Daemon::new(target(&m), Arc::new(launcher.clone())),
        watchful,
    );
    wait_for(
        &rt,
        &connector,
        "connected",
        Duration::from_secs(30),
        connected(Transport::Forwarded),
    );
    sim.fail_node(id);
    let not_running = |s: &LinkState| {
        matches!(
            s,
            LinkState::Unreachable {
                why: Unreachable::NotRunning,
                ..
            }
        )
    };
    let state = wait_for(
        &rt,
        &connector,
        "not running",
        Duration::from_secs(30),
        not_running,
    );
    assert!(state.to_string().contains("ended (NODE_FAIL"), "{state}");

    // A new job, on another node.
    sim.set(|c| c.node = "node018".to_owned());
    let started = crate::unix::block_on(within(
        STEP,
        "the new job's start",
        launcher.start(&m.plain()),
    ))
    .unwrap();
    assert_eq!(started.endpoint.host, "node018");
    wait_for(
        &rt,
        &connector,
        "connected to the new node",
        Duration::from_secs(30),
        connected(Transport::Forwarded),
    );
    let data = pattern(20_000, 13);
    assert!(echo(&rt, &connector, &data) == data);
    assert!(
        tunnel_calls(&m)
            .iter()
            .any(|c| c.kind == "link" && c.host == "node018")
    );
    // The login link stayed (no new sign-in for a job's end), and the node's new link got a
    // forward of its own.
    let calls = tunnel_calls(&m);
    let logins = calls
        .iter()
        .filter(|c| c.kind == "link" && c.host == "cluster")
        .count();
    assert_eq!(logins, 1, "the login node was logged in to again");
    let forwards: Vec<&str> = calls
        .iter()
        .filter(|c| c.op.as_deref() == Some("forward"))
        .map(|c| c.host.as_str())
        .collect();
    assert_eq!(forwards, ["node017", "node018"]);

    // Stopped with the launcher, which forgets it (the connector may see it end first).
    crate::unix::block_on(within(
        STEP,
        "the job's cancel",
        launcher.cancel(&m.plain()),
    ))
    .unwrap();
    wait_for(&rt, &connector, "no job", Duration::from_secs(30), |s| {
        not_running(s) && s.to_string().contains("no helper job")
    });
    rt.block_on(within(STEP, "the connector's close", connector.close()));
}

/// Answers prompts from a queue (then with the password), counting them.
struct Answers {
    queue: Mutex<VecDeque<Reply>>,
    asked: Mutex<Vec<PromptKind>>,
}

impl Answers {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            queue: Mutex::new(VecDeque::new()),
            asked: Mutex::new(Vec::new()),
        })
    }

    fn asked(&self) -> usize {
        self.asked.lock().unwrap().len()
    }
}

impl PromptHandler for Answers {
    fn prompt(&self, request: PromptRequest, _cancel: PromptCancel) -> PromptFuture<'_> {
        self.asked.lock().unwrap().push(request.kind);
        let reply = self
            .queue
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| Reply::Text(Secret::new("s3cr3t")));
        Box::pin(async move { reply })
    }
}

/// A password asked again while reconnecting goes through the askpass bridge, and is never
/// kept; a cancelled one stops the attempts until the person retries.
fn tunnel_askpass_during_a_reconnect() {
    let m = Machine::new();
    deploy_and_start(&m);
    std::fs::write(m.dir.path().join(PASSWORD), "s3cr3t").unwrap();
    let answers = Answers::new();
    let fake = m.fake(Remote::default());
    let ssh: Ssh = fake
        .ssh
        .clone()
        .with_multiplex(true)
        .with_prompts(askpass(), answers.clone());
    let target = on(&m, ssh);
    let rt = runtime();
    let launcher = Arc::new(DirectLauncher::new(launch_options()));
    let connector = start(&rt, Daemon::new(target, launcher), options());
    wait_for(
        &rt,
        &connector,
        "connected",
        Duration::from_secs(30),
        connected(Transport::Forwarded),
    );
    assert_eq!(answers.asked(), 1);

    net_set(&m, Net::Down);
    wait_for(
        &rt,
        &connector,
        "unverifiable",
        Duration::from_secs(15),
        unverifiable,
    );
    // Down until the link gives up (its keepalives), so that reconnecting signs in again.
    let link = newest_link(&m, "cluster");
    crate::unix::eventually("the link to time out", || !alive(link));
    net_set(&m, Net::Up);
    wait_for(
        &rt,
        &connector,
        "connected again",
        Duration::from_secs(30),
        connected(Transport::Forwarded),
    );
    assert_eq!(answers.asked(), 2, "asked again while reconnecting");
    let asked = std::fs::read_to_string(m.dir.path().join(ASKED)).unwrap();
    assert_eq!(asked.lines().collect::<Vec<_>>(), ["text", "text"]);

    // Cancelled while reconnecting: no more attempts, no more prompts, until a retry. (The link
    // is patient now that the forward is known to work: it is broken outright, as a reset
    // connection would be.)
    answers.queue.lock().unwrap().push_back(Reply::Cancel);
    kill_link(&m, "cluster");
    wait_for(
        &rt,
        &connector,
        "unreachable: sign-in",
        Duration::from_secs(30),
        |s| {
            matches!(
                s,
                LinkState::Unreachable {
                    why: Unreachable::SignIn,
                    ..
                }
            )
        },
    );
    let asked = answers.asked();
    std::thread::sleep(Duration::from_secs(4));
    assert_eq!(answers.asked(), asked, "asked again after a cancel");
    // An empty password was never sent.
    let sent = std::fs::read_to_string(m.dir.path().join(ASKED)).unwrap();
    assert!(!sent.contains("empty"), "{sent}");
    connector.retry();
    wait_for(
        &rt,
        &connector,
        "connected after the retry",
        Duration::from_secs(30),
        connected(Transport::Forwarded),
    );
    // The password is nowhere: not in the state, the connector, or any log of the fake's.
    let mut seen = format!("{connector:?} {}", connector.state());
    for entry in std::fs::read_dir(m.dir.path()).unwrap() {
        let path = entry.unwrap().path();
        if path.is_file() && path.file_name().unwrap() != PASSWORD {
            seen.push_str(&String::from_utf8_lossy(&std::fs::read(&path).unwrap()));
        }
    }
    assert!(!seen.contains("s3cr3t"));
    rt.block_on(connector.close());
    stop_helper(&m);
}

/// The endpoint check and the bridge (`'exec' <helper> connect …`) with each POSIX shell as the
/// machine's `sh`, through both transports.
fn tunnel_under_every_posix_sh() {
    let mut checked = Vec::new();
    for shell in crate::unix::posix_shells() {
        let m = Machine::with_shell(&shell);
        deploy_and_start(&m);
        let rt = runtime();
        let launcher = Arc::new(DirectLauncher::new(launch_options()));
        for transport in [Transport::Stdio, Transport::Forwarded] {
            let chosen = ConnectorOptions {
                transport: Some(transport),
                ..options()
            };
            let connector = start(&rt, Daemon::new(target(&m), launcher.clone()), chosen);
            wait_for(
                &rt,
                &connector,
                &format!("connected with {}", shell.display()),
                Duration::from_secs(30),
                connected(transport),
            );
            let data = pattern(30_000, 17);
            assert!(echo(&rt, &connector, &data) == data, "{}", shell.display());
            rt.block_on(connector.close());
        }
        stop_helper(&m);
        checked.push(shell.display().to_string());
    }
    println!("tunnels checked with sh = {checked:?}");
}

// ─── Limits and failures ───────────────────────────────────────────────────────────────────

/// The forwarded socket removed under the connector (a temporary-file cleaner, say): the probe
/// fails, the master still answers, and the forward is made again, without a new sign-in.
fn tunnel_a_removed_forward_is_made_again() {
    let m = Machine::new();
    deploy_and_start(&m);
    let fake = m.fake(Remote::default());
    let rt = runtime();
    let launcher = Arc::new(DirectLauncher::new(launch_options()));
    let connector = start(&rt, Daemon::new(reuse(&m, &fake), launcher), options());
    wait_for(
        &rt,
        &connector,
        "connected",
        Duration::from_secs(30),
        connected(Transport::Forwarded),
    );
    let dirs = private_dirs(&fake);
    let forwarded: Vec<PathBuf> = std::fs::read_dir(&dirs[0])
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.file_name().unwrap().to_str().unwrap().starts_with('f'))
        .collect();
    assert_eq!(forwarded.len(), 1);
    std::fs::remove_file(&forwarded[0]).unwrap();
    let forwards = |m: &Machine| {
        tunnel_calls(m)
            .iter()
            .filter(|c| c.op.as_deref() == Some("forward"))
            .count()
    };
    crate::unix::eventually("a new forward", || forwards(&m) == 2);
    wait_for(
        &rt,
        &connector,
        "connected again",
        Duration::from_secs(30),
        connected(Transport::Forwarded),
    );
    assert_eq!(links(&m), 1, "logged in again");
    let data = pattern(10_000, 35);
    assert!(echo(&rt, &connector, &data) == data);
    rt.block_on(connector.close());
    stop_helper(&m);
}

/// sshd's `MaxSessions` reached: one connection more is refused, as that connection's error
/// alone. Watching opens no session, the state stays connected and the link stays; once one
/// ends, the next opens.
fn tunnel_a_session_over_max_sessions_is_refused_alone() {
    let m = Machine::new();
    deploy_and_start(&m);
    std::fs::write(m.dir.path().join(MAX_SESSIONS), "2").unwrap();
    let rt = runtime();
    let launcher = Arc::new(DirectLauncher::new(launch_options()));
    let stdio = ConnectorOptions {
        transport: Some(Transport::Stdio),
        ..options()
    };
    let connector = start(&rt, Daemon::new(target(&m), launcher), stdio);
    wait_for(
        &rt,
        &connector,
        "connected",
        Duration::from_secs(30),
        connected(Transport::Stdio),
    );
    let a = open_when_free(&rt, &connector);
    let b = open_when_free(&rt, &connector);
    let err = rt.block_on(connector.connect()).unwrap_err();
    assert!(
        matches!(err, TunnelError::Ssh(SshError::SessionRefused { .. })),
        "{err:?}"
    );
    assert!(err.to_string().contains("MaxSessions"), "{err}");
    // Checks (`-O check`) and keepalives go on meanwhile; nothing else.
    let sessions = |m: &Machine| {
        tunnel_calls(m)
            .iter()
            .filter(|c| c.kind == "session")
            .count()
    };
    let before = sessions(&m);
    std::thread::sleep(Duration::from_secs(5));
    assert_eq!(
        connector.state(),
        LinkState::Connected {
            transport: Transport::Stdio
        }
    );
    assert_eq!(sessions(&m), before, "watching opened sessions");
    assert_eq!(links(&m), 1);
    // Both still carry their connections; then the next one opens.
    let data = pattern(20_000, 19);
    let (back_a, back_b) =
        rt.block_on(async { tokio::join!(exchange(a, data.clone()), exchange(b, data.clone())) });
    assert!(back_a == data && back_b == data);
    let c = open_when_free(&rt, &connector);
    assert!(rt.block_on(exchange(c, data.clone())) == data);
    rt.block_on(connector.close());
    stop_helper(&m);
}

/// A forward that failed for another reason than a refusal: the bridge carries the
/// connections, nothing is remembered, and the forward is tried again next time.
fn tunnel_a_forward_that_failed_once_is_not_remembered() {
    let m = Machine::new();
    deploy_and_start(&m);
    std::fs::write(m.dir.path().join(FORWARD_FAIL_ONCE), "").unwrap();
    let rt = runtime();
    let launcher = Arc::new(DirectLauncher::new(launch_options()));
    let connector = start(&rt, Daemon::new(target(&m), launcher), options());
    wait_for(
        &rt,
        &connector,
        "connected through the bridge",
        Duration::from_secs(30),
        connected(Transport::Stdio),
    );
    assert_eq!(connector.transport(), None);
    let data = pattern(20_000, 21);
    assert!(echo(&rt, &connector, &data) == data);

    net_set(&m, Net::Down);
    wait_for(
        &rt,
        &connector,
        "unverifiable",
        Duration::from_secs(15),
        unverifiable,
    );
    net_set(&m, Net::Up);
    wait_for(
        &rt,
        &connector,
        "connected through a forward",
        Duration::from_secs(30),
        connected(Transport::Forwarded),
    );
    assert_eq!(connector.transport(), Some(Transport::Forwarded));
    assert!(echo(&rt, &connector, &data) == data);
    rt.block_on(connector.close());
    stop_helper(&m);
}

/// Through srun, which passes a job step's output on a line at a time: the bridge frames its
/// output, so a reply without a newline still comes at once, even where the login node's
/// environment asks srun to label its lines (`SLURM_LABELIO`). And watching starts no job step:
/// one proves the way, then one per connection.
fn tunnel_srun_line_buffering_and_job_steps() {
    let site = Site {
        name: "node-local".to_owned(),
        socket: SocketPlace::NodeLocal,
        last_hop: LastHop::SrunOverlap,
        ..Site::default()
    };
    let (m, sim) = fake_slurm::machine(Config::default());
    let tmp = m.dir.path().join("node-tmp");
    private_dir(&tmp);
    sim.set(|c| {
        c.tmpdir = Some(tmp.clone());
        c.line_buffered = true;
    });
    let plain = m.plain();
    pitcrew_remote_deploy(&plain);
    let script = fake_slurm::render(&plain, &site, &JobOptions::default());
    let launcher = fake_slurm::launcher(&script);
    crate::unix::block_on(launcher.start(&plain)).unwrap();

    let rt = runtime();
    let labelling = m.fake(Remote {
        env: vec![("SLURM_LABELIO".to_owned(), "1".to_owned())],
        ..Remote::default()
    });
    let daemon = Daemon::new(reuse(&m, &labelling), Arc::new(launcher.clone()))
        .with_last_hop(LastHop::SrunOverlap);
    let connector = start(&rt, daemon, options());
    wait_for(
        &rt,
        &connector,
        "connected through srun",
        Duration::from_secs(30),
        connected(Transport::Stdio),
    );
    assert_eq!(sim.calls("srun").len(), 1);
    assert!(
        sim.calls("srun")[0].iter().any(|a| a == "--framed"),
        "{:?}",
        sim.calls("srun")
    );
    rt.block_on(async {
        let mut stream = connector.connect().await.unwrap();
        for word in [&b"ping"[..], b"pong"] {
            stream.write_all(word).await.unwrap();
            stream.flush().await.unwrap();
            let mut got = [0u8; 4];
            tokio::time::timeout(Duration::from_secs(5), stream.read_exact(&mut got))
                .await
                .expect("a reply without a newline was held back")
                .unwrap();
            assert_eq!(&got[..], word);
        }
    });
    let data = pattern(100_000, 23);
    assert!(echo(&rt, &connector, &data) == data);
    // Several rounds of checks later: still one step per connection.
    std::thread::sleep(Duration::from_secs(6));
    assert_eq!(sim.calls("srun").len(), 3);
    rt.block_on(connector.close());
    crate::unix::block_on(launcher.cancel(&plain)).unwrap();
}

/// An app that died with its connector running leaves its link (a ControlMaster, which outlives
/// it) and its directory. The next connector stops that link and removes the directory, and
/// leaves those of live connectors alone.
fn tunnel_links_left_by_a_crash_are_stopped() {
    let m = Machine::new();
    deploy_and_start(&m);
    let fake = m.fake(Remote::default());
    let rt = runtime();
    let launcher = Arc::new(DirectLauncher::new(launch_options()));
    let live = start(
        &rt,
        Daemon::new(reuse(&m, &fake), launcher.clone()),
        options(),
    );
    wait_for(
        &rt,
        &live,
        "connected",
        Duration::from_secs(30),
        connected(Transport::Forwarded),
    );

    let spec = m.dir.path().join("app.json");
    let app = AppSpec {
        ssh: fake.dir.join("ssh"),
        runtime_dir: fake.dir.join("rt"),
        root: m.layout().root().to_owned(),
        tool_path: m.bin.to_str().unwrap().to_owned(),
    };
    std::fs::write(&spec, serde_json::to_vec(&app).unwrap()).unwrap();
    let status = Command::new(me())
        .env(APP_ENV, &spec)
        .env(RUN_ENV, run_mark())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .status()
        .unwrap();
    assert!(status.success(), "the app did not connect: {status:?}");
    let pids: Vec<u32> = tunnel_calls(&m)
        .iter()
        .filter(|c| c.kind == "link")
        .map(|c| c.pid)
        .collect();
    assert_eq!(pids.len(), 2);
    let (live_link, left) = (pids[0], pids[1]);
    assert!(alive(left), "the crashed app's link is gone already");
    let dirs = private_dirs(&fake);
    assert_eq!(dirs.len(), 2, "{dirs:?}");

    let next = start(&rt, Daemon::new(reuse(&m, &fake), launcher), options());
    wait_for(
        &rt,
        &next,
        "connected",
        Duration::from_secs(30),
        connected(Transport::Forwarded),
    );
    crate::unix::eventually("the crashed app's link to stop", || !alive(left));
    let now = private_dirs(&fake);
    assert_eq!(now.len(), 2, "{now:?}");
    let gone: Vec<&PathBuf> = dirs.iter().filter(|d| !now.contains(d)).collect();
    assert_eq!(gone.len(), 1, "{dirs:?} {now:?}");
    assert!(alive(live_link));
    let data = pattern(10_000, 27);
    assert!(echo(&rt, &live, &data) == data);
    rt.block_on(live.close());
    rt.block_on(next.close());
    stop_helper(&m);
}

/// A burst of failed connections (the helper refuses its socket now): one check of where the
/// daemon is, not one per connection. Paced by events, not the clock: the check the first
/// failure asks for is held open until the burst is over (no other may start while one runs),
/// so the burst may take as long as the machine needs.
fn tunnel_a_burst_of_failures_makes_one_check() {
    use std::os::unix::fs::PermissionsExt as _;
    let m = Machine::new();
    deploy_and_start(&m);
    let rt = runtime();
    let launcher = Arc::new(DirectLauncher::new(launch_options()));
    let stdio = ConnectorOptions {
        transport: Some(Transport::Stdio),
        ..options()
    };
    let connector = start(&rt, Daemon::new(target(&m), launcher), stdio);
    wait_for(
        &rt,
        &connector,
        "connected",
        Duration::from_secs(30),
        connected(Transport::Stdio),
    );
    // The helper's `connect` refuses from now on.
    let binary = PathBuf::from(m.layout().binary("1.0.0"));
    let script = std::fs::read_to_string(&binary).unwrap();
    let mut refusing: String = script
        .lines()
        .map(|line| {
            if line.starts_with("connect)") {
                "connect) echo 'pitcrewd connect: the socket is not safe to use: x' >&2; exit 3 ;;"
            } else {
                line
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    refusing.push('\n');
    let was = mode(&binary);
    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::write(&binary, refusing).unwrap();
    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(was)).unwrap();

    let checks = |m: &Machine| {
        tunnel_calls(m)
            .iter()
            .filter(|c| {
                c.kind == "session" && !c.line.as_deref().is_some_and(|l| l.contains(" connect "))
            })
            .count()
    };
    let refused = || {
        let err = rt.block_on(connector.connect()).unwrap_err();
        assert!(matches!(err, TunnelError::Refused(_)), "{err:?}");
    };
    let before = checks(&m);
    let hold = m.dir.path().join(HOLD_CHECKS);
    std::fs::write(&hold, "").unwrap();
    refused();
    crate::unix::eventually("a check", || checks(&m) > before);
    for _ in 1..15 {
        refused();
    }
    assert_eq!(checks(&m) - before, 1, "checks for a burst of 15 failures");
    std::fs::remove_file(&hold).unwrap();
    // The failures after the first asked too: one more check comes once the first is over and
    // the gap (10 s) has passed, not none. Nothing asks for a third.
    let asked = Instant::now();
    while checks(&m) - before < 2 {
        assert!(
            asked.elapsed() < Duration::from_secs(120),
            "no deferred check"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    std::thread::sleep(Duration::from_secs(1));
    assert_eq!(checks(&m) - before, 2);
    assert_eq!(links(&m), 1);
    assert!(connector.state().is_connected(), "{}", connector.state());
    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::write(&binary, script).unwrap();
    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(was)).unwrap();
    let data = pattern(10_000, 29);
    assert!(echo(&rt, &connector, &data) == data);
    rt.block_on(connector.close());
    stop_helper(&m);
}

/// Each attempt from unreachable (a wake's, a retry's, or its time's) is `Connecting` until it
/// ends, so a watcher sees where it ends though it fails again for the same reason; connections
/// wait for it. Paced by holding the attempt's endpoint check, not by the clock.
fn tunnel_attempts_from_unreachable_show() {
    let m = Machine::new();
    pitcrew_remote_deploy(&m.plain());
    let rt = runtime();
    let launcher = Arc::new(DirectLauncher::new(launch_options()));
    // No attempt comes by itself while the case runs.
    let patient = ConnectorOptions {
        retry_every: Duration::from_secs(600),
        ..options()
    };
    let connector = start(&rt, Daemon::new(target(&m), launcher), patient);
    let not_running = |s: &LinkState| {
        matches!(
            s,
            LinkState::Unreachable {
                why: Unreachable::NotRunning,
                ..
            }
        )
    };
    let first = wait_for(
        &rt,
        &connector,
        "not running",
        Duration::from_secs(30),
        not_running,
    );
    let hold = m.dir.path().join(HOLD_CHECKS);

    // A wake's attempt: connecting while it runs, and a connection waits for it.
    std::fs::write(&hold, "").unwrap();
    connector.wake();
    wait_for(
        &rt,
        &connector,
        "the wake's attempt",
        Duration::from_secs(30),
        |s| *s == LinkState::Connecting,
    );
    let waiting = {
        let connector = connector.clone();
        rt.spawn(async move { connector.connect().await.map(drop) })
    };
    std::thread::sleep(Duration::from_millis(500));
    assert!(!waiting.is_finished(), "a connection did not wait");
    std::fs::remove_file(&hold).unwrap();
    let again = wait_for(
        &rt,
        &connector,
        "not running again",
        Duration::from_secs(30),
        not_running,
    );
    assert_eq!(again, first);
    let err = rt.block_on(waiting).unwrap().unwrap_err();
    assert!(
        matches!(err, TunnelError::NotConnected(ref s) if not_running(s)),
        "{err:?}"
    );

    // A retry's attempt, followed as the README shows: its start is a change, and so its end.
    std::fs::write(&hold, "").unwrap();
    let mut state = connector.watch();
    state.borrow_and_update();
    connector.retry();
    rt.block_on(state.changed()).unwrap();
    assert_eq!(*state.borrow_and_update(), LinkState::Connecting);
    std::fs::remove_file(&hold).unwrap();
    let end = rt
        .block_on(state.wait_for(|s| *s != LinkState::Connecting))
        .unwrap()
        .clone();
    assert_eq!(end, first);
    rt.block_on(connector.close());
}

/// Without connection reuse (as on Windows) each connection signs in by itself. Closing the
/// connector ends the open ones; a sign-in cancelled for a connection stops the attempts (no
/// more prompts) until the person retries.
fn tunnel_without_connection_reuse() {
    let m = Machine::new();
    deploy_and_start(&m);
    std::fs::write(m.dir.path().join(PASSWORD), "s3cr3t").unwrap();
    let answers = Answers::new();
    let fake = m.fake(Remote::default());
    let ssh = fake
        .ssh
        .clone()
        .with_multiplex(false)
        .with_prompts(askpass(), answers.clone());
    let target = on(&m, ssh);
    let rt = runtime();
    let launcher = Arc::new(DirectLauncher::new(launch_options()));
    let connector = start(
        &rt,
        Daemon::new(target.clone(), launcher.clone()),
        options(),
    );
    wait_for(
        &rt,
        &connector,
        "connected",
        Duration::from_secs(30),
        connected(Transport::Stdio),
    );
    let asked = answers.asked();
    let data = pattern(10_000, 31);
    assert!(echo(&rt, &connector, &data) == data);
    assert_eq!(answers.asked(), asked + 1, "a connection signs in");

    // Two held open: closing ends them, and their ssh.
    let mut held = vec![
        rt.block_on(connector.connect()).unwrap(),
        rt.block_on(connector.connect()).unwrap(),
    ];
    let logins: Vec<u32> = tunnel_calls(&m)
        .iter()
        .filter(|c| c.kind == "login")
        .map(|c| c.pid)
        .collect();
    rt.block_on(connector.close());
    rt.block_on(async {
        for stream in &mut held {
            let mut buf = [0u8; 16];
            let read = tokio::time::timeout(Duration::from_secs(5), stream.read(&mut buf))
                .await
                .expect("a connection outlived the connector");
            assert!(matches!(read, Ok(0) | Err(_)), "{read:?}");
        }
    });
    crate::unix::eventually("the connections' ssh to end", || {
        logins.iter().all(|pid| !alive(*pid))
    });

    // A prompt cancelled for a connection.
    let connector = start(&rt, Daemon::new(target, launcher), options());
    wait_for(
        &rt,
        &connector,
        "connected",
        Duration::from_secs(30),
        connected(Transport::Stdio),
    );
    answers.queue.lock().unwrap().push_back(Reply::Cancel);
    let err = rt.block_on(connector.connect()).unwrap_err();
    assert!(
        matches!(err, TunnelError::Ssh(SshError::Cancelled)),
        "{err:?}"
    );
    wait_for(
        &rt,
        &connector,
        "unreachable: sign-in",
        Duration::from_secs(15),
        |s| {
            matches!(
                s,
                LinkState::Unreachable {
                    why: Unreachable::SignIn,
                    ..
                }
            )
        },
    );
    let asked = answers.asked();
    std::thread::sleep(Duration::from_secs(4));
    assert_eq!(answers.asked(), asked, "asked again after a cancel");
    connector.retry();
    wait_for(
        &rt,
        &connector,
        "connected after the retry",
        Duration::from_secs(30),
        connected(Transport::Stdio),
    );
    assert!(echo(&rt, &connector, &data) == data);
    rt.block_on(connector.close());
    stop_helper(&m);
}

/// A link that keeps dropping soon after it connects: each drop counts towards giving up, and
/// the connector ends unreachable, saying so. If reconnecting asked the person each time (a
/// password here, a one-time code in life), it waits for a wake or a retry; if not, it tries
/// again every `retry_every`.
fn tunnel_a_link_that_keeps_dropping_is_given_up() {
    let keeps_dropping = |s: &LinkState| {
        matches!(
            s,
            LinkState::Unreachable {
                why: Unreachable::Network,
                ..
            }
        )
    };
    let short = ConnectorOptions {
        give_up_after: Duration::from_secs(5),
        ..options()
    };
    let rt = runtime();

    // Each reconnection asks for the password.
    let m = Machine::new();
    deploy_and_start(&m);
    std::fs::write(m.dir.path().join(DROP_AFTER), "2").unwrap();
    std::fs::write(m.dir.path().join(PASSWORD), "s3cr3t").unwrap();
    let answers = Answers::new();
    let fake = m.fake(Remote::default());
    let ssh = fake
        .ssh
        .clone()
        .with_multiplex(true)
        .with_prompts(askpass(), answers.clone());
    let launcher = Arc::new(DirectLauncher::new(launch_options()));
    let connector = start(&rt, Daemon::new(on(&m, ssh), launcher), short.clone());
    let state = wait_for(
        &rt,
        &connector,
        "unreachable",
        Duration::from_secs(40),
        keeps_dropping,
    );
    assert!(state.to_string().contains("keeps dropping"), "{state}");
    let tried = links(&m);
    assert!(tried >= 2, "{tried}");
    let asked = answers.asked();
    std::thread::sleep(Duration::from_secs(5));
    assert_eq!(links(&m), tried, "it tried again by itself");
    assert_eq!(answers.asked(), asked);
    std::fs::remove_file(m.dir.path().join(DROP_AFTER)).unwrap();
    connector.retry();
    wait_for(
        &rt,
        &connector,
        "connected after the retry",
        Duration::from_secs(30),
        connected(Transport::Forwarded),
    );
    rt.block_on(connector.close());
    stop_helper(&m);

    // Reconnecting asks nothing (keys): it goes on trying, every `retry_every` (2 s here).
    let m = Machine::new();
    deploy_and_start(&m);
    std::fs::write(m.dir.path().join(DROP_AFTER), "2").unwrap();
    let launcher = Arc::new(DirectLauncher::new(launch_options()));
    let connector = start(&rt, Daemon::new(target(&m), launcher), short);
    wait_for(
        &rt,
        &connector,
        "unreachable",
        Duration::from_secs(40),
        keeps_dropping,
    );
    let tried = links(&m);
    crate::unix::eventually("another try", || links(&m) > tried);
    std::fs::remove_file(m.dir.path().join(DROP_AFTER)).unwrap();
    wait_for(
        &rt,
        &connector,
        "connected by itself",
        Duration::from_secs(30),
        connected(Transport::Forwarded),
    );
    rt.block_on(connector.close());
    stop_helper(&m);
}

/// A link known to carry a working forward is patient: a short silence makes the state
/// unverifiable within seconds (the forward's probe goes unanswered), but the link stays, and
/// when answers come again the state is connected again, on the same link.
fn tunnel_a_short_silence_keeps_a_patient_link() {
    let m = Machine::new();
    deploy_and_start(&m);
    let rt = runtime();
    let launcher = Arc::new(DirectLauncher::new(launch_options()));
    let known = ConnectorOptions {
        transport: Some(Transport::Forwarded),
        ..options()
    };
    let connector = start(&rt, Daemon::new(target(&m), launcher), known);
    wait_for(
        &rt,
        &connector,
        "connected",
        Duration::from_secs(30),
        connected(Transport::Forwarded),
    );
    let calls = tunnel_calls(&m);
    let link = calls.iter().find(|c| c.kind == "link").unwrap();
    assert!(
        link.args.iter().any(|a| a == "ServerAliveCountMax=14"),
        "{:?}",
        link.args
    );

    let lost = Instant::now();
    net_set(&m, Net::Down);
    let state = wait_for(
        &rt,
        &connector,
        "unverifiable",
        Duration::from_secs(15),
        unverifiable,
    );
    let noticed = lost.elapsed();
    println!("the silence was noticed after {noticed:.1?}: {state}");
    assert!(noticed < Duration::from_secs(10), "{noticed:?}");
    // Longer than an impatient link would last.
    std::thread::sleep(Duration::from_secs(8));
    net_set(&m, Net::Up);
    wait_for(
        &rt,
        &connector,
        "connected again",
        Duration::from_secs(15),
        connected(Transport::Forwarded),
    );
    assert_eq!(links(&m), 1, "the link was replaced");
    let data = pattern(10_000, 33);
    assert!(echo(&rt, &connector, &data) == data);
    rt.block_on(connector.close());
    stop_helper(&m);
}

/// Through srun nothing watches the job (a probe would be a job step): when it ends, the next
/// connection fails ("Invalid job id"), and that makes the connector ask where the helper is.
/// The machine is then unreachable, not running, saying why; a new job is picked up.
fn tunnel_an_srun_job_that_ends_is_noticed() {
    let site = Site {
        name: "node-local".to_owned(),
        socket: SocketPlace::NodeLocal,
        last_hop: LastHop::SrunOverlap,
        ..Site::default()
    };
    let (m, sim) = fake_slurm::machine(Config::default());
    let tmp = m.dir.path().join("node-tmp");
    private_dir(&tmp);
    sim.set(|c| c.tmpdir = Some(tmp.clone()));
    let plain = m.plain();
    pitcrew_remote_deploy(&plain);
    let script = fake_slurm::render(&plain, &site, &JobOptions::default());
    let launcher = fake_slurm::launcher(&script);
    let started = crate::unix::block_on(launcher.start(&plain)).unwrap();
    let id = started.endpoint.job.unwrap();

    let rt = runtime();
    let daemon =
        Daemon::new(target(&m), Arc::new(launcher.clone())).with_last_hop(LastHop::SrunOverlap);
    let connector = start(&rt, daemon, options());
    wait_for(
        &rt,
        &connector,
        "connected through srun",
        Duration::from_secs(30),
        connected(Transport::Stdio),
    );
    sim.fail_node(id);
    // Nothing notices by itself.
    std::thread::sleep(Duration::from_secs(3));
    assert!(connector.state().is_connected(), "{}", connector.state());
    let err = rt.block_on(connector.connect()).unwrap_err();
    assert!(
        matches!(&err, TunnelError::Bridge(why) if why.contains("Invalid job id")),
        "{err:?}"
    );
    let state = wait_for(
        &rt,
        &connector,
        "not running",
        Duration::from_secs(30),
        |s| {
            matches!(
                s,
                LinkState::Unreachable {
                    why: Unreachable::NotRunning,
                    ..
                }
            )
        },
    );
    assert!(state.to_string().contains("ended (NODE_FAIL"), "{state}");

    // A new job.
    let again = crate::unix::block_on(launcher.start(&plain)).unwrap();
    assert_ne!(again.endpoint.job, Some(id));
    wait_for(
        &rt,
        &connector,
        "connected to the new job",
        Duration::from_secs(30),
        connected(Transport::Stdio),
    );
    let data = pattern(20_000, 37);
    assert!(echo(&rt, &connector, &data) == data);
    rt.block_on(connector.close());
    crate::unix::block_on(launcher.cancel(&plain)).unwrap();
}

/// A node-local socket is never forwarded, even where the node takes ssh and forwarding works:
/// its directory goes with the job, and someone else on the node could make it again, which a
/// forward would not notice. The bridge, which checks who listens, carries the connections.
fn tunnel_a_node_local_socket_is_never_forwarded() {
    let site = Site {
        name: "node-local".to_owned(),
        socket: SocketPlace::NodeLocal,
        last_hop: LastHop::Ssh,
        ..Site::default()
    };
    let (m, sim) = fake_slurm::machine(Config::default());
    let tmp = m.dir.path().join("node-tmp");
    private_dir(&tmp);
    sim.set(|c| c.tmpdir = Some(tmp.clone()));
    let plain = m.plain();
    pitcrew_remote_deploy(&plain);
    let script = fake_slurm::render(&plain, &site, &JobOptions::default());
    let launcher = fake_slurm::launcher(&script);
    crate::unix::block_on(launcher.start(&plain)).unwrap();

    let rt = runtime();
    for transport in [None, Some(Transport::Forwarded)] {
        let chosen = ConnectorOptions {
            transport,
            ..options()
        };
        let connector = start(
            &rt,
            Daemon::new(target(&m), Arc::new(launcher.clone())),
            chosen,
        );
        wait_for(
            &rt,
            &connector,
            "connected to the node",
            Duration::from_secs(30),
            connected(Transport::Stdio),
        );
        let data = pattern(20_000, 39);
        assert!(echo(&rt, &connector, &data) == data);
        rt.block_on(connector.close());
    }
    let calls = tunnel_calls(&m);
    assert!(
        calls.iter().all(|c| c.op.as_deref() != Some("forward")),
        "{calls:?}"
    );
    // The node's links waited no more than 8 s of silence: nothing else watches them.
    for link in calls
        .iter()
        .filter(|c| c.kind == "link" && c.host == "node017")
    {
        assert!(link.args.iter().any(|a| a == "ServerAliveCountMax=3"));
    }
    crate::unix::block_on(launcher.cancel(&plain)).unwrap();
}

/// A forward that worked before (remembered) gives no answer now: the bridge carries the
/// connections, and the link, started patient for the forward, is started again impatient, so a
/// lost network is still noticed within ten seconds.
fn tunnel_a_silent_forward_leaves_no_patient_link() {
    let m = Machine::new();
    deploy_and_start(&m);
    std::fs::write(m.dir.path().join(FORWARD_SILENT), "").unwrap();
    let rt = runtime();
    let launcher = Arc::new(DirectLauncher::new(launch_options()));
    let remembered = ConnectorOptions {
        transport: Some(Transport::Forwarded),
        ..options()
    };
    let connector = start(&rt, Daemon::new(target(&m), launcher), remembered);
    wait_for(
        &rt,
        &connector,
        "connected through the bridge",
        Duration::from_secs(30),
        connected(Transport::Stdio),
    );
    let calls = tunnel_calls(&m);
    let counts: Vec<bool> = calls
        .iter()
        .filter(|c| c.kind == "link")
        .map(|c| c.args.iter().any(|a| a == "ServerAliveCountMax=14"))
        .collect();
    assert_eq!(counts, [true, false], "patient, then not");
    let lost = Instant::now();
    net_set(&m, Net::Down);
    wait_for(
        &rt,
        &connector,
        "unverifiable",
        Duration::from_secs(15),
        unverifiable,
    );
    let noticed = lost.elapsed();
    assert!(noticed < Duration::from_secs(10), "{noticed:?}");
    net_set(&m, Net::Up);
    std::fs::remove_file(m.dir.path().join(FORWARD_SILENT)).unwrap();
    wait_for(
        &rt,
        &connector,
        "connected again",
        Duration::from_secs(30),
        |s| s.is_connected(),
    );
    rt.block_on(connector.close());
    stop_helper(&m);
}

/// squeue fails for a while (the controller restarting): each try asks again through the same
/// login, never signing in again for it.
fn tunnel_a_failing_endpoint_check_keeps_the_login() {
    let (m, sim, launcher, _id) = job(&fake_slurm_generic(), Config::default());
    sim.set(|c| {
        c.squeue_error = Some(
            "slurm_load_jobs error: Unable to contact slurm controller (connect failure)"
                .to_owned(),
        );
    });
    let rt = runtime();
    let connector = start(
        &rt,
        Daemon::new(target(&m), Arc::new(launcher.clone())),
        options(),
    );
    let state = wait_for(
        &rt,
        &connector,
        "unverifiable",
        Duration::from_secs(30),
        unverifiable,
    );
    assert!(state.to_string().contains("Unable to contact"), "{state}");
    // Several back-off steps (200 ms to 2 s).
    let tries = || sim.calls("squeue").len();
    crate::unix::eventually("four tries", || tries() >= 4);
    let logins = |m: &Machine| {
        tunnel_calls(m)
            .iter()
            .filter(|c| c.kind == "link" && c.host == "cluster")
            .count()
    };
    assert_eq!(logins(&m), 1, "signed in again for a failing squeue");
    sim.set(|c| c.squeue_error = None);
    wait_for(
        &rt,
        &connector,
        "connected",
        Duration::from_secs(30),
        connected(Transport::Forwarded),
    );
    assert_eq!(logins(&m), 1);
    rt.block_on(connector.close());
    crate::unix::block_on(launcher.cancel(&m.plain())).unwrap();
}

/// Without connection reuse (Windows) each endpoint check is a login of its own: once one asked
/// for a password, a queued job is asked about no more often than `retry_every`, not at every
/// back-off step.
fn tunnel_polling_that_asks_for_a_password_is_rare() {
    let (m, sim) = fake_slurm::machine(Config::default());
    sim.set(|c| c.start = false);
    let plain = m.plain();
    pitcrew_remote_deploy(&plain);
    let script = fake_slurm::render(&plain, &fake_slurm_generic(), &JobOptions::default());
    let launcher = fake_slurm::launcher(&script);
    crate::unix::block_on(launcher.submit(&plain)).unwrap();
    std::fs::write(m.dir.path().join(PASSWORD), "s3cr3t").unwrap();
    let answers = Answers::new();
    let fake = m.fake(Remote::default());
    let ssh = fake
        .ssh
        .clone()
        .with_multiplex(false)
        .with_prompts(askpass(), answers.clone());
    let rt = runtime();
    let polling = ConnectorOptions {
        retry_every: Duration::from_secs(30),
        ..options()
    };
    let connector = start(
        &rt,
        Daemon::new(on(&m, ssh), Arc::new(launcher.clone())),
        polling,
    );
    let state = wait_for(
        &rt,
        &connector,
        "queued",
        Duration::from_secs(30),
        unverifiable,
    );
    assert!(state.to_string().contains("pending"), "{state}");
    let asked = answers.asked();
    std::thread::sleep(Duration::from_secs(6));
    assert_eq!(answers.asked(), asked, "asked again within retry_every");
    // A retry asks now.
    connector.retry();
    crate::unix::eventually("asked after the retry", || answers.asked() > asked);
    rt.block_on(connector.close());
    crate::unix::block_on(launcher.cancel(&plain)).unwrap();
}

// ─── The bridge alone ──────────────────────────────────────────────────────────────────────

/// The fake daemon, serving `<dir>/run/pitcrewd.sock`.
struct Served {
    daemon: std::process::Child,
    socket: PathBuf,
}

impl Served {
    fn new(dir: &Path) -> Self {
        let run = dir.join("run");
        private_dir(&run);
        let socket = run.join("pitcrewd.sock");
        let daemon = Command::new(daemon())
            .env("PITCREW_FAKE_DAEMON", "serve")
            .env(RUN_ENV, run_mark())
            .arg("serve")
            .arg("--listen")
            .arg(format!("unix:{}", socket.display()))
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        crate::unix::eventually("the daemon's socket", || socket.exists());
        Self { daemon, socket }
    }
}

impl Drop for Served {
    fn drop(&mut self) {
        let _ = self.daemon.kill();
        let _ = self.daemon.wait();
    }
}

/// `pitcrewd connect --socket <socket>`, with pipes for stdin, stdout and stderr.
fn bridge(socket: &Path) -> std::process::Child {
    Command::new(daemon())
        .env("PITCREW_FAKE_DAEMON", "connect")
        .env(RUN_ENV, run_mark())
        .arg("connect")
        .arg("--socket")
        .arg(socket)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap()
}

/// Reads the bridge's ready mark, which comes first.
fn read_ready(out: &mut impl Read) {
    let mut mark = vec![0u8; pitcrew_remote::bridge::READY.len()];
    out.read_exact(&mut mark).unwrap();
    assert_eq!(mark, pitcrew_remote::bridge::READY);
}

/// Every byte value both ways, and a large transfer, exactly.
fn bridge_is_byte_exact_both_ways() {
    let dir = tempfile::tempdir().unwrap();
    let served = Served::new(dir.path());
    for data in [
        (0..=255u8)
            .cycle()
            .skip(1)
            .take(256 * 40)
            .collect::<Vec<u8>>(),
        pattern(24 * 1024 * 1024, 1),
    ] {
        let mut child = bridge(&served.socket);
        let mut stdin = child.stdin.take().unwrap();
        let mut stdout = child.stdout.take().unwrap();
        let sent = data.clone();
        let writer = std::thread::spawn(move || {
            stdin.write_all(&sent).unwrap();
        });
        read_ready(&mut stdout);
        let mut back = Vec::new();
        stdout.read_to_end(&mut back).unwrap();
        writer.join().unwrap();
        assert_eq!(back.len(), data.len());
        assert!(back == data, "the echo differs");
        assert!(child.wait().unwrap().success());
    }
}

/// Either side may stop sending first; the other direction goes on.
fn bridge_half_closes_both_ways() {
    let dir = tempfile::tempdir().unwrap();
    let served = Served::new(dir.path());
    // The client is done first: the daemon reads end of file, and still answers.
    let mut child = bridge(&served.socket);
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(b"the whole request\n").unwrap();
    drop(stdin);
    let mut stdout = child.stdout.take().unwrap();
    read_ready(&mut stdout);
    let mut back = Vec::new();
    stdout.read_to_end(&mut back).unwrap();
    assert_eq!(back, b"the whole request\n");
    assert!(child.wait().unwrap().success());

    // The daemon is done first: the client reads end of file while the bridge still runs, and
    // what it sends then still arrives.
    let mut child = bridge(&served.socket);
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = child.stdout.take().unwrap();
    stdin.write_all(CLOSE_WRITE).unwrap();
    read_ready(&mut stdout);
    let mut back = Vec::new();
    stdout.read_to_end(&mut back).unwrap();
    assert_eq!(back, b"closing\n");
    assert!(
        child.try_wait().unwrap().is_none(),
        "the bridge ended early"
    );
    stdin.write_all(b"after the daemon's end").unwrap();
    drop(stdin);
    assert!(child.wait().unwrap().success());
    let got = PathBuf::from(format!("{}.got", served.socket.display()));
    crate::unix::eventually("the daemon to get the rest", || {
        std::fs::read(&got).is_ok_and(|g| g == b"after the daemon's end")
    });
}

/// A socket that is not the user's alone is refused before a byte passes, with no path in the
/// message; so is a missing one.
fn bridge_refuses_sockets_that_are_not_ours() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = tempfile::tempdir().unwrap();
    let served = Served::new(dir.path());
    let run = served.socket.parent().unwrap().to_path_buf();
    let refused = |socket: &Path, code: u8| {
        let child = bridge(socket);
        let out = child.wait_with_output().unwrap();
        assert_eq!(
            out.status.code(),
            Some(i32::from(code)),
            "{}",
            socket.display()
        );
        assert!(out.stdout.is_empty(), "something passed");
        let said = String::from_utf8_lossy(&out.stderr).into_owned();
        assert!(said.starts_with("pitcrewd connect: "), "{said}");
        assert!(!said.contains(dir.path().to_str().unwrap()), "{said}");
        said
    };
    use pitcrew_remote::bridge::{EXIT_NO_DAEMON, EXIT_UNSAFE, EXIT_USAGE};
    // Its directory open to the group.
    std::fs::set_permissions(&run, std::fs::Permissions::from_mode(0o750)).unwrap();
    let said = refused(&served.socket, EXIT_UNSAFE);
    assert!(said.contains("open to others"), "{said}");
    std::fs::set_permissions(&run, std::fs::Permissions::from_mode(0o700)).unwrap();
    // A link to it, a file that is not a socket, a link to its directory.
    let link = run.join("link.sock");
    std::os::unix::fs::symlink(&served.socket, &link).unwrap();
    refused(&link, EXIT_UNSAFE);
    let file = run.join("file.sock");
    std::fs::write(&file, "").unwrap();
    refused(&file, EXIT_UNSAFE);
    let dir_link = dir.path().join("run-link");
    std::os::unix::fs::symlink(&run, &dir_link).unwrap();
    refused(&dir_link.join("pitcrewd.sock"), EXIT_UNSAFE);
    // Nothing there, or a relative path.
    refused(&run.join("missing.sock"), EXIT_NO_DAEMON);
    refused(Path::new("run/pitcrewd.sock"), EXIT_USAGE);
    // And the real one passes.
    let mut child = bridge(&served.socket);
    drop(child.stdin.take());
    let mut stdout = child.stdout.take().unwrap();
    read_ready(&mut stdout);
    assert!(child.wait().unwrap().success());
    assert!(alive(served.daemon.id()));
}

pub(crate) const CASES: &[(&str, fn())] = &[
    (
        "tunnel_forwarded_socket_with_many_connections",
        tunnel_forwarded_socket_with_many_connections,
    ),
    (
        "tunnel_forwarding_refused_then_the_stdio_bridge",
        tunnel_forwarding_refused_then_the_stdio_bridge,
    ),
    (
        "tunnel_a_refused_forward_is_not_tried_again",
        tunnel_a_refused_forward_is_not_tried_again,
    ),
    (
        "tunnel_bridge_through_srun_to_a_node_local_socket",
        tunnel_bridge_through_srun_to_a_node_local_socket,
    ),
    ("tunnel_proxyjump_to_a_node", tunnel_proxyjump_to_a_node),
    (
        "tunnel_a_node_that_fails_the_check_is_refused",
        tunnel_a_node_that_fails_the_check_is_refused,
    ),
    (
        "tunnel_a_dropped_stream_is_unverifiable_within_ten_seconds_then_recovers",
        tunnel_a_dropped_stream_is_unverifiable_within_ten_seconds_then_recovers,
    ),
    ("tunnel_a_wall_clock_jump", tunnel_a_wall_clock_jump),
    (
        "tunnel_a_job_that_ended_then_moved",
        tunnel_a_job_that_ended_then_moved,
    ),
    (
        "tunnel_askpass_during_a_reconnect",
        tunnel_askpass_during_a_reconnect,
    ),
    ("tunnel_under_every_posix_sh", tunnel_under_every_posix_sh),
    (
        "tunnel_a_removed_forward_is_made_again",
        tunnel_a_removed_forward_is_made_again,
    ),
    (
        "tunnel_a_session_over_max_sessions_is_refused_alone",
        tunnel_a_session_over_max_sessions_is_refused_alone,
    ),
    (
        "tunnel_a_forward_that_failed_once_is_not_remembered",
        tunnel_a_forward_that_failed_once_is_not_remembered,
    ),
    (
        "tunnel_srun_line_buffering_and_job_steps",
        tunnel_srun_line_buffering_and_job_steps,
    ),
    (
        "tunnel_links_left_by_a_crash_are_stopped",
        tunnel_links_left_by_a_crash_are_stopped,
    ),
    (
        "tunnel_a_burst_of_failures_makes_one_check",
        tunnel_a_burst_of_failures_makes_one_check,
    ),
    (
        "tunnel_attempts_from_unreachable_show",
        tunnel_attempts_from_unreachable_show,
    ),
    (
        "tunnel_without_connection_reuse",
        tunnel_without_connection_reuse,
    ),
    (
        "tunnel_a_link_that_keeps_dropping_is_given_up",
        tunnel_a_link_that_keeps_dropping_is_given_up,
    ),
    (
        "tunnel_a_short_silence_keeps_a_patient_link",
        tunnel_a_short_silence_keeps_a_patient_link,
    ),
    (
        "tunnel_an_srun_job_that_ends_is_noticed",
        tunnel_an_srun_job_that_ends_is_noticed,
    ),
    (
        "tunnel_a_node_local_socket_is_never_forwarded",
        tunnel_a_node_local_socket_is_never_forwarded,
    ),
    (
        "tunnel_a_silent_forward_leaves_no_patient_link",
        tunnel_a_silent_forward_leaves_no_patient_link,
    ),
    (
        "tunnel_a_failing_endpoint_check_keeps_the_login",
        tunnel_a_failing_endpoint_check_keeps_the_login,
    ),
    (
        "tunnel_polling_that_asks_for_a_password_is_rare",
        tunnel_polling_that_asks_for_a_password_is_rare,
    ),
    (
        "bridge_is_byte_exact_both_ways",
        bridge_is_byte_exact_both_ways,
    ),
    ("bridge_half_closes_both_ways", bridge_half_closes_both_ways),
    (
        "bridge_refuses_sockets_that_are_not_ours",
        bridge_refuses_sockets_that_are_not_ours,
    ),
];
