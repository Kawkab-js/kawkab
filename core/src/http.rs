// Incremental HTTP/1.1 request parsing with `httparse`; header slices borrow the read buffer.

use std::{
    io::{self},
    sync::Arc,
};

use bytes::{BufMut, BytesMut};
use httparse::{Request, Status, EMPTY_HEADER};

/// Initial read buffer for incoming request headers.
/// 8 KiB covers > 99% of real-world HTTP requests without reallocation.
const HEADER_BUF_SIZE: usize = 8 * 1024;
/// Max headers to parse (httparse stack array — keep this small).
const MAX_HEADERS: usize = 64;
/// Max request line + headers size we'll accept (guards against header bombs).
const MAX_HEADER_BYTES: usize = 16 * 1024;

// ── Parsed HTTP request ───────────────────────────────────────────────────────

/// A zero-copy view of a parsed HTTP request.
///
/// All `&str` fields point into `raw_buf`, so their lifetime is tied to the
/// buffer. In practice the event loop immediately converts needed fields to
/// owned values before handing control to JS.
#[derive(Debug)]
pub struct ParsedRequest<'buf> {
    pub method: &'buf str,
    pub path: &'buf str,
    pub version: u8,
    pub headers: Vec<ParsedHeader<'buf>>,
    /// Byte offset where the body begins in the raw buffer.
    pub body_start: usize,
    /// Content-Length if provided, else None (chunked or unknown).
    pub content_length: Option<usize>,
}

#[derive(Debug)]
pub struct ParsedHeader<'buf> {
    pub name: &'buf str,
    pub value: &'buf str,
}

// ── HttpParser ────────────────────────────────────────────────────────────────

/// Stateful incremental HTTP/1.1 request parser.
///
/// Feed bytes in via `feed()`; call `parse()` to attempt extraction once
/// you think you have a complete header section.
pub struct HttpParser {
    buf: BytesMut,
    /// Bytes consumed in the last successful parse (header section length).
    consumed: usize,
}

impl HttpParser {
    pub fn new() -> Self {
        Self {
            buf: BytesMut::with_capacity(HEADER_BUF_SIZE),
            consumed: 0,
        }
    }

    /// Append raw bytes from the network into the internal buffer.
    ///
    /// No allocation if `bytes.len() <= remaining capacity`.
    #[inline]
    pub fn feed(&mut self, bytes: &[u8]) -> Result<(), io::Error> {
        if self.buf.len() + bytes.len() > MAX_HEADER_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "HTTP headers too large",
            ));
        }
        self.buf.put_slice(bytes);
        Ok(())
    }

    /// Attempt to parse a complete HTTP/1.1 request header section.
    ///
    /// Returns `Ok(None)` if the headers are not yet complete (need more data).
    /// Returns `Ok(Some(...))` on success.
    /// Returns `Err` on a malformed request.
    ///
    /// # Zero-copy guarantee
    /// All string slices in `ParsedRequest` point into `self.buf`. No
    /// String allocations occur until the caller converts them.
    pub fn parse(&mut self) -> Result<Option<ParsedRequest<'_>>, io::Error> {
        // Stack-allocate the header array — no heap involvement.
        let mut headers = [EMPTY_HEADER; MAX_HEADERS];
        let mut req = Request::new(&mut headers);

        match req.parse(&self.buf) {
            Ok(Status::Complete(header_len)) => {
                self.consumed = header_len;

                // Extract content-length from headers.
                let content_length = req.headers.iter().find_map(|h| {
                    if h.name.eq_ignore_ascii_case("content-length") {
                        std::str::from_utf8(h.value)
                            .ok()
                            .and_then(|s| s.trim().parse::<usize>().ok())
                    } else {
                        None
                    }
                });

                // Convert httparse headers to our type (all zero-copy borrows).
                let parsed_headers: Vec<ParsedHeader<'_>> = req
                    .headers
                    .iter()
                    .take_while(|h| !h.name.is_empty())
                    .map(|h| ParsedHeader {
                        name: h.name,
                        value: std::str::from_utf8(h.value).unwrap_or(""),
                    })
                    .collect();

                Ok(Some(ParsedRequest {
                    method: req.method.unwrap_or("GET"),
                    path: req.path.unwrap_or("/"),
                    version: req.version.unwrap_or(1),
                    headers: parsed_headers,
                    body_start: header_len,
                    content_length,
                }))
            }
            Ok(Status::Partial) => Ok(None),
            Err(e) => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("HTTP parse error: {e:?}"),
            )),
        }
    }

    /// Access the raw buffer (e.g. to read the body after headers parsed).
    #[inline]
    pub fn buffer(&self) -> &[u8] {
        &self.buf
    }

    /// Body bytes available in the buffer after header parsing.
    #[inline]
    pub fn body_bytes(&self) -> &[u8] {
        &self.buf[self.consumed..]
    }

    /// Reset for the next request (connection keep-alive).
    ///
    /// Retains capacity to avoid re-allocation on the next request.
    pub fn reset(&mut self) {
        self.buf.clear();
        self.consumed = 0;
    }
}

// ── HTTP response writer ───────────────────────────────────────────────────────

/// Minimal HTTP/1.1 response builder. Writes to a `Vec<u8>` that can be sent
/// via io_uring in a single write call.
pub struct ResponseBuilder {
    buf: Vec<u8>,
}

impl ResponseBuilder {
    pub fn new(status: u16, reason: &str) -> Self {
        let mut buf = Vec::with_capacity(256);
        let status_line = format!("HTTP/1.1 {status} {reason}\r\n");
        buf.extend_from_slice(status_line.as_bytes());
        Self { buf }
    }

    pub fn header(mut self, name: &str, value: &str) -> Self {
        self.buf.extend_from_slice(name.as_bytes());
        self.buf.extend_from_slice(b": ");
        self.buf.extend_from_slice(value.as_bytes());
        self.buf.extend_from_slice(b"\r\n");
        self
    }

    pub fn body(mut self, body: &[u8]) -> Self {
        // Inject Content-Length automatically.
        let cl = format!("Content-Length: {}\r\n\r\n", body.len());
        self.buf.extend_from_slice(cl.as_bytes());
        self.buf.extend_from_slice(body);
        self
    }

    /// Finalise and return the raw response bytes.
    pub fn finish(self) -> Vec<u8> {
        self.buf
    }

    /// Finalise as an `Arc<[u8]>` for zero-copy hand-off to io_uring.
    pub fn finish_arc(self) -> Arc<[u8]> {
        Arc::from(self.buf.as_slice())
    }
}
