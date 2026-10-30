//! Single-threaded epoll event loop.
//!
//! One `epoll_wait` per loop iteration; every read and write on a socket or
//! CGI pipe happens only after epoll reported readiness. Responses are never
//! written directly after parsing: the connection switches to EPOLLOUT first.

use crate::cgi::{self, CgiProc};
use crate::config::Config;
use crate::epoll::{self, Epoll};
use crate::handler::{self, Action};
use crate::http::{ParseResult, Request, RequestParser, Response};
use crate::router;
use crate::session::SessionStore;
use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::fd::{AsRawFd, RawFd};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const MAX_EVENTS: usize = 256;
const READ_TIMEOUT: Duration = Duration::from_secs(15); // no bytes while a request is in flight
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30); // whole request must arrive in this time
const KEEPALIVE_IDLE: Duration = Duration::from_secs(15);
const WRITE_TIMEOUT: Duration = Duration::from_secs(30);
const CGI_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_CGI_OUTPUT: usize = 32 * 1024 * 1024;
const SESSION_TTL: Duration = Duration::from_secs(1800);
const MAX_SESSIONS: usize = 10_000;

static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(_sig: libc::c_int) {
    STOP.store(true, Ordering::SeqCst);
}

fn install_signals() {
    unsafe {
        libc::signal(libc::SIGINT, on_signal as *const () as libc::sighandler_t);
        libc::signal(libc::SIGTERM, on_signal as *const () as libc::sighandler_t);
    }
}

#[derive(Clone, Copy, PartialEq)]
enum State {
    Reading,
    Cgi,
    Writing,
}

struct Listener {
    sock: TcpListener,
    /// Indices into `Config::servers` sharing this host:port (first = default).
    servers: Vec<usize>,
    port: u16,
}

struct Conn {
    stream: TcpStream,
    listener: RawFd,
    server_idx: usize,
    parser: RequestParser,
    state: State,
    out: Vec<u8>,
    out_pos: usize,
    keep_alive: bool,
    last_activity: Instant,
    /// When the first byte of the in-flight request arrived.
    started: Option<Instant>,
    cgi: Option<CgiProc>,
    cgi_head_only: bool,
    cgi_keep_alive: bool,
}

impl Conn {
    fn new(stream: TcpStream, listener: RawFd, server_idx: usize) -> Self {
        Self {
            stream,
            listener,
            server_idx,
            parser: RequestParser::new(),
            state: State::Reading,
            out: Vec::new(),
            out_pos: 0,
            keep_alive: true,
            last_activity: Instant::now(),
            started: None,
            cgi: None,
            cgi_head_only: false,
            cgi_keep_alive: false,
        }
    }
}

enum Expire {
    Close,
    RequestTimeout,
    CgiTimeout,
}

struct Server {
    epoll: Epoll,
    cfg: Config,
    listeners: HashMap<RawFd, Listener>,
    conns: HashMap<RawFd, Conn>,
    /// CGI pipe fd -> (client fd, is_stdin)
    cgi_fds: HashMap<RawFd, (RawFd, bool)>,
    sessions: SessionStore,
    max_conns: usize,
}

pub fn run(cfg: Config) -> Result<(), String> {
    install_signals();
    let mut server = Server::new(cfg)?;
    server.event_loop();
    println!("shutting down");
    Ok(())
}

impl Server {
    fn new(cfg: Config) -> Result<Server, String> {
        let nofile = epoll::raise_nofile_limit();
        let max_conns = nofile.saturating_sub(64).clamp(16, 10_000);

        let epoll = Epoll::new().map_err(|e| format!("epoll_create1: {}", e))?;

        // Group servers sharing the same host:port under one listening socket.
        let mut groups: Vec<((String, u16), Vec<usize>)> = Vec::new();
        for (i, s) in cfg.servers.iter().enumerate() {
            let host = if s.host == "localhost" {
                "127.0.0.1".to_string()
            } else {
                s.host.clone()
            };
            for &p in &s.ports {
                if let Some(pos) = groups.iter().position(|(k, _)| k.0 == host && k.1 == p) {
                    groups[pos].1.push(i);
                } else {
                    groups.push(((host.clone(), p), vec![i]));
                }
            }
        }

        let mut listeners = HashMap::new();
        for ((host, port), servers) in groups {
            let sock = TcpListener::bind((host.as_str(), port))
                .map_err(|e| format!("cannot bind {}:{}: {}", host, port, e))?;
            sock.set_nonblocking(true)
                .map_err(|e| format!("set_nonblocking: {}", e))?;
            let fd = sock.as_raw_fd();
            epoll
                .add(fd, epoll::IN)
                .map_err(|e| format!("epoll_ctl: {}", e))?;
            println!("listening on {}:{} ({} server block(s))", host, port, servers.len());
            listeners.insert(fd, Listener { sock, servers, port });
        }

        Ok(Server {
            epoll,
            cfg,
            listeners,
            conns: HashMap::new(),
            cgi_fds: HashMap::new(),
            sessions: SessionStore::new(SESSION_TTL, MAX_SESSIONS),
            max_conns,
        })
    }

