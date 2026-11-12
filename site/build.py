#!/usr/bin/env python3
"""Generates site/public/index.html.

Every number comes from the README of release 0.2 and bench/results/ in the
repository. Change a number in CHARTS or in the text tables T below, run
`python3 site/build.py`, and the page in site/public/ follows.
"""
import html
import math
import re
import sys
from pathlib import Path

REPO = "https://github.com/giong-run/Giong"
BLOB = REPO + "/blob/main/"
SITE = "https://giong.run/"


# ---------------------------------------------------------------- helpers
def md(s):
    """**bold** and `code` in otherwise plain text."""
    s = html.escape(s, quote=False)
    s = re.sub(r"\*\*(.+?)\*\*", r"<strong>\1</strong>", s)
    s = re.sub(r"`(.+?)`", r"<code>\1</code>", s)
    return s


KEYWORDS = ("use fn let async await move match while loop if else return "
            "struct impl pub mut for in as drop").split()
TOKEN = re.compile(
    r"(?P<c>//[^\n]*)"
    r"|(?P<s>b?\"(?:[^\"\\]|\\.)*\")"
    r"|(?P<k>\b(?:" + "|".join(KEYWORDS) + r")\b)"
    r"|(?P<t>\b[A-Z][A-Za-z0-9]*\b)"
)


def rust(src):
    out, pos = [], 0
    for m in TOKEN.finditer(src):
        out.append(html.escape(src[pos:m.start()], quote=False))
        kind = m.lastgroup
        out.append(f'<span class="{kind}">{html.escape(m.group(), quote=False)}</span>')
        pos = m.end()
    out.append(html.escape(src[pos:], quote=False))
    return "".join(out)


def code_only(s):
    """`code` in a string that already holds trusted HTML."""
    return re.sub(r"`(.+?)`", r"<code>\1</code>", s)


def shell(src):
    lines = []
    for line in src.split("\n"):
        code, sep, comment = line.partition("  #")
        line = html.escape(code, quote=False)
        if sep:
            line += f'<span class="c">  #{html.escape(comment, quote=False)}</span>'
        lines.append(line)
    return "\n".join(lines)


def ring(lit=14, start=225.0, n=24, gap=3.0, r_out=190.0, r_in=152.0, c=200.0):
    """The submission ring: n segments, clockwise, the first `lit` of them on.
    The oldest three wear the fading tones of the mane in the logo."""
    step = 360.0 / n
    paths = []
    for i in range(n):
        a0 = math.radians(start + i * step + gap / 2)
        a1 = math.radians(start + (i + 1) * step - gap / 2)
        def pt(r, a):
            return f"{c + r * math.sin(a):.1f} {c - r * math.cos(a):.1f}"
        d = (f"M{pt(r_out, a0)}A{r_out:g} {r_out:g} 0 0 1 {pt(r_out, a1)}"
             f"L{pt(r_in, a1)}A{r_in:g} {r_in:g} 0 0 0 {pt(r_in, a0)}Z")
        cls = "seg"
        if i < lit:
            cls += " on" + (f" a{i + 1}" if i < 3 else "")
        paths.append(f'<path class="{cls}" d="{d}"/>')
    return "".join(paths)


# ---------------------------------------------------------------- data
# Server names as they appear in the README tables.
NAMES = {
    "giong": "giong",
    "spin": "giong + spin",
    "http": "giong-http",
    "monoio": "Monoio (tuned)",
    "monoio0": "Monoio (default)",
    "tokio": "Raw Tokio",
    "epoll": "Hand-written epoll",
    "glommio": "Glommio",
    "axum": "Axum",
    "bun": "Bun",
    "node": "Node.js",
    "std": "std (blocking)",
    "tokio1": "Tokio, one core",
    "tokio4": "Tokio, 4 cores",
    "axumtls": "Axum + rustls",
    "httptls": "giong-http + giong-tls",
}
G = {"giong", "httptls"}          # ember
ALT = {"spin", "http"}            # giong's other configurations

