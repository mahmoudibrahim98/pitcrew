//! Just enough HTTP/1.1: one request per connection (`Connection: close`), responses framed by
//! `Content-Length`, chunked encoding, or the end of the stream.

use std::io::{self, Read, Write};

/// Longest response head accepted.
const MAX_HEAD: usize = 64 * 1024;

/// A request to send.
#[derive(Clone, Copy, Debug)]
pub struct Request<'a> {
    /// `GET`, `POST`, …
    pub method: &'a str,
    /// Path and query, already encoded.
    pub target: &'a str,
    /// The `Host` header.
    pub host: &'a str,
    /// Sent as `Authorization: Bearer <token>` when present.
    pub token: Option<&'a str>,
    /// A JSON body.
    pub body: Option<&'a [u8]>,
}

impl Request<'_> {
    /// The request's bytes, written with one `write_all`.
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut head = format!(
            "{} {} HTTP/1.1\r\nHost: {}\r\nUser-Agent: pitcrew/{}\r\nAccept: application/json\r\nConnection: close\r\n",
            self.method,
            self.target,
            self.host,
            env!("CARGO_PKG_VERSION"),
        );
        if let Some(token) = self.token {
            head.push_str("Authorization: Bearer ");
            head.push_str(token);
            head.push_str("\r\n");
        }
        let body = self.body.unwrap_or_default();
        if self.body.is_some() {
            head.push_str("Content-Type: application/json\r\n");
        }
        if self.body.is_some() || self.method != "GET" {
            head.push_str(&format!("Content-Length: {}\r\n", body.len()));
        }
        head.push_str("\r\n");
        let mut bytes = head.into_bytes();
        bytes.extend_from_slice(body);
        bytes
    }
}

/// A response.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Response {
    /// Status code.
    pub status: u16,
    /// Body, decoded from chunked encoding if need be.
    pub body: Vec<u8>,
}

impl Response {
    /// 2xx.
    #[must_use]
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

/// Sends `request` and reads the response, keeping at most `max_body` bytes of body.
///
/// # Errors
/// I/O errors, a malformed response, or a body over `max_body`.
pub fn exchange<S: Read + Write>(
    stream: &mut S,
    request: &Request<'_>,
    max_body: usize,
) -> io::Result<Response> {
    stream.write_all(&request.to_bytes())?;
    stream.flush()?;
    read_response(stream, request.method == "HEAD", max_body)
}

/// Reads one response, skipping any interim (1xx) ones.
///
/// # Errors
/// I/O errors, a malformed response, or a body over `max_body`.
pub fn read_response<R: Read>(
    stream: &mut R,
    head_only: bool,
    max_body: usize,
) -> io::Result<Response> {
    let mut reader = Buffered::new(stream);
    loop {
        let head = reader.read_head()?;
        let parsed = parse_head(&head)?;
        if (100..200).contains(&parsed.status) {
            continue;
        }
        let body = if head_only || parsed.status == 204 || parsed.status == 304 {
            Vec::new()
        } else if parsed.chunked {
            reader.read_chunked(max_body)?
        } else if let Some(len) = parsed.content_length {
            if len > max_body {
                return Err(too_large(max_body));
            }
            reader.read_exact_vec(len)?
        } else {
            reader.read_to_end_capped(max_body)?
        };
        return Ok(Response {
            status: parsed.status,
            body,
        });
    }
}

struct Head {
    status: u16,
    content_length: Option<usize>,
    chunked: bool,
}

fn malformed(what: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("malformed HTTP response: {what}"),
    )
}

fn too_large(max: usize) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("the response is larger than {max} bytes"),
    )
}

fn parse_head(head: &[u8]) -> io::Result<Head> {
    let text = std::str::from_utf8(head).map_err(|_| malformed("the head is not UTF-8"))?;
    let mut lines = text.split("\r\n");
    let status_line = lines.next().unwrap_or_default();
    let mut parts = status_line.splitn(3, ' ');
    let version = parts.next().unwrap_or_default();
    if !version.starts_with("HTTP/1.") {
        return Err(malformed("not HTTP/1.x"));
    }
    let status = parts
        .next()
        .filter(|s| s.len() == 3)
        .and_then(|s| s.parse::<u16>().ok())
        .ok_or_else(|| malformed("no status code"))?;
    let mut content_length = None;
    let mut chunked = false;
    for line in lines.filter(|l| !l.is_empty()) {
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| malformed("a header without a colon"))?;
        let value = value.trim();
        if name.eq_ignore_ascii_case("content-length") {
            content_length = Some(
                value
                    .parse::<usize>()
                    .map_err(|_| malformed("a bad Content-Length"))?,
            );
        } else if name.eq_ignore_ascii_case("transfer-encoding") {
            chunked = value
                .rsplit(',')
                .next()
                .is_some_and(|last| last.trim().eq_ignore_ascii_case("chunked"));
        }
    }
    Ok(Head {
        status,
        content_length,
        chunked,
    })
}

