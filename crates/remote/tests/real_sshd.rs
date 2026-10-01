//! Against a real `sshd`, only when `PITCREW_TEST_SSH_HOST` names a host that this machine's
//! `ssh` reaches without prompting (keys, known host). Skipped otherwise.
//!
//! ```sh
//! PITCREW_TEST_SSH_HOST=localhost cargo test -p pitcrew-remote --test real_sshd
//! ```

use pitcrew_remote::{
    DeployOptions, DirectLauncher, Helper, HelperState, Launcher, Layout, Platform, Ssh, Target,
    deploy,
};
use sha2::{Digest as _, Sha256};

fn host() -> Option<String> {
    std::env::var("PITCREW_TEST_SSH_HOST")
        .ok()
        .filter(|h| !h.is_empty())
}

#[tokio::test]
async fn runs_quotes_and_probes_a_real_host() {
    let Some(host) = host() else {
        eprintln!("skipped: PITCREW_TEST_SSH_HOST is not set");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let ssh = Ssh::default().with_runtime_dir(dir.path().join("rt"));

    let hostile = ["it's", "$(echo no)", "`id`", "a\nb", "-rf", "日本"];
    let mut argv = vec!["printf", "%s\\0"];
    argv.extend(hostile);
    let out = ssh.run(&host, &argv).await.unwrap();
    assert!(out.success(), "{out:?}");
    let words: Vec<&str> = std::str::from_utf8(&out.stdout)
        .unwrap()
        .split_terminator('\0')
        .collect();
    assert_eq!(words, hostile);

    // A second call reuses the master connection on Unix.
    let out = ssh.run(&host, &["sh", "-c", "exit 7"]).await.unwrap();
    assert_eq!(out.code, Some(7));

    let probe = ssh.probe(&host).await.unwrap();
    assert!(!probe.info.os.is_empty() && probe.info.os != "unknown");
    assert!(probe.home.is_some());

    let resolved = ssh.resolve(&host).await.unwrap();
    assert!(!resolved.hostname.is_empty());
}

/// Deploys a stand-in helper (a script answering `--version`) into a throwaway directory in the
/// host's home, so the user's own `~/.pitcrew` is never touched, then removes it.
#[tokio::test]
async fn deploys_to_a_real_host() {
    let Some(host) = host() else {
        eprintln!("skipped: PITCREW_TEST_SSH_HOST is not set");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let ssh = Ssh::default().with_runtime_dir(dir.path().join("rt"));
    let probe = ssh.probe(&host).await.unwrap();
    let platform = Platform::detect(&probe.info).unwrap();
    let home = probe.home.clone().unwrap();
    let tag = format!(
        "{:x}{:x}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .subsec_nanos()
    );
    let root = format!("{}/.pitcrew-test-{tag}", home.trim_end_matches('/'));
    let target =
        Target::with_layout(ssh.clone(), &host, Layout::at(&root).unwrap(), platform).unwrap();

    let bytes = b"#!/bin/sh\ncase \"$1\" in\n--version) echo 'pitcrewd 0.0.0-test (protocol 1)' ;;\n*) exit 2 ;;\nesac\n".to_vec();
    let hash: String = Sha256::digest(&bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let helper = Helper::new(platform, "0.0.0-test", &hash, bytes).unwrap();
    let options = DeployOptions::default();
    let first = deploy(&target, &helper, &options).await;
    let again = deploy(&target, &helper, &options).await;
    let status = DirectLauncher::default().status(&target).await;
    // Clean up before asserting.
    let removed = ssh.run(&host, &["rm", "-rf", &root]).await.unwrap();
    assert!(removed.success(), "{removed:?}");

    let first = first.unwrap();
    assert!(first.uploaded);
    assert_eq!(first.sha256, hash);
    let again = again.unwrap();
    assert!(!again.uploaded);
    let status = status.unwrap();
    assert_eq!(status.state, HelperState::NotRunning);
    assert_eq!(status.installed.as_deref(), Some("0.0.0-test"));
}

#[cfg(unix)]
mod tunnel {
    use super::host;
    use pitcrew_remote::helper::{HelperFuture, Started, Status, Stopped};
    use pitcrew_remote::{
        Connector, ConnectorOptions, Daemon, Endpoint, HelperError, HelperState, Launcher, Layout,
        LinkState, Platform, Ssh, Target, Transport,
    };
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    /// A launcher that says the helper runs at `endpoint`.
    #[derive(Debug)]
    struct Fixed(Endpoint);

    impl Launcher for Fixed {
        fn name(&self) -> &'static str {
            "direct"
        }

        fn start<'a>(&'a self, _: &'a Target) -> HelperFuture<'a, Started> {
            Box::pin(async { Err(HelperError::InvalidArgument("not here".to_owned())) })
        }

        fn status<'a>(&'a self, _: &'a Target) -> HelperFuture<'a, Status> {
            let endpoint = self.0.clone();
            Box::pin(async move {
                Ok(Status {
                    state: HelperState::Running,
                    endpoint: Some(endpoint),
                    installed: None,
                    socket_ready: true,
                    tmux_session: None,
                    slurm: None,
                })
            })
        }

        fn stop<'a>(&'a self, _: &'a Target) -> HelperFuture<'a, Stopped> {
            Box::pin(async {
                Ok(Stopped {
                    pid: None,
                    forced: false,
                })
            })
        }
    }

    /// The tunnel through a real OpenSSH: its own ControlMaster, a forward added to it, many
    /// connections, and everything gone on close. Only on a host that shares this machine's files
    /// (`localhost`-style), where a stand-in daemon served by this test can be reached; skipped on
    /// any other.
    #[tokio::test(flavor = "multi_thread")]
    async fn tunnels_to_a_real_host() {
        let Some(host) = host() else {
            eprintln!("skipped: PITCREW_TEST_SSH_HOST is not set");
            return;
        };
        let dir = tempfile::Builder::new().prefix("pc").tempdir().unwrap();
        let run = dir.path().join("run");
        std::fs::create_dir(&run).unwrap();
        let socket = run.join("pitcrewd.sock");
        let ssh = Ssh::default().with_runtime_dir(dir.path().join("rt"));
        let here = ssh
            .run(&host, &["test", "-d", run.to_str().unwrap()])
            .await
            .unwrap();
        if !here.success() {
            eprintln!("skipped: {host} does not share this machine's files");
            return;
        }
        // The stand-in daemon: HTTP for the probe, an echo for the rest.
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let served = tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let mut head = [0u8; 4];
                    if stream.read_exact(&mut head).await.is_err() {
                        return;
                    }
                    if &head == b"GET " {
                        let _ = stream
                            .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\n\r\n")
                            .await;
                        return;
                    }
                    let _ = stream.write_all(&head).await;
                    let (mut from, mut to) = stream.split();
                    let _ = tokio::io::copy(&mut from, &mut to).await;
                    let _ = to.shutdown().await;
                });
            }
        });
        let endpoint = Endpoint {
            pid: std::process::id(),
            host: "here".to_owned(),
            version: "1.0.0".to_owned(),
            started: 0,
            launcher: "direct".to_owned(),
            socket: socket.to_str().unwrap().to_owned(),
            job: None,
        };
        let target = Target::with_layout(
            ssh,
            &host,
            Layout::at(dir.path().to_str().unwrap()).unwrap(),
            Platform::LinuxX86_64,
        )
        .unwrap();
        let connector = Connector::start(
            Daemon::new(target, Arc::new(Fixed(endpoint))),
            ConnectorOptions::default(),
        )
        .unwrap();
        let mut state = connector.watch();
        let connected = tokio::time::timeout(
            Duration::from_secs(60),
            state.wait_for(|s| s.is_connected() || matches!(s, LinkState::Unreachable { .. })),
        )
        .await
        .unwrap()
        .unwrap()
        .clone();
        assert_eq!(
            connected,
            LinkState::Connected {
                transport: Transport::Forwarded
            }
        );
        for i in 0..4u8 {
            let mut stream = connector.connect().await.unwrap();
            let data: Vec<u8> = (0..100_000u32).map(|n| (n as u8) ^ i ^ 0x55).collect();
            stream.write_all(&data).await.unwrap();
            stream.shutdown().await.unwrap();
            let mut back = Vec::new();
            stream.read_to_end(&mut back).await.unwrap();
            assert!(back == data, "the echo differs");
        }
        connector.close().await;
        assert_eq!(connector.state(), LinkState::Closed);
        served.abort();
        let left: Vec<_> = std::fs::read_dir(dir.path().join("rt"))
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().starts_with('t'))
            .collect();
        assert!(left.is_empty(), "the private directory is left");
    }
}
