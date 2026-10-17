//! The application's HTTP request/response contract.
//!
//! The wire server is `axum` (see `crate::http`): one listener serves both the
//! existing route table — through the synchronous [`Handler`] trait, bridged
//! with `spawn_blocking` at the axum boundary — and the native async job API.
//!
//! These two plain structs deliberately stay the internal contract: every
//! existing route method compiles unchanged, and the `Range` headers built in
//! `app::stream_file_range` (`206` / `Content-Range` / `Accept-Ranges` / `416`)
//! survive the port byte-for-byte, so seeking in the player keeps working.

use std::collections::HashMap;

/// Application request handler (implemented by the app state).
pub trait Handler: Send + Sync {
    fn handle(&self, req: &Request) -> Response;
}

pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Response {
    pub fn bytes(status: u16, content_type: &str, body: Vec<u8>) -> Self {
        Response {
            status,
            headers: vec![("Content-Type".into(), content_type.into())],
            body,
        }
    }
    pub fn text(status: u16, content_type: &str, body: impl Into<String>) -> Self {
        Self::bytes(status, content_type, body.into().into_bytes())
    }
    pub fn not_found() -> Self {
        Self::text(404, "text/plain; charset=utf-8", "Not Found")
    }
    pub fn error(status: u16, msg: &str) -> Self {
        Self::text(status, "application/json; charset=utf-8", format!("{{\"error\": {}}}", serde_json::to_string(msg).unwrap()))
    }
}

pub struct Request {
    pub method: String,
    pub path: String,
    pub query: HashMap<String, String>,
    pub headers: HashMap<String, String>,
    pub body: Vec<u8>,
}

pub fn parse_query(q: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for pair in q.split('&').filter(|s| !s.is_empty()) {
        if let Some((k, v)) = pair.split_once('=') {
            map.insert(url_decode(k), url_decode(v));
        }
    }
    map
}

pub fn url_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("20");
                if let Ok(v) = u8::from_str_radix(hex, 16) {
                    out.push(v);
                    i += 3;
                    continue;
                }
                out.push(b'%');
                i += 1;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn decodes_path_encoding() {
        assert_eq!(url_decode("a%20b+c"), "a b c");
    }

    #[test]
    fn parses_query_into_decoded_pairs() {
        let q = parse_query("force=1&path=%2Ftmp%2Fa+b.mp4&empty=");
        assert_eq!(q.get("force").map(String::as_str), Some("1"));
        assert_eq!(q.get("path").map(String::as_str), Some("/tmp/a b.mp4"));
        assert_eq!(q.get("empty").map(String::as_str), Some(""));
    }
}
