# giong

Async runtime cho Linux theo mô hình **thread-per-core**, xây thẳng trên **io_uring**.

Tên lấy từ Thánh Gióng: nằm im ba năm, cất tiếng là vươn vai thành người khổng lồ. Runtime này lúc rảnh gần như không tốn gì, có tải thì dùng hết mọi core.

> **Trạng thái:** v0.1, bản đầu tiên chạy được đầy đủ. 62 test đều qua, đã kiểm tra bộ nhớ bằng valgrind. Chưa chạy production, chưa có TLS/HTTP/2 — xem mục [Giới hạn](#giới-hạn-hiện-tại).

## Yêu cầu

- **Linux, kernel 6.0 trở lên** (nên dùng 6.1+). Phát triển và kiểm thử trên 6.18.
- Rust 1.80+.
- **Không chạy trên Windows hay macOS**, vì io_uring chỉ có trên Linux. Trên Windows có ba cách:
  - WSL2 với kernel 6.x. Kiểm tra bằng `uname -r`; nếu còn 5.15 thì chạy `wsl --update`.
  - Máy ảo hoặc server Linux.
  - Docker, nhưng phải thêm `--security-opt seccomp=unconfined` vì cấu hình mặc định của Docker chặn io_uring.

## Chạy thử

```bash
cargo test                                   # 62 test
cargo run --release --example echo           # echo server, cổng 7000
cargo run --release --example http           # HTTP server, cổng 3006
curl localhost:3006/json
bench/run.sh                                 # benchmark với 7 server khác (cần wrk, node, bun)
```

## Ví dụ

```rust
use giong::net::TcpListener;

fn main() -> std::io::Result<()> {
    giong::Builder::new()
        .shutdown_on_signals(true)          // Ctrl-C / SIGTERM → dừng êm
        .build()?
        .run(|_worker| async {
            // Mỗi worker tự bind một listener SO_REUSEPORT trên cùng cổng;
            // kernel chia kết nối cho các core.
            let listener = TcpListener::bind("0.0.0.0:8080").expect("bind");
            while let Some(Ok(stream)) = giong::until_shutdown(listener.accept()).await {
                giong::spawn(async move {
                    loop {
                        let Ok(data) = stream.recv().await else { return };
                        if data.is_empty() { return }       // phía kia đã đóng
                        let reply = data.to_vec();
                        drop(data);                          // trả buffer về pool
                        if stream.write_all(reply).await.0.is_err() { return }
                    }
                });
            }
        })
}
```

## Kiến trúc

```
        worker 0                 worker 1                 worker N
 ┌────────────────────┐   ┌────────────────────┐   ┌────────────────────┐
 │ task (!Send)       │   │                    │   │                    │
 │ run queue          │   │        ...         │   │        ...         │
 │ timer wheel        │   │                    │   │                    │
 │ pool buffer nhận   │   │                    │   │                    │
 │ io_uring riêng     │   │                    │   │                    │
 └─────────┬──────────┘   └─────────┬──────────┘   └─────────┬──────────┘
           │   eventfd + injector (chỉ dùng khi thread khác đánh thức)   │
           └──────────────────────────┴──────────────────────────────────┘
```

Mỗi worker là một thread ghim vào một core, sở hữu riêng mọi thứ. Vòng lặp của nó:

```
chạy các task đang sẵn sàng  →  io_uring_enter (gửi mọi SQE + nhận mọi CQE)  →  bắn timer  →  lặp lại
```

Các quyết định thiết kế chính:

| Phần | Cách làm | Vì sao |
|---|---|---|
| **Luồng** | Thread-per-core, không chia sẻ gì. Task không bao giờ đổi thread. | Future không cần `Send`, dùng thẳng `Rc`/`RefCell`. Đường nóng không có lock. |
| **Waker** | Waker không sở hữu task, chỉ giữ số slot và handle tới worker. Cùng thread thì đẩy vào hàng đợi cục bộ; khác thread thì qua injector + eventfd. | Waker bắt buộc phải `Send + Sync` trong khi task thì không. Cách này an toàn mà không cần `unsafe`. |
| **I/O** | Mọi thao tác của một vòng lặp (đọc, ghi, accept, đóng, huỷ) đi vào kernel bằng **một** `io_uring_enter`. Dùng `DEFER_TASKRUN` + `SINGLE_ISSUER`. | Ít syscall nhất có thể, completion chỉ được xử lý khi chính worker vào kernel. |
| **Accept / nhận** | Multishot accept; multishot recv với **provided buffer ring**. | Một SQE cho cả vòng đời listener/kết nối. Kernel chỉ lấy buffer khi có dữ liệu, nên kết nối rảnh không giữ bộ nhớ. |
| **Ghi** | Trao quyền sở hữu buffer: `write_all(buf)` trả lại `(kết quả, buf)`. | Kernel giữ con trỏ tới buffer cho đến khi xong; API này làm điều đó đúng theo kiểu Rust. |
| **Huỷ thao tác** | Future bị drop giữa chừng thì buffer chuyển cho driver giữ, gửi lệnh huỷ, chỉ giải phóng khi kernel báo xong. | Tránh use-after-free — lỗi kinh điển của runtime kiểu completion. |
| **Backpressure** | Kết nối đọc chậm: tạm dừng multishot (TCP tự hãm phía gửi), dữ liệu dư chép ra heap. Pool cạn: thu hồi buffer đang nằm chờ. Accept tồn quá 1024: tạm dừng. | Một kết nối treo không thể làm đói các kết nối khác. |
| **Timer** | Timing wheel phân cấp (6 tầng × 64 ô, độ phân giải 1 ms) trong user space. | Đặt và huỷ timer là O(1), không tốn syscall. Gần như mọi timer là timeout I/O bị huỷ trước khi nổ. |
| **Panic** | Panic trong task được bắt lại, trả về qua `JoinHandle`. | Một kết nối lỗi không kéo sập worker. |
| **Tắt** | Huỷ mọi thao tác, chờ kernel xác nhận rồi mới giải phóng. Nếu không xác nhận được thì cố ý rò bộ nhớ thay vì liều. | An toàn bộ nhớ quan trọng hơn vài MB lúc thoát. |

## API

| Nhóm | Hàm |
|---|---|
| Khởi chạy | `giong::block_on(fut)` (một thread), `Builder::new().workers(n).build()?.run(\|i\| async { .. })` (nhiều core) |
| Cấu hình | `.workers(n)`, `.pin_cores(bool)`, `.ring_entries(n)`, `.recv_buffers(số, cỡ)`, `.shutdown_on_signals(bool)` |
| Task | `spawn(fut) -> JoinHandle<T>`, `handle.abort()`, `yield_now()` |
| Mạng | `TcpListener::bind` / `accept`, `TcpStream::connect` / `recv` / `read` / `write` / `write_all` / `shutdown` / `set_nodelay` |
| Thời gian | `time::sleep`, `sleep_until`, `timeout(dur, fut)` |
| Tắt | `shutdown_signal()`, `until_shutdown(fut)`, `Handle::shutdown()` |
| Chéo thread | `Handle::spawn_on(worker, \|\| async { .. })`, `sync::mpsc::channel()` |

Ba điều cần nhớ khi dùng:

1. `recv()` trả về `RecvBuf` mượn từ pool. **Buffer rỗng nghĩa là phía kia đã đóng.** Chép phần cần dùng rồi drop sớm, đừng giữ qua `.await`.
2. `recv()` và `accept()` an toàn khi bị huỷ: bọc trong `timeout` không làm mất dữ liệu hay kết nối.
3. Timer có độ phân giải 1 ms và không bao giờ nổ sớm.

## Kiểm thử

62 test (`cargo test`), chạy lặp 15–25 lần ở chế độ release không lỗi lần nào:

| File | Kiểm tra gì |
|---|---|
| `tests/executor.rs` (14) | spawn/join, panic, abort, drop khi runtime kết thúc, chạy nhiều runtime nối tiếp |
| `tests/time.rs` (7) | sleep, timeout, thứ tự timer, 5.000 timer |
| `src/time.rs` (4) | Timing wheel so với mô hình tham chiếu: 87 chuỗi thao tác ngẫu nhiên, timer từ 1 ms tới 30 ngày |
| `tests/net.rs` (13) | echo, truyền 8 MB hai chiều không sai byte nào, EOF, 500 và 1.300 kết nối, client `std` thường |
| `tests/cancel.rs` (10) | **Buffer còn sống cho tới khi kernel xác nhận huỷ**, pool 4 buffer cho 32 kết nối, kết nối treo không làm đói kết nối khác, tắt runtime khi mọi thứ đang dở |
| `tests/multi.rs` (10) | nhiều worker, đánh thức chéo thread, channel 30.000 tin, SO_REUSEPORT, SIGTERM, worker panic |
| Khác (4) | slab, 3 ví dụ trong tài liệu |

Ngoài ra:

- **valgrind memcheck** trên 43 test đơn luồng (`scripts/valgrind.sh`): 0 lỗi truy cập bộ nhớ, 0 rò rỉ.
- **Tải thật** lên HTTP server mẫu: 267.000 kết nối mở-đóng liên tục, 5.000 kết nối đồng thời, giết client đột ngột 6 lần. Sau mỗi đợt số file descriptor về đúng mức ban đầu (7). RSS lúc rảnh là 2,4 MB, sau 5.000 kết nối dừng ở ~15 MB và không tăng thêm qua các đợt.

Những gì **chưa** kiểm chứng được:

- valgrind 3.22 không chạy được các test đa luồng: nó coi `io_uring_enter` là không chặn nên các thread tự khoá nhau. Phần đa luồng viết hoàn toàn bằng Rust an toàn (Mutex, atomic) và có test chạy thật, nhưng chưa qua ThreadSanitizer (cần Rust nightly).
- valgrind không thấy được việc kernel ghi vào bộ nhớ, nên tính đúng của phần huỷ thao tác dựa vào thiết kế và test `a_cancelled_operation_owns_its_buffer_until_the_kernel_confirms`.
- Các nhánh dự phòng cho kernel 6.0 (không có `DEFER_TASKRUN`) chưa được chạy thử, vì máy thử chỉ có kernel 6.18.

## Benchmark

Sáu server cùng làm ba route, mỗi server chạy trên **một core**, `wrk` chạy ở core khác với 50 kết nối. Đo 5 vòng xen kẽ, lấy trung vị. Máy: VM 2 vCPU Xeon 2.1 GHz, kernel 6.18, Rust 1.97, Node 22, Bun 1.4.

**Request/giây (trung vị, trong ngoặc là thấp nhất–cao nhất):**

| Server | `/json` | `/users` (JSON 8 KB) | `/compute` (fib 25) |
|---|---|---|---|
| Node.js (`http`) | 40.8k | 15.0k | 1.2k |
| Bun | 121.5k | 18.5k | 2.3k |
| Axum (Tokio + hyper) | 130.8k | 18.0k | 4.8k |
| Tokio thuần, tự parse HTTP | 145.7k (136–151k) | 34.0k (33–36k) | 5.0k |
| epoll tự viết, không async | 141.5k (133–142k) | 35.2k (33–37k) | 4.9k |
| **giong** | **157.5k (141–172k)** | **58.2k (54–59k)** | 4.4k |

**Độ trễ p99:** `/json` giong 0,80 ms, Tokio thuần 0,90 ms; `/users` giong 1,79 ms, Tokio thuần 2,37 ms.

**RSS sau tải:** giong 5,1 MB, Tokio thuần 4,8 MB, epoll tự viết 3,4 MB, Bun 28 MB, Node 84 MB.

**Cơ chế** (`bench/syscalls.sh`):

| | Syscall mỗi request | CPU kernel mỗi request, `/json` | CPU kernel mỗi request, `/users` |
|---|---|---|---|
| Tokio thuần | 2,04 | 4,9 µs | 13,8 µs |
| epoll tự viết | 2,03 | 5,0 µs | 12,5 µs |
| **giong** | **0,09** | 4,8 µs | **5,1 µs** |

Cách đọc các con số này cho đúng:

- **giong dùng ít syscall hơn khoảng 22 lần** (0,09 so với 2,04 mỗi request). Đây là kết quả chắc chắn nhất, đúng với mục tiêu thiết kế.
- **`/json`: không có khác biệt đo được so với Tokio thuần.** Lượt đo này giong hơn khoảng 8%, lượt đo sau (bảng bên dưới) Tokio thuần lại nhỉnh hơn 2%; hai khoảng đo chồng lên nhau. Thời gian kernel mỗi request gần như bằng nhau (4,8 so với 4,9 µs): với request nhỏ, phần tốn là TCP stack chứ không phải việc vào/ra kernel. Giảm syscall không cắt được phần này.
- **`/users`: nhanh hơn ~70%, nhưng phần lớn là do môi trường đo.** Khi cho server và `wrk` chạy chung một core, hai bên gần bằng nhau (Tokio 48–49k, giong 50–52k). Khoảng cách lớn chỉ xuất hiện khi chạy khác core: server epoll gửi từng response một, mỗi lần lại đánh thức vCPU đang rảnh của `wrk`, việc này rất đắt trong máy ảo. giong gửi cả lô trong một syscall nên đánh thức ít lần hơn. Lợi ích này có thật, nhưng trên máy thật với card mạng thật, mức chênh nhiều khả năng nhỏ hơn đáng kể.
- **`/compute`: giong chậm hơn ~10%, không phải do runtime.** Mã máy của hàm `fib` trong hai binary giống hệt nhau, và độ trễ một request đơn của hai bên bằng nhau (24,8k so với 24,2k req/s với 1 kết nối). Nhiều khả năng do vị trí hàm trong binary (code alignment). Với việc nặng CPU, runtime nào cũng như nhau.
- **Chưa đo:** nhiều core (máy thử chỉ có 2 vCPU, không đủ để tách server khỏi máy bắn tải) và mạng thật.

Chạy lại: `bench/run.sh [số vòng] [giây mỗi lần đo]`. Kết quả nằm ở `bench/results/`.

### So với Monoio và Glommio

Cùng ba route, cùng parser, viết trên Monoio 0.2.4 và Glommio 0.9.0 (`bench/monoio`, `bench/glommio`). Một lượt đo riêng, 5 vòng xen kẽ, trung vị (thấp nhất–cao nhất):

| Server | `/json` | `/users` | `/compute` | Pipelining | Syscall mỗi request | RSS |
|---|---|---|---|---|---|---|
| **giong** | 147.8k (143–161k) | 56.8k (50–59k) | 5.1k | 1,51 triệu | 0,10 | 5,1 MB |
| Monoio (mặc định) | 119.1k (114–123k) | 56.5k (52–58k) | 5.2k | 1,15 triệu | 0,08 | 4,2 MB |
| Glommio | 111.4k (101–118k) | 32.2k (32–33k) | 4.4k | 1,04 triệu | 2,09 | 15,7 MB |
| Tokio thuần | 150.9k (146–154k) | 34.6k (30–36k) | 4.8k | 1,45 triệu | 2,04 | 4,8 MB |

- **giong và Monoio cùng một hạng.** Cả hai gom mọi thao tác vào `io_uring_enter` (dưới 0,1 syscall mỗi request) và hoà nhau ở `/users`.
- **Khoảng cách ở `/json` là do cấu hình mặc định, không phải do thiết kế.** Monoio tạo ring không bật cờ nào. Truyền `COOP_TASKRUN` hoặc `DEFER_TASKRUN` qua `uring_builder` (hai dòng code) thì Monoio đạt 143–151k, ngang giong (`bench/results/io-uring-runtimes/monoio-ring-flags.txt`). giong bật sẵn `DEFER_TASKRUN`.
- **Glommio ở hạng epoll trên đường mạng.** Nó thử `recv`/`send` trực tiếp trước rồi mới dùng io_uring để chờ, nên vẫn tốn 2 syscall mỗi request. Glommio được thiết kế cho lưu trữ (Direct I/O) và lập lịch theo độ ưu tiên, không tối ưu cho bài này.
- **`/compute` của giong lượt này là 5,1k**, lượt trước 4,4k, sau khi binary đổi vì sửa tài liệu. Khớp với nhận định đây là nhiễu do vị trí mã trong binary.
- Vẫn là một core, qua loopback, trong máy ảo. Bản crates.io mới nhất của Monoio là tháng 8/2024 và của Glommio là tháng 3/2024; nhánh chính của họ có thể đã khác.

## Giới hạn hiện tại

- **Chỉ có TCP.** Chưa có UDP, Unix socket, file I/O, DNS bất đồng bộ (`bind`/`connect` phân giải tên theo kiểu chặn).
- **Chưa có TLS và HTTP/2.** HTTP server trong `examples/` là ví dụ tối thiểu (HTTP/1.1, bỏ qua body), không phải thư viện. Production cần đặt sau Nginx/Envoy hoặc viết thêm tầng TLS.
- **Không tương thích hệ sinh thái Tokio.** Crate phụ thuộc Tokio (hyper, sqlx, reqwest…) không chạy được trên giong. Crate chỉ dùng `std::future` thì chạy được.
- **Task không chuyển core.** Một core quá tải không được core khác gánh hộ; cần tự chia việc bằng `spawn_on` hoặc channel.
- **Tắt là cắt.** Khi main future của worker kết thúc, các kết nối còn lại bị drop ngay; chưa có cơ chế chờ request đang dở.
- **Chưa dùng:** fixed file, zero-copy send, `SQPOLL`, busy polling.

## Cấu trúc thư mục

```
src/
  lib.rs        điểm vào, tài liệu
  runtime.rs    Builder, Runtime, Handle, vòng lặp sự kiện, shutdown, tín hiệu
  executor.rs   task, waker, spawn, JoinHandle
  driver.rs     io_uring: gửi/nhận, theo dõi thao tác, huỷ, backpressure
  op.rs         một thao tác đơn (1 SQE → 1 CQE) dưới dạng future
  buf.rs        IoBuf/IoBufMut, pool buffer chia sẻ với kernel, RecvBuf
  net.rs        TcpListener, TcpStream
  time.rs       timing wheel, sleep, timeout
  sync.rs       channel mpsc
  slab.rs       slab tối giản
examples/       echo.rs, http.rs
tests/          executor, time, net, cancel, multi
bench/          run.sh, syscalls.sh, 7 server so sánh, results/
scripts/        valgrind.sh
.github/        CI và mẫu pull request
```

Khoảng 3.900 dòng mã nguồn và 1.400 dòng test. Phụ thuộc: `io-uring`, `libc`, `socket2`. `unsafe` chỉ nằm ở bốn file (`buf`, `driver`, `net`, `runtime`), mỗi chỗ đều có chú thích lý do.

## Đóng góp và lộ trình

- [CONTRIBUTING.md](CONTRIBUTING.md): cách build, các kiểm tra bắt buộc, quy tắc cho `unsafe` và cho việc huỷ thao tác, quy ước commit.
- [ROADMAP.md](ROADMAP.md): kế hoạch phát triển theo từng mốc, và những thứ dự án cố ý không làm.
- [CHANGELOG.md](CHANGELOG.md): nhật ký thay đổi theo từng phiên bản.

Ba tài liệu này viết bằng tiếng Anh để người ngoài Việt Nam cũng đóng góp được.

## Giấy phép

giong dùng giấy phép kép, người dùng tự chọn một trong hai:

- Apache License 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT ([LICENSE-MIT](LICENSE-MIT))

Mọi đóng góp gửi vào dự án được hiểu là cấp theo cùng giấy phép kép này, không kèm điều kiện nào khác, trừ khi người đóng góp nói rõ khác đi.
