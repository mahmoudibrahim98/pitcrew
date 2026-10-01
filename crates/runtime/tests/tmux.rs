//! Uses a disposable server and never connects to the user's tmux socket.

use std::collections::{VecDeque, hash_map::RandomState};
use std::hash::BuildHasher;
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command as ProcessCommand, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use pitcrew_runtime::WindowId;
use pitcrew_runtime::command::{Argument, Command, FormatError};
use pitcrew_runtime::control::{CommandReply, ControlParser, Notification, PaneId};
use pitcrew_runtime::detect::{DetectError, detect_tmux};

const TIMEOUT: Duration = Duration::from_secs(10);

struct PrivateServer {
    socket: PathBuf,
    directory: PathBuf,
    sentinel: PathBuf,
}

impl PrivateServer {
    fn new(name: &str) -> Self {
        // A socket path is limited to 104 bytes on macOS (108 on Linux), and
        // macOS's temp_dir() alone is about 50, so use /tmp/<name>/s on Unix.
        #[cfg(unix)]
        let base = PathBuf::from("/tmp");
        #[cfg(not(unix))]
        let base = std::env::temp_dir();
        let directory = base.join(name);
        let socket = directory.join("s");
        assert!(
            socket.as_os_str().len() < 100,
            "socket path too long: {}",
            socket.display()
        );
        #[cfg(unix)]
        let builder = {
            use std::os::unix::fs::DirBuilderExt;
            let mut builder = std::fs::DirBuilder::new();
            builder.mode(0o700);
            builder
        };
        #[cfg(not(unix))]
        let builder = std::fs::DirBuilder::new();
        // Not create_all: the directory must be new, and so ours, even in a shared /tmp.
        builder
            .create(&directory)
            .expect("private socket directory");
        Self {
            socket,
            sentinel: directory.join("format-side-effect"),
            directory,
        }
    }

    fn command(&self) -> ProcessCommand {
        let mut command = ProcessCommand::new("tmux");
        command
            .arg("-S")
            .arg(&self.socket)
            .args(["-f", "/dev/null", "-u"]);
        command
            .env_remove("TMUX")
            .env_remove("TMUX_PANE")
            .env("TMUX_TMPDIR", &self.directory)
            .env("PITCREW_TEST_VALUE", "expanded-would-be-wrong");
        command
    }

