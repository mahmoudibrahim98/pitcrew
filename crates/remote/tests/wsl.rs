//! Portable discovery tests. The copied test executable plays wsl.exe from recorded bytes.

use pitcrew_remote::{SshError, Wsl, WslDistro};
use std::error::Error;
use std::io::Write as _;

fn main() -> Result<(), Box<dyn Error>> {
    let executable = std::env::current_exe()?;
    let dir = executable.parent().ok_or("no executable directory")?;
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
    std::fs::hard_link(&executable, &fake)?;
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
    Ok(())
}