# rows: (server, value, label, lo, hi) ; a str instead of a tuple starts a group
CHARTS = [
    dict(
        title="System calls per request",
        sub="`/json`, 50 connections · lower is better",
        max=2.2,
        rows=[("giong", 0.03, "0.03"), ("http", 0.03, "0.03"), ("monoio", 0.04, "0.04"),
              ("epoll", 2.03, "2.03"), ("tokio", 2.05, "2.05"), ("glommio", 2.10, "2.10")],
        note=("The structural difference, and the most robust result here. On the small response it "
              "buys little, because a request costs about 4 µs of kernel time whichever way it is "
              "delivered. On 8 KB responses, where a whole batch leaves in one call, it is worth "
              "almost half the CPU: 16.0 µs per request against 27.9 for raw Tokio."),
        src=["bench/syscalls.sh"],
    ),
    dict(
        title="One connection: median latency",
        sub="`/json`, microseconds · lower is better",
        max=36,
        rows=[("spin", 19, "19 µs"), ("giong", 28, "28 µs"), ("monoio", 28, "28 µs"),
              ("tokio", 29, "29 µs"), ("epoll", 29, "29 µs"), ("http", 30, "30 µs"),
              ("glommio", 33, "33 µs")],
        note=("With one connection every request finds a sleeping server. `spin_before_park(200 µs)` "
              "keeps the worker watching its ring: 47.9k requests/s instead of 32.4k. It costs CPU, "
              "20.5 instead of 13.6 µs per request, and the worker keeps its core busy between "
              "requests. Without it giong is the same as the others."),
        src=["bench/scale.sh"],
    ),
    dict(
        title="5,000 connections: CPU per request",
        sub="`/json`, microseconds of user plus kernel time · lower is better",
        max=9,
        rows=[("giong", 4.42, "4.42 µs"), ("epoll", 4.99, "4.99 µs"), ("spin", 5.12, "5.12 µs"),
              ("monoio", 5.25, "5.25 µs"), ("http", 5.37, "5.37 µs"), ("tokio", 5.74, "5.74 µs"),
              ("glommio", 8.29, "8.29 µs")],
        note=("giong is 11% below hand-written epoll and 16% below tuned Monoio, with the lowest "
              "p99 (26 ms; epoll 32, Monoio 44, raw Tokio 54). The likely reason is the provided "
              "buffer ring: receive buffers are reused while still warm in cache. At 50 and 1,000 "
              "connections giong, tuned Monoio and epoll are level."),
        src=["bench/scale.sh"],
    ),
    dict(
        title="HTTPS on one core",
        sub="requests per second, TLS 1.3, the same rustls on both sides, thin line from lowest to highest round · higher is better",
        max=160,
        rows=["`/json`, kept-alive",
              ("httptls", 144.8, "144.8k", 142.3, 152.9), ("axumtls", 81.0, "81.0k", 80.2, 82.8),
              "`/users`, 8 KB, kept-alive",
              ("httptls", 47.8, "47.8k", 43.4, 48.8), ("axumtls", 19.2, "19.2k", 18.7, 19.8),
              "a new handshake per request",
              ("httptls", 7.1, "7.1k", 6.7, 7.5), ("axumtls", 7.3, "7.3k", 6.9, 7.6)],
        note=("TLS costs giong 4% on `/json` and 14% on 8 KB responses. Handshakes are level: a "
              "handshake is almost all cryptography, and both sides use the same library for it. "
              "Most of the `/users` factor is there without TLS."),
        src=["bench/tls.sh"],
    ),
    dict(
        title="File reads on one core",
        sub="4 KiB at a random offset, one task, operations per second, thin line from lowest to highest round · higher is better",
        max=1500,
        rows=[("std", 1395, "1.40M", 1342, 1464), ("giong", 1014, "1.01M", 868, 1153),
              ("tokio1", 184, "184k", 171, 188), ("tokio4", 33, "33k", 32, 34)],
        note=("5.5 times Tokio, where every read is a hand-off to another thread and back; on giong "
              "it is one entry in a batch. A blocking `std` call is 28% faster for cached data. What "
              "giong buys is a worker that never waits when the data is not cached, which this "
              "benchmark does not measure."),
        src=["bench/fs.sh"],
    ),
    dict(
        title="Small responses at 50 connections: a tie",
        sub="`/json`, requests per second, thin line from lowest to highest round",
        max=220,
        rows=[("http", 195.7, "195.7k", 193.3, 212.6), ("monoio", 176.0, "176.0k", 167.2, 181.5),
              ("tokio", 174.1, "174.1k", 168.5, 178.7), ("giong", 173.5, "173.5k", 165.0, 180.4),
              ("epoll", 170.9, "170.9k", 159.0, 174.6), ("axum", 161.0, "161.0k", 153.2, 167.9),
              ("bun", 147.7, "147.7k", 136.7, 153.1), ("glommio", 143.9, "143.9k", 133.4, 149.0),
              ("node", 39.5, "39.5k", 37.6, 40.8)],
        note=("giong, tuned Monoio, raw Tokio and epoll are within noise: the kernel's TCP stack, "
              "not the runtime, is most of the cost, and `wrk` is the limit. `giong-http` reads "
              "higher and we do not know why. It costs 7% more CPU per request, as checking every "
              "header should, and keeps its core busier. Treat that row as a measurement of the setup."),
        src=["bench/run.sh"],
    ),
]

