//! A portable recorded WSL machine. Only this test executable and synthetic files are used.
//!
//! State lives in files next to the executable: `booted` (the distro runs; without it, the
//! first call waits [`BOOT`] for it to start), `offline` (the distro was shut down: calls fail
//! and the heartbeat ends), `installed` and `started` (the helper). Every call is recorded in
//! `calls.jsonl`, and the words of each command in `commands.jsonl`.
use std::error::Error;
use std::io::{BufRead as _, Read as _, Write as _};
use std::path::Path;
use std::time::Duration;

pub const DISTRO: &str = "Lab 'quoted' distro";
/// Names a careless Windows command line would split or change, or a shell would read: each
/// must reach wsl.exe as one argument, unchanged.
pub const HOSTILE: [&str; 3] = ["a \"b\" c\\", "trailing\\", "& ^ % !"];
pub const WORKSPACE: &str = "01JB000000000000000WSP0001";
pub const TOKEN: &str = "pcd_SYNTHETICWSLTEST000000000000000000000000000000";
/// How long the stopped distro takes to start on its first call.
pub const BOOT: Duration = Duration::from_millis(1500);
/// What wsl.exe says on its own before the distro's output (in UTF-16LE).
pub const NOTICE: &str = "wsl: A localhost proxy configuration was detected but not mirrored into WSL. WSL in NAT mode does not support localhost proxies.\r\n";

/// The `wsl.exe` arguments of a transport call, around the distro and the command.
const TRANSPORT: [&str; 5] = ["--cd", "~", "--exec", "/bin/sh", "-c"];

pub fn run(dir: &Path, args: &[String]) -> Result<(), Box<dyn Error>> {
    let mut log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("calls.jsonl"))?;
    writeln!(log, "{}", serde_json::to_string(args)?)?;
    let booted = dir.join("booted").exists();
    if args.first().is_some_and(|s| s == "--list") {
        // The listing is parsed as UTF-16LE: WSL_UTF8 would change it.
        if std::env::var_os("WSL_UTF8").is_some() {
            return Err("the distribution listing must not ask for UTF-8".into());
        }
        let state = if booted { "Running" } else { "Stopped" };
        let text = if args.iter().any(|s| s == "--running") {
            if booted {
                format!("{DISTRO}\r\n")
            } else {
                String::new()
            }
        } else {
            format!(
                "\u{feff}  NAME                         STATE           VERSION\r\n* {DISTRO}          {state}         2\r\n  Legacy distro                Stopped         1\r\n"
            )
        };
        std::io::stdout().write_all(&utf16(&text))?;
        return Ok(());
    }
    if args.len() != 8
        || args[0] != "-d"
        || !(args[1] == DISTRO || HOSTILE.contains(&args[1].as_str()))
        || args[2..7] != TRANSPORT
    {
        return Err("unexpected WSL arguments".into());
    }
    if std::env::var("WSL_UTF8").as_deref() != Ok("1") {
        return Err("a transport call without WSL_UTF8=1".into());
    }
    let words = decode(&args[7])?;
    let mut commands = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("commands.jsonl"))?;
    writeln!(commands, "{}", serde_json::to_string(&words)?)?;
    if dir.join("offline").exists() {
        let _ = std::fs::remove_file(dir.join("booted"));
        return Err("distro unavailable".into());
    }
    if !booted {
        std::thread::sleep(BOOT);
        std::fs::write(dir.join("booted"), [])?;
    }
    if words
        .get(2)
        .is_some_and(|s| s.contains("pitcrew-wsl-ready"))
    {
        let tag = words.get(4).ok_or("missing ready tag")?;
        // wsl.exe's notice comes first, in UTF-16LE: on stderr, and even on stdout, so the
        // mark follows a line that starts with a NUL.
        std::io::stderr().write_all(&utf16(NOTICE))?;
        let mut stdout = std::io::stdout().lock();
        stdout.write_all(&utf16(NOTICE))?;
        writeln!(stdout, "@@pitcrew-wsl-ready-{tag}")?;
        stdout.flush()?;
        while !dir.join("offline").exists() {
            std::thread::sleep(Duration::from_millis(25));
        }
        let _ = std::fs::remove_file(dir.join("booted"));
        return Err("distro stopped".into());
    }
    if words == ["true"] {
        return Ok(());
    }
    if words
        .get(2)
        .is_some_and(|s| s.contains("pitcrew-probe-begin"))
    {
        let tag = words.last().ok_or("missing probe tag")?;
        println!(
            "@@pitcrew-probe-begin-{tag}\nos=Linux\narch=x86_64\nhostname=lab\nhome=/home/sam\nshell=/bin/sh\nfs=ext4\ntmux_found=1\ntmux=tmux 3.3\n@@pitcrew-probe-end-{tag}"
        );
    } else if words
        .get(2)
        .is_some_and(|s| s.contains("pitcrew-helper-script-begin"))
    {
        let mut input = Vec::new();
        std::io::stdin().read_to_end(&mut input)?;
        let count: usize = words.get(4).ok_or("no script length")?.parse()?;
        if input.get(..count) != Some(include_bytes!("../../src/helper/helper.sh")) {
            return Err("helper script did not arrive intact".into());
        }
        let action = words.get(7).ok_or("no action")?;
        let tag = words.get(8).ok_or("no tag")?;
        let root = words.get(9).ok_or("no root")?;
        println!("@@pitcrew-helper-begin-{tag}");
        match action.as_str() {
            "check" if !dir.join("installed").exists() => println!("state=absent"),
            "check" | "install" => {
                let version = words.get(10).ok_or("no version")?;
                let expected = words.get(11).ok_or("no digest")?;
                if action == "install" {
                    use sha2::{Digest as _, Sha256};
                    let payload = input.get(count..).ok_or("no payload")?;
                    if Sha256::digest(payload)
                        .iter()
                        .map(|b| format!("{b:02x}"))
                        .collect::<String>()
                        != *expected
                    {
                        return Err("wrong upload digest".into());
                    }
                    std::fs::write(dir.join("installed"), version)?;
                }
                println!(
                    "state=installed\ncurrent={version}\nsha256={expected}\nversion_line=pitcrewd {version}\ntool=sha256sum\natomic=1\nuploaded={}",
                    u8::from(action == "install")
                );
            }
            "start" | "status" => {
                let launcher = words.get(10).ok_or("no launcher")?;
                if action == "start" {
                    std::fs::write(dir.join("started"), launcher)?;
                }
                if dir.join("started").exists() {
                    let version = std::fs::read_to_string(dir.join("installed"))?;
                    let endpoint = serde_json::json!({"pid":123,"host":"lab","version":version,"started":1000,"launcher":launcher,"socket":format!("{root}/run/pitcrewd.sock")});
                    println!(
                        "state=running\nendpoint={endpoint}\ninstalled={version}\nsocket=1\nstarted=1"
                    );
                } else {
                    println!("state=stopped");
                }
            }
            "stop" => {
                if dir.join("started").exists() {
                    std::fs::remove_file(dir.join("started"))?;
                }
                println!("stopped=1");
            }
            _ => return Err("unexpected helper action".into()),
        }
        println!("@@pitcrew-helper-end-{tag}");
    } else if words.get(2).is_some_and(|s| s.contains("token show-path")) {
        println!(
            "{}\n{TOKEN}\n{}",
            words.get(5).ok_or("no token begin")?,
            words.get(6).ok_or("no token end")?
        );
    } else if words.iter().any(|s| s == "connect") {
        let nonce = words.last().ok_or("no nonce")?;
        let mut stdout = std::io::stdout().lock();
        stdout.write_all(&pitcrew_remote::bridge::ready_mark(Some(nonce)))?;
        stdout.flush()?;
        let mut stdin = std::io::BufReader::new(std::io::stdin().lock());
        let mut request = String::new();
        loop {
            request.clear();
            if stdin.read_line(&mut request)? == 0 {
                break;
            }
            if request == "\r\n" {
                let body = format!(r#"{{"workspace":{{"id":"{WORKSPACE}","name":"WSL Lab"}}}}"#);
                write!(
                    stdout,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                    body.len()
                )?;
                stdout.flush()?;
            }
        }
    } else {
        return Err("unrecorded WSL command".into());
    }
    Ok(())
}