/// A small read buffer over the stream. The end of the stream, including a broken pipe (how a
/// Windows pipe ends), reads as 0.
struct Buffered<'a, R> {
    inner: &'a mut R,
    buf: Vec<u8>,
    pos: usize,
}

impl<'a, R: Read> Buffered<'a, R> {
    fn new(inner: &'a mut R) -> Self {
        Self {
            inner,
            buf: Vec::with_capacity(8192),
            pos: 0,
        }
    }

    /// Reads more into the buffer. Returns how many bytes came.
    fn fill(&mut self) -> io::Result<usize> {
        if self.pos == self.buf.len() {
            self.buf.clear();
            self.pos = 0;
        }
        let start = self.buf.len();
        self.buf.resize(start + 8192, 0);
        let n = loop {
            match self.inner.read(&mut self.buf[start..]) {
                Ok(n) => break n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) if e.kind() == io::ErrorKind::BrokenPipe => break 0,
                Err(e) => {
                    self.buf.truncate(start);
                    return Err(e);
                }
            }
        };
        self.buf.truncate(start + n);
        Ok(n)
    }

    fn pending(&self) -> &[u8] {
        &self.buf[self.pos..]
    }

    /// Up to and including the blank line; returned without it.
    fn read_head(&mut self) -> io::Result<Vec<u8>> {
        let mut searched = 0;
        loop {
            if let Some(i) = find(&self.pending()[searched..], b"\r\n\r\n") {
                let end = self.pos + searched + i;
                let head = self.buf[self.pos..end].to_vec();
                self.pos = end + 4;
                return Ok(head);
            }
            searched = self.pending().len().saturating_sub(3);
            if self.pending().len() > MAX_HEAD {
                return Err(malformed("the head is too long"));
            }
            if self.fill()? == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "the daemon closed the connection without answering",
                ));
            }
        }
    }

    fn read_exact_vec(&mut self, len: usize) -> io::Result<Vec<u8>> {
        let mut out = Vec::with_capacity(len.min(1 << 20));
        while out.len() < len {
            if self.pending().is_empty() && self.fill()? == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "the response ended early",
                ));
            }
            let take = self.pending().len().min(len - out.len());
            out.extend_from_slice(&self.buf[self.pos..self.pos + take]);
            self.pos += take;
        }
        Ok(out)
    }

    fn read_to_end_capped(&mut self, max: usize) -> io::Result<Vec<u8>> {
        let mut out = Vec::new();
        loop {
            out.extend_from_slice(self.pending());
            self.pos = self.buf.len();
            if out.len() > max {
                return Err(too_large(max));
            }
            if self.fill()? == 0 {
                return Ok(out);
            }
        }
    }

    fn read_line(&mut self) -> io::Result<Vec<u8>> {
        loop {
            if let Some(i) = find(self.pending(), b"\r\n") {
                let line = self.buf[self.pos..self.pos + i].to_vec();
                self.pos += i + 2;
                return Ok(line);
            }
            if self.pending().len() > MAX_HEAD {
                return Err(malformed("a chunk line is too long"));
            }
            if self.fill()? == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "the response ended early",
                ));
            }
        }
    }

    fn read_chunked(&mut self, max: usize) -> io::Result<Vec<u8>> {
        let mut out = Vec::new();
        loop {
            let line = self.read_line()?;
            let size_text = std::str::from_utf8(&line)
                .ok()
                .and_then(|l| l.split(';').next())
                .map(str::trim)
                .ok_or_else(|| malformed("a bad chunk size"))?;
            let size =
                usize::from_str_radix(size_text, 16).map_err(|_| malformed("a bad chunk size"))?;
            if size == 0 {
                // Trailers, up to the blank line.
                while !self.read_line()?.is_empty() {}
                return Ok(out);
            }
            if out.len() + size > max {
                return Err(too_large(max));
            }
            out.extend(self.read_exact_vec(size)?);
            if !self.read_line()?.is_empty() {
                return Err(malformed("a chunk without its line end"));
            }
        }
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Percent-encodes a path segment or query value: everything but `A-Z a-z 0-9 - . _ ~`.
#[must_use]
pub fn encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for b in value.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(raw: &[u8]) -> io::Result<Response> {
        read_response(&mut &raw[..], false, 1024)
    }

    /// Returns its bytes one at a time, then fails as a finished Windows pipe does.
    struct Trickle<'a>(&'a [u8]);
    impl Read for Trickle<'_> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            match self.0.split_first() {
                Some((b, rest)) if !buf.is_empty() => {
                    buf[0] = *b;
                    self.0 = rest;
                    Ok(1)
                }
                _ => Err(io::ErrorKind::BrokenPipe.into()),
            }
        }
    }

    #[test]
    fn content_length() {
        let r = read(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}extra").unwrap();
        assert_eq!(r.status, 200);
        assert_eq!(r.body, b"{}");
    }

    #[test]
    fn chunked_with_extensions_and_trailers() {
        let raw = b"HTTP/1.1 201 Created\r\nTransfer-Encoding: chunked\r\n\r\n3;x=y\r\nabc\r\nA\r\n0123456789\r\n0\r\nX-T: 1\r\n\r\n";
        let r = read(raw).unwrap();
        assert_eq!(r.status, 201);
        assert_eq!(r.body, b"abc0123456789");
    }

    #[test]
    fn to_the_end_of_the_stream_and_a_broken_pipe_is_the_end() {
        let raw = b"HTTP/1.0 202 Accepted\r\nX: y\r\n\r\nhello";
        let r = read_response(&mut Trickle(raw), false, 1024).unwrap();
        assert_eq!((r.status, r.body.as_slice()), (202, &b"hello"[..]));
    }

    #[test]
    fn interim_responses_are_skipped_and_204_has_no_body() {
        let r = read(b"HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 204 No Content\r\n\r\n").unwrap();
        assert_eq!(r.status, 204);
        assert!(r.body.is_empty());
    }

    #[test]
    fn bad_and_oversized_responses_fail() {
        assert!(read(b"").is_err());
        assert!(read(b"SSH-2.0-OpenSSH\r\n\r\n").is_err());
        assert!(read(b"HTTP/1.1 2000 OK\r\n\r\n").is_err());
        assert!(read(b"HTTP/1.1 200 OK\r\nContent-Length: 5000\r\n\r\n").is_err());
        assert!(read(b"HTTP/1.1 200 OK\r\nContent-Length: 9\r\n\r\nshort").is_err());
        let big = [b"HTTP/1.1 200 OK\r\n\r\n".as_slice(), &[b'x'; 2000]].concat();
        assert!(read(&big).is_err());
    }

    #[test]
    fn requests_carry_the_token_only_in_the_header() {
        let req = Request {
            method: "POST",
            target: "/v1/tasks/PAP-1/move",
            host: "localhost",
            token: Some("tok"),
            body: Some(br#"{"to":"review"}"#),
        };
        let text = String::from_utf8(req.to_bytes()).unwrap();
        assert!(text.starts_with("POST /v1/tasks/PAP-1/move HTTP/1.1\r\nHost: localhost\r\n"));
        assert!(text.contains("\r\nAuthorization: Bearer tok\r\n"));
        assert!(text.contains("\r\nContent-Length: 15\r\n"));
        assert!(text.ends_with("\r\n\r\n{\"to\":\"review\"}"));

        let get = Request {
            method: "GET",
            target: "/v1/host/info",
            host: "localhost",
            token: None,
            body: None,
        };
        let text = String::from_utf8(get.to_bytes()).unwrap();
        assert!(!text.contains("Authorization"));
        assert!(!text.contains("Content-Length"));
    }

    #[test]
    fn encoding() {
        assert_eq!(encode("PAP-1"), "PAP-1");
        assert_eq!(encode("a/b?c d&é"), "a%2Fb%3Fc%20d%26%C3%A9");
    }
}