CODE = [
    dict(
        tab="HTTP on every core", file="giong-http",
        foot="One listener per worker; requests in progress are answered before a worker stops.",
        src='''use std::time::Duration;
use giong::net::TcpListener;
use giong_http::{Request, Response, Server};

async fn handler(req: Request) -> Response {
    match (req.method(), req.path()) {
        ("GET", "/health") => Response::text("ok"),
        ("POST", "/echo") => Response::new(200).body(req.body().to_vec()),
        _ => Response::new(404),
    }
}

fn main() -> std::io::Result<()> {
    giong::Builder::new()
        .shutdown_on_signals(true)                  // Ctrl-C / SIGTERM stop it gracefully
        .shutdown_grace(Duration::from_secs(10))    // requests in progress are answered
        .build()?
        .run(|_worker| async {
            // Every worker binds its own SO_REUSEPORT listener on the same port;
            // the kernel spreads connections across cores.
            let listener = TcpListener::bind("0.0.0.0:8080").expect("bind");
            Server::new().serve(listener, handler).await;
        })
}'''),
    dict(
        tab="HTTPS", file="giong-http + giong-tls",
        foot=("Turn on the `tls` feature of `giong-http`. Handlers, limits, timeouts and graceful "
              "shutdown are the same as for plain HTTP."),
        src='''let acceptor = giong_tls::TlsAcceptor::from_pem(
    &std::fs::read("cert.pem")?,
    &std::fs::read("key.pem")?,
)?;

// ... in each worker:
let listener = TcpListener::bind("0.0.0.0:8443").expect("bind");
Server::new().serve_tls(listener, acceptor.clone(), handler).await;'''),
    dict(
        tab="The bare runtime", file="giong",
        foot=("`recv()` lends a buffer from the worker's pool. An empty buffer means the peer "
              "closed the connection."),
        src='''use giong::net::TcpListener;

fn main() -> std::io::Result<()> {
    giong::Builder::new()
        .shutdown_on_signals(true)
        .build()?
        .run(|_worker| async {
            let listener = TcpListener::bind("0.0.0.0:8080").expect("bind");
            while let Some(Ok(stream)) = giong::until_shutdown(listener.accept()).await {
                giong::spawn(async move {
                    loop {
                        let Ok(data) = stream.recv().await else { return };
                        if data.is_empty() { return }       // peer closed
                        let reply = data.to_vec();
                        drop(data);                          // buffer back to the pool
                        if stream.write_all(reply).await.0.is_err() { return }
                    }
                });
            }
        })
}'''),
    dict(
        tab="UDP and files", file="giong::net · giong::fs",
        foot=("Receiving borrows a buffer from the pool; whatever you hand to the kernel goes in "
              "by value and comes back with the result."),
        src='''use giong::{fs, fs::File, net::UdpSocket};

async fn demo() -> std::io::Result<()> {
    let socket = UdpSocket::bind("127.0.0.1:9000")?;
    let (datagram, peer) = socket.recv_from().await?;      // multishot, cancel safe
    let reply = datagram.to_vec();
    drop(datagram);                                        // buffer back to the pool
    socket.send_to(reply, peer).await.0?;

    let file = File::create("log.txt").await?;
    file.write_all_at(b"hello\\n".to_vec(), 0).await.0?;
    file.sync_all().await?;
    let text = fs::read("log.txt").await?;
    assert_eq!(text, b"hello\\n");
    fs::rename("log.txt", "log.old").await
}'''),
]

START_SH = '''git clone https://github.com/giong-run/Giong.git && cd Giong
cargo test --workspace --all-features   # all three crates
cargo run --release --example echo      # TCP echo, port 7000
cargo run --release --example http      # HTTP/1.1, port 3006
curl localhost:3006/json'''

