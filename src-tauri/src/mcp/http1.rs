//! Minimal HTTP/1.1 request/response layer for the embedded MCP server
//! (tauri-free, testable).
//!
//! WHY THIS EXISTS: `Cargo.toml` is frozen and contains no HTTP server crate
//! (no hyper/axum) and none of the `http`/`http-body` façade crates as direct
//! dependencies, so rmcp's `StreamableHttpService` tower layer cannot be
//! polled from this crate. Instead the streamable-HTTP endpoint is served by
//! this deliberately small HTTP/1.1 implementation (loopback only, JSON
//! bodies, keep-alive) and MCP semantics are provided by rmcp's tested
//! service core (`rmcp::serve_server`) — see `server.rs`.
//!
//! Scope (all this server ever needs):
//! * Requests: request-line + headers + `Content-Length` body. Chunked
//!   transfer encoding is rejected (`411`) — MCP SDK clients send sized JSON
//!   bodies.
//! * Responses: sized bodies, `HTTP/1.1`, keep-alive by default.
//! * Hard limits: 32 KiB head (enforced mid-line, before buffering), 4 MiB
//!   body.

use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Maximum accepted request head (request line + headers).
pub const MAX_HEAD_BYTES: usize = 32 * 1024;
/// Maximum accepted request body.
pub const MAX_BODY_BYTES: usize = 4 * 1024 * 1024;

/// One parsed HTTP request.
#[derive(Debug)]
pub struct Request {
    pub method: String,
    /// Request target as sent (path + optional query).
    pub target: String,
    /// Header names lowercased; values trimmed.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Request {
    /// First header with this (lowercase) name.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }

    /// Path without the query string.
    pub fn path(&self) -> &str {
        self.target.split('?').next().unwrap_or(&self.target)
    }

    /// True when the client asked to close the connection after this request.
    pub fn wants_close(&self) -> bool {
        self.header("connection")
            .is_some_and(|v| v.to_ascii_lowercase().contains("close"))
    }
}

/// Parse failure → the HTTP status the caller should answer with.
#[derive(Debug, PartialEq, Eq)]
pub struct ParseError {
    pub status: u16,
    pub message: &'static str,
}

impl ParseError {
    const fn new(status: u16, message: &'static str) -> Self {
        Self { status, message }
    }
}

/// Read one CRLF/LF-terminated line, charging every byte against `budget`.
/// Fails with 431 the moment the budget would be exceeded — even mid-line —
/// so a client that never sends a newline cannot make us buffer unboundedly
/// (pre-auth OOM). Returns the number of bytes appended (0 = clean EOF
/// before any byte).
async fn read_line_bounded<R: AsyncBufRead + Unpin>(
    reader: &mut R,
    line: &mut Vec<u8>,
    budget: &mut usize,
) -> Result<usize, ParseError> {
    let start = line.len();
    loop {
        let buf = reader
            .fill_buf()
            .await
            .map_err(|_| ParseError::new(400, "read error"))?;
        if buf.is_empty() {
            return Ok(line.len() - start); // EOF
        }
        match buf.iter().position(|&b| b == b'\n') {
            Some(pos) => {
                let take = pos + 1;
                if take > *budget {
                    return Err(ParseError::new(431, "request head too large"));
                }
                line.extend_from_slice(&buf[..take]);
                reader.consume(take);
                *budget -= take;
                return Ok(line.len() - start);
            }
            None => {
                // No newline yet: the full line needs at least one byte more
                // than what is buffered, so a buffer that already fills the
                // remaining budget can never complete within it.
                if buf.len() >= *budget {
                    return Err(ParseError::new(431, "request head too large"));
                }
                let take = buf.len();
                line.extend_from_slice(buf);
                reader.consume(take);
                *budget -= take;
            }
        }
    }
}

