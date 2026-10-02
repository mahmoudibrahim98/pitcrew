//! The protocol's edges, on Unix: a real ptyd fed malformed or hostile frames keeps serving
//! everyone else, and `PtyRuntime` facing a ptyd that misbehaves (never answers, speaks another
//! protocol, sends garbage) stays bounded in time and says why.

#![cfg(unix)]

mod common;

use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::time::{Duration, Instant};

use common::{Fixture, wait_for};
use pitcrew_interfaces::runtime::{Runtime, RuntimeError};
use pitcrew_protocol::ids::TerminalId;
use pitcrew_runtime::pty::proto::{
    FailureKind, Frame, Hello, MAX_FRAME, MAX_WRITE, Op, PROTOCOL, Reply, Request,
};

fn frame(request: &Request, payload: Vec<u8>) -> Vec<u8> {
    Frame::new(request, payload).expect("frame").encode()
}

/// Reads one frame's header, or `None` at the end of the stream.
fn read_reply(stream: &mut UnixStream) -> Option<Reply> {
    let mut word = [0u8; 4];
    stream.read_exact(&mut word).ok()?;
    let rest = u32::from_le_bytes(word) as usize;
    let mut body = vec![0; rest];
    stream.read_exact(&mut body).ok()?;
    let header = u32::from_le_bytes(body[..4].try_into().ok()?) as usize;
    serde_json::from_slice(&body[4..4 + header]).ok()
}

fn hello(stream: &mut UnixStream, protocol: u32) -> Option<Reply> {
    let request = Request {
        id: 1,
        op: Op::Hello { protocol },
    };
    stream.write_all(&frame(&request, Vec::new())).ok()?;
    read_reply(stream)
}

fn connect(fx: &Fixture) -> UnixStream {
    let stream = UnixStream::connect(&fx.options.endpoint).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(20)))
        .expect("timeout");
    stream
}

/// True once the other end has closed the connection.
fn closed(stream: &mut UnixStream) -> bool {
    let mut byte = [0u8; 1];
    loop {
        match stream.read(&mut byte) {
            Ok(0) => return true,
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::ConnectionReset => return true,
            Err(_) => return false,
        }
    }
}

#[test]
fn hostile_clients_are_refused_and_others_keep_being_served() {
    let fx = Fixture::new("hostile-client");
    let rt = fx.runtime();
    let t = rt
        .start(&fx.script("steady", "printf ready; exec cat"))
        .expect("start");
    fx.saw_ptyd(&rt);
    wait_for(&rt, t.id, 0, b"ready");

    // A frame claiming 4 GiB is refused before anything is allocated.
    let mut huge = connect(&fx);
    huge.write_all(&u32::MAX.to_le_bytes()).expect("write");
    assert!(closed(&mut huge));
    // Anything before hello ends the connection.
    let mut early = connect(&fx);
    early
        .write_all(&frame(
            &Request {
                id: 5,
                op: Op::List,
            },
            Vec::new(),
        ))
        .expect("write");
    assert!(closed(&mut early));
    // Another protocol is refused, with a reason.
    let mut other = connect(&fx);
    let refused = hello(&mut other, PROTOCOL + 1).expect("a reply");
    let failure = refused.err.expect("refused");
    assert_eq!(failure.kind, FailureKind::Unsupported);
    assert!(failure.message.contains("protocol"), "{}", failure.message);
    assert!(closed(&mut other));
    // A good client: unknown requests and oversized input are refused, not fatal.
    let mut good = connect(&fx);
    let welcome = hello(&mut good, PROTOCOL).expect("a reply");
    let hello: Hello = serde_json::from_value(welcome.ok.expect("ok")).expect("hello");
    assert_eq!(hello.protocol, PROTOCOL);
    assert_eq!(Some(hello.pid), rt.ptyd_pid());
    good.write_all(&raw_frame(br#"{"id":9,"op":"format_disk"}"#))
        .expect("write");
    let unknown = read_reply(&mut good).expect("a reply");
    assert_eq!(
        (unknown.id, unknown.err.map(|f| f.kind)),
        (9, Some(FailureKind::Invalid))
    );
    let write = Request {
        id: 10,
        op: Op::Write { terminal: t.id },
    };
    good.write_all(&frame(&write, vec![b'x'; MAX_WRITE + 1]))
        .expect("write");
    let big = read_reply(&mut good).expect("a reply");
    assert_eq!(
        (big.id, big.err.map(|f| f.kind)),
        (10, Some(FailureKind::Invalid))
    );
    let missing = Request {
        id: 11,
        op: Op::Info {
            terminal: TerminalId::new(),
        },
    };
    good.write_all(&frame(&missing, Vec::new())).expect("write");
    let not_found = read_reply(&mut good).expect("a reply");
    assert_eq!(not_found.err.map(|f| f.kind), Some(FailureKind::NotFound));
    let size = Request {
        id: 12,
        op: Op::Resize {
            terminal: t.id,
            cols: 1001,
            rows: 10,
        },
    };
    good.write_all(&frame(&size, Vec::new())).expect("write");
    assert_eq!(
        read_reply(&mut good).and_then(|r| r.err).map(|f| f.kind),
        Some(FailureKind::Invalid)
    );
    // A header that is not JSON at all ends that connection only.
    good.write_all(b"\x05\x00\x00\x00\x01\x00\x00\x00{")
        .expect("write");
    assert!(closed(&mut good));
    // A client that never says hello holds up nobody.
    let silent = connect(&fx);
    rt.write(t.id, b"still\r").expect("write");
    wait_for(&rt, t.id, 0, b"still\r\nstill");
    drop(silent);
    rt.kill(t.id).expect("kill");
    drop(rt);
    fx.finish();
}

#[test]
fn calls_to_a_stopped_ptyd_are_bounded_and_work_again_when_it_resumes() {
    let fx = Fixture::new("stopped");
    let mut options = fx.options.clone();
    options.call_timeout = Duration::from_millis(500);
    let rt = pitcrew_runtime::PtyRuntime::new(options).expect("runtime");
    let t = rt
        .start(&fx.script("steady", "printf ready; exec cat"))
        .expect("start");
    fx.saw_ptyd(&rt);
    wait_for(&rt, t.id, 0, b"ready");
    let ptyd = rt.ptyd_pid().expect("connected").to_string();
    let signal = |name: &str| {
        let status = std::process::Command::new("kill")
            .args([name, ptyd.as_str()])
            .status()
            .expect("kill");
        assert!(status.success());
    };
    signal("-STOP");
    for call in 0..3 {
        let started = Instant::now();
        let answer = match call {
            0 => rt.read_output(t.id, 0, 10).map(drop),
            1 => rt.screen(t.id).map(drop),
            _ => rt.write(t.id, b"x"),
        };
        signal_if_failed(&answer, || signal("-CONT"));
        assert!(
            matches!(answer, Err(RuntimeError::Unavailable(_))),
            "{answer:?}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "{:?}",
            started.elapsed()
        );
    }
    signal("-CONT");
    // The input sent while it was stopped may still arrive; later calls work.
    rt.write(t.id, b"\rback\r").expect("write");
    wait_for(&rt, t.id, 0, b"back");
    rt.kill(t.id).expect("kill");
    drop(rt);
    fx.finish();
}

/// Resumes a stopped ptyd before a failed assertion, so it is not left stopped.
fn signal_if_failed<T>(answer: &Result<T, RuntimeError>, resume: impl FnOnce()) {
    if !matches!(answer, Err(RuntimeError::Unavailable(_))) {
        resume();
    }
}

/// `json` as a frame with no payload, whatever it says.
fn raw_frame(json: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&u32::try_from(json.len() + 4).expect("u32").to_le_bytes());
    out.extend_from_slice(&u32::try_from(json.len()).expect("u32").to_le_bytes());
    out.extend_from_slice(json);
    out
}

/// A fake ptyd on the fixture's endpoint, answering each connection with `serve`.
fn fake(fx: &Fixture, serve: impl Fn(UnixStream) + Send + Clone + 'static) {
    std::fs::create_dir_all(fx.options.endpoint.parent().expect("dir")).expect("dir");
    let listener = UnixListener::bind(&fx.options.endpoint).expect("bind");
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let serve = serve.clone();
            std::thread::spawn(move || serve(stream));
        }
    });
}