    fn event_loop(&mut self) {
        let mut events = vec![libc::epoll_event { events: 0, u64: 0 }; MAX_EVENTS];
        let mut last_sweep = Instant::now();

        while !STOP.load(Ordering::SeqCst) {
            let n = match self.epoll.wait(&mut events, 500) {
                Ok(n) => n,
                Err(e) => {
                    eprintln!("epoll_wait failed: {}", e);
                    break;
                }
            };
            for i in 0..n {
                let ev = events[i];
                let fd = ev.u64 as RawFd;
                let flags = ev.events;
                let res = catch_unwind(AssertUnwindSafe(|| self.dispatch(fd, flags)));
                if res.is_err() {
                    eprintln!("recovered from a panic while handling fd {}", fd);
                    self.recover(fd);
                }
            }
            if last_sweep.elapsed() >= Duration::from_millis(500) {
                if catch_unwind(AssertUnwindSafe(|| self.sweep())).is_err() {
                    eprintln!("recovered from a panic in sweep");
                }
                last_sweep = Instant::now();
            }
        }
    }

    fn dispatch(&mut self, fd: RawFd, flags: u32) {
        if self.listeners.contains_key(&fd) {
            self.accept(fd);
        } else if self.conns.contains_key(&fd) {
            self.on_client(fd, flags);
        } else if self.cgi_fds.contains_key(&fd) {
            self.on_cgi(fd, flags);
        }
    }

    /// After a panic the connection may be half-removed; make sure the fd is gone.
    fn recover(&mut self, fd: RawFd) {
        if let Some(c) = self.conns.remove(&fd) {
            self.close_conn(fd, c);
        } else {
            let _ = self.epoll.del(fd);
        }
    }

    // ------------------------------------------------------------- accept

    fn accept(&mut self, lfd: RawFd) {
        for _ in 0..64 {
            let stream = match self.listeners.get(&lfd) {
                Some(l) => match l.sock.accept() {
                    Ok((s, _)) => s,
                    Err(_) => break, // WouldBlock or transient error
                },
                None => return,
            };
            if self.conns.len() >= self.max_conns {
                continue; // dropping the stream closes it
            }
            if stream.set_nonblocking(true).is_err() {
                continue;
            }
            let _ = stream.set_nodelay(true);
            let fd = stream.as_raw_fd();
            if self.epoll.add(fd, epoll::IN).is_err() {
                continue;
            }
            let first = match self.listeners.get(&lfd) {
                Some(l) => l.servers[0],
                None => return,
            };
            self.conns.insert(fd, Conn::new(stream, lfd, first));
        }
    }

    // ------------------------------------------------------ client events

    fn on_client(&mut self, fd: RawFd, flags: u32) {
        let mut conn = match self.conns.remove(&fd) {
            Some(c) => c,
            None => return,
        };
        if self.drive_client(fd, &mut conn, flags) {
            self.conns.insert(fd, conn);
        } else {
            self.close_conn(fd, conn);
        }
    }

    fn close_conn(&mut self, fd: RawFd, mut conn: Conn) {
        self.abort_cgi(&mut conn);
        let _ = self.epoll.del(fd);
        drop(conn);
    }