# ---------------------------------------------------------------- text
T = dict(
    title="giong: a thread-per-core async runtime for Linux on io_uring",
    desc=("giong is a thread-per-core async runtime for Rust on Linux, built directly on io_uring. "
          "One syscall per tick, not per operation. TCP, UDP, Unix sockets, files, HTTP/1.1 and TLS."),
    skip="Skip to content",
    nav=[("why", "Why"), ("numbers", "Numbers"), ("code", "Code"), ("design", "Design"),
         ("crates", "Crates"), ("limits", "Limits")],
    status=["<b>v0.2</b>", "Linux 6.0+", "Rust 1.80+", "MIT OR Apache-2.0"],
    h1="One syscall per tick, <em>not per operation.</em>",
    lede=("giong is a thread-per-core async runtime for Rust on Linux, built directly on io_uring. "
          "Every read, write, accept and close that one turn of the event loop produces enters "
          "the kernel in **a single `io_uring_enter`**."),
    cta_start="Run it", cta_numbers="See the numbers", copy="copy", copied="copied",
    tick_label="Illustration: the operations of one turn of the event loop enter the kernel in one system call.",
    tick_ops="operations queued", tick_unit1="syscall this tick", tick_unitn="syscalls this tick",
    vs_giong="giong", vs_epoll="epoll style",
    tick_cap=("One turn of the event loop, drawn to explain the mechanism. Measured on `/json` at "
              "50 connections: 0.03 system calls per request for giong, 2.05 for raw Tokio."),
    legend=("Named after **Thánh Gióng**, the child of Vietnamese legend who lay silent for three "
            "years and then rose a giant. giong costs next to nothing while idle and uses every "
            "core the moment load arrives."),
    facts=[("5,400", "lines", "of source in the runtime, with a comment on every `unsafe` block"),
           ("3", "", "dependencies: `io-uring`, `libc`, `socket2`"),
           ("169", "tests", "passing on three io_uring setups, clean under ThreadSanitizer and valgrind"),
           ("5.6", "MB", "resident memory after the 50-connection benchmark")],
    why=("why", "Where the design pays off",
         "giong commits to io_uring and to one thread per core, and to nothing else. "
         "These are the six things that buys."),
    pillars=[
        ("io_uring_enter · DEFER_TASKRUN · SINGLE_ISSUER", "One syscall per tick",
         "Every read, write, accept, close and cancellation a turn of the loop produces enters the "
         "kernel together, and every ready completion comes back in the same call. A request costs "
         "about **0.03 system calls**; an epoll-based server needs 2."),
        ("multishot accept · multishot recv · provided buffers", "Idle connections hold no buffer",
         "One submission lasts the life of a listener or connection. The kernel picks a buffer only "
         "when data arrives, so it is still warm in cache. At **5,000 connections** giong spends the "
         "least CPU per request of the runtimes measured."),
        ("tests/cancel.rs", "Cancel safe by construction",
         "Drop an I/O future mid-flight and its buffer moves to the driver until the kernel confirms "
         "the cancel. A `timeout` around `recv`, `recv_from` or `accept` loses no data, datagram "
         "or connection."),
        ("Builder::spin_before_park", "A latency dial",
         "Let a worker watch its ring for a while before it sleeps. On a single connection that is "
         "**19 µs median instead of 28** and 48% more requests. It costs CPU, so it is off by default."),
        ("giong::fs", "Files on the same ring",
         "Open, read, write, fsync, statx, rename and remove are entries in the worker's ring, next "
         "to the network. No pool of blocking threads. A file has no cursor: every read and write "
         "names its offset."),
        ("!Send · Rc · RefCell", "Shared nothing",
         "One thread per core, and a task never changes thread. Futures need not be `Send`; `Rc` "
         "and `RefCell` just work. No locks on the hot path."),
    ],
    numbers=("numbers", "Measured, with the script next to every number",
             "One core per server, loopback, medians of five interleaved rounds on a 4-vCPU virtual "
             "machine. giong is not faster than everything at everything. The charts say where the "
             "design pays off and where it does not."),
    caveats_h="What these numbers do not show",
    caveats=[
        "**One machine.** Everything is loopback inside a 4-vCPU virtual machine, where the load "
        "generator competes for the same few cores.",
        "**Not measured:** more than two cores, a physical network, and disks. The file benchmark "
        "reads cached data only.",
        "**8 KB responses:** giong and Monoio serve 63–64k requests/s, everything else 35–37k. Most "
        "of that gap comes from the virtual machine. On physical hardware expect a smaller one.",
        "**`/compute` is the same for everyone**, as it should be: the time goes into the function, "
        "not the runtime.",
        "**Two cores are measured and are not a ranking.** Two `wrk` threads are as likely to be "
        "the limit as the server.",
        "**Monoio is compared tuned**, with `DEFER_TASKRUN` passed in. Its default ring is about "
        "20% slower on the small response; comparing against that would flatter giong.",
    ],
    full_tables="Full tables, method and raw output: ",
    code=("code", "What the code looks like",
          "Every worker binds its own `SO_REUSEPORT` listener on the same port and the kernel "
          "spreads connections across cores. Buffers go to the kernel by value and come back "
          "with the result."),
    design=("design", "Each worker owns everything it touches",
            "A worker is a thread pinned to one core, with its own tasks, run queue, timer wheel, "
            "receive buffers and ring. Other threads reach it only through an injector and an eventfd."),
    loop=[("run ready tasks", "Tasks queue their reads, writes, accepts and closes. Nothing enters the kernel yet."),
          ("io_uring_enter", "Submit everything, collect everything that is ready. One system call."),
          ("fire timers", "A timing wheel in user space: arming and cancelling make no system call.")],
    loop_back="repeat",
    worker_items=["tasks (!Send)", "run queue", "timer wheel", "receive buffers", "its own io_uring"],
    worker_n="one per core",
    bus="eventfd + injector, only when another thread wakes a task",
    decisions=[
        ("Wakers", "A waker does not own its task; it holds a slot number and a handle to the worker. "
                   "Wakers are `Send + Sync` although tasks are not, without `unsafe`."),
        ("Writes", "Ownership passing: `write_all(buf)` returns `(result, buf)`. The kernel keeps a "
                   "pointer to the buffer until it is done, and this API makes that sound in Rust."),
        ("Backpressure", "A slow reader has its multishot receive paused, so TCP slows the sender. A dry "
                         "pool reclaims buffers that hold unread data. One stalled connection cannot "
                         "starve the rest."),
        ("Timers", "A hierarchical timing wheel, six levels of 64 slots, 1 ms ticks. Nearly every timer "
                   "is an I/O timeout that is cancelled before it fires, so both are O(1)."),
        ("Panics", "A panic in a task is caught and reported through its `JoinHandle`. One bad "
                   "connection does not take down the worker."),
        ("Shutdown", "With a grace period, requests in progress are answered. Then: cancel everything, "
                     "wait for the kernel to confirm, free. If confirmation does not come, leak on purpose."),
        ("UDP", "Multishot `recvmsg` on the same buffer pool as TCP. Every worker binds its own "
                "`SO_REUSEPORT` socket and `recv_from` is cancel safe."),
        ("Old kernels", "Three ring setups, tried in order: `DEFER_TASKRUN` (6.1+), `COOP_TASKRUN` "
                        "(6.0), no flags. The fallbacks are tested on every change by forcing them."),
    ],
    crates=("crates", "Three crates, one version number",
            "The runtime, and the two layers a service needs on top of it. Together they serve HTTPS "
            "at 144.8k requests/s on one core, against 81.0k for Axum on the same rustls."),
    crate=[
        ("giong", "The runtime: TCP, UDP, Unix sockets, files, timers, a cross-thread channel and "
                  "graceful shutdown.",
         ["`Builder::new().workers(n)`, one worker per core",
          "`TcpListener`, `TcpStream`, `UdpSocket`, `UnixListener`, `UnixStream`",
          "`fs::File` with `read_at` and `write_at`, `fs::read`, `rename`, `remove_file`",
          "`time::sleep`, `timeout`, `spawn`, `Handle::spawn_on`, `sync::mpsc`"],
         "about <b>5,400</b> lines · <b>115</b> tests"),
        ("giong-http", "A small, strict HTTP/1.1 server with one async handler.",
         ["Keep-alive, pipelining, bodies by `Content-Length` or chunked",
          "Limits on head and body size; timeouts for idle, slow and slow-reading clients",
          "Answers `400` to any request a proxy in front could read differently",
          "`Server::serve`, `serve_unix`, `serve_tls`"],
         "about <b>1,850</b> lines · <b>46</b> tests, among them 500,000 random inputs for the parser"),
        ("giong-tls", "Server-side TLS 1.2 and 1.3 on rustls with `ring`. No OpenSSL.",
         ["`TlsAcceptor::from_pem(cert, key)`, or bring your own `rustls::ServerConfig`",
          "`recv()` is cancel safe",
          "A cancelled write poisons the sending side instead of corrupting the record stream",
          "ALPN and a handshake timeout"],
         "about <b>390</b> lines · <b>8</b> tests · TLS costs 4% on `/json`"),
    ],
    verified=("verified", "What has been checked, and what has not",
              "A runtime that hands buffers to the kernel has to earn trust. This is the evidence "
              "so far, and the gaps in it."),
    checked_h="Checked", unchecked_h="Not checked, and you should know it",
    checked=[
        "**169 tests** across the three crates, repeated in release mode on all three io_uring setups "
        "without a failure.",
        "**ThreadSanitizer** over 157 tests: no data race.",
        "**valgrind memcheck** over the single-threaded tests: no invalid access, no leak.",
        "**Load:** over 250,000 connections opened and closed, 5,000 at once, the client killed "
        "abruptly six times. File descriptors return to the idle count and memory does not grow "
        "between runs.",
        "**Graceful shutdown under load:** SIGTERM during 40 slow requests. With a grace period, "
        "400 of 400 were answered; without one, 156 of 200.",
        "**CI** runs formatting, clippy, the documentation build, a build with Rust 1.80, the "
        "repeated tests, the fallback setups and ThreadSanitizer.",
    ],
    unchecked=[
        "giong has **not run in production**.",
        "**No real 6.0 kernel.** The fallback setups were forced on 6.18. That checks giong's code "
        "paths, not the behaviour of the old kernel itself.",
        "Neither sanitizer nor valgrind can see the kernel writing into memory, so cancellation "
        "safety rests on the design and on `tests/cancel.rs`.",
        "The cross-thread wake-up has not been model-checked with loom.",
        "`giong-tls` has had **no independent security audit**. The cryptography and the protocol "
        "are rustls's; giong's part is connecting rustls to the socket.",
    ],
    limits=("limits", "When giong is the wrong choice",
            "0.2 is complete enough to write real servers. It is also young, narrow on purpose, "
            "and Linux only."),
    limit=[
        ("Linux only", "Kernel 6.0 or newer. There is no epoll, kqueue or IOCP fallback, and there "
                       "will not be one."),
        ("Not Tokio's ecosystem", "Crates that need the Tokio runtime (hyper, sqlx, reqwest) do not "
                                  "run on giong. Crates that only use `std::future` do."),
        ("No HTTP/2", "`giong-http` speaks HTTP/1.1 and receives a body in full before it calls the "
                      "handler. A body over the limit is refused, not streamed."),
        ("TLS is server side only", "No TLS client, no kTLS, no certificate reload built in."),
        ("Tasks do not migrate", "An overloaded core is not helped by an idle one. Spread work "
                                 "yourself with `spawn_on` or a channel."),
        ("No asynchronous DNS", "`bind` and `connect` resolve names with a blocking call. Also "
                                "missing: Unix datagram sockets, multicast, reading directories."),
    ],
    else_h="Pick something else when",
    else_p=[
        "**Monoio** is the mature choice and runs in production at ByteDance. If you need an HTTP "
        "client, HTTP/2 or Tokio-compatible crates today, use it.",
        "**Glommio** is built for storage engines: Direct I/O and priority scheduling are its "
        "strengths.",
        "**Tokio** is the general-purpose runtime. giong is not trying to replace it for every workload.",
    ],
    start=("start", "Run it",
           "Clone the repository and run the suite. Version 0.2 is not on crates.io yet, so build "
           "it from source."),
    reqs=[("kernel", "**Linux 6.0 or newer**, 6.1+ recommended. Developed and tested on 6.18."),
          ("rust", "1.80 or newer."),
          ("windows", "giong does not build on Windows or macOS. Use WSL2 with a 6.x kernel, a Linux "
                      "virtual machine, or Docker started with `--security-opt seccomp=unconfined`."),
          ("license", "MIT OR Apache-2.0, at your option.")],
    roadmap="Roadmap", changelog="Changelog", contributing="Contributing", issues="Issues",
    foot="Dual-licensed under MIT or Apache-2.0.",
)

