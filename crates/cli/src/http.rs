//! Just enough HTTP/1.1: one request per connection (`Connection: close`), responses framed by
//! `Content-Length`, chunked encoding, or the end of the stream.

use std::io::{self, Read, Write};

/// Longest response head accepted.
const MAX_HEAD: usize = 64 * 1024;

/// A request to send. Its `Debug` output never shows the token.
#[derive(Clone, Copy)]
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

impl std::fmt::Debug for Request<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Request")
            .field("method", &self.method)
            .field("target", &self.target)
            .field("host", &self.host)
            .field("token", &self.token.map(|_| "<redacted>"))
            .field("body_len", &self.body.map(<[u8]>::len))
            .finish()
    }
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

/// Reads one response, skipping any interim (1xx) ones. Besides the body cap, everything read
/// (heads, interim responses, chunk lines, trailers) counts against `max_body + MAX_HEAD`.
///
/// # Errors
/// I/O errors, a malformed response, a body over `max_body`, or more bytes than the total cap.
pub fn read_response<R: Read>(
    stream: &mut R,
    head_only: bool,
    max_body: usize,
) -> io::Result<Response> {
    let mut reader = Buffered::new(stream, max_body.saturating_add(MAX_HEAD));
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

/// A small read buffer over the stream that reads at most `limit` bytes in all. The end of the
/// stream, including a broken pipe (how a Windows pipe ends), reads as 0.
struct Buffered<'a, R> {
    inner: &'a mut R,
    /// Storage; only `pos..end` holds unconsumed bytes. It grows, and is never cleared, so a
    /// server sending one byte at a time costs no more than one sending many.
    buf: Vec<u8>,
    pos: usize,
    end: usize,
    limit: usize,
    /// How many more bytes may be read from the stream.
    remaining: usize,
}

impl<'a, R: Read> Buffered<'a, R> {
    fn new(inner: &'a mut R, limit: usize) -> Self {
        Self {
            inner,
            buf: Vec::new(),
            pos: 0,
            end: 0,
            limit,
            remaining: limit,
        }
    }

