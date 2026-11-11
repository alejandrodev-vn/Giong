//! Native HTTP/1.1 server: thread-per-core, raw epoll, SO_REUSEPORT,
//! hand-rolled parser (keep-alive + pipelining), zero-copy-ish buffers, no async runtime.
use std::hint::black_box;
use std::io::Write;
use std::os::fd::RawFd;
use std::ptr::null_mut;

const PORT: u16 = 3004;

struct Conn { rbuf: Vec<u8>, wbuf: Vec<u8>, wpos: usize, want_out: bool }

fn errno() -> i32 { std::io::Error::last_os_error().raw_os_error().unwrap_or(0) }

fn fib(n: u32) -> u64 { if n < 2 { n as u64 } else { fib(n - 1) + fib(n - 2) } }

unsafe fn listener() -> RawFd {
    let fd = libc::socket(libc::AF_INET, libc::SOCK_STREAM | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC, 0);
    assert!(fd >= 0);
    let one: libc::c_int = 1;
    let sz = std::mem::size_of::<libc::c_int>() as u32;
    libc::setsockopt(fd, libc::SOL_SOCKET, libc::SO_REUSEADDR, &one as *const _ as *const libc::c_void, sz);
    libc::setsockopt(fd, libc::SOL_SOCKET, libc::SO_REUSEPORT, &one as *const _ as *const libc::c_void, sz);
    let addr = libc::sockaddr_in {
        sin_family: libc::AF_INET as u16,
        sin_port: PORT.to_be(),
        sin_addr: libc::in_addr { s_addr: u32::from_be_bytes([127, 0, 0, 1]).to_be() },
        sin_zero: [0; 8],
    };
    assert!(libc::bind(fd, &addr as *const _ as *const libc::sockaddr,
        std::mem::size_of::<libc::sockaddr_in>() as u32) == 0, "bind failed");
    assert!(libc::listen(fd, 4096) == 0);
    fd
}

unsafe fn ctl(ep: RawFd, op: i32, fd: RawFd, events: u32) {
    let mut e = libc::epoll_event { events, u64: fd as u64 };
    libc::epoll_ctl(ep, op, fd, &mut e);
}

fn find_head_end(b: &[u8]) -> Option<usize> { b.windows(4).position(|w| w == b"\r\n\r\n") }

fn content_length(head: &[u8]) -> usize {
    for line in head.split(|&c| c == b'\n') {
        if line.len() > 15 && line[..15].eq_ignore_ascii_case(b"content-length:") {
            return line[15..].iter().filter(|c| c.is_ascii_digit())
                .fold(0usize, |a, &c| a * 10 + (c - b'0') as usize);
        }
    }
    0
}

fn path_of(head: &[u8]) -> &[u8] {
    let mut it = head.split(|&c| c == b' ');
    it.next();
    it.next().unwrap_or(b"/")
}

fn query_n(q: &[u8]) -> u32 {
    q.split(|&c| c == b'&').find(|kv| kv.starts_with(b"n="))
        .map(|kv| kv[2..].iter().take_while(|c| c.is_ascii_digit())
            .fold(0u32, |a, &c| a * 10 + (c - b'0') as u32))
        .unwrap_or(25)
}