    /// Returns false when the connection must be closed.
    fn drive_client(&mut self, fd: RawFd, conn: &mut Conn, flags: u32) -> bool {
        if flags & (epoll::ERR | epoll::HUP) != 0 {
            return false;
        }
        match conn.state {
            State::Reading => {
                if flags & epoll::IN == 0 {
                    return true;
                }
                self.do_read(fd, conn)
            }
            State::Writing => {
                if flags & epoll::OUT == 0 {
                    return true;
                }
                self.do_write(fd, conn)
            }
            State::Cgi => true,
        }
    }

    fn do_read(&mut self, fd: RawFd, conn: &mut Conn) -> bool {
        let mut buf = [0u8; 16384];
        match conn.stream.read(&mut buf) {
            Ok(0) => return false,
            Ok(n) => {
                let now = Instant::now();
                conn.last_activity = now;
                if conn.started.is_none() {
                    conn.started = Some(now);
                }
                conn.parser.feed(&buf[..n]);
            }
            Err(e)
                if e.kind() == io::ErrorKind::WouldBlock
                    || e.kind() == io::ErrorKind::Interrupted =>
            {
                return true;
            }
            Err(_) => return false,
        }
        self.process(fd, conn)
    }

    fn do_write(&mut self, fd: RawFd, conn: &mut Conn) -> bool {
        match conn.stream.write(&conn.out[conn.out_pos..]) {
            Ok(0) => return false,
            Ok(n) => {
                conn.out_pos += n;
                conn.last_activity = Instant::now();
            }
            Err(e)
                if e.kind() == io::ErrorKind::WouldBlock
                    || e.kind() == io::ErrorKind::Interrupted =>
            {
                return true;
            }
            Err(_) => return false,
        }
        if conn.out_pos < conn.out.len() {
            return true;
        }
        if !conn.keep_alive {
            return false;
        }
        // Response fully sent: back to reading the next request.
        conn.out = Vec::new();
        conn.out_pos = 0;
        conn.state = State::Reading;
        if self.epoll.modify(fd, epoll::IN).is_err() {
            return false;
        }
        if conn.parser.is_empty() {
            true
        } else {
            self.process(fd, conn) // pipelined bytes already buffered
        }
    }

    /// Runs the parser over buffered bytes and acts on the result.
    fn process(&mut self, fd: RawFd, conn: &mut Conn) -> bool {
        let result = {
            let cfg = &self.cfg;
            let lst = match self.listeners.get(&conn.listener) {
                Some(l) => l,
                None => return false,
            };
            let limit = |r: &Request| {
                let i = router::select_server(cfg, &lst.servers, r.header("host"));
                cfg.servers[i].max_body
            };
            conn.parser.parse(&limit)
        };
        match result {
            ParseResult::NeedMore => {
                conn.started = if conn.parser.is_empty() {
                    None
                } else {
                    conn.started.or(Some(Instant::now()))
                };
                true
            }
            ParseResult::Err(code) => self.start_error(fd, conn, code, false),
            ParseResult::Done(req) => self.dispatch_request(fd, conn, req),
        }
    }

    fn dispatch_request(&mut self, fd: RawFd, conn: &mut Conn, req: Request) -> bool {
        conn.started = None;
        let (sidx, port) = match self.listeners.get(&conn.listener) {
            Some(l) => (
                router::select_server(&self.cfg, &l.servers, req.header("host")),
                l.port,
            ),
            None => return false,
        };
        conn.server_idx = sidx;
        let keep = req.keep_alive();
        let head_only = req.method == "HEAD";

        let action = handler::handle(&self.cfg.servers[sidx], port, &req, &mut self.sessions);
        match action {
            Action::Respond(resp) => self.set_response(fd, conn, resp, head_only, keep),
            Action::Cgi(job) => match cgi::spawn(job, CGI_TIMEOUT) {
                Ok(proc_) => self.attach_cgi(fd, conn, proc_, head_only, keep),
                Err(e) => {
                    eprintln!("cgi spawn failed: {}", e);
                    self.start_error(fd, conn, 500, keep)
                }
            },
        }
    }

    /// Queues an error response. `keep` = whether the connection may stay open.
    fn start_error(&mut self, fd: RawFd, conn: &mut Conn, code: u16, keep: bool) -> bool {
        let idx = conn.server_idx.min(self.cfg.servers.len().saturating_sub(1));
        let resp = handler::error_response(&self.cfg.servers[idx], code);
        self.set_response(fd, conn, resp, false, keep)
    }

