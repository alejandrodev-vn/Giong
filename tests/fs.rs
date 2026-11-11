//! File I/O through the ring.

use std::io::ErrorKind;
use std::os::fd::AsRawFd;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use giong::fs::{File, OpenOptions};
use giong::time::sleep;

/// A unique path in the temp directory, removed when dropped.
struct TempPath(PathBuf);

impl TempPath {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!("giong-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_file(&path);
        Self(path)
    }
}

impl Drop for TempPath {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn fd_is_open(fd: i32) -> bool {
    unsafe { libc::fcntl(fd, libc::F_GETFD) != -1 }
}

fn payload(len: usize, seed: u64) -> Vec<u8> {
    let mut x = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x as u8
        })
        .collect()
}

#[test]
fn create_write_read_roundtrip() {
    let tmp = TempPath::new("roundtrip");
    giong::block_on(async {
        let file = File::create(&tmp.0).await.unwrap();
        let (res, buf) = file.write_all_at(b"hello, giong".to_vec(), 0).await;
        res.unwrap();
        assert_eq!(buf, b"hello, giong");
        file.sync_all().await.unwrap();
        assert_eq!(file.len().unwrap(), 12);
        file.close().await.unwrap();

        let file = File::open(&tmp.0).await.unwrap();
        let (res, buf) = file.read_at(Vec::with_capacity(64), 0).await;
        assert_eq!(res.unwrap(), 12);
        assert_eq!(buf, b"hello, giong");
    });
    assert_eq!(std::fs::read(&tmp.0).unwrap(), b"hello, giong");
}

#[test]
fn reads_and_writes_are_positional() {
    let tmp = TempPath::new("positional");
    giong::block_on(async {
        let file = File::options()
            .read(true)
            .write(true)
            .create(true)
            .open(&tmp.0)
            .await
            .unwrap();
        file.write_all_at(&b"world"[..], 100).await.0.unwrap();
        file.write_all_at(&b"hello"[..], 0).await.0.unwrap();
        assert_eq!(file.len().unwrap(), 105);

        let (res, buf) = file.read_at(Vec::with_capacity(5), 100).await;
        assert_eq!(res.unwrap(), 5);
        assert_eq!(buf, b"world");
        // The hole in between reads as zeroes.
        let (res, buf) = file.read_at(Vec::with_capacity(4), 50).await;
        assert_eq!(res.unwrap(), 4);
        assert_eq!(buf, [0, 0, 0, 0]);
        // At and past the end: zero bytes.
        let (res, buf) = file.read_at(Vec::with_capacity(8), 105).await;
        assert_eq!(res.unwrap(), 0);
        assert!(buf.is_empty());
        let (res, _) = file.read_at(Vec::with_capacity(8), 10_000).await;
        assert_eq!(res.unwrap(), 0);
        // A short read at the tail.
        let (res, buf) = file.read_at(Vec::with_capacity(64), 102).await;
        assert_eq!(res.unwrap(), 3);
        assert_eq!(buf, b"rld");
    });
}

#[test]
fn open_errors_are_reported() {
    let tmp = TempPath::new("errors");
    giong::block_on(async {
        let err = File::open(&tmp.0).await.unwrap_err();
        assert_eq!(err.kind(), ErrorKind::NotFound);

        File::create(&tmp.0).await.unwrap();
        let err = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp.0)
            .await
            .unwrap_err();
        assert_eq!(err.kind(), ErrorKind::AlreadyExists);

        // Nonsensical option sets are rejected before reaching the kernel.
        for opts in [
            OpenOptions::new(),
            OpenOptions::new().read(true).truncate(true),
            OpenOptions::new().read(true).create(true),
            OpenOptions::new().append(true).truncate(true),
        ] {
            assert_eq!(
                opts.open(&tmp.0).await.unwrap_err().kind(),
                ErrorKind::InvalidInput
            );
        }
        let err = File::open("bad\0path").await.unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidInput);

        // Writing to a read-only handle fails, and hands the buffer back.
        let file = File::open(&tmp.0).await.unwrap();
        let (res, buf) = file.write_at(b"nope".to_vec(), 0).await;
        assert!(res.is_err());
        assert_eq!(buf, b"nope");

        assert_eq!(giong::stats().tracked_ops, 0);
    });
}

