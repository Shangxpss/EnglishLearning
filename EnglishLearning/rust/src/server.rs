//! Minimal, dependency-free HTTP/1.1 server over `std::net`. Good enough for a
//! single-user local desktop app: serves static assets, JSON, and media files
//! with `Range` support. No tokio/hyper weight → tiny, single static binary.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;

/// Application request handler (implemented by the app state).
pub trait Handler: Send + Sync {
    fn handle(&self, req: &Request) -> Response;
}

pub struct Response {
    pub status: u16,
    pub reason: &'static str,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Response {
    pub fn bytes(status: u16, reason: &'static str, content_type: &str, body: Vec<u8>) -> Self {
        Response {
            status,
            reason,
            headers: vec![("Content-Type".into(), content_type.into())],
            body,
        }
    }
    pub fn text(status: u16, content_type: &str, body: impl Into<String>) -> Self {
        Self::bytes(status, "", content_type, body.into().into_bytes())
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

fn parse_query(q: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for pair in q.split('&').filter(|s| !s.is_empty()) {
        if let Some((k, v)) = pair.split_once('=') {
            map.insert(url_decode(k), url_decode(v));
        }
    }
    map
}

fn url_decode(s: &str) -> String {
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

fn read_request(stream: &mut TcpStream) -> std::io::Result<Option<Request>> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 8192];
    let header_end;
    loop {
        let n = stream.read(&mut tmp)?;
        if n == 0 {
            return Ok(None);
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(pos) = find_subsequence(&buf, b"\r\n\r\n") {
            header_end = pos + 4;
            break;
        }
        if buf.len() > 65536 {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "headers too large"));
        }
    }

    let head = String::from_utf8_lossy(&buf[..header_end]);
    let mut lines = head.split("\r\n");
    let request_line = lines.next().unwrap_or_default();
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("GET").to_string();
    let target = parts.next().unwrap_or("/").to_string();

    let mut headers = HashMap::new();
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }

    let (raw_path, query_str) = match target.split_once('?') {
        Some((p, q)) => (url_decode(p), q),
        None => (url_decode(&target), ""),
    };

    let content_length = headers
        .get("content-length")
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(0);
    let mut body = buf[header_end..].to_vec();
    while body.len() < content_length {
        let n = stream.read(&mut tmp)?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&tmp[..n]);
    }
    body.truncate(content_length);

    Ok(Some(Request {
        method,
        path: raw_path,
        query: parse_query(query_str),
        headers,
        body,
    }))
}

fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    haystack
        .windows(needle.len())
        .position(|w| w == needle)
}

fn write_response(stream: &mut TcpStream, res: &Response) -> std::io::Result<()> {
    let mut out = Vec::new();
    let status_line = format!("HTTP/1.1 {} {}\r\n", res.status, res.reason);
    out.extend_from_slice(status_line.as_bytes());
    for (k, v) in &res.headers {
        out.extend_from_slice(format!("{k}: {v}\r\n").as_bytes());
    }
    out.extend_from_slice(format!("Content-Length: {}\r\n", res.body.len()).as_bytes());
    out.extend_from_slice(b"Connection: close\r\n\r\n");
    out.extend_from_slice(&res.body);
    stream.write_all(&out)?;
    stream.flush()
}

/// Bind a listener, print the address, and spawn a connection-handling loop.
/// Returns the bound `SocketAddr` immediately so the caller can open the
/// browser. The loop keeps running on the spawned thread.
pub fn start(bind: &str, handler: Arc<dyn Handler + Send + Sync>) -> std::net::SocketAddr {
    let listener = TcpListener::bind(bind).unwrap_or_else(|e| {
        eprintln!("sentence-video: failed to bind {bind}: {e}");
        std::process::exit(1);
    });
    let addr = listener.local_addr().unwrap();
    println!("sentence-video: serving on http://{addr}/");

    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let handler = Arc::clone(&handler);
            std::thread::spawn(move || {
                if let Ok(Some(req)) = read_request(&mut stream) {
                    let res = handler.handle(&req);
                    let _ = write_response(&mut stream, &res);
                }
            });
        }
    });
    addr
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn decodes_path_encoding() {
        assert_eq!(url_decode("a%20b+c"), "a b c");
    }
}