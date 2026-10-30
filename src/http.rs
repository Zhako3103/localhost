//! HTTP/1.1 primitives: incremental request parser, response builder, helpers.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

pub const MAX_HEADER_BYTES: usize = 16 * 1024;

// ---------------------------------------------------------------- helpers

pub fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() {
        return None;
    }
    hay.windows(needle.len()).position(|w| w == needle)
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Percent-decodes a string. Returns None on malformed escapes or invalid UTF-8.
pub fn url_decode(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            if i + 2 >= b.len() {
                return None;
            }
            let hi = hex_val(b[i + 1])?;
            let lo = hex_val(b[i + 2])?;
            out.push(hi * 16 + lo);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// Percent-encodes everything except unreserved characters (and '/' if asked).
pub fn pct_encode(s: &str, keep_slash: bool) -> String {
    let mut out = String::with_capacity(s.len());
    for &c in s.as_bytes() {
        let keep = c.is_ascii_alphanumeric()
            || c == b'-'
            || c == b'.'
            || c == b'_'
            || c == b'~'
            || (keep_slash && c == b'/');
        if keep {
            out.push(c as char);
        } else {
            out.push_str(&format!("%{:02X}", c));
        }
    }
    out
}

pub fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

/// Looks up a key in a query string (`a=1&b=2`), decoding `+` and `%XX`.
pub fn query_param(query: &str, key: &str) -> Option<String> {
    for pair in query.split('&') {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        if k == key {
            return url_decode(&v.replace('+', " "));
        }
    }
    None
}

pub fn mime_type(path: &Path) -> &'static str {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "html" | "htm" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" => "application/javascript; charset=utf-8",
        "json" => "application/json",
        "txt" | "md" | "log" => "text/plain; charset=utf-8",
        "xml" => "application/xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        "ico" => "image/x-icon",
        "webp" => "image/webp",
        "pdf" => "application/pdf",
        "zip" => "application/zip",
        "mp3" => "audio/mpeg",
        "mp4" => "video/mp4",
        "woff2" => "font/woff2",
        _ => "application/octet-stream",
    }
}

