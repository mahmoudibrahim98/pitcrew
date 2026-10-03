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
    let temporary = tempfile::tempdir()?;
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
    let local = tempfile::tempdir()?;
    let transport = Ssh::wsl("wsl.exe").with_runtime_dir(local.path().join("rt"));
    let limits = Limits {
        max_output: Some(1024 * 1024),
        timeout: Some(std::time::Duration::from_secs(30)),
    };
    let startup = Limits {
        timeout: Some(std::time::Duration::from_secs(120)),
        ..limits
    };
    let created = transport.run_limited(&distro, &["sh", "-c", "umask 077; HOME=$(mktemp -d /tmp/pitcrew-wsl-test.XXXXXX) || exit; export HOME; printf '%s\\n' \"$HOME\""], startup).await?;
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
        Connector, ConnectorOptions, Daemon, DeployOptions, DirectLauncher, Helper, Launcher, Ssh,
        Target, Transport,
    };
    use sha2::{Digest as _, Sha256};
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    let temp = tempfile::tempdir()?;
    std::fs::write(temp.path().join("fake-wsl"), [])?;
    let program = temp
        .path()
        .join(format!("wsl{}", std::env::consts::EXE_SUFFIX));
    std::fs::copy(std::env::current_exe()?, &program)?;
    let transport = Ssh::wsl(&program).with_runtime_dir(temp.path().join("rt"));
    let probe = transport.probe(fake::DISTRO).await?;
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
    println!("test fake_wsl_probe_deploy_atomic_verify_launch_stdio ... ok");
    Ok(())
}
