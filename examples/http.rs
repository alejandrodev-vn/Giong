//! A small HTTP/1.1 server on giong: keep-alive, pipelining, idle timeouts,
//! header size limit, graceful shutdown on SIGINT/SIGTERM.
//!
//!     cargo run --release --example http -- 127.0.0.1:3006
//!     curl localhost:3006/json
//!     curl localhost:3006/users
//!     curl 'localhost:3006/compute?n=25'
//!
//! Environment: `GIONG_WORKERS=<n>` overrides the number of worker threads
//! (default: one per CPU the process may run on).
//!
//! The same three routes are implemented by the servers in `bench/`, which is
//! what the benchmark numbers in the README compare.

use std::cell::RefCell;
use std::hint::black_box;
use std::io::Write;
use std::time::Duration;

use giong::net::{TcpListener, TcpStream};
use giong::time::{sleep, timeout};

/// A connection that sends nothing for this long is closed.
const IDLE_TIMEOUT: Duration = Duration::from_secs(30);
/// A peer that does not read our response for this long is dropped.
const WRITE_TIMEOUT: Duration = Duration::from_secs(30);
/// Upper bound for a request head (and for buffered partial requests).
const MAX_HEAD: usize = 64 * 1024;
/// Upper bound for a request body, which this server reads and discards.
const MAX_BODY: usize = 1024 * 1024;

thread_local! {
    /// Response buffers, recycled between the connections of one worker. A
    /// connection only holds one while it is actually writing, so idle
    /// keep-alive connections cost almost no memory.
    static BUFFERS: RefCell<Vec<Vec<u8>>> = const { RefCell::new(Vec::new()) };
    /// Scratch space for building a response body (never held across an await).
    static SCRATCH: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

fn take_buffer() -> Vec<u8> {
    BUFFERS
        .with(|pool| pool.borrow_mut().pop())
        .unwrap_or_else(|| Vec::with_capacity(4 * 1024))
}

fn give_buffer(mut buf: Vec<u8>) {
    buf.clear();
    // Keep a bounded number of reasonably sized buffers around.
    if buf.capacity() <= 256 * 1024 {
        BUFFERS.with(|pool| {
            let mut pool = pool.borrow_mut();
            if pool.len() < 1024 {
                pool.push(buf);
            }
        });
    }
}

fn fib(n: u32) -> u64 {
    if n < 2 {
        n as u64
    } else {
        fib(n - 1) + fib(n - 2)
    }
}

fn find_head_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

fn content_length(head: &[u8]) -> usize {
    for line in head.split(|&c| c == b'\n') {
        if line.len() > 15 && line[..15].eq_ignore_ascii_case(b"content-length:") {
            return line[15..]
                .iter()
                .filter(|c| c.is_ascii_digit())
                .fold(0usize, |acc, &c| {
                    acc.saturating_mul(10).saturating_add((c - b'0') as usize)
                });
        }
    }
    0
}

/// `true` if the client asked for the connection to be closed after this response.
fn wants_close(head: &[u8]) -> bool {
    head.split(|&c| c == b'\n').any(|line| {
        line.len() > 11
            && line[..11].eq_ignore_ascii_case(b"connection:")
            && line[11..]
                .windows(5)
                .any(|w| w.eq_ignore_ascii_case(b"close"))
    })
}

fn path_of(head: &[u8]) -> &[u8] {
    let mut parts = head.split(|&c| c == b' ');
    parts.next();
    parts.next().unwrap_or(b"/")
}

fn query_n(query: &[u8]) -> u32 {
    query
        .split(|&c| c == b'&')
        .find(|kv| kv.starts_with(b"n="))
        .map(|kv| {
            kv[2..]
                .iter()
                .take_while(|c| c.is_ascii_digit())
                .fold(0u32, |acc, &c| {
                    acc.saturating_mul(10).saturating_add((c - b'0') as u32)
                })
        })
        .unwrap_or(25)
        .min(40)
}

/// Appends the response for `path` to `out`. `body` is scratch space.
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
                if i > 0 {
                    body.push(b',');
                }
                let _ = write!(
                    body,
                    r#"{{"id":{i},"name":"User {i}","email":"user{i}@example.com","active":{},"score":{}}}"#,
                    i % 2 == 0,
                    i as f64 * 1.5
                );
            }
            body.push(b']');
        }
        b"/compute" => {
            let _ = write!(body, r#"{{"result":{}}}"#, fib(black_box(query_n(query))));
        }
        _ => {
            out.extend_from_slice(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n");
            return;
        }
    }
    let _ = write!(
        out,
        "HTTP/1.1 200 OK\r\nServer: giong\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
        body.len()
    );
    out.extend_from_slice(body);
}