pub fn reason(code: u16) -> &'static str {
    match code {
        100 => "Continue",
        200 => "OK",
        201 => "Created",
        204 => "No Content",
        301 => "Moved Permanently",
        302 => "Found",
        303 => "See Other",
        307 => "Temporary Redirect",
        308 => "Permanent Redirect",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        408 => "Request Timeout",
        411 => "Length Required",
        413 => "Payload Too Large",
        414 => "URI Too Long",
        500 => "Internal Server Error",
        501 => "Not Implemented",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        _ => "Unknown",
    }
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// RFC 7231 IMF-fixdate.
pub fn http_date(secs: i64) -> String {
    const WD: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    const MO: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let wd = (days + 4).rem_euclid(7) as usize;
    let (y, m, d) = civil_from_days(days);
    format!(
        "{}, {:02} {} {} {:02}:{:02}:{:02} GMT",
        WD[wd],
        d,
        MO[(m - 1) as usize],
        y,
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

// ---------------------------------------------------------------- request

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Request {
    pub method: String,
    pub target: String,
    /// Percent-decoded path (no query).
    pub path: String,
    pub query: String,
    pub version: String,
    /// Header names are lower-cased.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Request {
    pub fn header(&self, name: &str) -> Option<&str> {
        let n = name.to_ascii_lowercase();
        self.headers
            .iter()
            .find(|(k, _)| *k == n)
            .map(|(_, v)| v.as_str())
    }

    pub fn keep_alive(&self) -> bool {
        let conn = self.header("connection").map(|v| v.to_ascii_lowercase());
        let conn = conn.as_deref().unwrap_or("");
        if self.version == "HTTP/1.0" {
            conn.contains("keep-alive")
        } else {
            !conn.contains("close")
        }
    }

    pub fn cookie(&self, name: &str) -> Option<String> {
        let h = self.header("cookie")?;
        for pair in h.split(';') {
            if let Some((k, v)) = pair.trim().split_once('=') {
                if k == name {
                    return Some(v.to_string());
                }
            }
        }
        None
    }
}

fn is_token(s: &str) -> bool {
    !s.is_empty()
        && s.bytes().all(|b| {
            b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b)
        })
}

/// Parses the request line and headers (without the trailing blank line).
pub fn parse_head(bytes: &[u8]) -> Result<Request, u16> {
    let text = std::str::from_utf8(bytes).map_err(|_| 400u16)?;
    let mut lines = text.split("\r\n");
    let line = lines.next().ok_or(400u16)?;

    let mut parts = line.split(' ');
    let method = parts.next().ok_or(400u16)?;
    let target = parts.next().ok_or(400u16)?;
    let version = parts.next().ok_or(400u16)?;
    if parts.next().is_some() {
        return Err(400);
    }
    if method.is_empty() || !method.bytes().all(|b| b.is_ascii_uppercase()) {
        return Err(400);
    }
    if version != "HTTP/1.1" && version != "HTTP/1.0" {
        return Err(400);
    }
    if !target.starts_with('/') || target.len() > 8192 {
        return Err(400);
    }

    let (raw_path, query) = match target.split_once('?') {
        Some((p, q)) => (p, q),
        None => (target, ""),
    };
    let path = url_decode(raw_path).ok_or(400u16)?;
    if path.contains('\0') {
        return Err(400);
    }

    let mut headers = Vec::new();
    for l in lines {
        if l.is_empty() {
            continue;
        }
        let (name, value) = l.split_once(':').ok_or(400u16)?;
        if !is_token(name) {
            return Err(400);
        }
        headers.push((name.to_ascii_lowercase(), value.trim().to_string()));
    }

    let req = Request {
        method: method.to_string(),
        target: target.to_string(),
        path,
        query: query.to_string(),
        version: version.to_string(),
        headers,
        body: Vec::new(),
    };

    if version == "HTTP/1.1" && req.header("host").is_none() {
        return Err(400);
    }
    if req.headers.iter().filter(|(k, _)| k == "content-length").count() > 1 {
        return Err(400);
    }
    Ok(req)
}

#[derive(Debug, PartialEq)]
pub enum ParseResult {
    NeedMore,
    Done(Request),
    Err(u16),
}

#[derive(Clone, Copy)]
enum ChunkPhase {
    Size,
    Data(usize),
    DataEnd,
    Trailer,
}

#[derive(Clone, Copy)]
enum Phase {
    Head,
    Length(usize),
    Chunked(ChunkPhase),
}

/// Incremental parser: feed bytes as they arrive, call `parse` until it yields
/// `Done`/`Err`. Bytes beyond one request stay buffered (pipelining).
pub struct RequestParser {
    buf: Vec<u8>,
    phase: Phase,
    req: Request,
    limit: usize,
    total: usize,
}

impl RequestParser {
    pub fn new() -> Self {
        Self {
            buf: Vec::new(),
            phase: Phase::Head,
            req: Request::default(),
            limit: 0,
            total: 0,
        }
    }

    pub fn feed(&mut self, data: &[u8]) {
        self.buf.extend_from_slice(data);
    }

    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    fn done(&mut self) -> ParseResult {
        self.phase = Phase::Head;
        ParseResult::Done(std::mem::take(&mut self.req))
    }

    /// `limit_for` returns the maximum body size for the parsed headers.
    pub fn parse(&mut self, limit_for: &dyn Fn(&Request) -> usize) -> ParseResult {
        loop {
            match self.phase {
                Phase::Head => {
                    while self.buf.starts_with(b"\r\n") {
                        self.buf.drain(..2);
                    }
                    if self.buf.is_empty() {
                        return ParseResult::NeedMore;
                    }
                    let end = match find(&self.buf, b"\r\n\r\n") {
                        Some(e) => e,
                        None => {
                            return if self.buf.len() > MAX_HEADER_BYTES {
                                ParseResult::Err(400)
                            } else {
                                ParseResult::NeedMore
                            };
                        }
                    };
                    if end > MAX_HEADER_BYTES {
                        return ParseResult::Err(400);
                    }
                    let req = match parse_head(&self.buf[..end]) {
                        Ok(r) => r,
                        Err(c) => return ParseResult::Err(c),
                    };
                    self.buf.drain(..end + 4);

                    let te = req
                        .header("transfer-encoding")
                        .map(|v| v.to_ascii_lowercase());
                    let cl = req.header("content-length").map(|v| v.to_string());
                    self.limit = limit_for(&req);
                    self.total = 0;
                    self.req = req;

                    if let Some(te) = te {
                        if cl.is_some() || te.trim() != "chunked" {
                            return ParseResult::Err(400);
                        }
                        self.phase = Phase::Chunked(ChunkPhase::Size);
                        continue;
                    }
                    match cl {
                        None => return self.done(),
                        Some(v) => {
                            let v = v.trim();
                            if v.is_empty() || !v.bytes().all(|b| b.is_ascii_digit()) {
                                return ParseResult::Err(400);
                            }
                            let n = match v.parse::<usize>() {
                                Ok(n) => n,
                                Err(_) => return ParseResult::Err(413),
                            };
                            if n > self.limit {
                                return ParseResult::Err(413);
                            }
                            if n == 0 {
                                return self.done();
                            }
                            self.phase = Phase::Length(n);
                        }
                    }
                }
                Phase::Length(n) => {
                    if self.buf.len() < n {
                        return ParseResult::NeedMore;
                    }
                    self.req.body = self.buf.drain(..n).collect();
                    return self.done();
                }
                Phase::Chunked(cp) => match cp {
                    ChunkPhase::Size => {
                        let pos = match find(&self.buf, b"\r\n") {
                            Some(p) => p,
                            None => {
                                return if self.buf.len() > 1024 {
                                    ParseResult::Err(400)
                                } else {
                                    ParseResult::NeedMore
                                };
                            }
                        };
                        let line = String::from_utf8_lossy(&self.buf[..pos]).to_string();
                        self.buf.drain(..pos + 2);
                        let hex = line.split(';').next().unwrap_or("").trim();
                        if hex.is_empty() || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
                            return ParseResult::Err(400);
                        }
                        let size = match usize::from_str_radix(hex, 16) {
                            Ok(s) => s,
                            Err(_) => return ParseResult::Err(413),
                        };
                        if size == 0 {
                            self.phase = Phase::Chunked(ChunkPhase::Trailer);
                        } else {
                            self.total = match self.total.checked_add(size) {
                                Some(t) => t,
                                None => return ParseResult::Err(413),
                            };
                            if self.total > self.limit {
                                return ParseResult::Err(413);
                            }
                            self.phase = Phase::Chunked(ChunkPhase::Data(size));
                        }
                    }
                    ChunkPhase::Data(n) => {
                        if self.buf.len() < n {
                            return ParseResult::NeedMore;
                        }
                        let chunk: Vec<u8> = self.buf.drain(..n).collect();
                        self.req.body.extend_from_slice(&chunk);
                        self.phase = Phase::Chunked(ChunkPhase::DataEnd);
                    }
                    ChunkPhase::DataEnd => {
                        if self.buf.len() < 2 {
                            return ParseResult::NeedMore;
                        }
                        if &self.buf[..2] != &b"\r\n"[..] {
                            return ParseResult::Err(400);
                        }
                        self.buf.drain(..2);
                        self.phase = Phase::Chunked(ChunkPhase::Size);
                    }
                    ChunkPhase::Trailer => {
                        let pos = match find(&self.buf, b"\r\n") {
                            Some(p) => p,
                            None => {
                                return if self.buf.len() > 8192 {
                                    ParseResult::Err(400)
                                } else {
                                    ParseResult::NeedMore
                                };
                            }
                        };
                        self.buf.drain(..pos + 2);
                        if pos == 0 {
                            return self.done();
                        }
                    }
                },
            }
        }
    }
}

