# Changelog

All notable changes to giong are recorded here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the
project follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html). While the
version is `0.x`, a minor release (`0.1` → `0.2`) may contain breaking changes.

## [Unreleased]

## [0.1.0] - 2026-11-11

First release: a thread-per-core async runtime for Linux on io_uring, with TCP, UDP, files,
timers and cross-thread messaging. Not production-tested; see the limitations in the README.

### Added

- **Runtime**
  - `block_on` for a single-threaded runtime; `Builder` / `Runtime::run` for one worker
    thread per CPU, each pinned to its core with its own io_uring, run queue, timers and
    receive buffer pool.
  - `Handle` (`shutdown`, `is_shutdown`, `spawn_on`, `workers`), `handle()`, `worker_id()`.
  - Graceful shutdown: `shutdown_signal()`, `until_shutdown(fut)`, and
    `Builder::shutdown_on_signals` for SIGINT / SIGTERM.
  - A panic in a worker's main future stops the runtime and is propagated to the caller.
  - `Builder::spin_before_park`: an opt-in busy-poll mode that trades CPU for latency. A
    worker watches its ring for a while before sleeping, so it reacts to a request without
    a wake-up from the kernel.
- **Tasks**
  - `spawn` for `!Send` futures, `JoinHandle` (await, `abort`, `is_finished`), `JoinError`,
    `yield_now`.
  - A panic inside a task is caught and reported through its `JoinHandle`.
- **Networking**
  - `TcpListener` (`bind` with `SO_REUSEPORT`, `accept`, `from_std`) on multishot accept.
  - `TcpStream` (`connect`, `recv`, `read`, `write`, `write_all`, `shutdown`,
    `set_nodelay`, `peer_addr`, `local_addr`, `from_std`).
  - `recv` uses multishot receive with a kernel-provided buffer ring and returns a
    `RecvBuf` borrowed from the worker's pool.
  - `UdpSocket` (`bind`, `connect`, `send`, `recv`, `send_to`, `recv_from`, `from_std`),
    IPv4 and IPv6.
  - Ownership-passing buffer traits `IoBuf` / `IoBufMut`, implemented for `Vec<u8>`,
    `String`, `Box<[u8]>`, `Rc<[u8]>`, `Arc<[u8]>`, `&'static [u8]` and `&'static str`.
- **Files**
  - `fs::File` and `fs::OpenOptions`: open, positional `read_at` / `write_at` /
    `write_all_at`, `sync_all`, `sync_data`, `close`. Every operation goes through the
    ring; there is no blocking thread pool.
- **Time**
  - `sleep`, `sleep_until`, `timeout`, backed by a six-level hierarchical timing wheel
    with 1 ms resolution. Timers never fire early.
- **Sync**
  - `sync::mpsc` unbounded channel, usable across workers and from threads outside the
    runtime.
- **Safety and backpressure**
  - Cancellation safety: when an in-flight operation's future is dropped, its buffer is
    kept alive by the driver until the kernel confirms the cancellation.
  - `recv` and `accept` are cancel safe: wrapping them in `timeout` loses no data and no
    connection.
  - A slow or stalled reader cannot starve other connections: its multishot receive is
    paused and its backlog is moved off the shared pool.
  - A multishot accept pauses once 1024 accepted connections are waiting.
  - On shutdown every operation is cancelled and awaited before memory is released.
- **Examples**: `echo`, `http` (HTTP/1.1 with keep-alive, pipelining, idle and write
  timeouts, size limits), `udp_echo` and `cat`.
- **Benchmarks**: `bench/run.sh` (throughput), `bench/scale.sh` (one connection to
  thousands, with CPU time per request) and `bench/syscalls.sh`, with seven reference
  servers (Node.js, Bun, Axum, raw Tokio, hand-written epoll, Monoio, Glommio) and the
  recorded results.
- **Tests**: 62 tests covering the executor, timers, TCP, cancellation, buffer exhaustion
  and multi-worker behaviour; `scripts/valgrind.sh` for a memcheck pass.

### Known limitations

- No Unix sockets and no asynchronous DNS (`bind` and `connect` resolve names blocking).
- No TLS and no HTTP/2.
- Not compatible with crates that require the Tokio runtime.
- Tasks never migrate between workers.
- Shutdown drops remaining connections; it does not wait for requests in progress.
- Not yet measured on more than one core or on a physical network.
- The declared minimum Rust version (1.80) and the fallback paths for kernel 6.0 have not
  been exercised yet.
