//! A TCP echo server on every core.
//!
//!     cargo run --release --example echo -- 127.0.0.1:7000
//!     nc 127.0.0.1 7000

use std::time::Duration;

use giong::net::{TcpListener, TcpStream};
use giong::time::sleep;

async fn echo(stream: TcpStream) {
    loop {
        // `recv` hands out a buffer from the worker's kernel-shared pool.
        let Ok(data) = stream.recv().await else {
            return;
        };
        if data.is_empty() {
            return; // peer closed
        }
        let reply = data.to_vec();
        drop(data); // give the pool buffer back before awaiting again
        if stream.write_all(reply).await.0.is_err() {
            return;
        }
    }
}

fn main() -> std::io::Result<()> {
    let addr = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "127.0.0.1:7000".into());
    println!("echo server on {addr} (Ctrl-C to stop)");

    giong::Builder::new()
        .shutdown_on_signals(true)
        .build()?
        .run(move |worker| {
            let addr = addr.clone();
            async move {
                // Every worker binds its own SO_REUSEPORT listener on the same port.
                let listener = TcpListener::bind(&addr).expect("bind");
                while let Some(conn) = giong::until_shutdown(listener.accept()).await {
                    match conn {
                        Ok(stream) => {
                            giong::spawn(echo(stream));
                        }
                        Err(e) => {
                            eprintln!("worker {worker}: accept failed: {e}");
                            sleep(Duration::from_millis(50)).await;
                        }
                    }
                }
            }
        })
}
