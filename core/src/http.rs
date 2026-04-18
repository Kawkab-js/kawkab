use std::{
    io::{self},
    sync::Arc,
};

use bytes::{BufMut, BytesMut};
use httparse::{Request, Status, EMPTY_HEADER};

/// Default header read buffer size (8 KiB).
const HEADER_BUF_SIZE: usize = 8 * 1024;
/// Stack array size passed to httparse.
const MAX_HEADERS: usize = 64;
/// Max request-line + headers bytes accepted.
const MAX_HEADER_BYTES: usize = 16 * 1024;

/// Parsed request head; `&str` fields borrow the parser buffer until copied out.
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

/// Incremental HTTP/1.1 request-head parser (`feed` then `parse`).
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

    /// Append wire bytes (no alloc when capacity allows).
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

    /// Parse a complete header section, or `Ok(None)` if more bytes are needed.
    /// On success, slices in `ParsedRequest` reference `self.buf` (zero-copy until owned).
    pub fn parse(&mut self) -> Result<Option<ParsedRequest<'_>>, io::Error> {
        let mut headers = [EMPTY_HEADER; MAX_HEADERS];
        let mut req = Request::new(&mut headers);

        match req.parse(&self.buf) {
            Ok(Status::Complete(header_len)) => {
                self.consumed = header_len;

                let content_length = req.headers.iter().find_map(|h| {
                    if h.name.eq_ignore_ascii_case("content-length") {
                        std::str::from_utf8(h.value)
                            .ok()
                            .and_then(|s| s.trim().parse::<usize>().ok())
                    } else {
                        None
                    }
                });

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

    /// Full read buffer (headers + any body bytes already read).
    #[inline]
    pub fn buffer(&self) -> &[u8] {
        &self.buf
    }

    /// Body prefix already in `buf` after `body_start`.
    #[inline]
    pub fn body_bytes(&self) -> &[u8] {
        &self.buf[self.consumed..]
    }

    /// Clear state for the next request on a keep-alive connection (keeps capacity).
    pub fn reset(&mut self) {
        self.buf.clear();
        self.consumed = 0;
    }
}

/// Build a raw HTTP/1.1 response in a `Vec<u8>` (single write to the socket).
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
        let cl = format!("Content-Length: {}\r\n\r\n", body.len());
        self.buf.extend_from_slice(cl.as_bytes());
        self.buf.extend_from_slice(body);
        self
    }

    /// Consume into owned bytes.
    pub fn finish(self) -> Vec<u8> {
        self.buf
    }

    /// Like `finish` but `Arc` for sharing with I/O.
    pub fn finish_arc(self) -> Arc<[u8]> {
        Arc::from(self.buf.as_slice())
    }
}