GH_ICON = ('<svg viewBox="0 0 16 16" aria-hidden="true"><path d="M8 0C3.58 0 0 3.58 0 8c0 3.54 2.29 6.53 '
           '5.47 7.59.4.07.55-.17.55-.38 0-.19-.01-.82-.01-1.49-2.01.37-2.53-.49-2.69-.94-.09-.23-.48-.94-'
           '.82-1.13-.28-.15-.68-.52-.01-.53.63-.01 1.08.58 1.23.82.72 1.21 1.87.87 2.33.66.07-.52.28-.87'
           '.51-1.07-1.78-.2-3.64-.89-3.64-3.95 0-.87.31-1.59.82-2.15-.08-.2-.36-1.02.08-2.12 0 0 .67-.21 '
           '2.2.82.64-.18 1.32-.27 2-.27s1.36.09 2 .27c1.53-1.04 2.2-.82 2.2-.82.44 1.1.16 1.92.08 2.12.51'
           '.56.82 1.27.82 2.15 0 3.07-1.87 3.75-3.65 3.95.29.25.54.73.54 1.48 0 1.07-.01 1.93-.01 2.2 0 '
           '.21.15.46.55.38A8.01 8.01 0 0 0 16 8c0-4.42-3.58-8-8-8Z"/></svg>')


