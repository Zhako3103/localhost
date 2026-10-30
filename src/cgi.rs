//! CGI support: spawning the interpreter with non-blocking pipes and parsing
//! its output into an HTTP response.

use crate::epoll::set_nonblocking;
use crate::http::{find, Response};
use std::io;
use std::os::fd::AsRawFd;
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

pub struct CgiJob {
    pub interpreter: String,
    /// Absolute path of the script (passed as first argument).
    pub script: PathBuf,
    /// Directory the CGI runs in (the script's directory).
    pub workdir: PathBuf,
    pub env: Vec<(String, String)>,
    pub body: Vec<u8>,
}

pub struct CgiProc {
    pub child: Child,
    pub stdin: Option<ChildStdin>,
    pub stdout: ChildStdout,
    pub in_buf: Vec<u8>,
    pub in_pos: usize,
    pub out_buf: Vec<u8>,
    pub deadline: Instant,
}

impl Drop for CgiProc {
    /// Never leave zombies or runaway children behind.
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub fn spawn(job: CgiJob, timeout: Duration) -> io::Result<CgiProc> {
    let mut cmd = Command::new(&job.interpreter);
    cmd.arg(&job.script).current_dir(&job.workdir).env_clear();
    for (k, v) in &job.env {
        cmd.env(k, v);
    }
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = cmd.spawn()?;

    let stdout = match child.stdout.take() {
        Some(s) => s,
        None => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(io::Error::new(io::ErrorKind::Other, "cgi: no stdout pipe"));
        }
    };
    let stdin = child.stdin.take();

    let mut res = set_nonblocking(stdout.as_raw_fd());
    if res.is_ok() {
        if let Some(si) = &stdin {
            res = set_nonblocking(si.as_raw_fd());
        }
    }
    if let Err(e) = res {
        let _ = child.kill();
        let _ = child.wait();
        return Err(e);
    }

    // Without a body, close stdin right away so the CGI sees EOF.
    let stdin = if job.body.is_empty() { None } else { stdin };

    Ok(CgiProc {
        child,
        stdin,
        stdout,
        in_buf: job.body,
        in_pos: 0,
        out_buf: Vec::new(),
        deadline: Instant::now() + timeout,
    })
}

/// Turns raw CGI output (headers, blank line, body) into a Response.
pub fn parse_output(out: &[u8]) -> Option<Response> {
    let (head_end, sep) = match (find(out, b"\r\n\r\n"), find(out, b"\n\n")) {
        (Some(a), Some(b)) if b < a => (b, 2),
        (Some(a), _) => (a, 4),
        (None, Some(b)) => (b, 2),
        (None, None) => return None,
    };
    let head = String::from_utf8_lossy(&out[..head_end]).to_string();
    let body = out[head_end + sep..].to_vec();

    let mut status: Option<u16> = None;
    let mut headers: Vec<(String, String)> = Vec::new();
    for line in head.lines() {
        let line = line.trim_end_matches('\r');
        if line.is_empty() {
            continue;
        }
        let (name, value) = line.split_once(':')?;
        let (name, value) = (name.trim(), value.trim());
        match name.to_ascii_lowercase().as_str() {
            "status" => {
                let code: u16 = value.split_whitespace().next()?.parse().ok()?;
                if !(100..=599).contains(&code) {
                    return None;
                }
                status = Some(code);
            }
            "content-length" | "connection" | "transfer-encoding" | "date" | "server" => {}
            _ => headers.push((name.to_string(), value.to_string())),
        }
    }
    let has_location = headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("location"));
    let code = status.unwrap_or(if has_location { 302 } else { 200 });
    let mut resp = Response::new(code);
    for (k, v) in &headers {
        resp = resp.header(k, v);
    }
    if !headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("content-type")) {
        resp = resp.header("Content-Type", "text/plain; charset=utf-8");
    }
    resp.body = body;
    Some(resp)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_basic_output() {
        let r = parse_output(b"Content-Type: text/plain\r\n\r\nhello").unwrap();
        assert_eq!(r.status, 200);
        assert_eq!(r.body, b"hello");
        assert!(r.headers.iter().any(|(k, v)| k == "Content-Type" && v == "text/plain"));
    }

    #[test]
    fn parses_status_and_lf_only_separator() {
        let r = parse_output(b"Status: 404 Not Found\nContent-Type: text/html\n\n<b>x</b>").unwrap();
        assert_eq!(r.status, 404);
        assert_eq!(r.body, b"<b>x</b>");
    }

    #[test]
    fn location_implies_redirect() {
        let r = parse_output(b"Location: /x\r\n\r\n").unwrap();
        assert_eq!(r.status, 302);
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_output(b"no headers at all").is_none());
        assert!(parse_output(b"Status: 999\r\n\r\n").is_none());
        assert!(parse_output(b"not a header line\r\n\r\nbody").is_none());
    }

    #[test]
    fn strips_hop_headers() {
        let r = parse_output(b"Content-Length: 99\r\nContent-Type: a/b\r\n\r\nx").unwrap();
        assert!(!r.headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("content-length")));
    }
}
