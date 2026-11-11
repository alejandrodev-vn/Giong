//! Prints files to standard output, reading them through io_uring.
//!
//!     cargo run --release --example cat -- Cargo.toml README.md

use std::io::Write;

use giong::fs::File;

async fn cat(path: &str) -> std::io::Result<()> {
    let file = File::open(path).await?;
    let stdout = std::io::stdout();
    let mut buf = Vec::with_capacity(64 * 1024);
    let mut offset = 0u64;
    loop {
        // Reads are positional: we keep the offset, the file has no cursor.
        let (res, b) = file.read_at(buf, offset).await;
        buf = b;
        let n = res?;
        if n == 0 {
            return file.close().await;
        }
        stdout.lock().write_all(&buf)?;
        offset += n as u64;
    }
}

fn main() -> std::io::Result<()> {
    let paths: Vec<String> = std::env::args().skip(1).collect();
    if paths.is_empty() {
        eprintln!("usage: cat <file>...");
        std::process::exit(2);
    }
    giong::block_on(async move {
        for path in &paths {
            if let Err(e) = cat(path).await {
                eprintln!("cat: {path}: {e}");
                std::process::exit(1);
            }
        }
        Ok(())
    })
}