    /// Serialises the response and waits for EPOLLOUT; never writes directly.
    fn set_response(
        &mut self,
        fd: RawFd,
        conn: &mut Conn,
        resp: Response,
        head_only: bool,
        keep_alive: bool,
    ) -> bool {
        conn.keep_alive = keep_alive;
        conn.out = resp.into_bytes(keep_alive, head_only);
        conn.out_pos = 0;
        conn.state = State::Writing;
        conn.started = None;
        conn.last_activity = Instant::now();
        self.epoll.modify(fd, epoll::OUT).is_ok()
    }

    // ---------------------------------------------------------------- CGI

    fn attach_cgi(
        &mut self,
        fd: RawFd,
        conn: &mut Conn,
        proc_: CgiProc,
        head_only: bool,
        keep: bool,
    ) -> bool {
        let out_fd = proc_.stdout.as_raw_fd();
        let in_fd = proc_.stdin.as_ref().map(|s| s.as_raw_fd());

        if self.epoll.add(out_fd, epoll::IN).is_err() {
            return self.start_error(fd, conn, 500, keep); // proc_ dropped: killed + reaped
        }
        self.cgi_fds.insert(out_fd, (fd, false));
        if let Some(i) = in_fd {
            if self.epoll.add(i, epoll::OUT).is_err() {
                let _ = self.epoll.del(out_fd);
                self.cgi_fds.remove(&out_fd);
                return self.start_error(fd, conn, 500, keep);
            }
            self.cgi_fds.insert(i, (fd, true));
        }
        conn.cgi = Some(proc_);
        conn.cgi_head_only = head_only;
        conn.cgi_keep_alive = keep;
        conn.state = State::Cgi;
        conn.started = None;
        conn.last_activity = Instant::now();
        // Stop watching the client socket for data while the CGI runs
        // (ERR/HUP are still reported).
        self.epoll.modify(fd, 0).is_ok()
    }

    fn on_cgi(&mut self, fd: RawFd, flags: u32) {
        let (cfd, is_stdin) = match self.cgi_fds.get(&fd) {
            Some(&x) => x,
            None => return,
        };
        let mut conn = match self.conns.remove(&cfd) {
            Some(c) => c,
            None => {
                // Orphaned pipe: forget it.
                let _ = self.epoll.del(fd);
                self.cgi_fds.remove(&fd);
                return;
            }
        };
        if self.drive_cgi(fd, cfd, is_stdin, flags, &mut conn) {
            self.conns.insert(cfd, conn);
        } else {
            self.close_conn(cfd, conn);
        }
    }

    fn drive_cgi(
        &mut self,
        fd: RawFd,
        cfd: RawFd,
        is_stdin: bool,
        flags: u32,
        conn: &mut Conn,
    ) -> bool {
        if conn.state != State::Cgi {
            return true;
        }
        conn.last_activity = Instant::now();

        if is_stdin {
            let mut done = flags & (epoll::ERR | epoll::HUP) != 0;
            if !done && flags & epoll::OUT != 0 {
                if let Some(p) = conn.cgi.as_mut() {
                    if let Some(si) = p.stdin.as_mut() {
                        match si.write(&p.in_buf[p.in_pos..]) {
                            Ok(n) => {
                                p.in_pos += n;
                                if p.in_pos >= p.in_buf.len() {
                                    done = true;
                                }
                            }
                            Err(e)
                                if e.kind() == io::ErrorKind::WouldBlock
                                    || e.kind() == io::ErrorKind::Interrupted => {}
                            Err(_) => done = true, // e.g. EPIPE: script stopped reading
                        }
                    } else {
                        done = true;
                    }
                }
            }
            if done {
                let _ = self.epoll.del(fd);
                self.cgi_fds.remove(&fd);
                if let Some(p) = conn.cgi.as_mut() {
                    p.stdin = None; // closes the pipe -> CGI sees EOF
                }
            }
            return true;
        }

        // CGI stdout
        if flags & (epoll::IN | epoll::HUP | epoll::ERR) == 0 {
            return true;
        }
        let mut buf = [0u8; 32768];
        let p = match conn.cgi.as_mut() {
            Some(p) => p,
            None => return true,
        };
        match p.stdout.read(&mut buf) {
            Ok(0) => self.finish_cgi(cfd, conn),
            Ok(n) => {
                if p.out_buf.len() + n > MAX_CGI_OUTPUT {
                    self.abort_cgi(conn);
                    return self.start_error(cfd, conn, 500, false);
                }
                p.out_buf.extend_from_slice(&buf[..n]);
                true
            }
            Err(e)
                if e.kind() == io::ErrorKind::WouldBlock
                    || e.kind() == io::ErrorKind::Interrupted =>
            {
                true
            }
            Err(_) => self.finish_cgi(cfd, conn),
        }
    }