/// Read one request. `Ok(None)` = clean EOF before any byte (keep-alive
/// connection closed by the client).
pub async fn read_request<R: AsyncBufRead + Unpin>(
    reader: &mut R,
) -> Result<Option<Request>, ParseError> {
    let mut line = Vec::with_capacity(256);
    let mut budget = MAX_HEAD_BYTES;

    // -- request line -------------------------------------------------------
    let n = read_line_bounded(reader, &mut line, &mut budget).await?;
    if n == 0 {
        return Ok(None);
    }
    let request_line = String::from_utf8(line.clone())
        .map_err(|_| ParseError::new(400, "request line is not UTF-8"))?;
    let request_line = request_line.trim_end();
    let mut parts = request_line.split_ascii_whitespace();
    let (method, target, version) = match (parts.next(), parts.next(), parts.next()) {
        (Some(m), Some(t), Some(v)) => (m.to_string(), t.to_string(), v),
        _ => return Err(ParseError::new(400, "malformed request line")),
    };
    if !version.starts_with("HTTP/1.") {
        return Err(ParseError::new(505, "HTTP version not supported"));
    }

    // -- headers ------------------------------------------------------------
    let mut headers = Vec::with_capacity(16);
    loop {
        line.clear();
        let n = read_line_bounded(reader, &mut line, &mut budget).await?;
        if n == 0 {
            return Err(ParseError::new(400, "unexpected EOF in headers"));
        }
        let raw = std::str::from_utf8(&line)
            .map_err(|_| ParseError::new(400, "header is not UTF-8"))?
            .trim_end_matches(['\r', '\n']);
        if raw.is_empty() {
            break;
        }
        let Some((name, value)) = raw.split_once(':') else {
            return Err(ParseError::new(400, "malformed header"));
        };
        headers.push((name.trim().to_ascii_lowercase(), value.trim().to_string()));
    }

    let req = Request { method, target, headers, body: Vec::new() };

    // -- body ---------------------------------------------------------------
    if req
        .header("transfer-encoding")
        .is_some_and(|v| !v.eq_ignore_ascii_case("identity"))
    {
        return Err(ParseError::new(411, "chunked bodies not supported; send Content-Length"));
    }
    let mut lengths = req.headers.iter().filter(|(n, _)| n == "content-length");
    let len = match lengths.next() {
        None => 0usize,
        Some((_, v)) => {
            // Duplicate Content-Length headers are a request-smuggling vector
            // (leftover bytes would be parsed as a pipelined request): reject.
            if lengths.next().is_some() {
                return Err(ParseError::new(400, "duplicate Content-Length"));
            }
            v.parse::<usize>()
                .map_err(|_| ParseError::new(400, "bad Content-Length"))?
        }
    };
    if len > MAX_BODY_BYTES {
        return Err(ParseError::new(413, "body too large"));
    }
    let mut req = req;
    if len > 0 {
        req.body = vec![0u8; len];
        reader
            .read_exact(&mut req.body)
            .await
            .map_err(|_| ParseError::new(400, "body shorter than Content-Length"))?;
    }
    Ok(Some(req))
}

