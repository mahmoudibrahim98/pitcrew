//! Uses a disposable server and never connects to the user's tmux socket.

use std::collections::{VecDeque, hash_map::RandomState};
use std::hash::BuildHasher;
use std::io::{self, Read, Write};
use std::process::{Child, ChildStdin, Command as ProcessCommand, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use pitcrew_runtime::command::{Argument, Command};
use pitcrew_runtime::control::{CommandReply, ControlParser, Notification, PaneId};

const TIMEOUT: Duration = Duration::from_secs(10);

struct PrivateServer {
    socket: String,
}

impl PrivateServer {
    fn command(&self) -> ProcessCommand {
        let mut command = ProcessCommand::new("tmux");
        command.args(["-L", &self.socket, "-f", "/dev/null", "-u"]);
        command
            .env_remove("TMUX")
            .env_remove("TMUX_PANE")
            .env("PITCREW_TEST_VALUE", "expanded-would-be-wrong");
        command
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
                    Ok(0) => break,
                    Ok(count) => {
                        if sender.send(Ok(parser.feed(&bytes[..count]))).is_err() {
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
    match ProcessCommand::new("tmux").arg("-V").output() {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            eprintln!("skipped real tmux test: tmux is not installed");
            return;
        }
        Err(error) => panic!("could not probe tmux: {error}"),
        Ok(output) => assert!(output.status.success(), "tmux -V failed"),
    }
    let random = RandomState::new().hash_one((std::process::id(), SystemTime::now()));
    let name = format!("pitcrew-test-{random:016x}");
    let server = PrivateServer {
        socket: name.clone(),
    };
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
                .arg(Argument::Text("-p"))
                .expect("flag")
                .arg(Argument::Text("-t"))
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
            .arg(Argument::Text("-p"))
            .expect("flag")
            .arg(Argument::Text("pitcrew-reply"))
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
            .arg(Argument::Text("-p"))
            .expect("flag")
            .arg(Argument::Text("no-extra-commands"))
            .expect("text"),
    );
    assert_eq!(barrier.lines, vec![b"no-extra-commands".to_vec()]);
    let reply = client.run(
        Command::new("kill-session")
            .expect("command")
            .arg(Argument::Text("-t"))
            .expect("flag")
            .arg(Argument::Text(&name))
            .expect("target"),
    );
    assert!(!reply.failed);
    let deadline = Instant::now() + TIMEOUT;
    while !matches!(client.next(deadline), Notification::Exit { .. }) {}
}
