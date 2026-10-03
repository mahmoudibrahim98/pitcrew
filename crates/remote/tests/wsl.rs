//! Portable discovery tests. The copied test executable plays wsl.exe from recorded bytes.

use pitcrew_remote::{SshError, Wsl, WslDistro};
use std::error::Error;
use std::io::Write as _;

#[path = "support/wsl_fake.rs"]
mod fake;

fn main() -> Result<(), Box<dyn Error>> {
    let executable = std::env::current_exe()?;
    let dir = executable.parent().ok_or("no executable directory")?;
    if dir.join("fake-wsl").exists() {
        return fake::run(dir, &std::env::args().skip(1).collect::<Vec<_>>());
    }
    if dir.join("listing.bin").is_file() {
        let args: Vec<String> = std::env::args().skip(1).collect();
        let mut log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("args.json"))?;
        writeln!(log, "{}", serde_json::to_string(&args)?)?;
        if dir.join("unavailable").is_file() {
            std::process::exit(1);
        }
        let output = if args == ["--list", "--running", "--quiet"] {
            "running.bin"
        } else {
            "listing.bin"
        };
        std::io::stdout().write_all(&std::fs::read(dir.join(output))?)?;
        return Ok(());
    }
    let runtime = tokio::runtime::Runtime::new()?;
    let temporary = pitcrew_fixtures::temp::short_tempdir()?;
    let fake = temporary
        .path()
        .join(format!("wsl{}", std::env::consts::EXE_SUFFIX));
    std::fs::copy(&executable, &fake)?;
    let listing = "\u{feff}  NOM                        ETAT                    VERSION\r\n* Lab 'quoted' distro         En cours d’exécution    2\r\n  Stopped distro             Arrêté                  2\r\n  Legacy distro              Arrêté                  1\r\n";
    let bytes: Vec<u8> = listing.encode_utf16().flat_map(u16::to_le_bytes).collect();
    std::fs::write(temporary.path().join("listing.bin"), bytes)?;
    let running: Vec<u8> = "Lab 'quoted' distro\r\n"
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect();
    std::fs::write(temporary.path().join("running.bin"), running)?;
    let answer = runtime.block_on(Wsl::new(&fake).distros())?;
    assert!(answer.available);
    assert_eq!(
        answer.distros[0],
        WslDistro {
            name: "Lab 'quoted' distro".into(),
            default: true,
            running: true,
            version: 2
        }
    );
    assert_eq!(answer.distros[1].name, "Stopped distro");
    assert!(!answer.distros[1].running);
    assert_eq!(answer.distros[2].version, 1);
    let log = std::fs::read_to_string(temporary.path().join("args.json"))?;
    let args: Vec<Vec<String>> = log
        .lines()
        .map(serde_json::from_str)
        .collect::<Result<_, _>>()?;
    assert_eq!(
        args,
        vec![
            vec!["--list", "--verbose"],
            vec!["--list", "--running", "--quiet"]
        ]
    );
    println!("test fake_wsl_utf16_default_spaces_quotes_stopped_and_wsl1 ... ok");

    std::fs::write(temporary.path().join("unavailable"), [])?;
    let answer = runtime.block_on(Wsl::new(&fake).distros())?;
    assert!(!answer.available);
    assert!(answer.distros.is_empty());
    println!("test fake_wsl_not_installed ... ok");

    let answer = runtime.block_on(Wsl::new(temporary.path().join("missing.exe")).distros())?;
    assert!(!answer.available);
    assert!(answer.distros.is_empty());
    println!("test missing_wsl_executable ... ok");

    std::fs::remove_file(temporary.path().join("unavailable"))?;
    std::fs::write(temporary.path().join("listing.bin"), [0])?;
    assert!(matches!(
        runtime.block_on(Wsl::new(&fake).distros()),
        Err(SshError::InvalidArgument(_))
    ));
    println!("test truncated_utf16_is_refused ... ok");

    std::fs::write(
        temporary.path().join("listing.bin"),
        vec![0; 1024 * 1024 + 2],
    )?;
    assert!(matches!(
        runtime.block_on(Wsl::new(&fake).distros()),
        Err(SshError::OutputTooLarge { limit: 1_048_576 })
    ));
    println!("test listing_output_is_bounded ... ok");
    println!("test result: ok. 5 passed; 0 failed");
    runtime.block_on(transport_flow())?;
    runtime.block_on(real_wsl())?;
    Ok(())
}

