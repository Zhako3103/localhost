//! Thin unsafe wrapper over Linux epoll + a few fd helpers.

use std::io;
use std::os::fd::RawFd;

pub const IN: u32 = libc::EPOLLIN as u32;
pub const OUT: u32 = libc::EPOLLOUT as u32;
pub const ERR: u32 = libc::EPOLLERR as u32;
pub const HUP: u32 = libc::EPOLLHUP as u32;

pub struct Epoll {
    fd: RawFd,
}

impl Epoll {
    pub fn new() -> io::Result<Self> {
        let fd = unsafe { libc::epoll_create1(libc::EPOLL_CLOEXEC) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { fd })
    }

    fn ctl(&self, op: i32, fd: RawFd, events: u32) -> io::Result<()> {
        let mut ev = libc::epoll_event {
            events,
            u64: fd as u64,
        };
        let r = unsafe { libc::epoll_ctl(self.fd, op, fd, &mut ev) };
        if r < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    pub fn add(&self, fd: RawFd, events: u32) -> io::Result<()> {
        self.ctl(libc::EPOLL_CTL_ADD, fd, events)
    }

    pub fn modify(&self, fd: RawFd, events: u32) -> io::Result<()> {
        self.ctl(libc::EPOLL_CTL_MOD, fd, events)
    }

    pub fn del(&self, fd: RawFd) -> io::Result<()> {
        self.ctl(libc::EPOLL_CTL_DEL, fd, 0)
    }

    /// Waits for events. EINTR is reported as `Ok(0)` so the caller can
    /// re-check its stop flag.
    pub fn wait(&self, buf: &mut [libc::epoll_event], timeout_ms: i32) -> io::Result<usize> {
        let n = unsafe { libc::epoll_wait(self.fd, buf.as_mut_ptr(), buf.len() as i32, timeout_ms) };
        if n < 0 {
            let e = io::Error::last_os_error();
            if e.kind() == io::ErrorKind::Interrupted {
                return Ok(0);
            }
            return Err(e);
        }
        Ok(n as usize)
    }
}

impl Drop for Epoll {
    fn drop(&mut self) {
        unsafe {
            libc::close(self.fd);
        }
    }
}

pub fn set_nonblocking(fd: RawFd) -> io::Result<()> {
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL);
        if flags < 0 {
            return Err(io::Error::last_os_error());
        }
        if libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) < 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

/// Raises RLIMIT_NOFILE to the hard limit and returns the resulting soft limit.
pub fn raise_nofile_limit() -> usize {
    unsafe {
        let mut rl = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        if libc::getrlimit(libc::RLIMIT_NOFILE, &mut rl) != 0 {
            return 1024;
        }
        if rl.rlim_cur < rl.rlim_max {
            let mut wanted = rl;
            wanted.rlim_cur = rl.rlim_max;
            if libc::setrlimit(libc::RLIMIT_NOFILE, &wanted) == 0 {
                rl = wanted;
            }
        }
        rl.rlim_cur as usize
    }
}