    /// CGI closed its stdout: reap it and turn the output into a response.
    fn finish_cgi(&mut self, cfd: RawFd, conn: &mut Conn) -> bool {
        let mut p = match conn.cgi.take() {
            Some(p) => p,
            None => return true,
        };
        if let Some(si) = &p.stdin {
            let f = si.as_raw_fd();
            let _ = self.epoll.del(f);
            self.cgi_fds.remove(&f);
        }
        let of = p.stdout.as_raw_fd();
        let _ = self.epoll.del(of);
        self.cgi_fds.remove(&of);
        p.stdin = None;

        // Reap: if it is still running after closing stdout, kill it.
        if !matches!(p.child.try_wait(), Ok(Some(_))) {
            let _ = p.child.kill();
            let _ = p.child.wait();
        }
        let out = std::mem::take(&mut p.out_buf);
        drop(p);

        let (head_only, keep) = (conn.cgi_head_only, conn.cgi_keep_alive);
        match cgi::parse_output(&out) {
            Some(resp) => self.set_response(cfd, conn, resp, head_only, keep),
            None => self.start_error(cfd, conn, 500, keep),
        }
    }

    /// Detaches and kills a running CGI (timeouts, client gone, errors).
    fn abort_cgi(&mut self, conn: &mut Conn) {
        if let Some(mut p) = conn.cgi.take() {
            if let Some(si) = &p.stdin {
                let f = si.as_raw_fd();
                let _ = self.epoll.del(f);
                self.cgi_fds.remove(&f);
            }
            let of = p.stdout.as_raw_fd();
            let _ = self.epoll.del(of);
            self.cgi_fds.remove(&of);
            p.stdin = None;
            drop(p); // Drop: kill + wait
        }
    }

    // ------------------------------------------------------------ timeouts

    fn sweep(&mut self) {
        let now = Instant::now();
        self.sessions.purge();
        let conns = &self.conns;
        self.cgi_fds.retain(|_, (cfd, _)| conns.contains_key(cfd));

        let mut expired: Vec<(RawFd, Expire)> = Vec::new();
        for (&fd, c) in self.conns.iter() {
            match c.state {
                State::Reading => match c.started {
                    Some(st) => {
                        if now.duration_since(st) > REQUEST_TIMEOUT
                            || now.duration_since(c.last_activity) > READ_TIMEOUT
                        {
                            expired.push((fd, Expire::RequestTimeout));
                        }
                    }
                    None => {
                        if now.duration_since(c.last_activity) > KEEPALIVE_IDLE {
                            expired.push((fd, Expire::Close));
                        }
                    }
                },
                State::Writing => {
                    if now.duration_since(c.last_activity) > WRITE_TIMEOUT {
                        expired.push((fd, Expire::Close));
                    }
                }
                State::Cgi => match &c.cgi {
                    Some(p) if now > p.deadline => expired.push((fd, Expire::CgiTimeout)),
                    Some(_) => {}
                    None => expired.push((fd, Expire::Close)),
                },
            }
        }

        for (fd, why) in expired {
            let mut conn = match self.conns.remove(&fd) {
                Some(c) => c,
                None => continue,
            };
            let keep = match why {
                Expire::Close => false,
                Expire::RequestTimeout => self.start_error(fd, &mut conn, 408, false),
                Expire::CgiTimeout => {
                    self.abort_cgi(&mut conn);
                    self.start_error(fd, &mut conn, 504, false)
                }
            };
            if keep {
                self.conns.insert(fd, conn);
            } else {
                self.close_conn(fd, conn);
            }
        }
    }
}
