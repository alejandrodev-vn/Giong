# giong-tls

TLS phía server cho runtime [giong](../README.vi.md), xây trên [rustls](https://github.com/rustls/rustls) (TLS 1.2 và 1.3, mật mã của `ring`, không cần OpenSSL).

*[English](README.md)*

```rust
use giong::net::TcpListener;
use giong_tls::TlsAcceptor;

fn main() -> std::io::Result<()> {
    let acceptor = TlsAcceptor::from_pem(
        &std::fs::read("cert.pem")?,
        &std::fs::read("key.pem")?,
    )?;

    giong::block_on(async move {
        let listener = TcpListener::bind("0.0.0.0:8443")?;
        loop {
            let tcp = listener.accept().await?;
            let acceptor = acceptor.clone();
            giong::spawn(async move {
                let Ok(tls) = acceptor.accept(tcp).await else { return };
                while let Ok(data) = tls.recv().await {
                    if data.is_empty() {
                        break; // phía kia đã đóng
                    }
                    if tls.write_all(data).await.0.is_err() {
                        break;
                    }
                }
                let _ = tls.shutdown().await;
            });
        }
    })
}
```

Muốn HTTPS thì không cần tự viết vòng lặp này: bật feature `tls` của [`giong-http`](../giong-http/README.vi.md) và gọi `Server::serve_tls`.

## API

| | |
|---|---|
| `TlsAcceptor::from_pem(cert, key)` | Một chuỗi chứng chỉ và khoá riêng ở dạng PEM. TLS 1.2 và 1.3, bộ mã mặc định của rustls, không xác thực client, ALPN `http/1.1`. |
| `TlsAcceptor::new(config)` | Dùng `rustls::ServerConfig` tự cấu hình (nhiều chứng chỉ theo SNI, xác thực client, ALPN khác). Crate xuất lại `rustls` để dùng đúng phiên bản. |
| `.handshake_timeout(thời gian)` | Thời gian tối đa cho một handshake, mặc định 10 s. |
| `acceptor.accept(tcp)` | Thực hiện handshake, trả về `TlsStream`. |
| `tls.recv()` | Dữ liệu đã giải mã. Buffer rỗng nghĩa là phía kia đã đóng. |
| `tls.write_all(buf)` | Mã hoá và gửi hết `buf`, rồi trả lại buffer, giống `TcpStream::write_all`. |
| `tls.shutdown()` | Gửi `close_notify` rồi đóng chiều gửi. |
| `tls.alpn_protocol()`, `tls.tcp()` | Giao thức đã thoả thuận qua ALPN; socket TCP bên dưới. |

Bốn điều cần nhớ:

1. `recv()` an toàn khi bị huỷ: bọc trong `timeout` không làm mất dữ liệu.
2. `write_all()` thì không. Nếu future bị huỷ giữa chừng một bản ghi TLS, luồng bản ghi đã bị cắt ngang, nên mọi lần gửi sau đó trả về lỗi thay vì gửi tiếp dữ liệu hỏng. Dữ liệu phía kia đã gửi vẫn nhận được.
3. Một task nhận trong khi task khác gửi trên cùng một `TlsStream` là được; các bản ghi không bị trộn vào nhau.
4. Dữ liệu nhận được nguyên vẹn luôn được trả về trước lỗi xảy ra sau nó.

## Hiệu năng

Đo bằng `bench/tls.sh` trên một core, cùng rustls và `ring` ở cả hai bên: HTTPS qua `giong-http` đạt 144,8k req/s ở `/json` so với 81,0k của Axum + rustls; so với HTTP thường trên giong, TLS tốn 4% ở `/json` và 14% với response 8 KB. Số handshake mỗi giây ngang nhau (7,1k so với 7,3k), vì phần đó gần như chỉ là mật mã. Chi tiết và cách đọc ở [README chính (tiếng Anh)](../README.md#https).

## Chưa có

Phía client (`connect`), kTLS, tải lại chứng chỉ khi đang chạy (tự làm được qua `ServerConfig::cert_resolver`). Crate chưa được kiểm toán bảo mật độc lập; phần mật mã và giao thức là của rustls.

## Kiểm thử

`cargo test -p giong-tls`: 7 test trên socket thật với client rustls (TLS 1.2 và 1.3, 4 MB mỗi chiều, ALPN, `recv` bị huỷ liên tục, hai task cùng gửi, ghi bị huỷ giữa chừng, handshake hỏng hoặc treo) và 1 ví dụ trong tài liệu. Chín test HTTPS đầu-cuối nằm trong `giong-http` (`cargo test -p giong-http --features tls`).