# ---------------------------------------------------------------- pieces
def chart(c):
    rows = []
    for row in c["rows"]:
        if isinstance(row, str):
            rows.append(f'<li class="bar-group">{md(row)}</li>')
            continue
        key, value, label = row[0], row[1], row[2]
        cls = "bar" + (" is-giong" if key in G else " is-alt" if key in ALT else "")
        pct = lambda v: f"{100 * v / c['max']:.2f}"
        rng, tip = "", ""
        if len(row) == 5:
            rng = f'<span class="bar-range" style="--lo:{pct(row[3])};--hi:{pct(row[4])}"></span>'
        rows.append(
            f'<li class="{cls}"><span class="bar-name">{NAMES[key]}</span>'
            f'<span class="bar-track"><span class="bar-fill" style="--v:{pct(value)}"></span>{rng}</span>'
            f'<span class="bar-val">{label}</span></li>')
    src = " · ".join(f'<a href="{BLOB}{s}">{s}</a>' for s in c["src"])
    return (f'<figure class="chart"><figcaption class="chart-head"><h3>{c["title"]}</h3>'
            f'<p class="chart-sub">{md(c["sub"])}</p></figcaption>'
            f'<ol class="bars">{"".join(rows)}</ol>'
            f'<p class="chart-note">{md(c["note"])}</p>'
            f'<p class="chart-src">{src}</p></figure>')


def head(t, sec):
    eyebrow, h2, lede = sec
    return (f'<header class="sec-head"><p class="eyebrow">{eyebrow}</p><h2>{h2}</h2>'
            f'<p class="sec-lede">{md(lede)}</p></header>')


