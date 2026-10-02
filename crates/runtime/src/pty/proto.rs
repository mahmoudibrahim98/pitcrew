//! The protocol between [`super::PtyRuntime`] and `pitcrew-ptyd`, version [`PROTOCOL`]. It is
//! separate from API v1 and changes rarely: a ptyd outlives the daemons that use it, so a newer
//! daemon may meet an older ptyd.
//!
//! **Frames.** Each message is one frame: a little-endian `u32` with the length of the rest
//! (at most [`MAX_FRAME`]), a little-endian `u32` with the header's length (at most
//! [`MAX_HEADER`]), the header (JSON), then the payload: raw bytes (typed input for `write`,
//! output for `read`), empty for everything else.
//!
//! **Requests** ([`Request`]) carry an id the client chooses; every request gets exactly one
//! [`Reply`] with the same id, in any order. A connection starts with `hello`; the server
//! answers with its version and pid, or refuses a protocol it does not speak and closes.
//!
//! **Order.** The server applies `write`, `keys` and `resize` in the order a connection sends
//! them; other requests may be answered out of order.
//!
//! **Limits.** The server refuses frames over [`MAX_FRAME`] and headers over [`MAX_HEADER`]
//! (closing the connection), `write` payloads over [`MAX_WRITE`], and sizes outside 1 to 1000.
//! A `read` returns at most [`MAX_READ`] bytes and waits at most [`MAX_WAIT_MS`].

use pitcrew_interfaces::runtime::TerminalInfo;
use pitcrew_protocol::ids::TerminalId;
use pitcrew_protocol::runner::Key;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// This protocol's version.
pub const PROTOCOL: u32 = 1;
/// The largest frame, header and payload together.
pub const MAX_FRAME: usize = (4 << 20) + MAX_HEADER;
/// The largest header.
pub const MAX_HEADER: usize = 1 << 20;
/// The most input one `write` carries.
pub const MAX_WRITE: usize = 1 << 20;
/// The most output one `read` returns.
pub const MAX_READ: usize = 4 << 20;
/// The longest a `read` waits for output.
pub const MAX_WAIT_MS: u64 = 10_000;

/// A request: an id of the client's choosing and what to do.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Request {
    /// Echoed in the reply.
    pub id: u64,
    /// What to do.
    #[serde(flatten)]
    pub op: Op,
}

/// What a request asks for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Op {
    /// The first request on a connection. Answer: [`Hello`].
    Hello {
        /// The protocol the client speaks.
        protocol: u32,
    },
    /// Starts a program in a new terminal. Answer: [`Terminal`].
    Start {
        /// The program and its arguments, never a shell command line. The program is found as
        /// a file: a path (relative to `cwd`), or a name on `env`'s `PATH`, else ptyd's own
        /// (absolute entries only).
        argv: Vec<String>,
        /// The working directory: absolute, and it must exist.
        cwd: String,
        /// Variables set on top of ptyd's own environment.
        env: Vec<(String, String)>,
        /// A name for people.
        name: String,
        /// Size: 1 to 1000.
        cols: u16,
        /// Size: 1 to 1000.
        rows: u16,
    },
    /// Types the payload into a terminal. Answer: empty.
    Write {
        /// The terminal.
        terminal: TerminalId,
    },
    /// Sends named keys, in order. Answer: empty.
    Keys {
        /// The terminal.
        terminal: TerminalId,
        /// The keys.
        keys: Vec<Key>,
    },
    /// Resizes a terminal. Answer: empty.
    Resize {
        /// The terminal.
        terminal: TerminalId,
        /// Size: 1 to 1000.
        cols: u16,
        /// Size: 1 to 1000.
        rows: u16,
    },
    /// The visible screen. Answer: [`pitcrew_interfaces::runtime::Screen`].
    Screen {
        /// The terminal.
        terminal: TerminalId,
    },
    /// Output from `from`, at most `max` bytes (and [`MAX_READ`]). With `wait_ms`, waits that
    /// long at most (and [`MAX_WAIT_MS`]) while the output ends at or before `from` and the
    /// program runs. Answer: [`Chunk`], with the bytes as the payload.
    Read {
        /// The terminal.
        terminal: TerminalId,
        /// The first offset wanted.
        from: u64,
        /// The most bytes wanted.
        max: u64,
        /// How long to wait for output past `from`, in milliseconds.
        #[serde(default)]
        wait_ms: u64,
    },
    /// Every terminal, running or ended (ended ones are kept until 16 more have ended). Answer:
    /// a list of [`Terminal`].
    List,
    /// Ends a terminal's program and everything it started. Answer: empty.
    Kill {
        /// The terminal.
        terminal: TerminalId,
    },
    /// One terminal. Answer: [`Terminal`].
    Info {
        /// The terminal.
        terminal: TerminalId,
    },
}

/// The answer to a request: `ok` (shaped by the request) or `err`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Reply {
    /// The request's id.
    pub id: u64,
    /// The answer, if it succeeded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ok: Option<serde_json::Value>,
    /// Why it failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub err: Option<Failure>,
}

