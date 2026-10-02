//! A small HTTP and WebSocket client over loopback TCP, for driving a `pitcrewd` the way the
//! desktop and `pitcrew hook` do. Test-only in spirit: it handles what the daemon sends
//! (`Content-Length` or chunked bodies, unfragmented server frames), nothing more.

use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

/// A response.
#[derive(Debug)]
pub struct Reply {
    /// The status code.
    pub status: u16,
    /// The body as text.
    pub body: String,
}

/// One HTTP/1.1 request on a fresh connection, with a bearer `token`.
///
/// # Errors
///
/// The connection fails or the answer is not HTTP.
pub fn request(
    port: u16,
    method: &str,
    path: &str,
    token: Option<&str>,
    body: Option<&str>,
) -> io::Result<Reply> {
    let mut stream = TcpStream::connect(("127.0.0.1", port))?;
    stream.set_read_timeout(Some(Duration::from_secs(60)))?;
    stream.set_nodelay(true)?;
    let mut head =
        format!("{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n");
    if let Some(token) = token {
        head.push_str(&format!("Authorization: Bearer {token}\r\n"));
    }
    let body = body.unwrap_or_default();
    if !body.is_empty() {
        head.push_str("Content-Type: application/json\r\n");
    }
    if !body.is_empty() || method == "POST" {
        head.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes())?;
    stream.write_all(body.as_bytes())?;

    let mut raw = Vec::new();
    stream.read_to_end(&mut raw)?;
    let split = find(&raw, b"\r\n\r\n").ok_or_else(|| bad("no complete response head"))?;
    let (status, chunked) = parse_head(&raw[..split])?;
    let mut body = raw[split + 4..].to_vec();
    if chunked {
        body = dechunk(&body)?;
    }
    Ok(Reply {
        status,
        body: String::from_utf8_lossy(&body).into_owned(),
    })
}

fn bad(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, what.to_owned())
}

fn parse_head(head: &[u8]) -> io::Result<(u16, bool)> {
    let text = String::from_utf8_lossy(head);
    let mut lines = text.split("\r\n");
    let status = lines
        .next()
        .and_then(|l| l.split(' ').nth(1))
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| bad("bad status line"))?;
    let chunked = lines.any(|l| {
        l.split_once(':').is_some_and(|(n, v)| {
            n.trim().eq_ignore_ascii_case("transfer-encoding")
                && v.trim().eq_ignore_ascii_case("chunked")
        })
    });
    Ok((status, chunked))
}

