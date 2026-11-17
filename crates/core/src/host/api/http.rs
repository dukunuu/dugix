//! HTTP/1.1 request parsing and response writing for `dugix host`.

use std::collections::HashMap;
use std::io;

use serde::Serialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use super::{BODY_CHUNK_BYTES, CORS_HEADERS, MAX_HEADER_BYTES};

#[derive(Debug)]
pub(super) struct RequestHead {
    pub(super) method: String,
    pub(super) target: String,
    pub(super) headers: HashMap<String, String>,
    pub(super) content_length: usize,
    /// Body bytes read with the headers, so pipelined data is not lost when auth
    /// rejects.
    pub(super) prefix: Vec<u8>,
}

#[derive(Debug)]
pub(super) struct Request {
    pub(super) method: String,
    pub(super) target: String,
    pub(super) headers: HashMap<String, String>,
    pub(super) body: Vec<u8>,
}

#[derive(Debug)]
pub(super) struct ReadError {
    pub(super) status: u16,
    pub(super) message: &'static str,
}

pub(super) async fn read_request_head(stream: &mut TcpStream) -> Result<RequestHead, ReadError> {
    let mut buffer = Vec::with_capacity(4096);
    let mut chunk = [0u8; 4096];
    let header_end = loop {
        if let Some(position) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
            break position;
        }
        let remaining = MAX_HEADER_BYTES.saturating_sub(buffer.len());
        if remaining == 0 {
            return Err(ReadError {
                status: 431,
                message: "request headers too large",
            });
        }
        let read_len = remaining.min(chunk.len());
        let n = stream
            .read(&mut chunk[..read_len])
            .await
            .map_err(|_| ReadError {
                status: 400,
                message: "could not read request",
            })?;
        if n == 0 {
            return Err(ReadError {
                status: 400,
                message: "incomplete request",
            });
        }
        buffer.extend_from_slice(&chunk[..n]);
    };
    let header_text = std::str::from_utf8(&buffer[..header_end]).map_err(|_| ReadError {
        status: 400,
        message: "request headers are not UTF-8",
    })?;
    let mut lines = header_text.lines();
    let request_line = lines.next().ok_or(ReadError {
        status: 400,
        message: "missing request line",
    })?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let target = parts.next().unwrap_or_default().to_string();
    if method.is_empty() || target.is_empty() {
        return Err(ReadError {
            status: 400,
            message: "bad request line",
        });
    }
    let mut headers = HashMap::new();
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            return Err(ReadError {
                status: 400,
                message: "bad request header",
            });
        };
        headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_string());
    }
    let content_length = headers
        .get("content-length")
        .map(|value| {
            value.parse::<usize>().map_err(|_| ReadError {
                status: 400,
                message: "invalid content length",
            })
        })
        .transpose()?
        .unwrap_or(0);
    if headers
        .get("transfer-encoding")
        .is_some_and(|value| !value.eq_ignore_ascii_case("identity"))
    {
        return Err(ReadError {
            status: 501,
            message: "chunked request bodies are not supported",
        });
    }
    let body_start = header_end + 4;
    let already = buffer.len().saturating_sub(body_start).min(content_length);
    let prefix = buffer[body_start..body_start + already].to_vec();
    Ok(RequestHead {
        method,
        target,
        headers,
        content_length,
        prefix,
    })
}

/// Read `content_length` body bytes, `prefix` having arrived with the headers;
/// authenticate before calling for protected routes.
pub(super) async fn read_body(
    stream: &mut TcpStream,
    prefix: &[u8],
    content_length: usize,
) -> Result<Vec<u8>, ReadError> {
    let incomplete = ReadError {
        status: 400,
        message: "incomplete request body",
    };
    let mut body = Vec::with_capacity(content_length.min(BODY_CHUNK_BYTES).max(prefix.len()));
    body.extend_from_slice(prefix);
    while body.len() < content_length {
        let start = body.len();
        let want = (content_length - start).min(BODY_CHUNK_BYTES);
        body.resize(start + want, 0);
        let n = stream
            .read(&mut body[start..])
            .await
            .map_err(|_| ReadError {
                status: incomplete.status,
                message: incomplete.message,
            })?;
        body.truncate(start + n);
        if n == 0 {
            return Err(incomplete);
        }
    }
    Ok(body)
}