/// Handles every complete request in `buf`, appending responses to `out`.
///
/// Returns how many bytes were consumed and whether the connection should be
/// closed once the responses are sent, or `None` if a request is invalid.
fn handle_requests(buf: &[u8], out: &mut Vec<u8>, body: &mut Vec<u8>) -> Option<(usize, bool)> {
    let mut used = 0;
    while let Some(end) = find_head_end(&buf[used..]) {
        let head = &buf[used..used + end];
        let body_len = content_length(head);
        if body_len > MAX_BODY {
            return None;
        }
        let total = end + 4 + body_len;
        if buf.len() - used < total {
            break;
        }
        respond(path_of(head), out, body);
        used += total;
        if wants_close(head) {
            return Some((used, true));
        }
    }
    Some((used, false))
}

async fn serve(stream: TcpStream) {
    let _ = stream.set_nodelay(true);
    // Bytes of a request that has not arrived completely yet (usually empty).
    let mut pending: Vec<u8> = Vec::new();

    loop {
        // `recv` is cancel safe, so wrapping it in a timeout loses no data.
        let chunk = match timeout(IDLE_TIMEOUT, stream.recv()).await {
            Ok(Ok(chunk)) if !chunk.is_empty() => chunk,
            _ => return, // idle for too long, error, or closed by the peer
        };

        let mut out = take_buffer();
        let parsed = SCRATCH.with(|scratch| {
            let body = &mut *scratch.borrow_mut();
            if pending.is_empty() {
                // Fast path: parse straight out of the kernel-provided buffer.
                let (used, close) = handle_requests(&chunk, &mut out, body)?;
                pending.extend_from_slice(&chunk[used..]);
                Some(close)
            } else {
                pending.extend_from_slice(&chunk);
                let (used, close) = handle_requests(&pending, &mut out, body)?;
                pending.drain(..used);
                Some(close)
            }
        });
        // Hand the pool buffer back before awaiting anything else.
        drop(chunk);
        let Some(close) = parsed else { return };
        if pending.len() > MAX_HEAD + MAX_BODY {
            return;
        }

        if out.is_empty() {
            give_buffer(out);
        } else {
            // One send per batch of pipelined requests. The buffer comes back
            // with the result and returns to the worker's pool.
            match timeout(WRITE_TIMEOUT, stream.write_all(out)).await {
                Ok((Ok(()), buf)) => give_buffer(buf),
                _ => return,
            }
        }
        if close {
            return; // dropping the stream closes the connection
        }
    }
}

fn main() -> std::io::Result<()> {
    let addr = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "127.0.0.1:3006".into());
    let mut builder = giong::Builder::new().shutdown_on_signals(true);
    if let Some(n) = std::env::var("GIONG_WORKERS")
        .ok()
        .and_then(|v| v.parse().ok())
    {
        builder = builder.workers(n);
    }
    let runtime = builder.build()?;
    eprintln!(
        "giong http: listening on {addr} with {} worker(s)",
        runtime.handle().workers()
    );

    runtime.run(move |worker| {
        let addr = addr.clone();
        async move {
            let listener = TcpListener::bind(&addr).expect("bind");
            while let Some(conn) = giong::until_shutdown(listener.accept()).await {
                match conn {
                    Ok(stream) => {
                        giong::spawn(serve(stream));
                    }
                    Err(e) => {
                        // E.g. EMFILE: back off instead of spinning.
                        eprintln!("worker {worker}: accept failed: {e}");
                        sleep(Duration::from_millis(50)).await;
                    }
                }
            }
        }
    })
}