// --------------------------------------------------------------- response

#[derive(Debug, Clone)]
pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Response {
    pub fn new(status: u16) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: Vec::new(),
        }
    }

    pub fn with_body(status: u16, content_type: &str, body: Vec<u8>) -> Self {
        let mut r = Self::new(status).header("Content-Type", content_type);
        r.body = body;
        r
    }

    /// Adds a header; CR/LF in the value are neutralised (header injection).
    pub fn header(mut self, name: &str, value: &str) -> Self {
        let clean: String = value
            .chars()
            .map(|c| if c == '\r' || c == '\n' { ' ' } else { c })
            .collect();
        self.headers.push((name.to_string(), clean));
        self
    }

    pub fn into_bytes(self, keep_alive: bool, head_only: bool) -> Vec<u8> {
        let no_body = self.status == 204 || self.status == 304 || self.status / 100 == 1;
        let mut out = Vec::with_capacity(self.body.len() + 256);
        out.extend_from_slice(
            format!("HTTP/1.1 {} {}\r\n", self.status, reason(self.status)).as_bytes(),
        );
        out.extend_from_slice(
            format!(
                "Date: {}\r\nServer: localhost-rs/1.0\r\n",
                http_date(now_secs())
            )
            .as_bytes(),
        );
        for (k, v) in &self.headers {
            out.extend_from_slice(k.as_bytes());
            out.extend_from_slice(b": ");
            out.extend_from_slice(v.as_bytes());
            out.extend_from_slice(b"\r\n");
        }
        if !no_body {
            out.extend_from_slice(format!("Content-Length: {}\r\n", self.body.len()).as_bytes());
        }
        out.extend_from_slice(if keep_alive {
            b"Connection: keep-alive\r\n"
        } else {
            b"Connection: close\r\n"
        });
        out.extend_from_slice(b"\r\n");
        if !no_body && !head_only {
            out.extend_from_slice(&self.body);
        }
        out
    }
}

