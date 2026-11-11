# giong

**Async runtime cho Linux theo mô hình thread-per-core, xây thẳng trên io_uring.**

[![CI](https://github.com/thekivashyn/Giong/actions/workflows/ci.yml/badge.svg)](https://github.com/thekivashyn/Giong/actions/workflows/ci.yml)
![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue)
![Platform: Linux 6.0+](https://img.shields.io/badge/platform-Linux%206.0%2B-informational)

Tên lấy từ Thánh Gióng: nằm im ba năm, cất tiếng là vươn vai thành người khổng lồ. giong
lúc rảnh gần như không tốn gì, có tải thì dùng hết mọi core.

*Bản đầy đủ (thiết kế, phương pháp đo, toàn bộ bảng số liệu) nằm ở [README.md](README.md),
viết bằng tiếng Anh. File này là bản tóm tắt.*

> **Trạng thái: 0.1.** Đủ để viết server thật: TCP, UDP, file, timer, nhiều core, tắt êm.
> 87 test đều qua, các bộ test đơn luồng sạch dưới valgrind. **Chưa** chạy production, chưa có
> TLS và HTTP/2.

## Vì sao là giong

- **Một syscall cho cả vòng lặp, không phải cho từng thao tác.** Mọi lệnh đọc, ghi, accept,
  đóng, huỷ của một vòng đi vào kernel bằng một lần `io_uring_enter`. Mỗi request tốn
  khoảng **0,13 syscall**; server dùng epoll tốn 2.
- **Núm chỉnh độ trễ.** `Builder::spin_before_park` cho worker canh ring thay vì đi ngủ.
  Với một kết nối: **nhiều hơn 46% request và độ trễ giảm một phần ba** so với mọi runtime
  đã đo.
- **Trụ được khi kết nối dồn lên.** Ở 1.000 và 5.000 kết nối, giong tốn ít CPU mỗi request
  nhất và có p99 thấp nhất trong các runtime đã đo.
- **An toàn khi huỷ thao tác.** Drop một future I/O không bao giờ giải phóng vùng nhớ kernel
  còn ghi vào; bọc `timeout` quanh `recv` hay `accept` không làm mất dữ liệu hay kết nối.
- **Đủ nhỏ để đọc hết.** Khoảng 4.700 dòng, ba thư viện phụ thuộc, mỗi khối `unsafe` đều
  có chú thích lý do.

Điều giong **không** phải: nhanh hơn tất cả ở mọi bài. Ở bài 50 kết nối quen thuộc, giong,
Monoio đã chỉnh và Tokio thuần nằm trong khoảng nhiễu của nhau, vì 90% chi phí là TCP stack
của kernel chứ không phải runtime.

## Kết quả chính

Mỗi server một core, qua loopback, trung vị của 5 vòng đo xen kẽ.

| | giong | Monoio (đã chỉnh) | Tokio thuần | epoll tự viết | Glommio |
|---|---|---|---|---|---|
| Syscall mỗi request | 0,13 | 0,11 | 2,04 | 2,02 | 2,10 |
| 1 kết nối: request/giây | **37,9k** ¹ | 25,1k | 26,3k | 25,5k | 23,3k |
| 1 kết nối: độ trễ trung vị | **24 µs** ¹ | 37 µs | 35 µs | 36 µs | 40 µs |
| 50 kết nối: request/giây | 143,5k (159,9k ¹) | 157,1k | 140,3k | 141,1k | 110,5k |
| 1.000 kết nối: CPU mỗi request | **5,47 µs** | 5,76 µs | 6,08 µs | 5,69 µs | 7,86 µs |
| 1.000 kết nối: p99 | **7,7 ms** | 17,1 ms | 101,6 ms | 8,4 ms | 12,8 ms |
| 5.000 kết nối: CPU mỗi request | **6,34 µs** | 6,59 µs | 7,47 µs | 7,04 µs | 13,69 µs |
| 5.000 kết nối: p99 | **37 ms** | 61 ms | 54 ms | 47 ms | 85 ms |

¹ với `spin_before_park(200 µs)`. Không bật thì giong đạt 25,8k request/giây, 36 µs, ngang
các runtime khác.

Cách đọc cho đúng:

- "Monoio (đã chỉnh)" là Monoio bật cờ `DEFER_TASKRUN`. Cấu hình mặc định của Monoio chậm
  hơn khoảng 20% ở response nhỏ; so với bản mặc định thì giong sẽ được lợi không công bằng.
- Về số request/giây, giong và Monoio đã chỉnh ngang nhau ở mọi mức kết nối (các khoảng đo
  chồng lên nhau). Ở 50 kết nối Monoio tốn ít CPU hơn 4%; từ 1.000 kết nối giong tốn ít hơn
  4–5%.
- Chưa đo: nhiều core (máy thử chỉ có 2 vCPU), mạng thật, và nhánh chính mới nhất của
  Monoio/Glommio.

## Yêu cầu

- **Linux, kernel 6.0 trở lên** (nên dùng 6.1+). Rust 1.80+.
- **Không chạy trên Windows hay macOS**, vì io_uring chỉ có trên Linux. Trên Windows:
  - WSL2 với kernel 6.x (`uname -r`; nếu còn 5.15 thì chạy `wsl --update`).
  - Máy ảo hoặc server Linux.
  - Docker, thêm `--security-opt seccomp=unconfined` vì cấu hình mặc định chặn io_uring.

## Chạy thử

```bash
git clone https://github.com/thekivashyn/Giong.git && cd Giong
cargo test                              # toàn bộ test
cargo run --release --example echo      # echo server, cổng 7000
cargo run --release --example http      # HTTP server, cổng 3006
bench/run.sh                            # thông lượng, so với 7 server khác
bench/scale.sh                          # từ 1 đến 5.000 kết nối
```

```rust
use giong::net::TcpListener;

fn main() -> std::io::Result<()> {
    giong::Builder::new()
        .shutdown_on_signals(true)              // Ctrl-C / SIGTERM → dừng êm
        .build()?
        .run(|_worker| async {
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

## Có gì trong hộp

| Nhóm | API |
|---|---|
| Khởi chạy | `block_on`, `Builder` (`workers`, `pin_cores`, `spin_before_park`, `recv_buffers`, `shutdown_on_signals`) |
| Task | `spawn`, `JoinHandle::abort`, `yield_now`; future không cần `Send` |
| TCP | `TcpListener`, `TcpStream` (`recv`, `read`, `write`, `write_all`) |
| UDP | `UdpSocket` (`send`, `recv`, `send_to`, `recv_from`) |
| File | `fs::File` (`read_at`, `write_at`, `write_all_at`, `sync_all`, `close`) |
| Thời gian | `sleep`, `sleep_until`, `timeout` |
| Tắt | `shutdown_signal`, `until_shutdown`, `Handle::shutdown` |
| Chéo thread | `Handle::spawn_on`, `sync::mpsc` |

## Giới hạn hiện tại

- Chưa có Unix socket, DNS bất đồng bộ, TLS, HTTP/2.
- Không tương thích hệ sinh thái Tokio (hyper, sqlx, reqwest không chạy được).
- Task không chuyển core.
- Tắt là cắt kết nối, chưa chờ request đang dở.

## Tài liệu khác

- Mã nguồn và issue: <https://github.com/thekivashyn/Giong>
- [README.md](README.md): thiết kế chi tiết, benchmark đầy đủ, kiểm thử, so sánh với Monoio và Glommio.
- [CONTRIBUTING.md](CONTRIBUTING.md), [ROADMAP.md](ROADMAP.md), [CHANGELOG.md](CHANGELOG.md).

## Giấy phép

Giấy phép kép, người dùng tự chọn: Apache-2.0 ([LICENSE-APACHE](LICENSE-APACHE)) hoặc MIT
([LICENSE-MIT](LICENSE-MIT)). Mọi đóng góp gửi vào dự án được hiểu là cấp theo cùng giấy
phép kép này, trừ khi người đóng góp nói rõ khác đi.