fn dechunk(mut body: &[u8]) -> io::Result<Vec<u8>> {
    let mut out = Vec::new();
    loop {
        let line_end = find(body, b"\r\n").ok_or_else(|| bad("no chunk size"))?;
        let size = String::from_utf8_lossy(&body[..line_end]);
        let size = usize::from_str_radix(size.split(';').next().unwrap_or("").trim(), 16)
            .map_err(|_| bad("bad chunk size"))?;
        body = &body[line_end + 2..];
        if size == 0 {
            return Ok(out);
        }
        if body.len() < size + 2 {
            return Err(bad("short chunk"));
        }
        out.extend_from_slice(&body[..size]);
        body = &body[size + 2..];
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// A WebSocket connection that only reads: the daemon's stream sends, the client listens.
#[derive(Debug)]
pub struct Ws {
    stream: TcpStream,
    buf: Vec<u8>,
}

impl Ws {
    /// Opens `path` with the token as a subprotocol, as the browser does.
    ///
    /// # Errors
    ///
    /// The connection fails, or the server does not switch protocols.
    pub fn connect(port: u16, path: &str, token: &str) -> io::Result<Self> {
        let mut stream = TcpStream::connect(("127.0.0.1", port))?;
        stream.set_nodelay(true)?;
        stream.set_read_timeout(Some(Duration::from_secs(30)))?;
        let head = format!(
            "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nUpgrade: websocket\r\n\
             Connection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
             Sec-WebSocket-Version: 13\r\n\
             Sec-WebSocket-Protocol: pitcrew.v1, pitcrew.bearer.{token}\r\n\r\n"
        );
        stream.write_all(head.as_bytes())?;
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        let split = loop {
            if let Some(split) = find(&buf, b"\r\n\r\n") {
                break split;
            }
            let n = stream.read(&mut chunk)?;
            if n == 0 {
                return Err(bad("the server closed during the handshake"));
            }
            buf.extend_from_slice(&chunk[..n]);
        };
        let (status, _) = parse_head(&buf[..split])?;
        if status != 101 {
            return Err(bad(&format!("the stream answered {status}, not 101")));
        }
        let rest = buf[split + 4..].to_vec();
        Ok(Self { stream, buf: rest })
    }

    /// The next text frame, waiting at most `within`. `Ok(None)` when the server closed.
    ///
    /// # Errors
    ///
    /// No frame in time, or the connection fails.
    pub fn next_text(&mut self, within: Duration) -> io::Result<Option<String>> {
        let deadline = Instant::now() + within;
        loop {
            if let Some((opcode, payload)) = self.parse() {
                match opcode {
                    0x1 => return Ok(Some(String::from_utf8_lossy(&payload).into_owned())),
                    0x8 => return Ok(None),
                    _ => continue,
                }
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(io::Error::new(io::ErrorKind::TimedOut, "no frame in time"));
            }
            self.stream.set_read_timeout(Some(left))?;
            let mut chunk = [0u8; 16 * 1024];
            match self.stream.read(&mut chunk) {
                Ok(0) => return Ok(None),
                Ok(n) => self.buf.extend_from_slice(&chunk[..n]),
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) =>
                {
                    return Err(io::Error::new(io::ErrorKind::TimedOut, "no frame in time"));
                }
                Err(e) => return Err(e),
            }
        }
    }

    /// One whole server frame from the buffer: its opcode and payload.
    fn parse(&mut self) -> Option<(u8, Vec<u8>)> {
        let b = &self.buf;
        if b.len() < 2 {
            return None;
        }
        let opcode = b[0] & 0x0f;
        let (len, header) = match b[1] & 0x7f {
            126 if b.len() >= 4 => (usize::from(u16::from_be_bytes([b[2], b[3]])), 4),
            127 if b.len() >= 10 => (
                usize::try_from(u64::from_be_bytes(b[2..10].try_into().ok()?)).ok()?,
                10,
            ),
            126 | 127 => return None,
            n => (usize::from(n), 2),
        };
        if b.len() < header + len {
            return None;
        }
        let payload = b[header..header + len].to_vec();
        self.buf.drain(..header + len);
        Some((opcode, payload))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::thread;

    #[test]
    fn reads_a_content_length_and_a_chunked_answer() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            for reply in [
                "HTTP/1.1 202 Accepted\r\nContent-Length: 2\r\n\r\nok",
                "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n2\r\nde\r\n0\r\n\r\n",
            ] {
                let (mut s, _) = listener.accept().unwrap();
                let mut seen = [0u8; 1024];
                let _ = s.read(&mut seen).unwrap();
                s.write_all(reply.as_bytes()).unwrap();
            }
        });
        let a = request(port, "POST", "/x", Some("t"), Some("{}")).unwrap();
        assert_eq!((a.status, a.body.as_str()), (202, "ok"));
        let b = request(port, "GET", "/y", None, None).unwrap();
        assert_eq!((b.status, b.body.as_str()), (200, "abcde"));
        server.join().unwrap();
    }

    #[test]
    fn reads_frames_of_every_length_and_skips_pings() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let long = "x".repeat(70_000);
        let sent = long.clone();
        let server = thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut seen = [0u8; 1024];
            let _ = s.read(&mut seen).unwrap();
            let mut out =
                b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\n\r\n".to_vec();
            out.extend_from_slice(&[0x81, 2, b'h', b'i']);
            out.extend_from_slice(&[0x89, 0]);
            out.extend_from_slice(&[0x81, 126, 0x01, 0x2c]);
            out.extend_from_slice(&[b'a'; 300]);
            out.extend_from_slice(&[0x81, 127]);
            out.extend_from_slice(&(sent.len() as u64).to_be_bytes());
            out.extend_from_slice(sent.as_bytes());
            out.extend_from_slice(&[0x88, 0]);
            s.write_all(&out).unwrap();
            thread::sleep(Duration::from_millis(200));
        });
        let mut ws = Ws::connect(port, "/v1/stream", "t").unwrap();
        let wait = Duration::from_secs(5);
        assert_eq!(ws.next_text(wait).unwrap().as_deref(), Some("hi"));
        assert_eq!(ws.next_text(wait).unwrap().map(|t| t.len()), Some(300));
        assert_eq!(ws.next_text(wait).unwrap().as_deref(), Some(long.as_str()));
        assert_eq!(ws.next_text(wait).unwrap(), None);
        server.join().unwrap();
    }
}
