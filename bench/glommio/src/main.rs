//! The benchmark routes on Glommio 0.9, hand-rolled HTTP parser.
use std::hint::black_box;
use std::io::Write;

use futures_lite::{AsyncReadExt, AsyncWriteExt};
use glommio::net::{TcpListener, TcpStream};
use glommio::{LocalExecutorBuilder, Placement};

fn fib(n: u32) -> u64 { if n < 2 { n as u64 } else { fib(n - 1) + fib(n - 2) } }

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



/// Handles every complete request in `buf`; returns how many bytes were consumed.
fn handle_requests(buf: &[u8], out: &mut Vec<u8>, body: &mut Vec<u8>) -> usize {
    let mut used = 0;
    while let Some(end) = find_head_end(&buf[used..]) {
        let head = &buf[used..used + end];
        let total = end + 4 + content_length(head);
        if buf.len() - used < total {
            break;
        }
        respond(path_of(head), out, body);
        used += total;
    }
    used
}

async fn serve(mut stream: TcpStream) {
    let _ = stream.set_nodelay(true);
    let mut chunk = vec![0u8; 8192];
    let mut pending: Vec<u8> = Vec::new();
    let mut out: Vec<u8> = Vec::with_capacity(8192);
    let mut body: Vec<u8> = Vec::with_capacity(16384);
    loop {
        let n = match stream.read(&mut chunk).await {
            Ok(0) | Err(_) => return,
            Ok(n) => n,
        };
        if pending.is_empty() {
            let used = handle_requests(&chunk[..n], &mut out, &mut body);
            pending.extend_from_slice(&chunk[used..n]);
        } else {
            pending.extend_from_slice(&chunk[..n]);
            let used = handle_requests(&pending, &mut out, &mut body);
            pending.drain(..used);
        }
        if pending.len() > 64 * 1024 {
            return;
        }
        if !out.is_empty() {
            if stream.write_all(&out).await.is_err() {
                return;
            }
            out.clear();
        }
    }
}

fn main() {
    // Not pinned here: the benchmark script pins the process with taskset.
    // GLOMMIO_SPIN_US=<microseconds> turns on busy-polling before sleeping.
    let mut builder = LocalExecutorBuilder::new(Placement::Unbound);
    if let Some(us) = std::env::var("GLOMMIO_SPIN_US").ok().and_then(|v| v.parse().ok()) {
        builder = builder.spin_before_park(std::time::Duration::from_micros(us));
    }
    builder
        .spawn(|| async move {
            let listener = TcpListener::bind("127.0.0.1:3008").expect("bind");
            loop {
                if let Ok(stream) = listener.accept().await {
                    glommio::spawn_local(serve(stream)).detach();
                }
            }
        })
        .expect("glommio executor")
        .join()
        .expect("glommio executor panicked");
}
