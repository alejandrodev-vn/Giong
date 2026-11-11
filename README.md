# giong

**A thread-per-core async runtime for Linux, built directly on io_uring.**

Named after Thánh Gióng, the child of Vietnamese legend who lay silent for three years and
then rose a giant: giong costs next to nothing while idle and uses every core the moment
load arrives.

*[Đọc bằng tiếng Việt](README.vi.md)*

> **Status: 0.1.** Complete enough to write real servers: TCP, UDP, files, timers,
> multi-core, graceful shutdown. 87 tests pass and the single-threaded suites are clean
> under valgrind. It has **not** run in production, and there is no TLS or HTTP/2 yet.
> See [Limitations](#limitations).

## Why giong

- **One system call per tick, not per operation.** Every read, write, accept, close and
  cancellation a turn of the event loop produces goes to the kernel in a single
  `io_uring_enter`, which also returns every completion that is ready. A request costs
  about **0.13 system calls**; an epoll-based server needs 2.
- **A latency dial.** `Builder::spin_before_park` lets a worker watch its ring instead of
  going to sleep. On a single connection that is **46% more requests and a third less
  latency** than any other runtime measured here.
- **It holds up when connections pile up.** At 1,000 and 5,000 connections giong spends
  the least CPU per request and has the lowest p99 of the runtimes measured.
- **Cancellation-safe by construction.** Dropping an I/O future never frees memory the
  kernel may still write to, and wrapping `recv` or `accept` in a timeout loses neither
  data nor connections.
- **Small enough to read.** About 4,700 lines, three dependencies, and a comment on every
  `unsafe` block.

What it is **not**: faster than everything at everything. On the classic 50-connection
benchmark giong, a tuned Monoio and raw Tokio are within noise of each other, because the
kernel's TCP stack, not the runtime, is 90% of the cost. The numbers below say where the
design pays off and where it does not.

## Results at a glance

One core per server, loopback, medians of five interleaved rounds. Full tables and method
under [Benchmarks](#benchmarks).

| | giong | Monoio (tuned) | Raw Tokio | Hand-written epoll | Glommio |
|---|---|---|---|---|---|
| System calls per request | 0.13 | 0.11 | 2.04 | 2.02 | 2.10 |
| 1 connection: requests/s | **37.9k** ¹ | 25.1k | 26.3k | 25.5k | 23.3k |
| 1 connection: median latency | **24 µs** ¹ | 37 µs | 35 µs | 36 µs | 40 µs |
| 50 connections: requests/s | 143.5k (159.9k ¹) | 157.1k | 140.3k | 141.1k | 110.5k |
| 1,000 connections: CPU per request | **5.47 µs** | 5.76 µs | 6.08 µs | 5.69 µs | 7.86 µs |
| 1,000 connections: p99 latency | **7.7 ms** | 17.1 ms | 101.6 ms | 8.4 ms | 12.8 ms |
| 5,000 connections: CPU per request | **6.34 µs** | 6.59 µs | 7.47 µs | 7.04 µs | 13.69 µs |
| 5,000 connections: p99 latency | **37 ms** | 61 ms | 54 ms | 47 ms | 85 ms |

¹ with `spin_before_park(200 µs)`. Without it giong does 25.8k requests/s at 36 µs on one
connection, the same as the others.

## Requirements

- **Linux 6.0 or newer** (6.1+ recommended). Developed and tested on 6.18.
- Rust 1.80 or newer.
- giong does **not** build on Windows or macOS: io_uring is a Linux interface. On Windows
  use WSL2 with a 6.x kernel (`uname -r`; run `wsl --update` if it is older), a Linux
  virtual machine, or Docker started with `--security-opt seccomp=unconfined` (Docker's
  default profile blocks io_uring).

## Quick start

```bash
cargo test                              # the whole suite
cargo run --release --example echo      # TCP echo server on port 7000
cargo run --release --example http      # HTTP/1.1 server on port 3006
cargo run --release --example udp_echo  # UDP echo server on port 7001
cargo run --release --example cat -- Cargo.toml   # read a file through the ring
```

A TCP echo server on every core:

```rust
use giong::net::TcpListener;

fn main() -> std::io::Result<()> {
    giong::Builder::new()
        .shutdown_on_signals(true)              // Ctrl-C / SIGTERM stop it gracefully
        .build()?
        .run(|_worker| async {
            // Every worker binds its own SO_REUSEPORT listener on the same port;
            // the kernel spreads connections across cores.
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
}
```

UDP and files work the same way: the buffer goes in by value and comes back with the result.

```rust
use giong::{fs::File, net::UdpSocket};

async fn demo() -> std::io::Result<()> {
    let socket = UdpSocket::bind("127.0.0.1:9000")?;
    let (res, buf) = socket.recv_from(Vec::with_capacity(1500)).await;
    let (_len, peer) = res?;
    socket.send_to(buf, peer).await.0?;

    let file = File::create("log.txt").await?;
    file.write_all_at(b"hello\n".to_vec(), 0).await.0?;
    file.sync_all().await
}
```

For a latency-sensitive service on dedicated cores:

```rust
let runtime = giong::Builder::new()
    .spin_before_park(std::time::Duration::from_micros(200))
    .build()?;
```

## What is in the box

| Area | API |
|---|---|
| Starting | `giong::block_on(fut)` for one thread; `Builder::new().workers(n).build()?.run(\|i\| async { .. })` for one worker per core |
| Configuration | `.workers(n)`, `.pin_cores(bool)`, `.spin_before_park(duration)`, `.ring_entries(n)`, `.recv_buffers(count, size)`, `.shutdown_on_signals(bool)` |
| Tasks | `spawn(fut) -> JoinHandle<T>`, `handle.abort()`, `yield_now()`; futures need not be `Send` |
| TCP | `TcpListener::bind` / `accept`; `TcpStream::connect` / `recv` / `read` / `write` / `write_all` / `shutdown` / `set_nodelay` |
| UDP | `UdpSocket::bind` / `connect` / `send` / `recv` / `send_to` / `recv_from` |
| Files | `fs::File::open` / `create` / `options`; `read_at` / `write_at` / `write_all_at` / `sync_all` / `sync_data` / `close` |
| Time | `time::sleep`, `sleep_until`, `timeout(duration, fut)` |
| Shutdown | `shutdown_signal()`, `until_shutdown(fut)`, `Handle::shutdown()` |
| Across threads | `Handle::spawn_on(worker, \|\| async { .. })`, `sync::mpsc::channel()` |

Three things to remember:

1. `TcpStream::recv()` returns a `RecvBuf` borrowed from the worker's pool. **An empty
   buffer means the peer closed the connection.** Copy what you need and drop it; do not
   hold it across an `.await`.
2. `recv()` and `accept()` are cancel safe: a `timeout` around them loses nothing.
3. Timers have 1 ms resolution and never fire early.

## Design

```
        worker 0                 worker 1                 worker N
 ┌────────────────────┐   ┌────────────────────┐   ┌────────────────────┐
 │ tasks (!Send)      │   │                    │   │                    │
 │ run queue          │   │        ...         │   │        ...         │
 │ timer wheel        │   │                    │   │                    │
 │ receive buffers    │   │                    │   │                    │
 │ its own io_uring   │   │                    │   │                    │
 └─────────┬──────────┘   └─────────┬──────────┘   └─────────┬──────────┘
           │   eventfd + injector (only when another thread wakes a task)  │
           └──────────────────────────┴────────────────────────────────────┘
```

Each worker is a thread pinned to one core that owns everything it touches. Its loop:

```
run the tasks that are ready  →  io_uring_enter (submit everything, collect everything)  →  fire timers  →  repeat
```

| Part | How | Why |
|---|---|---|
| **Threads** | Thread-per-core, shared-nothing. A task never changes thread. | Futures need not be `Send`; `Rc` and `RefCell` just work. No locks on the hot path. |
| **Wakers** | A waker does not own its task; it holds a slot number and a handle to the worker. Same thread: push onto the local queue. Other thread: injector plus eventfd. | Wakers must be `Send + Sync` although tasks are not. This gets there without `unsafe`. |
| **I/O** | Everything a tick produces enters the kernel in one `io_uring_enter`, with `DEFER_TASKRUN` and `SINGLE_ISSUER`. | The fewest system calls possible; completions are processed only when the worker asks. |
| **Accept and receive** | Multishot accept; multishot receive with a provided buffer ring. | One submission for the life of a listener or connection. The kernel picks a buffer only when data arrives, so the buffer is warm in cache and idle connections hold none. |
| **Writes, UDP, files** | Ownership passing: `write_all(buf)` returns `(result, buf)`. | The kernel keeps a pointer to the buffer until it is done; this API makes that sound in Rust. |
| **Cancellation** | When a future is dropped mid-flight, its buffer moves to the driver, a cancel is sent, and the memory is freed only when the kernel confirms. | Avoids use-after-free, the classic bug of completion-based runtimes. |
| **Backpressure** | A slow reader has its multishot receive paused (TCP then slows the sender) and its backlog copied off the pool. A dry pool reclaims buffers that hold unread data. An accept backlog over 1,024 pauses the accept. | One stalled connection cannot starve the rest. |
| **Timers** | A hierarchical timing wheel in user space (six levels of 64 slots, 1 ms ticks). | Arming and cancelling are O(1) and make no system call. Nearly every timer is an I/O timeout that is cancelled before it fires. |
| **Latency dial** | Optionally busy-poll the ring for a while before sleeping. | A sleeping thread must be woken by an interrupt and rescheduled, which costs tens of microseconds, more in a virtual machine. |
| **Panics** | A panic in a task is caught and reported through its `JoinHandle`. | One bad connection does not take down the worker. |
| **Shutdown** | Cancel everything, wait for the kernel to confirm, then free. If confirmation does not come, leak on purpose. | Memory safety matters more than a few megabytes at exit. |

## Benchmarks

Every server implements the same three routes with the same hand-written HTTP parser
(`bench/`): `/json` (a 27-byte body), `/users` (8 KB of JSON built per request) and
`/compute` (a recursive Fibonacci). Each server is pinned to one core; `wrk` runs on the
other. Servers are measured in interleaved rounds and the tables show the median, with the
range in parentheses. Machine: a 2-vCPU virtual machine (Xeon 2.1 GHz), Linux 6.18,
Rust 1.97, Node 22, Bun 1.4, Monoio 0.2.4, Glommio 0.9.0.

"Monoio (tuned)" is Monoio with `DEFER_TASKRUN` passed through `uring_builder`. Monoio's
default ring has no such flag and is about 20% slower on the small response; comparing
against the default would flatter giong.

### Throughput at 50 connections

| Server | `/json` | `/users` (8 KB) | `/compute` | Pipelined `/json` | Memory |
|---|---|---|---|---|---|
| Node.js (`http`) | 40.3k | 15.2k | 1.3k | 61k | 84.6 MB |
| Bun | 123.0k | 18.1k | 2.3k | 113k | 28.1 MB |
| Axum (Tokio + hyper) | 134.9k | 18.4k | 4.8k | 18k | 4.9 MB |
| Raw Tokio, own parser | 144.3k (139–156k) | 35.2k (34–37k) | 4.9k | 1.49M | 4.8 MB |
| Hand-written epoll | 145.8k (132–148k) | 34.7k (28–36k) | 4.9k | 1.45M | 3.3 MB |
| Glommio | 121.2k (116–125k) | 32.8k (30–34k) | 5.1k | 1.06M | 15.8 MB |
| Monoio (default) | 121.5k (110–124k) | 60.4k (53–61k) | 5.3k | 1.14M | 4.2 MB |
| Monoio (tuned) | 156.6k (142–158k) | 59.5k (57–60k) | 5.4k | 1.48M | 4.1 MB |
| **giong** | 148.4k (141–159k) | 56.3k (53–58k) | 5.4k | 1.46M | 6.0 MB |

Requests per second; "Pipelined" sends 16 requests per write; "Memory" is resident memory
after the run.

- **The three io_uring-style results and raw Tokio are within noise on `/json`.** At this
  rate `wrk` is the bottleneck: the fast servers are idle 10–20% of the time.
- **`/users` separates the batching runtimes from the rest**: giong and Monoio at 56–60k,
  everything else at 33–35k. They send a whole batch of responses in one system call; a
  server that issues one `send` per response wakes the other vCPU each time, which is
  expensive in a virtual machine. On physical hardware expect a smaller gap.
- **Between giong and Monoio, `/users` is a tie.** Monoio is 6% ahead in this table; in
  the `syscalls.sh` run recorded next to it giong is ahead of tuned Monoio by 4%.
- **`/compute` is the same for everyone**, as it should be: the time goes into the
  function, not the runtime. Differences of a few percent here are code-layout noise.

### From one connection to thousands

`bench/scale.sh`, `/json`. "CPU" is the server's user plus kernel time per request, read
from `/proc`. It is the number to trust when the load generator is the limit.

| Connections | | giong | giong + spin | Monoio (tuned) | Raw Tokio | Hand-written epoll | Glommio |
|---|---|---|---|---|---|---|---|
| 1 | requests/s | 25.8k | **37.9k** | 25.1k | 26.3k | 25.5k | 23.3k |
| | median latency | 36 µs | **24 µs** | 37 µs | 35 µs | 36 µs | 40 µs |
| 50 | requests/s | 143.5k | **159.9k** | 157.1k | 140.3k | 141.1k | 110.5k |
| | CPU per request | 5.60 µs | 6.13 µs | **5.35 µs** | 5.98 µs | 5.73 µs | 8.90 µs |
| 1,000 | requests/s | **146.8k** | 144.8k | 143.5k | 131.3k | 133.8k | 123.9k |
| | CPU per request | **5.47 µs** | 6.26 µs | 5.76 µs | 6.08 µs | 5.69 µs | 7.86 µs |
| | p99 latency | 7.7 ms | **7.5 ms** | 17.1 ms | 101.6 ms | 8.4 ms | 12.8 ms |
| 5,000 | requests/s | 100.7k | 101.3k | **105.9k** | 88.1k | 98.4k | 71.6k |
| | CPU per request | **6.34 µs** | 6.73 µs | 6.59 µs | 7.47 µs | 7.04 µs | 13.69 µs |
| | p99 latency | **37 ms** | 44 ms | 61 ms | 54 ms | 47 ms | 85 ms |

- **Spinning is the clearest win.** With one connection every request finds a sleeping
  server; keeping it awake cuts median latency from 36 to 24 µs and raises throughput by
  46%. It costs CPU: a spinning worker keeps its core busy between requests. Glommio has
  the same option; in a single run it made no difference to Glommio's numbers.
- **giong is the cheapest per request from 1,000 connections up**, by 4–5% over tuned
  Monoio and 10–15% over raw Tokio, and has the lowest p99. The likely reason is the
  provided buffer ring: receive buffers are reused while still warm in cache, where the
  other servers keep a read buffer per connection.
- **At 50 connections tuned Monoio is 4% cheaper per request**, and at 5,000 it serves 5%
  more requests while using more CPU for each. Throughput ranges overlap at every
  connection count; treat giong and tuned Monoio as equals on throughput.
- p99 is the noisiest number in the table. Raw Tokio's 101.6 ms at 1,000 connections is
  the median of five rounds, not a single outlier, but it is still one machine.

### Where the time goes

`bench/syscalls.sh`, `/json`, 50 connections:

| | System calls per request | User CPU | Kernel CPU | Total CPU |
|---|---|---|---|---|
| Raw Tokio | 2.04 | 0.74 µs | 4.97 µs | 5.72 µs |
| Hand-written epoll | 2.02 | 0.40 µs | 4.89 µs | 5.29 µs |
| Glommio | 2.10 | 1.26 µs | 7.32 µs | 8.59 µs |
| Monoio (default) | 0.05 | 0.35 µs | 7.60 µs | 7.96 µs |
| Monoio (tuned) | 0.11 | 0.43 µs | 5.05 µs | 5.48 µs |
| **giong** | 0.13 | 0.46 µs | 4.87 µs | 5.34 µs |

The system-call count is the structural difference and the most robust result here. Its
effect on total cost is modest on this machine because one request costs about 5 µs of
kernel time whichever way it is delivered: on loopback the server's `send` also runs the
client's receive path. Callgrind puts the runtime's own share at roughly 1,300 instructions
per request, a few percent of the total.

### What has not been measured

- **More than one core.** The test machine has two vCPUs, one of which runs the load
  generator. Thread-per-core scaling is designed for but not demonstrated.
- **A physical network.** Everything above is loopback inside a virtual machine.
- **Newer Monoio and Glommio.** The comparison uses the latest crates.io releases
  (August and March 2024); their main branches may differ.

Reproduce with `bench/run.sh`, `bench/scale.sh` and `bench/syscalls.sh`. Recorded output is
in `bench/results/`.

## Testing and safety

87 tests (`cargo test`), run in a loop in release mode without a failure:

| Suite | What it checks |
|---|---|
| `tests/executor.rs` | spawn and join, panics, abort, teardown, runtimes run back to back |
| `tests/time.rs`, `src/time.rs` | sleep, timeout, ordering, 5,000 timers; the timing wheel against a reference model over 87 random operation sequences with timers from 1 ms to 30 days |
| `tests/net.rs` | echo, 8 MB each way without a wrong byte, EOF, 500 and 1,300 connections, plain `std` clients |
| `tests/udp.rs` | connected and unconnected sockets, truncation, IPv6, 2,000 echoed datagrams, `std` interop |
| `tests/fs.rs` | positional reads and writes, holes, a 6 MB file, error cases, one file shared by 64 tasks, descriptors closed exactly once |
| `tests/cancel.rs` | **a buffer stays alive until the kernel confirms the cancellation**; 4 buffers for 32 connections; a stalled connection cannot starve another; teardown with everything in flight |
| `tests/multi.rs` | several workers, cross-thread wake-ups, 30,000 channel messages, `SO_REUSEPORT`, SIGTERM, a panicking worker |
| `tests/spin.rs` | the busy-poll mode behaves like the default: I/O, timers, cross-thread wake-ups, shutdown |

Also:

- **valgrind memcheck** over the 60 single-threaded tests (`scripts/valgrind.sh`): no
  invalid access, no leak.
- **Load** on the example HTTP server: over 250,000 connections opened and closed, 5,000
  concurrent connections, the client killed abruptly six times. File descriptors always
  return to the idle count; memory settles and does not grow between runs.

Not verified, and you should know it:

- valgrind 3.22 cannot run the multi-threaded tests (it treats `io_uring_enter` as
  non-blocking, so threads starve each other). The cross-thread code is safe Rust and has
  tests, but has not been through ThreadSanitizer or loom.
- valgrind cannot see the kernel writing into memory, so cancellation safety rests on the
  design and on `tests/cancel.rs`, not on memcheck.
- The fallback paths for kernel 6.0 (no `DEFER_TASKRUN`) and the declared minimum Rust
  version have not been exercised: only kernel 6.18 and Rust 1.97 were available.

## Compared with Monoio and Glommio

| | giong 0.1 | Monoio 0.2.4 | Glommio 0.9.0 |
|---|---|---|---|
| Platforms | Linux ≥ 6.0 | Linux ≥ 5.6, with an epoll/kqueue fallback (runs on macOS) | Linux ≥ 5.8, needs a raised memlock limit |
| Receiving | multishot, kernel-provided buffers | one submission per read, caller's buffer | direct `recv`, io_uring to wait |
| Accepting | multishot | one submission per accept | one submission per accept |
| Buffer API | ownership passing | ownership passing | borrowed (`AsyncRead` / `AsyncWrite`) |
| Timeout around a read | loses no data | needs the separate `cancelable_*` API | — |
| Ring defaults | `DEFER_TASKRUN` on | no flags; pass your own builder | — |
| Busy-poll option | yes | no | yes |
| Multi-core | built-in builder with graceful shutdown and signals | `#[monoio::main(threads = N)]` | executor pools placed by CPU topology |
| Scheduling | FIFO | FIFO | weighted queues with latency classes |
| I/O | TCP, UDP, files | TCP, UDP, Unix sockets, files | TCP, UDP, Unix sockets, Direct I/O files |
| Ecosystem | none yet | TLS, HTTP, codecs, Tokio compatibility layer | channels, semaphores, rate control |
| Size | ~4,700 lines | ~25,000 lines | ~40,500 lines |

Monoio is the mature choice and runs in production at ByteDance; if you need TLS, HTTP or
Tokio-compatible crates today, use it. Glommio is built for storage engines: Direct I/O and
priority scheduling are its strengths, not HTTP. giong's case is narrower: the receive path
(multishot with provided buffers, cancel safe without a second API), good defaults, the
latency dial, and code small enough to audit.

## Limitations

- **No Unix sockets and no asynchronous DNS.** `bind` and `connect` resolve names with a
  blocking call.
- **No TLS and no HTTP/2.** The HTTP server in `examples/` is a minimal example, not a
  library. Put giong behind a TLS-terminating proxy for now.
- **Not compatible with the Tokio ecosystem.** Crates that need the Tokio runtime (hyper,
  sqlx, reqwest) do not run on giong. Crates that only use `std::future` do.
- **Tasks do not migrate.** An overloaded core is not helped by an idle one; spread work
  yourself with `spawn_on` or a channel.
- **Shutdown cuts connections.** When a worker's main future returns, remaining
  connections are dropped; there is no draining of requests in progress yet.
- **UDP receives are single-shot** and file operations are limited to open, read, write,
  sync and close.
- **Unused so far:** fixed files, zero-copy send, `SQPOLL`.

## Project

- [CONTRIBUTING.md](CONTRIBUTING.md) — building, required checks, the rules for `unsafe`
  and cancellation, commit conventions.
- [ROADMAP.md](ROADMAP.md) — milestones and deliberate non-goals.
- [CHANGELOG.md](CHANGELOG.md) — what changed in each release.

```
src/
  lib.rs        entry point and docs
  runtime.rs    Builder, Runtime, Handle, the event loop, shutdown, signals
  executor.rs   tasks, wakers, spawn, JoinHandle
  driver.rs     io_uring: submit and reap, operation tracking, cancellation, backpressure
  op.rs         a single operation (one SQE, one CQE) as a future
  buf.rs        IoBuf / IoBufMut, the kernel-shared buffer pool, RecvBuf
  net.rs        TcpListener, TcpStream, UdpSocket
  fs.rs         File, OpenOptions
  time.rs       timing wheel, sleep, timeout
  sync.rs       mpsc channel
examples/       echo.rs, http.rs, udp_echo.rs, cat.rs
tests/          executor, time, net, udp, fs, cancel, multi, spin
bench/          run.sh, scale.sh, syscalls.sh, eight servers, results/
scripts/        valgrind.sh
```

## License

Dual-licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option. Unless you state otherwise, any contribution you submit is licensed the
same way, without additional terms.