pub(super) fn split_target(target: &str) -> (&str, &str) {
    target
        .split_once('?')
        .map_or((target, ""), |(path, query)| (path, query))
}

pub(super) fn hex(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

pub(super) async fn respond_text(
    stream: &mut TcpStream,
    status: u16,
    text: &str,
) -> io::Result<()> {
    respond_with_content_type(
        stream,
        status,
        "text/plain; charset=utf-8",
        text.as_bytes(),
        false,
    )
    .await
}

pub(super) async fn respond_error(
    stream: &mut TcpStream,
    status: u16,
    message: &str,
) -> io::Result<()> {
    respond_json(
        stream,
        status,
        &serde_json::json!({
            "error": { "message": message, "type": "invalid_request_error" }
        }),
    )
    .await
}

pub(super) async fn respond_json<T: Serialize>(
    stream: &mut TcpStream,
    status: u16,
    value: &T,
) -> io::Result<()> {
    let body = serde_json::to_vec(value)
        .unwrap_or_else(|_| b"{\"error\":{\"message\":\"serialization failed\"}}".to_vec());
    respond_with_content_type(
        stream,
        status,
        "application/json; charset=utf-8",
        &body,
        false,
    )
    .await
}

pub(super) async fn respond_empty(stream: &mut TcpStream, status: u16) -> io::Result<()> {
    respond_with_content_type(stream, status, "text/plain", &[], false).await
}

pub(super) async fn respond_with_content_type(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    body: &[u8],
    head: bool,
) -> io::Result<()> {
    respond_full(stream, status, content_type, "", body, head).await
}

/// `extra` is a pre-rendered block of additional `Name: value\r\n` lines.
pub(super) async fn respond_full(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    extra: &str,
    body: &[u8],
    head: bool,
) -> io::Result<()> {
    let header = format!(
        "HTTP/1.1 {status} {}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n{CORS_HEADERS}{extra}\r\n",
        reason(status),
        body.len(),
    );
    stream.write_all(header.as_bytes()).await?;
    if !head {
        stream.write_all(body).await?;
    }
    stream.shutdown().await
}

pub(super) fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        201 => "Created",
        202 => "Accepted",
        204 => "No Content",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        405 => "Method Not Allowed",
        413 => "Request Entity Too Large",
        422 => "Unprocessable Entity",
        431 => "Request Header Fields Too Large",
        500 => "Internal Server Error",
        501 => "Not Implemented",
        502 => "Bad Gateway",
        408 => "Request Timeout",
        409 => "Conflict",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        _ => "Error",
    }
}

/// Constant-time equality for the host token; the length difference is folded in.
pub(super) fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    let mut difference = left.len() ^ right.len();
    let length = left.len().max(right.len());
    for index in 0..length {
        let a = left.get(index).copied().unwrap_or(0);
        let b = right.get(index).copied().unwrap_or(0);
        difference |= usize::from(a ^ b);
    }
    difference == 0
}

/// Headers not relayed upstream; `accept-encoding` is excluded because proxies
/// forward the body verbatim while only relaying `Content-Type`.
pub(super) fn is_hop_by_hop(name: &str) -> bool {
    matches!(
        name,
        "host"
            | "authorization"
            | "content-length"
            | "transfer-encoding"
            | "accept-encoding"
            | "connection"
            | "keep-alive"
            | "expect"
            | "te"
            | "trailer"
            | "upgrade"
            | "proxy-authenticate"
            | "proxy-authorization"
    )
}

pub(super) fn query_without_token(query: &str) -> String {
    query
        .split('&')
        .filter(|pair| {
            let pair = *pair;
            let name = pair.split_once('=').map_or(pair, |(name, _)| name);
            !pair.is_empty() && !name.eq_ignore_ascii_case("token")
        })
        .collect::<Vec<_>>()
        .join("&")
}
