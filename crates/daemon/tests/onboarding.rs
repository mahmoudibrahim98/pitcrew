//! Device-only onboarding uses temporary homes and a Codex stand-in which is never executed.
mod common;
use common::{Daemon, Tmux, id, request};
use serde_json::json;
use std::fs;

#[test]
fn preview_confirmation_staleness_backups_and_restart() -> Result<(), Box<dyn std::error::Error>> {
    let tmp = tempfile::tempdir()?;
    let home = tmp.path().join("home");
    let bin = tmp.path().join("bin");
    fs::create_dir_all(home.join(".codex"))?;
    fs::create_dir(&bin)?;
    let suffix = if cfg!(windows) { ".exe" } else { "" };
    for name in ["pitcrew", "codex"] {
        fs::write(
            bin.join(format!("{name}{suffix}")),
            "synthetic stand-in, never executed",
        )?;
    }
    let env = [
        ("PATH", bin.into_os_string()),
        ("HOME", home.clone().into_os_string()),
        ("USERPROFILE", home.clone().into_os_string()),
        ("CODEX_HOME", home.join(".codex").into_os_string()),
    ];
    let state = tmp.path().join("state");
    let extra = ["--demo", "--no-runner", "--no-office"];
    let mut daemon = Daemon::start_with(&state, &extra, &env, Tmux::Refused);
    let token = daemon.device_token();
    let agent = daemon.agent_token();
    let diff = format!("/v1/machines/{}/hooks/diff", id::LAPTOP);
    let install = format!("/v1/machines/{}/hooks/install", id::LAPTOP);
    for route in [&diff, &install] {
        assert_eq!(daemon.post(route, Some(&agent), &json!({})).status, 403);
    }
    assert_eq!(
        daemon
            .post(
                &diff.replace(id::LAPTOP, id::CLUSTER),
                Some(&token),
                &json!({})
            )
            .status,
        501
    );
    let path = home.join(".codex/config.toml");
    let original = "# synthetic\r\nmodel = 'example-model'\r\n";
    fs::write(&path, original)?;
    let preview = daemon.post(&diff, Some(&token), &json!({}));
    assert_eq!(preview.status, 200);
    let preview = preview.json();
    assert_eq!(preview["files"][0]["before"], original);
    assert_eq!(fs::read_to_string(&path)?, original);
    let confirmation = json!({"revision": preview["revision"]});
    fs::write(&path, "# concurrent edit\n")?;
    assert_eq!(
        daemon.post(&install, Some(&token), &confirmation).status,
        409
    );
    assert_eq!(fs::read_to_string(&path)?, "# concurrent edit\n");
    assert_eq!(fs::read_dir(home.join(".codex"))?.count(), 1);
    fs::write(&path, original)?;
    for _ in 0..2 {
        assert_eq!(
            daemon.post(&install, Some(&token), &confirmation).status,
            200
        );
    }
    assert_eq!(
        fs::read_to_string(&path)?,
        preview["files"][0]["after"].as_str().ok_or("after")?
    );
    let backups: Vec<_> = fs::read_dir(home.join(".codex"))?.collect::<Result<_, _>>()?;
    assert_eq!(backups.len(), 2);
    let backup = backups
        .iter()
        .find(|file| file.path() != path)
        .ok_or("backup")?;
    assert_eq!(fs::read_to_string(backup.path())?, original);
    let settings = json!({"permission_mode":"plan", "back_office_enabled":true, "back_office_caps":{"max_auto_accept_per_hour":1}});
    assert_eq!(
        request(
            daemon.port,
            "PUT",
            "/v1/safety",
            Some(&token),
            Some(&settings),
            &[]
        )
        .status,
        200
    );
    daemon.stop();
    let mut daemon =
        Daemon::start_with(&state, &["--no-runner", "--no-office"], &env, Tmux::Refused);
    assert_eq!(daemon.get("/v1/safety", Some(&token)).json(), settings);
    assert_eq!(
        daemon.post(&install, Some(&token), &confirmation).status,
        409
    );
    assert!(
        daemon.post(&diff, Some(&token), &json!({})).json()["files"]
            .as_array()
            .ok_or("files")?
            .is_empty()
    );
    daemon.stop();
    Ok(())
}
