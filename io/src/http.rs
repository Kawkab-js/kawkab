//! Zero-copy oriented HTTP/1.1 request head parsing via `httparse`.
//!
//! Header names and the request line are turned into owned strings for the JS bridge.
//! The wire buffer can be kept as `Arc<[u8]>` so the body is exposed without an extra
//! body-sized memcpy (see `arraybuffer_from_arc_slice` in `core`).

use std::collections::HashMap;
use std::io::{self, ErrorKind};
use std::sync::Arc;

use httparse::{Request, Status, EMPTY_HEADER};

/// Maximum header fields per request (same order of magnitude as hyper/httparse examples).
const MAX_HEADERS: usize = 64;

/// Parsed request line + headers; `header_len` is the byte offset in the wire buffer where the body starts.
#[derive(Debug)]
pub struct ParsedHead {
    pub method: String,
    pub path: String,
    pub header_len: usize,
    pub headers: HashMap<String, String>,
}

/// Parse available bytes; returns `Ok(None)` if more data is needed.
pub fn parse_request_head(buf: &[u8]) -> io::Result<Option<ParsedHead>> {
    let mut storage = [EMPTY_HEADER; MAX_HEADERS];
    let mut req = Request::new(&mut storage);
    let status = req
        .parse(buf)
        .map_err(|_| io::Error::new(ErrorKind::InvalidData, "http parse error"))?;

    match status {
        Status::Complete(header_len) => {
            let method = req.method.unwrap_or("GET").to_string();
            let path = req.path.unwrap_or("/").to_string();
            let mut headers = HashMap::new();
            for h in req.headers.iter() {
                if h.name.is_empty() {
                    continue;
                }
                let name = h.name.to_ascii_lowercase();
                let value = std::str::from_utf8(h.value)
                    .map_err(|_| io::Error::new(ErrorKind::InvalidData, "non-utf8 header"))?
                    .to_string();
                headers.insert(name, value);
            }
            Ok(Some(ParsedHead {
                method,
                path,
                header_len,
                headers,
            }))
        }
        Status::Partial => Ok(None),
    }
}

#[inline]
pub fn content_length(headers: &HashMap<String, String>) -> usize {
    headers
        .get("content-length")
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(0)
}

/// Own the full read buffer so a subslice can be handed to JS as a single `Arc` backing store.
#[inline]
pub fn arc_buffer(buf: Vec<u8>) -> Arc<[u8]> {
    Arc::from(buf.into_boxed_slice())
}