pub fn error_hint(code: u16) -> &'static str {
    match code {
        400 => "Сервер не смог разобрать запрос.",
        403 => "Доступ к этому ресурсу запрещён.",
        404 => "Такой страницы здесь нет.",
        405 => "Этот метод не разрешён для данного адреса.",
        408 => "Время ожидания запроса истекло.",
        413 => "Тело запроса больше допустимого размера.",
        500 => "Внутренняя ошибка сервера.",
        503 => "Сервис временно недоступен.",
        504 => "Скрипт не ответил вовремя.",
        _ => "",
    }
}

pub fn default_error_page(code: u16) -> Vec<u8> {
    let inner = format!(
        "<header class=\"hero\"><div class=\"status-badge\"><span class=\"pulse-dot red\"></span> Ошибка</div>\
         <h1><span>{code}</span> {reason}</h1><p class=\"subtitle\">{hint}</p></header>\
         <a class=\"btn-upload btn-link\" href=\"/\">На главную</a>\
         <p class=\"footnote\">localhost-rs/1.0</p>",
        code = code,
        reason = reason(code),
        hint = error_hint(code)
    );
    crate::page::page(&format!("{} {}", code, reason(code)), &inner).into_bytes()
}

// ------------------------------------------------------------------ tests

#[cfg(test)]
mod tests {
    use super::*;

    fn lim(_: &Request) -> usize {
        1024
    }

    fn parse_one(raw: &[u8]) -> ParseResult {
        let mut p = RequestParser::new();
        p.feed(raw);
        p.parse(&lim)
    }

    #[test]
    fn parses_get_with_query_and_headers() {
        match parse_one(b"GET /a%20b?x=1&y=2 HTTP/1.1\r\nHost: example\r\nX-Test:  v \r\n\r\n") {
            ParseResult::Done(r) => {
                assert_eq!(r.method, "GET");
                assert_eq!(r.path, "/a b");
                assert_eq!(r.query, "x=1&y=2");
                assert_eq!(r.header("HOST"), Some("example"));
                assert_eq!(r.header("x-test"), Some("v"));
                assert!(r.body.is_empty());
            }
            other => panic!("unexpected: {:?}", other),
        }
    }