impl Reply {
    /// A success.
    pub fn ok(id: u64, answer: &impl Serialize) -> Self {
        match serde_json::to_value(answer) {
            Ok(value) => Self {
                id,
                ok: Some(value),
                err: None,
            },
            Err(e) => Self::err(
                id,
                FailureKind::Io,
                format!("cannot encode the answer: {e}"),
            ),
        }
    }

    /// A failure.
    pub fn err(id: u64, kind: FailureKind, message: impl Into<String>) -> Self {
        Self {
            id,
            ok: None,
            err: Some(Failure {
                kind,
                message: message.into(),
            }),
        }
    }
}

/// Why a request failed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Failure {
    /// The kind of failure.
    pub kind: FailureKind,
    /// For people.
    pub message: String,
}

/// Kinds of failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FailureKind {
    /// No such terminal.
    NotFound,
    /// The terminal's program has ended.
    Exited,
    /// The program could not be started.
    Spawn,
    /// The request was malformed or out of bounds.
    Invalid,
    /// Too much is waiting; try again later.
    Busy,
    /// The protocol or the request is not supported.
    Unsupported,
    /// Anything else.
    #[serde(other)]
    Io,
}

/// The answer to `hello`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    /// The protocol the server speaks.
    pub protocol: u32,
    /// The server's version (its crate version).
    pub version: String,
    /// The server's process id.
    pub pid: u32,
}

/// A terminal, as `start`, `info` and `list` describe it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Terminal {
    /// Id.
    pub id: TerminalId,
    /// Name.
    pub name: String,
    /// The program's process id. On Unix it leads its own session and process group.
    pub pid: Option<u32>,
    /// Whether the program is still running.
    pub alive: bool,
    /// Its exit code, once it has ended (on Unix, absent if a signal ended it).
    #[serde(default)]
    pub exit_code: Option<i64>,
    /// Size.
    pub cols: u16,
    /// Size.
    pub rows: u16,
}

impl Terminal {
    /// As the `Runtime` trait describes terminals. Nothing can attach to a PTY by hand, so it
    /// has no `native_target`.
    pub fn info(&self) -> TerminalInfo {
        TerminalInfo {
            id: self.id,
            name: self.name.clone(),
            pid: self.pid,
            alive: self.alive,
            native_target: None,
        }
    }
}

/// The answer to `read`; the bytes are the payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Chunk {
    /// The offset of the payload's first byte.
    pub offset: u64,
    /// Where the output ends now.
    pub end: u64,
    /// True if `from` had been dropped from the history, so `offset` is later than asked.
    pub truncated: bool,
    /// Whether the program is still running.
    pub alive: bool,
}

/// One frame: a JSON header and a payload.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Frame {
    /// JSON.
    pub header: Vec<u8>,
    /// Raw bytes.
    pub payload: Vec<u8>,
}

impl Frame {
    /// A frame of `header` (encoded as JSON) and `payload`.
    ///
    /// # Errors
    ///
    /// If the header cannot be encoded, or the frame would be over the limits.
    pub fn new(header: &impl Serialize, payload: Vec<u8>) -> std::io::Result<Self> {
        let header = serde_json::to_vec(header).map_err(std::io::Error::other)?;
        if header.len() > MAX_HEADER || header.len() + payload.len() > MAX_FRAME {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "the message is too large",
            ));
        }
        Ok(Self { header, payload })
    }

    /// The bytes on the wire.
    pub fn encode(&self) -> Vec<u8> {
        let rest = 4 + self.header.len() + self.payload.len();
        let mut out = Vec::with_capacity(4 + rest);
        // Lengths are within MAX_FRAME, far below u32::MAX.
        out.extend_from_slice(&u32::try_from(rest).unwrap_or(u32::MAX).to_le_bytes());
        out.extend_from_slice(
            &u32::try_from(self.header.len())
                .unwrap_or(u32::MAX)
                .to_le_bytes(),
        );
        out.extend_from_slice(&self.header);
        out.extend_from_slice(&self.payload);
        out
    }
}

/// Reads one frame. `Ok(None)` at the end of the stream between frames.
///
/// # Errors
///
/// `InvalidData` for a frame over the limits (the connection should be closed), or the stream's
/// own errors, including an end in the middle of a frame.
pub async fn read_frame<R: AsyncRead + Unpin>(reader: &mut R) -> std::io::Result<Option<Frame>> {
    let mut word = [0u8; 4];
    let mut got = 0;
    while got < 4 {
        let n = reader.read(&mut word[got..]).await?;
        if n == 0 {
            return if got == 0 {
                Ok(None)
            } else {
                Err(std::io::ErrorKind::UnexpectedEof.into())
            };
        }
        got += n;
    }
    let rest = u32::from_le_bytes(word) as usize;
    if !(4..=4 + MAX_FRAME).contains(&rest) {
        return Err(invalid(format!("a frame of {rest} bytes")));
    }
    reader.read_exact(&mut word).await?;
    let header_len = u32::from_le_bytes(word) as usize;
    if header_len > MAX_HEADER || header_len > rest - 4 {
        return Err(invalid(format!("a header of {header_len} bytes")));
    }
    let mut header = vec![0; header_len];
    reader.read_exact(&mut header).await?;
    let mut payload = vec![0; rest - 4 - header_len];
    reader.read_exact(&mut payload).await?;
    Ok(Some(Frame { header, payload }))
}

