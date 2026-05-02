//! Minimal HTTP parsing and response creation.

use std::collections::HashMap;

/// Parsed HTTP request fields used by rsgit.
#[derive(Debug, Clone)]
pub struct Request {
    method: String,
    path: String,
    query: HashMap<String, String>,
    host: Option<String>,
}

impl Request {
    /// HTTP method token.
    pub fn method(&self) -> &str {
        &self.method
    }
    /// Decoded URL path.
    pub fn path(&self) -> &str {
        &self.path
    }
    /// Optional query value.
    pub fn query(&self, key: &str) -> Option<&str> {
        self.query.get(key).map(String::as_str)
    }
    /// Parsed Host header.
    pub fn host(&self) -> Option<&str> {
        self.host.as_deref()
    }
}

/// Parse a small HTTP/1.x request. The server closes every connection and only
/// needs the request line, query string, and Host header.
pub fn parse(raw: &str) -> Option<Request> {
    let first = raw.lines().next()?;
    let mut parts = first.split_whitespace();
    let method = parts.next()?.to_string();
    let target = parts.next()?;
    let _version = parts.next()?;

    let (path_part, query_part) = target.split_once('?').unwrap_or((target, ""));
    Some(Request {
        method,
        path: url_decode(path_part),
        query: parse_query(query_part),
        host: parse_host(raw),
    })
}

/// Build a text response.
pub fn response(status: u16, reason: &str, content_type: &str, body: &str) -> Vec<u8> {
    response_bytes(status, reason, content_type, body.as_bytes().to_vec())
}

/// Build a byte response with common security headers.
pub fn response_bytes(status: u16, reason: &str, content_type: &str, body: Vec<u8>) -> Vec<u8> {
    let headers = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\nX-Content-Type-Options: nosniff\r\nContent-Security-Policy: default-src 'none'; style-src 'unsafe-inline'; form-action 'self'; base-uri 'none'; frame-ancestors 'none'\r\nReferrer-Policy: no-referrer\r\nX-Frame-Options: DENY\r\n\r\n",
        body.len()
    );
    let mut out = headers.into_bytes();
    out.extend_from_slice(&body);
    out
}

fn parse_query(raw: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for pair in raw.split('&').filter(|p| !p.is_empty()) {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        out.insert(url_decode(k), url_decode(v));
    }
    out
}

fn parse_host(raw: &str) -> Option<String> {
    raw.lines().skip(1).find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.eq_ignore_ascii_case("host")
            .then(|| value.trim().to_string())
    })
}

fn url_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                if let (Some(a), Some(b)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                    out.push((a << 4) | b);
                    i += 3;
                } else {
                    out.push(bytes[i]);
                    i += 1;
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_path_query_and_host() {
        let req =
            parse("GET /repo/foo/tree?path=src%2Fmain.rs HTTP/1.1\r\nHost: localhost:8080\r\n\r\n")
                .unwrap();
        assert_eq!(req.method(), "GET");
        assert_eq!(req.path(), "/repo/foo/tree");
        assert_eq!(req.query("path"), Some("src/main.rs"));
        assert_eq!(req.host(), Some("localhost:8080"));
    }
}