    #[test]
    fn parses_post_with_content_length() {
        match parse_one(b"POST /u HTTP/1.1\r\nHost: h\r\nContent-Length: 5\r\n\r\nhello") {
            ParseResult::Done(r) => {
                assert_eq!(r.method, "POST");
                assert_eq!(r.body, b"hello");
            }
            other => panic!("unexpected: {:?}", other),
        }
    }

    #[test]
    fn parses_delete() {
        match parse_one(b"DELETE /f.txt HTTP/1.1\r\nHost: h\r\n\r\n") {
            ParseResult::Done(r) => assert_eq!(r.method, "DELETE"),
            other => panic!("unexpected: {:?}", other),
        }
    }

    #[test]
    fn body_arrives_in_pieces() {
        let mut p = RequestParser::new();
        p.feed(b"POST / HTTP/1.1\r\nHost: h\r\nContent-Le");
        assert_eq!(p.parse(&lim), ParseResult::NeedMore);
        p.feed(b"ngth: 4\r\n\r\nab");
        assert_eq!(p.parse(&lim), ParseResult::NeedMore);
        p.feed(b"cd");
        match p.parse(&lim) {
            ParseResult::Done(r) => assert_eq!(r.body, b"abcd"),
            other => panic!("unexpected: {:?}", other),
        }
    }

    #[test]
    fn parses_chunked_body() {
        let raw = b"POST / HTTP/1.1\r\nHost: h\r\nTransfer-Encoding: chunked\r\n\r\n\
                    5\r\nhello\r\n6;ext=1\r\n world\r\n0\r\nTrailer: x\r\n\r\n";
        match parse_one(raw) {
            ParseResult::Done(r) => assert_eq!(r.body, b"hello world"),
            other => panic!("unexpected: {:?}", other),
        }
    }

    #[test]
    fn chunked_byte_by_byte() {
        let raw = b"POST / HTTP/1.1\r\nHost: h\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n0\r\n\r\n";
        let mut p = RequestParser::new();
        let mut result = ParseResult::NeedMore;
        for b in raw.iter() {
            p.feed(&[*b]);
            result = p.parse(&lim);
            if result != ParseResult::NeedMore {
                break;
            }
        }
        match result {
            ParseResult::Done(r) => assert_eq!(r.body, b"abc"),
            other => panic!("unexpected: {:?}", other),
        }
    }

    #[test]
    fn pipelined_requests_stay_buffered() {
        let mut p = RequestParser::new();
        p.feed(b"GET /1 HTTP/1.1\r\nHost: h\r\n\r\nGET /2 HTTP/1.1\r\nHost: h\r\n\r\n");
        match p.parse(&lim) {
            ParseResult::Done(r) => assert_eq!(r.path, "/1"),
            other => panic!("unexpected: {:?}", other),
        }
        assert!(!p.is_empty());
        match p.parse(&lim) {
            ParseResult::Done(r) => assert_eq!(r.path, "/2"),
            other => panic!("unexpected: {:?}", other),
        }
    }

    #[test]
    fn content_length_over_limit_is_413() {
        assert_eq!(
            parse_one(b"POST / HTTP/1.1\r\nHost: h\r\nContent-Length: 2000\r\n\r\n"),
            ParseResult::Err(413)
        );
    }

    #[test]
    fn chunked_over_limit_is_413() {
        let raw = b"POST / HTTP/1.1\r\nHost: h\r\nTransfer-Encoding: chunked\r\n\r\n800\r\n";
        assert_eq!(parse_one(raw), ParseResult::Err(413));
    }