fn respond(path: &[u8], out: &mut Vec<u8>, body: &mut Vec<u8>) {
    body.clear();
    let (route, query) = match path.iter().position(|&c| c == b'?') {
        Some(i) => (&path[..i], &path[i + 1..]),
        None => (path, &b""[..]),
    };
    match route {
        b"/json" => body.extend_from_slice(br#"{"message":"Hello, World!"}"#),
        b"/users" => {
            body.push(b'[');
            for i in 0..100u32 {
                if i > 0 { body.push(b','); }
                let _ = write!(body, r#"{{"id":{i},"name":"User {i}","email":"user{i}@example.com","active":{},"score":{}}}"#,
                    i % 2 == 0, i as f64 * 1.5);
            }
            body.push(b']');
        }
        b"/compute" => { let _ = write!(body, r#"{{"result":{}}}"#, fib(black_box(query_n(query)))); }
        _ => {
            out.extend_from_slice(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n");
            return;
        }
    }
    let _ = write!(out, "HTTP/1.1 200 OK\r\nServer: native\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n", body.len());
    out.extend_from_slice(body);
}

/// Returns true if the connection should be closed.
unsafe fn flush(fd: RawFd, c: &mut Conn, ep: RawFd) -> bool {
    while c.wpos < c.wbuf.len() {
        let r = libc::write(fd, c.wbuf.as_ptr().add(c.wpos) as *const libc::c_void, c.wbuf.len() - c.wpos);
        if r < 0 {
            match errno() {
                libc::EAGAIN => {
                    if !c.want_out { ctl(ep, libc::EPOLL_CTL_MOD, fd, (libc::EPOLLIN | libc::EPOLLOUT) as u32); c.want_out = true; }
                    return false;
                }
                libc::EINTR => continue,
                _ => return true,
            }
        }
        c.wpos += r as usize;
    }
    c.wbuf.clear();
    c.wpos = 0;
    if c.want_out { ctl(ep, libc::EPOLL_CTL_MOD, fd, libc::EPOLLIN as u32); c.want_out = false; }
    false
}

unsafe fn handle(fd: RawFd, ev: u32, c: &mut Conn, body: &mut Vec<u8>, ep: RawFd) -> bool {
    if ev & (libc::EPOLLERR | libc::EPOLLHUP) as u32 != 0 { return true; }
    if ev & libc::EPOLLIN as u32 != 0 {
        loop {
            if c.rbuf.capacity() - c.rbuf.len() < 2048 { c.rbuf.reserve(8192); }
            let len = c.rbuf.len();
            let room = c.rbuf.capacity() - len;
            let r = libc::read(fd, c.rbuf.as_mut_ptr().add(len) as *mut libc::c_void, room);
            if r == 0 { return true; }
            if r < 0 {
                match errno() { libc::EAGAIN => break, libc::EINTR => continue, _ => return true }
            }
            c.rbuf.set_len(len + r as usize);
            if (r as usize) < room { break; }
        }
        let mut off = 0;
        while let Some(end) = find_head_end(&c.rbuf[off..]) {
            let head = &c.rbuf[off..off + end];
            let total = end + 4 + content_length(head);
            if c.rbuf.len() - off < total { break; }
            respond(path_of(head), &mut c.wbuf, body);
            off += total;
        }
        c.rbuf.drain(..off);
        if c.rbuf.len() > 64 * 1024 { return true; } // oversized / malicious header
    }
    flush(fd, c, ep)
}

fn worker() {
    unsafe {
        let lfd = listener();
        let ep = libc::epoll_create1(libc::EPOLL_CLOEXEC);
        ctl(ep, libc::EPOLL_CTL_ADD, lfd, libc::EPOLLIN as u32);
        let mut conns: Vec<Option<Box<Conn>>> = Vec::new();
        let mut events = vec![libc::epoll_event { events: 0, u64: 0 }; 1024];
        let mut body = Vec::with_capacity(16 * 1024);
        loop {
            let n = libc::epoll_wait(ep, events.as_mut_ptr(), events.len() as i32, -1);
            if n < 0 { continue; }
            for i in 0..n as usize {
                let fd = events[i].u64 as RawFd;
                let ev = events[i].events;
                if fd == lfd {
                    loop {
                        let c = libc::accept4(lfd, null_mut(), null_mut(), libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC);
                        if c < 0 { break; }
                        let one: libc::c_int = 1;
                        libc::setsockopt(c, libc::IPPROTO_TCP, libc::TCP_NODELAY, &one as *const _ as *const libc::c_void, 4);
                        let idx = c as usize;
                        if conns.len() <= idx { conns.resize_with(idx + 1, || None); }
                        conns[idx] = Some(Box::new(Conn { rbuf: Vec::with_capacity(8192), wbuf: Vec::with_capacity(8192), wpos: 0, want_out: false }));
                        ctl(ep, libc::EPOLL_CTL_ADD, c, libc::EPOLLIN as u32);
                    }
                    continue;
                }
                let close = match conns.get_mut(fd as usize).and_then(|c| c.as_deref_mut()) {
                    Some(c) => handle(fd, ev, c, &mut body, ep),
                    None => false,
                };
                if close {
                    ctl(ep, libc::EPOLL_CTL_DEL, fd, 0);
                    libc::close(fd);
                    conns[fd as usize] = None;
                }
            }
        }
    }
}

fn main() {
    let n = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
    let hs: Vec<_> = (0..n).map(|_| std::thread::spawn(worker)).collect();
    for h in hs { let _ = h.join(); }
}