/// Writes one frame and flushes.
///
/// # Errors
///
/// The stream's.
pub async fn write_frame<W: AsyncWrite + Unpin>(
    writer: &mut W,
    frame: &Frame,
) -> std::io::Result<()> {
    writer.write_all(&frame.encode()).await?;
    writer.flush().await
}

fn invalid(what: String) -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        format!("{what}: over the protocol's limits"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block_on<F: std::future::Future>(future: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime")
            .block_on(future)
    }

    #[test]
    fn requests_and_replies_have_a_stable_shape() {
        let id: TerminalId = "01J9Z3B4C5D6E7F8G9H0JKMNPQ".parse().expect("id");
        let read = Request {
            id: 7,
            op: Op::Read {
                terminal: id,
                from: 10,
                max: 20,
                wait_ms: 0,
            },
        };
        let text = serde_json::to_string(&read).expect("encode");
        assert_eq!(
            text,
            r#"{"id":7,"op":"read","terminal":"01J9Z3B4C5D6E7F8G9H0JKMNPQ","from":10,"max":20,"wait_ms":0}"#
        );
        assert_eq!(
            serde_json::from_str::<Request>(&text).expect("decode"),
            read
        );
        let list: Request = serde_json::from_str(r#"{"id":1,"op":"list"}"#).expect("list");
        assert_eq!(list.op, Op::List);
        let keys: Request = serde_json::from_str(
            r#"{"id":2,"op":"keys","terminal":"01J9Z3B4C5D6E7F8G9H0JKMNPQ","keys":["up","ctrl_c"]}"#,
        )
        .expect("keys");
        assert_eq!(
            keys.op,
            Op::Keys {
                terminal: id,
                keys: vec![Key::Up, Key::CtrlC]
            }
        );
        for bad in [
            r#"{"id":1,"op":"format_disk"}"#,
            r#"{"op":"list"}"#,
            r#"{"id":1,"op":"resize","terminal":"x","cols":1,"rows":1}"#,
            r#"{"id":1,"op":"resize","terminal":"01J9Z3B4C5D6E7F8G9H0JKMNPQ","cols":70000,"rows":1}"#,
        ] {
            assert!(serde_json::from_str::<Request>(bad).is_err(), "{bad}");
        }
        let reply = Reply::err(3, FailureKind::NotFound, "no such terminal");
        let text = serde_json::to_string(&reply).expect("encode");
        assert_eq!(
            text,
            r#"{"id":3,"err":{"kind":"not_found","message":"no such terminal"}}"#
        );
        // A kind from a newer ptyd is still a failure.
        let newer: Reply =
            serde_json::from_str(r#"{"id":3,"err":{"kind":"quota","message":"x"}}"#).expect("kind");
        assert_eq!(newer.err.map(|f| f.kind), Some(FailureKind::Io));
    }

    #[test]
    fn frames_round_trip_and_bad_lengths_are_refused() {
        let frame = Frame::new(
            &Request {
                id: 1,
                op: Op::List,
            },
            b"payload".to_vec(),
        )
        .expect("frame");
        let bytes = frame.encode();
        let mut two = bytes.clone();
        two.extend_from_slice(&bytes);
        let (first, second, end) = block_on(async {
            let mut reader = two.as_slice();
            (
                read_frame(&mut reader).await,
                read_frame(&mut reader).await,
                read_frame(&mut reader).await,
            )
        });
        assert_eq!(first.expect("first"), Some(frame.clone()));
        assert_eq!(second.expect("second"), Some(frame));
        assert_eq!(end.expect("end"), None);

        let huge = u32::try_from(MAX_FRAME + 5).expect("u32").to_le_bytes();
        let short = 3u32.to_le_bytes();
        let mut lying = 10u32.to_le_bytes().to_vec();
        lying.extend_from_slice(&100u32.to_le_bytes());
        lying.extend_from_slice(&[0; 6]);
        for bad in [
            &huge[..],
            &short[..],
            &lying[..],
            &bytes[..bytes.len() - 1],
            &[1, 2][..],
        ] {
            let result = block_on(async {
                let mut reader = bad;
                read_frame(&mut reader).await
            });
            assert!(result.is_err(), "{bad:?}");
        }
        assert!(
            Frame::new(
                &Request {
                    id: 1,
                    op: Op::List
                },
                vec![0; MAX_FRAME]
            )
            .is_err()
        );
    }
}