def page():
    t = T
    mark = (f'<picture><source media="(prefers-color-scheme: light)" srcset="assets/giong-mark-light.svg">'
            f'<img src="assets/giong-mark.svg" alt="" width="32" height="32"></picture>')
    nav = "".join(f'<a href="#{a}">{label}</a>' for a, label in t["nav"])
    status = "".join(f"<span>{s}</span>" for s in t["status"])
    facts = "".join(
        f'<div><dt>{v}{f"<small>{u}</small>" if u else ""}</dt><dd>{md(d)}</dd></div>'
        for v, u, d in t["facts"])
    pillars = "".join(
        f'<article class="pillar"><p class="tag">{tag}</p><h3>{h}</h3><p>{md(p)}</p></article>'
        for tag, h, p in t["pillars"])
    charts = "".join(chart(c) for c in CHARTS)
    caveats = "".join(f"<li>{md(c)}</li>" for c in t["caveats"])
    tabs = "".join(
        f'<button class="tab" role="tab" id="tab-{n}" aria-controls="panel-{n}" '
        f'aria-selected="{"true" if n == 0 else "false"}"><b>{c["tab"]}</b><span>{c["file"]}</span></button>'
        for n, c in enumerate(CODE))
    panels = "".join(
        f'<div class="panel" role="tabpanel" id="panel-{n}" aria-labelledby="tab-{n}">'
        f'<p class="panel-title">{c["tab"]} · {c["file"]}</p>'
        f'<pre class="code" tabindex="0"><code>{rust(c["src"])}</code></pre>'
        f'<p class="panel-foot">{md(c["foot"])}</p></div>'
        for n, c in enumerate(CODE))
    loop = "".join(
        f'<div class="loop-step{" is-enter" if n == 1 else ""}"><b>{b}</b><span>{s}</span></div>'
        for n, (b, s) in enumerate(t["loop"]))
    items = "".join(f"<li>{x}</li>" for x in t["worker_items"])
    workers = (f'<div class="worker"><b>worker 0</b><ul>{items}</ul></div>'
               f'<div class="worker"><b>worker 1</b><ul>{items}</ul></div>'
               f'<div class="worker is-n"><b>worker N</b>{t["worker_n"]}</div>'
               f'<p class="bus">{t["bus"]}</p>')
    decisions = "".join(f"<div><dt>{a}</dt><dd>{md(b)}</dd></div>" for a, b in t["decisions"])
    crates = "".join(
        f'<article class="crate"><h3>{name}</h3><p>{md(p)}</p>'
        f'<ul>{"".join(f"<li>{md(x)}</li>" for x in items_)}</ul>'
        f'<p class="fact">{code_only(fact)}</p></article>'
        for name, p, items_, fact in t["crate"])
    checked = "".join(f"<li>{md(x)}</li>" for x in t["checked"])
    unchecked = "".join(f"<li>{md(x)}</li>" for x in t["unchecked"])
    limits = "".join(f'<article class="limit"><h3>{h}</h3><p>{md(p)}</p></article>' for h, p in t["limit"])
    else_p = "".join(f"<p>{md(p)}</p>" for p in t["else_p"])
    reqs = "".join(f"<div><dt>{a}</dt><dd>{md(b)}</dd></div>" for a, b in t["reqs"])
    marks_e = "".join('<i class="on"></i>' if n < 14 else "<i></i>" for n in range(20))
    marks_g = '<i class="on"></i>' + "<i></i>" * 19
    clone = "git clone https://github.com/giong-run/Giong.git"

    return f'''<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1, viewport-fit=cover">
<title>{t["title"]}</title>
<meta name="description" content="{html.escape(t["desc"])}">
<link rel="canonical" href="{SITE}">
<meta name="theme-color" content="#0A0C10" media="(prefers-color-scheme: dark)">
<meta name="theme-color" content="#F4F6F9" media="(prefers-color-scheme: light)">
<meta property="og:type" content="website">
<meta property="og:site_name" content="giong">
<meta property="og:title" content="{html.escape(t["title"])}">
<meta property="og:description" content="{html.escape(t["desc"])}">
<meta property="og:url" content="{SITE}">
<meta property="og:image" content="{SITE}assets/social-preview.png">
<meta property="og:image:width" content="1280">
<meta property="og:image:height" content="640">
<meta property="og:locale" content="en_US">
<meta name="twitter:card" content="summary_large_image">
<link rel="icon" href="assets/favicon.ico" sizes="any">
<link rel="icon" href="assets/giong-icon.svg" type="image/svg+xml">
<link rel="apple-touch-icon" href="assets/giong-icon-180.png">
<link rel="preload" href="fonts/inter-latin.woff2" as="font" type="font/woff2" crossorigin>
<link rel="stylesheet" href="styles.css">
<script>document.documentElement.classList.add("js")</script>
</head>
<body>
<a class="skip" href="#main">{t["skip"]}</a>

<header class="nav">
  <div class="wrap nav-in">
    <a class="brand" href="#" aria-label="giong">{mark}<span>giong</span></a>
    <nav class="nav-links" aria-label="Sections">{nav}</nav>
    <div class="nav-side">
      <a class="btn btn-sm" href="{REPO}">{GH_ICON}<span>GitHub</span></a>
    </div>
  </div>
</header>

<main id="main">
<section class="hero">
  <div class="wrap hero-grid">
    <div class="hero-copy">
      <p class="status">{status}</p>
      <h1>{t["h1"]}</h1>
      <p class="lede">{md(t["lede"])}</p>
      <div class="cta">
        <a class="btn btn-main" href="#start">{t["cta_start"]}</a>
        <a class="btn" href="#numbers">{t["cta_numbers"]}</a>
      </div>
      <div class="cmd"><code id="clone">{clone}</code><button class="copy" type="button" data-copy="clone" data-done="{t["copied"]}">{t["copy"]}</button></div>
    </div>

    <figure class="tick" id="tick" data-unit1="{t["tick_unit1"]}" data-unitn="{t["tick_unitn"]}">
      <div class="tick-ring" role="img" aria-label="{t["tick_label"]}">
        <svg viewBox="0 0 400 400" aria-hidden="true">{ring()}</svg>
        <div class="tick-core" aria-hidden="true">
          <span class="tick-ops"><b data-ops>14</b> {t["tick_ops"]}</span>
          <span class="tick-num" data-sys>1</span>
          <span class="tick-unit" data-unit>{t["tick_unit1"]}</span>
        </div>
      </div>
      <div class="vs" aria-hidden="true">
        <div class="vs-row is-giong"><span>{t["vs_giong"]}</span><span class="vs-marks" data-marks-giong>{marks_g}</span><b data-n-giong>1</b></div>
        <div class="vs-row"><span>{t["vs_epoll"]}</span><span class="vs-marks" data-marks-epoll>{marks_e}</span><b data-n-epoll>14</b></div>
      </div>
      <figcaption>{md(t["tick_cap"])} <a href="{BLOB}bench/syscalls.sh">bench/syscalls.sh</a></figcaption>
    </figure>
  </div>
</section>

<section class="legend">
  <div class="wrap legend-in">
    <p>{md(t["legend"])}</p>
    <dl class="idle">{facts}</dl>
  </div>
</section>

<section class="sec" id="why">
  <div class="wrap">
    {head(t, t["why"])}
    <div class="pillars">{pillars}</div>
  </div>
</section>

<section class="sec" id="numbers">
  <div class="wrap">
    {head(t, t["numbers"])}
    <div class="charts">{charts}</div>
    <div class="caveats">
      <h3>{t["caveats_h"]}</h3>
      <div>
        <ul>{caveats}</ul>
        <p class="chart-src" style="margin-top:1.5rem">{t["full_tables"]}<a href="{REPO}#benchmarks">README</a> · <a href="{REPO}/tree/main/bench/results">bench/results/</a></p>
      </div>
    </div>
  </div>
</section>

<section class="sec" id="code">
  <div class="wrap">
    {head(t, t["code"])}
    <div class="code-grid">
      <div class="tabs" role="tablist" aria-label="{t["code"][1]}">{tabs}</div>
      <div class="panels">{panels}</div>
    </div>
  </div>
</section>

<section class="sec" id="design">
  <div class="wrap">
    {head(t, t["design"])}
    <div class="loop">{loop}<p class="loop-back">↺ {t["loop_back"]}</p></div>
    <div class="workers">{workers}</div>
    <dl class="decisions">{decisions}</dl>
  </div>
</section>

<section class="sec" id="crates">
  <div class="wrap">
    {head(t, t["crates"])}
    <div class="crates">{crates}</div>
  </div>
</section>

<section class="sec" id="verified">
  <div class="wrap">
    {head(t, t["verified"])}
    <div class="proof">
      <div class="proof-col"><h3>{t["checked_h"]}</h3><ul>{checked}</ul></div>
      <div class="proof-col"><h3>{t["unchecked_h"]}</h3><ul>{unchecked}</ul></div>
    </div>
  </div>
</section>

<section class="sec" id="limits">
  <div class="wrap">
    {head(t, t["limits"])}
    <div class="limits">{limits}</div>
    <div class="else"><h3>{t["else_h"]}</h3><div>{else_p}</div></div>
  </div>
</section>

<section class="sec" id="start">
  <div class="wrap">
    {head(t, t["start"])}
    <div class="start">
      <dl class="reqs">{reqs}</dl>
      <div>
        <pre class="code" tabindex="0"><code>{shell(START_SH)}</code></pre>
        <div class="start-cta">
          <a class="btn btn-main" href="{REPO}">{GH_ICON}GitHub</a>
          <a class="btn" href="{BLOB}ROADMAP.md">{t["roadmap"]}</a>
          <a class="btn" href="{BLOB}CHANGELOG.md">{t["changelog"]}</a>
        </div>
      </div>
    </div>
  </div>
</section>
</main>

<footer class="foot">
  <div class="wrap foot-in">
    <a class="brand" href="#" aria-label="giong">{mark.replace('width="32" height="32"', 'width="24" height="24"')}<span>giong</span></a>
    <nav class="foot-links" aria-label="Project">
      <a href="{REPO}">GitHub</a>
      <a href="{BLOB}ROADMAP.md">{t["roadmap"]}</a>
      <a href="{BLOB}CHANGELOG.md">{t["changelog"]}</a>
      <a href="{BLOB}CONTRIBUTING.md">{t["contributing"]}</a>
      <a href="{REPO}/issues">{t["issues"]}</a>
    </nav>
    <span>{t["foot"]}</span>
    <span class="ramp" aria-hidden="true"><i></i><i></i><i></i><i></i><i></i></span>
  </div>
</footer>

<script src="main.js" defer></script>
</body>
</html>
'''


if __name__ == "__main__":
    out = Path(sys.argv[1]) if len(sys.argv) > 1 else Path(__file__).resolve().parent / "public"
    out.mkdir(parents=True, exist_ok=True)
    (out / "index.html").write_text(page(), encoding="utf-8")
    print("wrote", out / "index.html")