    #[test]
    fn malformed_requests_are_400() {
        assert_eq!(parse_one(b"GARBAGE\r\n\r\n"), ParseResult::Err(400));
        assert_eq!(parse_one(b"GET / HTTP/2.0\r\nHost: h\r\n\r\n"), ParseResult::Err(400));
        assert_eq!(parse_one(b"GET / HTTP/1.1\r\n\r\n"), ParseResult::Err(400)); // no Host
        assert_eq!(parse_one(b"get / HTTP/1.1\r\nHost: h\r\n\r\n"), ParseResult::Err(400));
        assert_eq!(parse_one(b"GET /%zz HTTP/1.1\r\nHost: h\r\n\r\n"), ParseResult::Err(400));
        assert_eq!(
            parse_one(b"POST / HTTP/1.1\r\nHost: h\r\nContent-Length: abc\r\n\r\n"),
            ParseResult::Err(400)
        );
        assert_eq!(
            parse_one(b"POST / HTTP/1.1\r\nHost: h\r\nContent-Length: 1\r\nTransfer-Encoding: chunked\r\n\r\n"),
            ParseResult::Err(400)
        );
        assert_eq!(
            parse_one(b"POST / HTTP/1.1\r\nHost: h\r\nTransfer-Encoding: chunked\r\n\r\nzz\r\n"),
            ParseResult::Err(400)
        );
    }

    #[test]
    fn oversized_headers_are_rejected() {
        let mut raw = b"GET / HTTP/1.1\r\nHost: h\r\nX: ".to_vec();
        raw.extend(std::iter::repeat(b'a').take(MAX_HEADER_BYTES + 10));
        assert_eq!(parse_one(&raw), ParseResult::Err(400));
    }

    #[test]
    fn keep_alive_rules() {
        let mut r = Request::default();
        r.version = "HTTP/1.1".into();
        assert!(r.keep_alive());
        r.headers.push(("connection".into(), "close".into()));
        assert!(!r.keep_alive());
        let mut r10 = Request::default();
        r10.version = "HTTP/1.0".into();
        assert!(!r10.keep_alive());
        r10.headers.push(("connection".into(), "Keep-Alive".into()));
        assert!(r10.keep_alive());
    }

    #[test]
    fn cookie_lookup() {
        let mut r = Request::default();
        r.headers.push(("cookie".into(), "a=1; SID=abc; b=2".into()));
        assert_eq!(r.cookie("SID"), Some("abc".to_string()));
        assert_eq!(r.cookie("zzz"), None);
    }

    #[test]
    fn date_formatting() {
        assert_eq!(http_date(0), "Thu, 01 Jan 1970 00:00:00 GMT");
        assert_eq!(http_date(1_700_000_000), "Tue, 14 Nov 2023 22:13:20 GMT");
    }

    #[test]
    fn response_serialisation() {
        let r = Response::with_body(404, "text/plain", b"nope".to_vec());
        let s = String::from_utf8(r.clone().into_bytes(false, false)).unwrap();
        assert!(s.starts_with("HTTP/1.1 404 Not Found\r\n"));
        assert!(s.contains("Content-Length: 4\r\n"));
        assert!(s.contains("Connection: close\r\n"));
        assert!(s.ends_with("\r\n\r\nnope"));
        let head = String::from_utf8(r.into_bytes(true, true)).unwrap();
        assert!(head.contains("Content-Length: 4\r\n"));
        assert!(head.ends_with("\r\n\r\n"));
    }

    #[test]
    fn header_injection_is_neutralised() {
        let r = Response::new(302).header("Location", "/a\r\nSet-Cookie: x=1");
        assert!(!r.headers[0].1.contains('\n'));
    }

    #[test]
    fn encode_decode_roundtrip() {
        assert_eq!(pct_encode("a b/ü", true), "a%20b/%C3%BC");
        assert_eq!(url_decode("a%20b%2F").as_deref(), Some("a b/"));
        assert_eq!(url_decode("%"), None);
        assert_eq!(url_decode("%4"), None);
        assert_eq!(query_param("a=1&name=J+D%21", "name").as_deref(), Some("J D!"));
    }
}