#[test]
fn append_and_truncate() {
    let tmp = TempPath::new("append");
    giong::block_on(async {
        File::create(&tmp.0)
            .await
            .unwrap()
            .write_all_at(&b"one"[..], 0)
            .await
            .0
            .unwrap();
        let file = OpenOptions::new().append(true).open(&tmp.0).await.unwrap();
        // In append mode the offset is ignored.
        file.write_all_at(&b"two"[..], 0).await.0.unwrap();
        file.sync_data().await.unwrap();
        drop(file);
        assert_eq!(std::fs::read(&tmp.0).unwrap(), b"onetwo");

        let file = File::create(&tmp.0).await.unwrap();
        assert!(file.is_empty().unwrap());
    });
}

#[test]
fn large_file_is_intact() {
    const LEN: usize = 6 * 1024 * 1024 + 123;
    let tmp = TempPath::new("large");
    let data = payload(LEN, 7);
    let expected = data.clone();
    giong::block_on(async move {
        let file = File::create(&tmp.0).await.unwrap();
        file.write_all_at(data, 0).await.0.unwrap();
        file.close().await.unwrap();
        assert_eq!(std::fs::read(&tmp.0).unwrap(), expected);

        let file = File::open(&tmp.0).await.unwrap();
        let mut got = Vec::with_capacity(LEN);
        let mut chunk = Vec::with_capacity(256 * 1024);
        loop {
            let (res, buf) = file.read_at(chunk, got.len() as u64).await;
            chunk = buf;
            if res.unwrap() == 0 {
                break;
            }
            got.extend_from_slice(&chunk);
        }
        assert!(got == expected, "file contents differ");
    });
}

#[test]
fn one_file_serves_many_tasks() {
    const BLOCK: usize = 4096;
    const BLOCKS: usize = 64;
    let tmp = TempPath::new("shared");
    std::fs::write(&tmp.0, payload(BLOCK * BLOCKS, 3)).unwrap();
    let expected = payload(BLOCK * BLOCKS, 3);
    giong::block_on(async move {
        let file = Rc::new(File::open(&tmp.0).await.unwrap());
        let readers: Vec<_> = (0..BLOCKS)
            .map(|i| {
                let file = file.clone();
                giong::spawn(async move {
                    let (res, buf) = file
                        .read_at(Vec::with_capacity(BLOCK), (i * BLOCK) as u64)
                        .await;
                    assert_eq!(res.unwrap(), BLOCK);
                    (i, buf)
                })
            })
            .collect();
        for reader in readers {
            let (i, buf) = reader.await.unwrap();
            assert_eq!(buf, expected[i * BLOCK..(i + 1) * BLOCK]);
        }
    });
}

#[test]
fn files_are_closed_exactly_once() {
    let tmp = TempPath::new("close");
    giong::block_on(async {
        // Explicit close.
        let file = File::create(&tmp.0).await.unwrap();
        let fd = file.as_raw_fd();
        assert!(fd_is_open(fd));
        file.close().await.unwrap();
        assert!(!fd_is_open(fd));

        // Drop closes in the background.
        let file = File::open(&tmp.0).await.unwrap();
        let fd = file.as_raw_fd();
        drop(file);
        sleep(Duration::from_millis(5)).await;
        assert!(!fd_is_open(fd));

        // A close future that is never polled still closes the file.
        let file = File::open(&tmp.0).await.unwrap();
        let fd = file.as_raw_fd();
        drop(file.close());
        sleep(Duration::from_millis(5)).await;
        assert!(!fd_is_open(fd));

        sleep(Duration::from_millis(5)).await;
        let stats = giong::stats();
        assert_eq!(stats.tracked_ops, 0, "{stats:?}");
    });
}

#[test]
fn from_std_file() {
    let tmp = TempPath::new("from-std");
    std::fs::write(&tmp.0, b"std wrote this").unwrap();
    giong::block_on(async {
        let file = File::from_std(std::fs::File::open(&tmp.0).unwrap());
        let (res, buf) = file.read_at(Vec::with_capacity(32), 4).await;
        assert_eq!(res.unwrap(), 10);
        assert_eq!(buf, b"wrote this");
    });
}

#[test]
fn a_file_may_outlive_its_runtime() {
    let tmp = TempPath::new("outlive");
    let file = giong::block_on(async { File::create(&tmp.0).await.unwrap() });
    let fd = file.as_raw_fd();
    assert!(fd_is_open(fd));
    drop(file);
    assert!(!fd_is_open(fd));
}
