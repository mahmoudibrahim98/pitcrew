//! Portable gateway plan/add/reconnect tests with a recorded WSL executable.
#[path = "../../../../crates/remote/tests/support/wsl_fake.rs"]
mod fake;

use pitcrew_desktop::keychain::{MemoryStore, TokenStore};
use pitcrew_desktop::registry::{Connection, Registry, WorkspaceState, WslTarget};
use pitcrew_desktop::remote::{Helpers, PromptHub, RemoteOptions, RemotePlanRequest, Remotes};
use sha2::{Digest as _, Sha256};
use std::error::Error;
use std::sync::Arc;
use std::time::Duration;

fn main() -> Result<(), Box<dyn Error>> {
    let exe = std::env::current_exe()?;
    let parent = exe.parent().ok_or("no executable parent")?;
    if parent.join("fake-wsl").exists() {
        return fake::run(parent, &std::env::args().skip(1).collect::<Vec<_>>());
    }
    let runtime = tokio::runtime::Runtime::new()?;
    runtime.block_on(check())
}

async fn check() -> Result<(), Box<dyn Error>> {
    let temp = pitcrew_fixtures::temp::short_tempdir()?;
    let dir = temp.path();
    std::fs::write(dir.join("fake-wsl"), [])?;
    let program = dir.join(format!("wsl{}", std::env::consts::EXE_SUFFIX));
    std::fs::copy(std::env::current_exe()?, &program)?;
    let helpers = dir.join("helpers");
    std::fs::create_dir(&helpers)?;
    let payload = b"synthetic helper bytes";
    std::fs::write(helpers.join("pitcrewd-x86_64-unknown-linux-musl"), payload)?;
    std::fs::write(
        helpers.join("manifest.json"),
        serde_json::to_vec(&serde_json::json!({
            "version":"0.0.0", "sha256":{"pitcrewd-x86_64-unknown-linux-musl":Sha256::digest(payload).iter().map(|b| format!("{b:02x}")).collect::<String>()}
        }))?,
    )?;
    let mut options = RemoteOptions::new(
        Err("SSH must not run".into()),
        Err("askpass must not run".into()),
        Helpers::in_dir(helpers),
    );
    options.wsl = program;
    options.runtime_dir = Some(dir.join("rt"));
    options.connect_wait = Duration::from_secs(10);
    options.connector.retry_every = Duration::from_secs(1);
    let registry_path = dir.join("registry.json");
    let registry = Arc::new(Registry::load(registry_path.clone()));
    let tokens: Arc<dyn TokenStore> = Arc::new(MemoryStore::default());
    let prompts = Arc::new(PromptHub::new(|_| panic!("WSL must never prompt")));
    let remotes = Remotes::new(
        options.clone(),
        registry.clone(),
        tokens.clone(),
        prompts.clone(),
        tokio::runtime::Handle::current(),
    );
    let target = WslTarget::Wsl {
        distro: fake::DISTRO.into(),
    };
    let list = remotes.wsl_distros().await?;
    assert!(list.available);
    assert!(!list.distros[0].running);
    assert_eq!(list.distros[0].name, fake::DISTRO);
    assert!(
        remotes
            .probe_target("ssh-host", Some(&target))
            .await
            .is_err()
    );
    assert!(
        remotes
            .probe_target(
                "",
                Some(&WslTarget::Wsl {
                    distro: "Legacy distro".into()
                })
            )
            .await
            .is_err()
    );
    let probe = remotes.probe_target("", Some(&target)).await?;
    assert_eq!(probe.os, "linux");
    assert!(probe.slurm.is_none());
    for launcher in ["direct", "tmux"] {
        let request: RemotePlanRequest = serde_json::from_value(
            serde_json::json!({"host":"", "target":{"kind":"wsl","distro":fake::DISTRO},"launcher":launcher}),
        )?;
        let plan = remotes.plan(request).await?;
        assert!(plan.steps.iter().any(|s| s.contains("WSL stdio")));
        assert!(!dir.join("installed").exists() || launcher == "tmux");
        let workspace = remotes.add(&plan.plan, Arc::new(|_| {})).await?;
        assert_eq!(workspace.id, fake::WORKSPACE);
        assert_eq!(
            workspace.host.as_deref(),
            Some(&format!("wsl:{}", fake::DISTRO)[..])
        );
        assert!(!serde_json::to_string(&workspace)?.contains(fake::TOKEN));
        assert!(remotes.add(&plan.plan, Arc::new(|_| {})).await.is_err());
        let record = registry.record(&workspace.id).ok_or("missing workspace")?;
        let Connection::Remote(remote) = record.connection else {
            return Err("not remote".into());
        };
        assert_eq!(remote.target, Some(target.clone()));
        assert_eq!(remote.transport, Some(pitcrew_remote::Transport::Stdio));
        if launcher == "direct" {
            remotes.remove(&workspace.id, true).await?;
            assert!(!dir.join("started").exists());
        }
    }
    let request: RemotePlanRequest = serde_json::from_value(
        serde_json::json!({"host":"", "target":{"kind":"wsl","distro":fake::DISTRO},"launcher":"slurm"}),
    )?;
    assert!(remotes.plan(request).await.is_err());
    std::fs::write(dir.join("offline"), [])?;
    wait_state(&registry, false).await?;
    // A reboot loses the helper as well as the transport.
    std::fs::remove_file(dir.join("started"))?;
    std::fs::remove_file(dir.join("offline"))?;
    remotes.retry(fake::WORKSPACE)?;
    wait_state(&registry, true).await?;
    assert!(dir.join("started").exists());
    remotes.shutdown().await;
    let loaded = Arc::new(Registry::load(registry_path));
    let resumed = Remotes::new(
        options,
        loaded.clone(),
        tokens,
        prompts,
        tokio::runtime::Handle::current(),
    );
    resumed.resume();
    wait_state(&loaded, true).await?;
    resumed.remove(fake::WORKSPACE, true).await?;
    assert!(loaded.list().is_empty());
    assert!(!dir.join("started").exists());
    resumed.shutdown().await;
    let calls = std::fs::read_to_string(dir.join("calls.jsonl"))?;
    for line in calls.lines() {
        let args: Vec<String> = serde_json::from_str(line)?;
        assert!(args[0] == "--list" || (args.len() == 6 && args[1] == fake::DISTRO));
        assert!(
            !args
                .iter()
                .any(|s| s == "-L" || s == "-G" || s == "--shutdown")
        );
    }
    println!("test fake_wsl_gateway_probe_plan_direct_tmux_add_reconnect_restart ... ok");
    Ok(())
}

async fn wait_state(registry: &Registry, ready: bool) -> Result<(), Box<dyn Error>> {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if registry
                .list()
                .iter()
                .any(|w| w.id == fake::WORKSPACE && (w.state == WorkspaceState::Ready) == ready)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await?;
    Ok(())
}