/// One response to serialize.
#[derive(Debug)]
pub struct Response {
    pub status: u16,
    pub reason: &'static str,
    /// Extra headers (Content-Length/Connection are added by `write`).
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Response {
    pub fn new(status: u16, reason: &'static str) -> Self {
        Self { status, reason, headers: Vec::new(), body: Vec::new() }
    }

    pub fn with_header(mut self, name: &str, value: impl Into<String>) -> Self {
        self.headers.push((name.to_string(), value.into()));
        self
    }

    pub fn text(status: u16, reason: &'static str, body: impl Into<String>) -> Self {
        let mut r = Self::new(status, reason).with_header("content-type", "text/plain");
        r.body = body.into().into_bytes();
        r
    }

    pub fn json(status: u16, reason: &'static str, value: &serde_json::Value) -> Self {
        let mut r = Self::new(status, reason).with_header("content-type", "application/json");
        r.body = value.to_string().into_bytes();
        r
    }

    pub async fn write<W: AsyncWrite + Unpin>(
        &self,
        writer: &mut W,
        close: bool,
    ) -> std::io::Result<()> {
        let mut head = format!("HTTP/1.1 {} {}\r\n", self.status, self.reason);
        for (n, v) in &self.headers {
            head.push_str(n);
            head.push_str(": ");
            head.push_str(v);
            head.push_str("\r\n");
        }
        head.push_str(&format!("content-length: {}\r\n", self.body.len()));
        head.push_str(if close { "connection: close\r\n" } else { "connection: keep-alive\r\n" });
        head.push_str("\r\n");
        writer.write_all(head.as_bytes()).await?;
        writer.write_all(&self.body).await?;
        writer.flush().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn parse(input: &[u8]) -> Result<Option<Request>, ParseError> {
        let mut reader = tokio::io::BufReader::new(input);
        read_request(&mut reader).await
    }

    #[tokio::test]
    async fn parses_post_with_body_and_headers() {
        let raw = b"POST /mcp HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer abc\r\nContent-Length: 4\r\n\r\n{\"a\"";
        let req = parse(raw).await.unwrap().unwrap();
        assert_eq!(req.method, "POST");
        assert_eq!(req.path(), "/mcp");
        assert_eq!(req.header("authorization"), Some("Bearer abc"));
        assert_eq!(req.body, b"{\"a\"");
        assert!(!req.wants_close());
    }

    #[tokio::test]
    async fn eof_before_request_is_none_and_truncated_body_errors() {
        assert!(parse(b"").await.unwrap().is_none());
        let err = parse(b"POST / HTTP/1.1\r\nContent-Length: 10\r\n\r\nabc")
            .await
            .unwrap_err();
        assert_eq!(err.status, 400);
    }

    #[tokio::test]
    async fn rejects_chunked_oversized_and_garbage() {
        let err = parse(b"POST / HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n")
            .await
            .unwrap_err();
        assert_eq!(err.status, 411);
        let err = parse(
            format!("POST / HTTP/1.1\r\nContent-Length: {}\r\n\r\n", MAX_BODY_BYTES + 1).as_bytes(),
        )
        .await
        .unwrap_err();
        assert_eq!(err.status, 413);
        assert_eq!(parse(b"garbage\r\n\r\n").await.unwrap_err().status, 400);
    }

    #[tokio::test]
    async fn oversized_line_without_newline_aborts_early() {
        // Request line larger than the whole head budget, no newline ever
        // sent — must fail with 431 instead of buffering forever.
        let big = vec![b'A'; MAX_HEAD_BYTES + 1];
        assert_eq!(parse(&big).await.unwrap_err().status, 431);

        // Same for a header line after a valid request line.
        let mut raw = b"POST / HTTP/1.1\r\nx-filler: ".to_vec();
        raw.extend(std::iter::repeat(b'A').take(MAX_HEAD_BYTES + 1));
        assert_eq!(parse(&raw).await.unwrap_err().status, 431);

        // Many valid header lines summing past the budget still 431.
        let mut raw = b"POST / HTTP/1.1\r\n".to_vec();
        for i in 0..2048 {
            raw.extend(format!("x-h{i}: {}\r\n", "v".repeat(64)).into_bytes());
        }
        raw.extend(b"\r\n");
        assert_eq!(parse(&raw).await.unwrap_err().status, 431);
    }

    #[tokio::test]
    async fn rejects_duplicate_content_length() {
        let raw = b"POST / HTTP/1.1\r\nContent-Length: 4\r\nContent-Length: 2\r\n\r\nabcd";
        let err = parse(raw).await.unwrap_err();
        assert_eq!(err.status, 400);
        assert!(err.message.contains("duplicate"), "{}", err.message);
        // Identical duplicates are rejected too — no leftover bytes may ever
        // be reinterpreted as a pipelined request.
        let raw = b"POST / HTTP/1.1\r\nContent-Length: 4\r\nContent-Length: 4\r\n\r\nabcd";
        assert_eq!(parse(raw).await.unwrap_err().status, 400);
    }

    #[tokio::test]
    async fn response_writes_status_headers_and_length() {
        let mut out = Vec::new();
        Response::json(200, "OK", &serde_json::json!({"ok": true}))
            .with_header("mcp-session-id", "s1")
            .write(&mut out, false)
            .await
            .unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(text.contains("mcp-session-id: s1\r\n"));
        assert!(text.contains("content-length: 11\r\n"));
        assert!(text.ends_with("{\"ok\":true}"));
    }
}
