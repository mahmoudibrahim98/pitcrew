//! Against a real `sshd`, only when `PITCREW_TEST_SSH_HOST` names a host that this machine's
//! `ssh` reaches without prompting (keys, known host). Skipped otherwise.
//!
//! ```sh
//! PITCREW_TEST_SSH_HOST=localhost cargo test -p pitcrew-remote --test real_sshd
//! ```

use pitcrew_remote::Ssh;

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