async fn real_wsl() -> Result<(), Box<dyn Error>> {
    use pitcrew_remote::{DeployOptions, Helper, Limits, Ssh, Target};
    use sha2::{Digest as _, Sha256};
    let Ok(distro) = std::env::var("PITCREW_TEST_WSL_DISTRO") else {
        println!("test real_wsl_isolated_home ... not requested (PITCREW_TEST_WSL_DISTRO unset)");
        return Ok(());
    };
    let local = pitcrew_fixtures::temp::short_tempdir()?;
    let program = pitcrew_remote::wsl::default_program();
    let transport = Ssh::wsl(&program).with_runtime_dir(local.path().join("rt"));
    let limits = Limits {
        max_output: Some(1024 * 1024),
        timeout: Some(std::time::Duration::from_secs(30)),
    };
    // The distro may be stopped: start it within its own, longer limit first.
    transport.start_wsl(&distro).await?;
    let created = transport.run_limited(&distro, &["sh", "-c", "umask 077; HOME=$(mktemp -d /tmp/pitcrew-wsl-test.XXXXXX) || exit; export HOME; printf '%s\\n' \"$HOME\""], limits).await?;
    if !created.success() {
        return Err("could not create isolated WSL HOME".into());
    }
    let home = created.stdout_text().trim().to_owned();
    if !home.starts_with("/tmp/pitcrew-wsl-test.")
        || home.contains(char::is_whitespace)
        || home.contains("..")
    {
        return Err("unexpected temporary HOME".into());
    }
    let transport = transport.with_wsl_home(&home)?;
    let result: Result<(), Box<dyn Error>> = async {
        let probe = transport.probe(&distro).await?;
        assert_eq!(probe.home.as_deref(), Some(home.as_str()));
        assert_eq!(probe.info.os, "linux");
        let quote = "space ' quote \" dollar $ semicolon ;";
        let output = transport
            .run_limited(&distro, &["printf", "%s", quote], limits)
            .await?;
        assert!(output.success());
        assert_eq!(output.stdout_text(), quote);
        // `--cd ~`: calls start in the distro user's home (from the passwd database; the HOME
        // given to commands is the temporary one), not in a translated Windows folder.
        let cwd = transport
            .run_limited(
                &distro,
                &[
                    "sh",
                    "-c",
                    "h=$(getent passwd \"$(id -u)\" | cut -d: -f6) && [ \"$(pwd -P)\" = \"$(cd \"$h\" && pwd -P)\" ]",
                ],
                limits,
            )
            .await?;
        assert!(cwd.success(), "not started in the home: {cwd:?}");
        let target = Target::new(transport.clone(), &distro, &probe)?;
        let bytes = b"#!/bin/sh\nprintf 'pitcrewd 0.0.0\\n'\n";
        let helper = Helper::new(
            target.platform(),
            "0.0.0",
            &Sha256::digest(bytes)
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>(),
            bytes.to_vec(),
        )?;
        let deployed = pitcrew_remote::deploy(&target, &helper, &DeployOptions::default()).await?;
        assert!(deployed.uploaded && deployed.atomic);
        assert!(
            !pitcrew_remote::deploy(&target, &helper, &DeployOptions::default())
                .await?
                .uploaded
        );
        let mode = transport
            .run_limited(
                &distro,
                &["sh", "-c", "stat -c '%a' \"$HOME/.pitcrew\""],
                limits,
            )
            .await?;
        assert_eq!(mode.stdout_text().trim(), "700");
        Ok(())
    }
    .await;
    // Only the freshly created directory is removed, even when a check failed.
    let cleaned = transport
        .run_limited(&distro, &["rm", "-rf", "--", &home], limits)
        .await?;
    if !cleaned.success() {
        return Err("could not remove isolated WSL HOME".into());
    }
    result?;
    println!(
        "test real_wsl_isolated_home_probe_quoting_deploy_verify_atomic_private_cleanup ... ok"
    );
    Ok(())
}

