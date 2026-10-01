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