/// The words of each command the fake ran, in order (`commands.jsonl`).
pub fn commands(dir: &Path) -> Result<Vec<Vec<String>>, Box<dyn Error>> {
    let Ok(text) = std::fs::read_to_string(dir.join("commands.jsonl")) else {
        return Ok(Vec::new());
    };
    Ok(text
        .lines()
        .map(serde_json::from_str)
        .collect::<Result<_, _>>()?)
}

fn utf16(text: &str) -> Vec<u8> {
    text.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

fn decode(wrapped: &str) -> Result<Vec<String>, Box<dyn Error>> {
    let escapes = wrapped
        .strip_prefix("/bin/sh -c 'unset -f printf 2>/dev/null; eval \"$(printf \"")
        .and_then(|s| s.strip_suffix("\")\"'"))
        .ok_or("unknown wrapper")?;
    let bytes = escapes
        .as_bytes()
        .chunks(4)
        .map(|c| {
            if c.len() != 4 || c[0] != b'\\' {
                return Err("bad escape".into());
            }
            Ok(u8::from_str_radix(std::str::from_utf8(&c[1..])?, 8)?)
        })
        .collect::<Result<Vec<u8>, Box<dyn Error>>>()?;
    let line = String::from_utf8(bytes)?;
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quoted = false;
    let mut escape = false;
    let mut active = false;
    for c in line.chars() {
        if escape {
            word.push(c);
            escape = false;
            active = true;
        } else if c == '\\' && !quoted {
            escape = true;
        } else if c == '\'' {
            quoted = !quoted;
            active = true;
        } else if c == ' ' && !quoted {
            if active {
                words.push(std::mem::take(&mut word));
                active = false;
            }
        } else {
            word.push(c);
            active = true;
        }
    }
    if quoted || escape {
        return Err("unfinished word".into());
    }
    if active {
        words.push(word);
    }
    Ok(words)
}
