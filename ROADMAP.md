# Roadmap

Where giong is going, in the order we intend to get there. Nothing here has a date:
a milestone is done when its exit criteria are met. [Open an issue](https://github.com/thekivashyn/Giong/issues) before starting on
an item so the work is not duplicated.

## Goal

A runtime for **network servers on Linux** that makes the fewest possible system calls and
keeps tail latency low, by committing fully to io_uring and to one thread per core.

## Non-goals

These are deliberate. Proposals that need them will be declined.

- **Other operating systems.** No epoll, kqueue or IOCP fallback.
- **Work stealing.** Tasks stay on the worker that created them.
- **A general-purpose runtime.** giong is not trying to replace Tokio for every workload.
- **`Send` futures as a requirement.** The API stays usable with `Rc` and `RefCell`.

## 0.1 — Foundation (released)

Executor, io_uring driver, TCP, UDP, files, timers, thread-per-core runtime, busy-poll
mode, cross-thread channel, cancellation safety, backpressure, tests and benchmarks. See [CHANGELOG.md](CHANGELOG.md).

## 0.2 — Prove it

The 0.1 numbers come from a 2-vCPU virtual machine over loopback. Before adding features,
find out what is actually true.

- [ ] Benchmark on a multi-core machine with the load generator on a separate host and a
      physical network card.
- [ ] Measure scaling from 1 to N workers.
- [x] Compare against Monoio and Glommio on one core over loopback (see the README).
- [ ] Repeat that comparison on several cores and a physical network.
- [ ] Run the multi-threaded tests under ThreadSanitizer (nightly).
- [ ] Model-check the cross-thread wake-up handshake with `loom`.
- [ ] Run the test suite on kernel 6.0 and 6.1 to exercise the ring setup fallbacks.
- [ ] Verify the declared minimum Rust version in CI, and correct it if needed.
- [ ] Fuzz the HTTP example's request parser.
- [ ] Add a soak test: hours of connection churn with fd and memory checks.

**Exit criteria:** published multi-core results with their method, and no open memory- or
thread-safety finding.

## 0.3 — Breadth

What a real server needs beyond TCP.

- [ ] Graceful connection draining on shutdown (wait for requests in progress, with a
      deadline).
- [ ] Vectored writes (`writev`) so a response head and body need no copy.
- [ ] Unix domain sockets.
- [ ] More file operations: metadata, rename, remove, directories, `fallocate`.
- [ ] Multishot receive for UDP.
- [ ] Asynchronous DNS resolution (today `bind` and `connect` resolve names blocking).
- [ ] A bounded channel and a oneshot channel in `sync`.
- [ ] `spawn_on` that returns a handle to the spawned task's result.

**Exit criteria:** every item above with tests, including cancellation tests for each new
operation.

## 0.4 — Speed

Each item lands only with a benchmark showing what it buys.

- [ ] Registered ring fd and fixed files (direct descriptors) to skip fd lookups.
- [ ] Zero-copy send for large responses.
- [ ] Receive bundles (several buffers per completion).
- [ ] Optional `SQPOLL` mode.
- [ ] Optional NAPI busy polling.
- [ ] Per-worker statistics (tasks, operations, buffer pool pressure) as a public API.

**Exit criteria:** a documented, reproducible gain for every option that is merged.

## 0.5 — Ecosystem

Making giong usable for applications rather than only for hand-written servers.

- [ ] `giong-http`: an HTTP/1.1 server library grown out of the example.
- [ ] TLS through `rustls`.
- [ ] Adapters for the `futures-io` / Tokio I/O traits, so protocol crates that are generic
      over the transport can run on giong.
- [ ] Decide, based on demand, whether a hyper integration is worth maintaining.

**Exit criteria:** an HTTPS server example serving real browsers.

## 1.0

A stable API. Requires: every public item documented, the limitations section of the
README reduced to deliberate non-goals, and at least one production user reporting back.