    /// Moves the unconsumed bytes to the front and reads more after them. Returns how many
    /// bytes came. With the budget spent it still reads one byte, to tell the end of the stream
    /// from a response that is too large.
    fn fill(&mut self) -> io::Result<usize> {
        if self.pos > 0 {
            self.buf.copy_within(self.pos..self.end, 0);
            self.end -= self.pos;
            self.pos = 0;
        }
        let want = self.remaining.clamp(1, 8192);
        if self.buf.len() < self.end + want {
            self.buf.resize(self.end + want, 0);
        }
        let n = loop {
            match self.inner.read(&mut self.buf[self.end..self.end + want]) {
                Ok(n) => break n.min(want),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) if e.kind() == io::ErrorKind::BrokenPipe => break 0,
                Err(e) => return Err(e),
            }
        };
        self.end += n;
        self.remaining = self
            .remaining
            .checked_sub(n)
            .ok_or_else(|| too_large(self.limit))?;
        Ok(n)
    }

    fn pending(&self) -> &[u8] {
        &self.buf[self.pos..self.end]
    }

    /// The bytes up to `delimiter` (consumed with it), at most `max` of them. The search resumes
    /// where the last one stopped, so a line that trickles in byte by byte stays linear.
    fn read_until(&mut self, delimiter: &[u8], max: usize, what: &str) -> io::Result<Vec<u8>> {
        let mut searched = 0;
        loop {
            if let Some(i) = find(&self.pending()[searched..], delimiter) {
                let end = self.pos + searched + i;
                let found = self.buf[self.pos..end].to_vec();
                self.pos = end + delimiter.len();
                return Ok(found);
            }
            searched = self
                .pending()
                .len()
                .saturating_sub(delimiter.len().saturating_sub(1));
            if self.pending().len() > max {
                return Err(malformed(&format!("{what} is too long")));
            }
            if self.fill()? == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "the response ended early",
                ));
            }
        }
    }

    /// Up to and including the blank line; returned without it.
    fn read_head(&mut self) -> io::Result<Vec<u8>> {
        self.read_until(b"\r\n\r\n", MAX_HEAD, "the head")
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
            self.pos = self.end;
            if out.len() > max {
                return Err(too_large(max));
            }
            if self.fill()? == 0 {
                return Ok(out);
            }
        }
    }

    fn read_line(&mut self) -> io::Result<Vec<u8>> {
        self.read_until(b"\r\n", MAX_HEAD, "a chunk line")
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
            // `out.len() <= max` always holds, so this cannot overflow.
            if size > max - out.len() {
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

    /// Sends `start`, then `repeat` forever, counting what it hands out.
    struct Endless {
        bytes: Vec<u8>,
        start: usize,
        pos: usize,
        served: usize,
    }

    impl Endless {
        fn new(start: &[u8], repeat: &[u8]) -> Self {
            Self {
                bytes: [start, repeat].concat(),
                start: start.len(),
                pos: 0,
                served: 0,
            }
        }
    }

    impl Read for Endless {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let n = buf.len().min(self.bytes.len() - self.pos);
            buf[..n].copy_from_slice(&self.bytes[self.pos..self.pos + n]);
            self.pos += n;
            if self.pos == self.bytes.len() {
                self.pos = self.start;
            }
            self.served += n;
            Ok(n)
        }
    }

    /// Reads with `max_body` 1024, so at most 1024 + 64 KiB (+1 to see the end) in all.
    fn read_endless(stream: &mut Endless) -> io::Error {
        let err = read_response(stream, false, 1024).unwrap_err();
        assert!(
            stream.served <= 1024 + MAX_HEAD + 1,
            "read {}",
            stream.served
        );
        err
    }

    #[test]
    fn a_huge_chunk_size_is_refused_without_overflow() {
        let raw = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n1\r\nx\r\nffffffffffffffff\r\nyyyy";
        let err = read(raw).unwrap_err();
        assert!(err.to_string().contains("larger than"), "{err}");
        let raw = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n1ffffffffffffffff\r\n";
        assert!(read(raw).is_err());
    }

    #[test]
    fn everything_read_counts_against_the_cap() {
        // Tiny chunks with long extensions: the body stays small, the response does not.
        let ext = [b"1;e=".as_slice(), &[b'a'; 1000], b"\r\nx\r\n"].concat();
        let mut stream = Endless::new(
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n",
            &ext,
        );
        let err = read_endless(&mut stream);
        assert!(err.to_string().contains("larger than"), "{err}");

        // Interim responses that never end.
        let mut stream = Endless::new(b"", b"HTTP/1.1 100 Continue\r\n\r\n");
        let err = read_endless(&mut stream);
        assert!(err.to_string().contains("larger than"), "{err}");

        // Trailers that never end.
        let mut stream = Endless::new(
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n0\r\n",
            b"X-T: 1\r\n",
        );
        read_endless(&mut stream);

        // One endless header line.
        let mut stream = Endless::new(b"HTTP/1.1 200 OK\r\nX: ", b"a");
        read_endless(&mut stream);
    }

    #[test]
    fn chunk_lines_split_across_reads() {
        let raw = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n1a;name=value\r\nabcdefghijklmnopqrstuvwxyz\r\n0\r\n\r\n";
        let r = read_response(&mut Trickle(raw), false, 1024).unwrap();
        assert_eq!(r.body, b"abcdefghijklmnopqrstuvwxyz");
    }

    #[test]
    fn a_response_exactly_at_the_cap_is_fine() {
        let head = b"HTTP/1.1 200 OK\r\n\r\n";
        let raw = [head.as_slice(), &[b'x'; 1024][..]].concat();
        let limit = head.len() + 1024;
        let mut reader = &raw[..];
        let mut buffered = Buffered::new(&mut reader, limit);
        buffered.read_head().unwrap();
        assert_eq!(buffered.read_to_end_capped(1024).unwrap().len(), 1024);
    }

    #[test]
    fn debug_never_shows_the_token() {
        let req = Request {
            method: "GET",
            target: "/v1/me",
            host: "localhost",
            token: Some("pca_secret"),
            body: Some(b"{}"),
        };
        let shown = format!("{req:?}");
        assert!(!shown.contains("pca_secret"), "{shown}");
        assert!(shown.contains(r#"token: Some("<redacted>")"#), "{shown}");
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
