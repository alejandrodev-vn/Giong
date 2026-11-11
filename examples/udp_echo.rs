//! A UDP echo server.
//!
//!     cargo run --release --example udp_echo -- 127.0.0.1:7001
//!     echo hello | nc -u -w1 127.0.0.1 7001

use giong::net::UdpSocket;

fn main() -> std::io::Result<()> {
    let addr = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "127.0.0.1:7001".into());
    giong::block_on(async move {
        let socket = UdpSocket::bind(&addr)?;
        println!("udp echo on {}", socket.local_addr()?);
        // One buffer for the whole lifetime of the server: it goes into each
        // operation by value and comes back with the result.
        let mut buf = Vec::with_capacity(64 * 1024);
        loop {
            let (res, b) = socket.recv_from(buf).await;
            let (_len, peer) = res?;
            let (res, b) = socket.send_to(b, peer).await;
            res?;
            buf = b;
        }
    })
}