#[test]
fn a_ptyd_that_never_answers_costs_a_bounded_wait() {
    let fx = Fixture::new("mute");
    fake(&fx, |mut stream| {
        // Takes everything, answers nothing.
        let mut sink = [0u8; 4096];
        while stream.read(&mut sink).is_ok_and(|n| n > 0) {}
    });
    let mut options = fx.options.clone();
    options.call_timeout = Duration::from_millis(500);
    let rt = pitcrew_runtime::PtyRuntime::new(options).expect("runtime");
    let started = Instant::now();
    match rt.list() {
        Err(RuntimeError::Unavailable(why)) => assert!(why.contains("answer"), "{why}"),
        other => panic!("{other:?}"),
    }
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "{:?}",
        started.elapsed()
    );
    drop(rt);
    let _ = std::fs::remove_file(&fx.options.endpoint);
    fx.finish();
}

#[test]
fn a_ptyd_of_another_protocol_or_one_that_sends_garbage_is_refused_with_a_reason() {
    let fx = Fixture::new("other");
    fake(&fx, |mut stream| {
        let mut word = [0u8; 4];
        if stream.read_exact(&mut word).is_err() {
            return;
        }
        let mut body = vec![0; u32::from_le_bytes(word) as usize];
        if stream.read_exact(&mut body).is_err() {
            return;
        }
        let hello = Hello {
            protocol: PROTOCOL + 1,
            version: "9.9.9".into(),
            pid: 4242,
        };
        let reply = Reply::ok(1, &hello);
        let _ = stream.write_all(&Frame::new(&reply, Vec::new()).expect("frame").encode());
        let mut sink = [0u8; 64];
        while stream.read(&mut sink).is_ok_and(|n| n > 0) {}
    });
    let rt = fx.runtime();
    match rt.list() {
        Err(RuntimeError::Unavailable(why)) => {
            assert!(why.contains("protocol 2") && why.contains("9.9.9"), "{why}");
        }
        other => panic!("{other:?}"),
    }
    drop(rt);
    let _ = std::fs::remove_file(&fx.options.endpoint);

    let fx2 = fx;
    fake(&fx2, |mut stream| {
        let _ = stream.write_all(&u32::try_from(MAX_FRAME * 2).expect("u32").to_le_bytes());
        let mut sink = [0u8; 64];
        while stream.read(&mut sink).is_ok_and(|n| n > 0) {}
    });
    let rt = fx2.runtime();
    let started = Instant::now();
    assert!(matches!(rt.list(), Err(RuntimeError::Unavailable(_))));
    assert!(started.elapsed() < Duration::from_secs(10));
    drop(rt);
    let _ = std::fs::remove_file(&fx2.options.endpoint);
    fx2.finish();
}