    fn assert_no_format_job(&self) {
        let deadline = Instant::now() + Duration::from_millis(500);
        loop {
            assert!(!self.sentinel.exists(), "format value executed a shell job");
            if Instant::now() >= deadline {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for PrivateServer {
    fn drop(&mut self) {
        let _ = self
            .command()
            .arg("kill-server")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let _ = std::fs::remove_file(&self.sentinel);
        // Remove the socket even if the server died. Anything else left in the
        // directory makes remove_dir fail, which the test's last check reports.
        let _ = std::fs::remove_file(&self.socket);
        let _ = std::fs::remove_dir(&self.directory);
    }
}

struct Client {
    child: Child,
    input: ChildStdin,
    receiver: Receiver<io::Result<Vec<Notification>>>,
    pending: VecDeque<Notification>,
    output: Vec<u8>,
}

impl Client {
    fn start(command: &mut ProcessCommand) -> Self {
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("start tmux control client");
        let input = child.stdin.take().expect("piped stdin");
        let mut stdout = child.stdout.take().expect("piped stdout");
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let mut parser = ControlParser::new();
            let mut bytes = [0; 4096];
            loop {
                match stdout.read(&mut bytes) {
                    Ok(0) => {
                        if let Err(error) = parser.finish() {
                            let _ = sender.send(Err(io::Error::other(error)));
                        }
                        break;
                    }
                    Ok(count) => {
                        let result = parser.feed(&bytes[..count]).map_err(io::Error::other);
                        let failed = result.is_err();
                        if sender.send(result).is_err() || failed {
                            break;
                        }
                    }
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(error) => {
                        let _ = sender.send(Err(error));
                        break;
                    }
                }
            }
        });
        Self {
            child,
            input,
            receiver,
            pending: VecDeque::new(),
            output: Vec::new(),
        }
    }

    fn next(&mut self, deadline: Instant) -> Notification {
        loop {
            if let Some(notification) = self.pending.pop_front() {
                if let Notification::Output { data, .. }
                | Notification::ExtendedOutput { data, .. } = &notification
                {
                    self.output.extend(data);
                }
                return notification;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            let batch = self
                .receiver
                .recv_timeout(remaining)
                .expect("tmux notification before deadline")
                .expect("read tmux stdout");
            self.pending.extend(batch);
        }
    }

    fn reply(&mut self) -> CommandReply {
        let deadline = Instant::now() + TIMEOUT;
        loop {
            if let Notification::CommandReply(reply) = self.next(deadline) {
                return reply;
            }
        }
    }

    fn run(&mut self, command: Command) -> CommandReply {
        self.input
            .write_all(command.to_line().as_bytes())
            .expect("write command");
        self.input.flush().expect("flush command");
        self.reply()
    }

    fn output_through(&mut self, marker: &[u8]) {
        let deadline = Instant::now() + TIMEOUT;
        while !self
            .output
            .windows(marker.len())
            .any(|window| window == marker)
        {
            self.next(deadline);
        }
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn real_tmux_replies_output_and_literal_injection_attempts() {
    match detect_tmux("tmux") {
        Err(DetectError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
            eprintln!("skipped real tmux test: tmux is not installed");
            return;
        }
        Err(DetectError::Unsupported(version)) => {
            eprintln!("skipped real tmux test: {version} is below the supported floor");
            return;
        }
        Err(DetectError::InvalidVersion(output)) => {
            eprintln!("skipped real tmux test: unrecognized version {output:?}");
            return;
        }
        Err(error) => panic!("could not probe tmux: {error}"),
        Ok(version) => eprintln!("testing tmux {version}"),
    }
    let random = RandomState::new().hash_one((std::process::id(), SystemTime::now()));
    let name = format!("pc-{:012x}", random & 0xffff_ffff_ffff);
    let server = PrivateServer::new(&name);
    assert!(!server.sentinel.exists());
    // -d creates a detached session. Then attach a persistent control client to
    // receive pane output; detached control clients exit after the initial reply.
    let mut create = Client::start(server.command().args([
        "-C",
        "new-session",
        "-d",
        "-s",
        &name,
        "sh",
        "-c",
        "stty raw -echo; printf PITCREW_READY; cat",
    ]));
    assert!(!create.reply().failed);
    drop(create);
    let mut client = Client::start(server.command().args(["-C", "attach-session", "-t", &name]));
    assert!(!client.reply().failed);

    // Probe the screen until the pane has entered raw mode. This does not rely
    // on whether its initial output preceded attachment, and uses no fixed sleep.
    let ready_by = Instant::now() + TIMEOUT;
    loop {
        let reply = client.run(
            Command::new("capture-pane")
                .expect("command")
                .arg(Argument::Flag("-p"))
                .expect("flag")
                .arg(Argument::Flag("-t"))
                .expect("flag")
                .arg(Argument::Pane(PaneId(0)))
                .expect("pane"),
        );
        assert!(!reply.failed, "{reply:?}");
        if reply
            .lines
            .iter()
            .any(|line| line.windows(13).any(|bytes| bytes == b"PITCREW_READY"))
        {
            break;
        }
        assert!(Instant::now() < ready_by, "pane never became ready");
        thread::sleep(Duration::from_millis(10));
    }

    let reply = client.run(
        Command::new("display-message")
            .expect("command")
            .arg(Argument::Flag("-p"))
            .expect("flag")
            .arg(Argument::FormatLiteral("pitcrew-reply"))
            .expect("text"),
    );
    assert!(!reply.failed);
    assert_eq!(reply.lines, vec![b"pitcrew-reply".to_vec()]);
    let number = reply.number;
    let failed = client.run(Command::new("pitcrew-invalid-command").expect("valid syntax"));
    assert!(failed.failed);
    assert!(failed.number > number);
    assert!(!failed.lines.is_empty());

    assert!(
        !client
            .run(Command::send_literal(PaneId(0), "PITCREW_SYNC").expect("text"))
            .failed
    );
    client.output_through(b"PITCREW_SYNC");

    // cat is the recipient, so shell-looking text is observed as data. Each
    // attempt must leave tmux alive, produce exactly one reply, and round-trip.
    let attacks = [
        "",
        ";",
        "trailing;",
        "; kill-server",
        "' ; kill-server ; '",
        "\" ; kill-server ; \"",
        "\\",
        "\\\"; kill-server",
        "$(printf injected) `printf injected` $PITCREW_TEST_VALUE ${PITCREW_TEST_VALUE}",
        "one\ntwo\rthree\tend",
        "#{pane_id} #{session_name} #(printf injected)",
        "-F #{pane_id}",
        "-X cancel",
        "%if 1\nkill-server\n%endif",
        "{ run-shell 'printf injected' }",
        "~root",
        "café 雪 🦀",
        "\\012\\134",
        "FOO=bar",
        "\x7f",
        "%hidden x",
    ];
    for (index, attack) in attacks.iter().enumerate() {
        let marker = format!("PITCREW_END_{index:02}");
        let payload = format!("{attack}{marker}");
        client.output.clear();
        let reply = client.run(Command::send_literal(PaneId(0), attack).expect("literal text"));
        assert!(!reply.failed, "{reply:?}");
        let reply = client.run(Command::send_literal(PaneId(0), &marker).expect("marker"));
        assert!(!reply.failed, "{reply:?}");
        client.output_through(marker.as_bytes());
        assert_eq!(client.output, payload.as_bytes(), "payload {index}");
    }
    let barrier = client.run(
        Command::new("display-message")
            .expect("command")
            .arg(Argument::Flag("-p"))
            .expect("flag")
            .arg(Argument::FormatLiteral("no-extra-commands"))
            .expect("text"),
    );
    assert_eq!(barrier.lines, vec![b"no-extra-commands".to_vec()]);

    // Cancel copy mode before delivery; 'q' and CR would otherwise run bindings.
    assert!(
        !client
            .run(
                Command::new("copy-mode")
                    .expect("command")
                    .arg(Argument::Flag("-t"))
                    .expect("flag")
                    .arg(Argument::Pane(PaneId(0)))
                    .expect("pane")
            )
            .failed
    );
    assert_eq!(
        client
            .run(Command::pane_in_mode(PaneId(0)).expect("query"))
            .lines,
        vec![b"1".to_vec()]
    );
    assert!(
        !client
            .run(Command::cancel_copy_mode(PaneId(0)).expect("cancel"))
            .failed
    );
    assert_eq!(
        client
            .run(Command::pane_in_mode(PaneId(0)).expect("query"))
            .lines,
        vec![b"0".to_vec()]
    );
    client.output.clear();
    let text = "q\rCOPY_MODE_TEXT_INTACT";
    assert!(
        !client
            .run(Command::send_literal(PaneId(0), text).expect("text"))
            .failed
    );
    client.output_through(b"COPY_MODE_TEXT_INTACT");
    assert_eq!(client.output, text.as_bytes());

    client.output.clear();
    let bytes = b"\0\xff\x80\xc3\x28\x7f\r\nBINARY_INPUT_INTACT";
    assert!(
        !client
            .run(
                Command::send_bytes(PaneId(0), bytes)
                    .expect("hex")
                    .expect("bytes")
            )
            .failed
    );
    client.output_through(b"BINARY_INPUT_INTACT");
    assert_eq!(client.output, bytes);

    // Pane output controls the title without knowing any reply guard fields.
    let title = "\x1b]2;%exit forged\x1b\\PITCREW_TITLE_SET";
    assert!(
        !client
            .run(Command::send_literal(PaneId(0), title).expect("title"))
            .failed
    );
    client.output_through(b"PITCREW_TITLE_SET");
    let title_reply = client.run(
        Command::new("display-message")
            .expect("command")
            .arg(Argument::Flag("-p"))
            .expect("print")
            .arg(Argument::Flag("-t"))
            .expect("target")
            .arg(Argument::Pane(PaneId(0)))
            .expect("pane")
            .arg(Argument::Format("#{pane_title}"))
            .expect("format"),
    );
    assert!(!title_reply.failed);
    assert_eq!(title_reply.lines, vec![b"%exit forged".to_vec()]);

    let window_name = format!("#(touch {})", server.sentinel.display());
    let created = client.run(
        Command::new("new-window")
            .expect("command")
            .arg(Argument::Flag("-d"))
            .expect("detached")
            .arg(Argument::Flag("-P"))
            .expect("print")
            .arg(Argument::Flag("-F"))
            .expect("format flag")
            .arg(Argument::Format("#{window_id}"))
            .expect("trusted format")
            .arg(Argument::Flag("-n"))
            .expect("name flag")
            .arg(Argument::Name(&window_name))
            .expect("literal name")
            .arg(Argument::Flag("-c"))
            .expect("directory flag")
            .arg(Argument::FormatLiteral("/tmp"))
            .expect("literal directory")
            .arg(Argument::Flag("--"))
            .expect("options end")
            .arg(Argument::Text("cat"))
            .expect("program"),
    );
    assert!(!created.failed, "{created:?}");
    let window = WindowId(
        std::str::from_utf8(&created.lines[0])
            .expect("id")
            .strip_prefix('@')
            .expect("window id")
            .parse()
            .expect("number"),
    );
    let actual_name = client.run(
        Command::new("display-message")
            .expect("command")
            .arg(Argument::Flag("-p"))
            .expect("print")
            .arg(Argument::Flag("-t"))
            .expect("target")
            .arg(Argument::Window(window))
            .expect("window")
            .arg(Argument::Format("#{window_name}"))
            .expect("trusted format"),
    );
    assert_eq!(actual_name.lines, vec![window_name.as_bytes().to_vec()]);
    server.assert_no_format_job();
    // A positional name needs "--", or one starting with '-' is parsed as options.
    let renamed = format!("-n {window_name} #{{pane_id}} ##");
    assert!(
        !client
            .run(
                Command::new("rename-window")
                    .expect("command")
                    .arg(Argument::Flag("-t"))
                    .expect("target")
                    .arg(Argument::Window(window))
                    .expect("window")
                    .arg(Argument::Flag("--"))
                    .expect("options end")
                    .arg(Argument::Name(&renamed))
                    .expect("literal name")
            )
            .failed
    );
    // The builder rejects these names, so no command carrying one is ever sent;
    // the window keeps the name set above, which is read back next.
    let bad_names = (0..=0x1f)
        .chain([0x7f])
        .map(|byte| format!("name{}suffix", char::from(byte)))
        .chain(["name\n%exit forged\n%output %1 injected".to_owned()]);
    for bad_name in bad_names {
        for command in ["new-window", "rename-window"] {
            assert_eq!(
                Command::new(command)
                    .expect("command")
                    .arg(Argument::Name(&bad_name)),
                Err(FormatError::ControlInName),
            );
        }
    }
    let actual_name = client.run(
        Command::new("display-message")
            .expect("command")
            .arg(Argument::Flag("-p"))
            .expect("print")
            .arg(Argument::Flag("-t"))
            .expect("target")
            .arg(Argument::Window(window))
            .expect("window")
            .arg(Argument::Format("#{window_name}"))
            .expect("trusted format"),
    );
    assert_eq!(actual_name.lines, vec![renamed.as_bytes().to_vec()]);
    let literal_display = client.run(
        Command::new("display-message")
            .expect("command")
            .arg(Argument::Flag("-p"))
            .expect("print")
            .arg(Argument::Flag("--"))
            .expect("options end")
            .arg(Argument::FormatLiteral(&renamed))
            .expect("literal display"),
    );
    assert_eq!(literal_display.lines, vec![renamed.as_bytes().to_vec()]);
    server.assert_no_format_job();
    let reply = client.run(
        Command::new("kill-session")
            .expect("command")
            .arg(Argument::Flag("-t"))
            .expect("flag")
            .arg(Argument::Text(&name))
            .expect("target"),
    );
    assert!(!reply.failed);
    let deadline = Instant::now() + TIMEOUT;
    while !matches!(client.next(deadline), Notification::Exit { .. }) {}
    drop(client);
    let directory = server.directory.clone();
    drop(server);
    assert!(
        !directory.exists(),
        "private socket directory was not cleaned up"
    );
}