async fn transport_flow() -> Result<(), Box<dyn Error>> {
    use pitcrew_remote::{
        Connector, ConnectorOptions, Daemon, DeployOptions, DirectLauncher, Helper, Launcher,
        Limits, Ssh, Target, Transport,
    };
    use sha2::{Digest as _, Sha256};
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    let temp = pitcrew_fixtures::temp::short_tempdir()?;
    std::fs::write(temp.path().join("fake-wsl"), [])?;
    let program = temp
        .path()
        .join(format!("wsl{}", std::env::consts::EXE_SUFFIX));
    std::fs::copy(std::env::current_exe()?, &program)?;
    let transport = Ssh::wsl(&program).with_runtime_dir(temp.path().join("rt"));

    // The distro is stopped: a call shorter than its start fails, the start's own limit is
    // enough, and afterwards it answers at once.
    let quick = Limits {
        max_output: Some(1024 * 1024),
        timeout: Some(fake::BOOT / 3),
    };
    assert!(matches!(
        transport.probe_with(fake::DISTRO, quick).await,
        Err(SshError::TimedOut(_))
    ));
    assert!(!temp.path().join("booted").exists());
    transport.start_wsl(fake::DISTRO).await?;
    assert!(temp.path().join("booted").exists());
    let probe = transport.probe(fake::DISTRO).await?;
    assert_eq!(
        fake::commands(temp.path())?.get(1),
        Some(&vec!["true".to_owned()])
    );
    assert!(matches!(
        Ssh::new(&program).start_wsl(fake::DISTRO).await,
        Err(SshError::InvalidArgument(_))
    ));
    println!("test fake_wsl_stopped_distro_gets_a_longer_first_call ... ok");

    // Each name reaches wsl.exe as one argument, unchanged (the fake knows no other).
    for name in fake::HOSTILE {
        assert_eq!(transport.probe(name).await?.info.hostname, "lab");
        let log = std::fs::read_to_string(temp.path().join("calls.jsonl"))?;
        let last: Vec<String> = serde_json::from_str(log.lines().last().ok_or("no call")?)?;
        assert_eq!(last.get(1).map(String::as_str), Some(name));
    }
    println!("test fake_wsl_hostile_distro_names_stay_one_argument ... ok");

    let target = Target::new(transport, fake::DISTRO, &probe)?;
    let bytes = b"synthetic helper bytes";
    let helper = Helper::new(
        target.platform(),
        "0.0.0",
        &Sha256::digest(bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>(),
        bytes.to_vec(),
    )?;
    let deployed = pitcrew_remote::deploy(&target, &helper, &DeployOptions::default()).await?;
    assert!(deployed.uploaded && deployed.atomic);
    assert!(
        !pitcrew_remote::deploy(&target, &helper, &DeployOptions::default())
            .await?
            .uploaded
    );
    let launcher = Arc::new(DirectLauncher::default());
    launcher.start(&target).await?;
    let connector = Connector::start(
        Daemon::new(target.clone(), launcher.clone()),
        ConnectorOptions::default(),
    )?;
    let mut states = connector.watch();
    // The heartbeat's mark follows wsl.exe's UTF-16LE notice on stderr and stdout: missing it
    // would leave the link waiting for its full START_WAIT (two minutes).
    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        states.wait_for(|s| s.is_connected()),
    )
    .await??;
    assert_eq!(connector.transport(), Some(Transport::Stdio));
    let mut stream = connector.connect().await?;
    stream
        .write_all(b"GET /v1/workspace HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .await?;
    let mut response = vec![0; 1024];
    let n = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        stream.read(&mut response),
    )
    .await??;
    assert!(String::from_utf8_lossy(&response[..n]).contains("200 OK"));
    drop(stream);
    connector.close().await;
    launcher.stop(&target).await?;
    println!("test fake_wsl_probe_deploy_atomic_verify_launch_stdio_past_utf16_notices ... ok");
    Ok(())
}
