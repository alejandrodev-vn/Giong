//! UDP sockets.

use std::net::SocketAddr;
use std::os::fd::AsRawFd;
use std::time::Duration;

use giong::net::UdpSocket;
use giong::time::{sleep, timeout};

fn socket() -> (UdpSocket, SocketAddr) {
    let s = UdpSocket::bind("127.0.0.1:0").unwrap();
    let addr = s.local_addr().unwrap();
    (s, addr)
}

#[test]
fn send_to_and_recv_from() {
    giong::block_on(async {
        let (a, a_addr) = socket();
        let (b, b_addr) = socket();

        let (res, sent) = a.send_to(b"ping".to_vec(), b_addr).await;
        assert_eq!(res.unwrap(), 4);
        assert_eq!(sent, b"ping");

        let (res, buf) = b.recv_from(Vec::with_capacity(1500)).await;
        let (len, peer) = res.unwrap();
        assert_eq!((len, peer), (4, a_addr));
        assert_eq!(buf, b"ping");

        // Reply to whoever sent it.
        b.send_to(&b"pong"[..], peer).await.0.unwrap();
        let (res, buf) = a.recv_from(Vec::with_capacity(1500)).await;
        assert_eq!(res.unwrap(), (4, b_addr));
        assert_eq!(buf, b"pong");

        assert_eq!(giong::stats().tracked_ops, 0);
    });
}

#[test]
fn connected_send_and_recv() {
    giong::block_on(async {
        let (a, a_addr) = socket();
        let (b, b_addr) = socket();
        a.connect(b_addr).unwrap();
        b.connect(a_addr).unwrap();
        assert_eq!(a.peer_addr().unwrap(), b_addr);

        for i in 0..100u32 {
            a.send(i.to_le_bytes().to_vec()).await.0.unwrap();
            let (res, buf) = b.recv(Vec::with_capacity(16)).await;
            assert_eq!(res.unwrap(), 4);
            assert_eq!(buf, i.to_le_bytes());
        }

        // A connected socket ignores datagrams from anyone else.
        let (stranger, _) = socket();
        stranger.send_to(&b"intruder"[..], a_addr).await.0.unwrap();
        b.send(&b"friend"[..]).await.0.unwrap();
        let (res, buf) = a.recv(Vec::with_capacity(16)).await;
        res.unwrap();
        assert_eq!(buf, b"friend");
    });
}

#[test]
fn long_datagrams_are_truncated_to_the_buffer() {
    giong::block_on(async {
        let (a, _) = socket();
        let (b, b_addr) = socket();
        a.send_to(vec![7u8; 1000], b_addr).await.0.unwrap();
        let (res, buf) = b.recv_from(Vec::with_capacity(100)).await;
        assert_eq!(res.unwrap().0, 100);
        assert_eq!(buf, vec![7u8; 100]);
        // The rest of that datagram is gone; the next one is intact.
        a.send_to(&b"next"[..], b_addr).await.0.unwrap();
        let (res, buf) = b.recv_from(Vec::with_capacity(100)).await;
        assert_eq!(res.unwrap().0, 4);
        assert_eq!(buf, b"next");
    });
}

#[test]
fn a_cancelled_receive_leaves_the_socket_usable() {
    giong::block_on(async {
        let (a, _) = socket();
        let (b, b_addr) = socket();
        for _ in 0..50 {
            assert!(timeout(
                Duration::from_micros(300),
                b.recv_from(Vec::with_capacity(512))
            )
            .await
            .is_err());
        }
        sleep(Duration::from_millis(20)).await;
        assert_eq!(
            giong::stats().tracked_ops,
            0,
            "cancelled receives were not cleaned up"
        );

        a.send_to(&b"still here"[..], b_addr).await.0.unwrap();
        let (res, buf) = timeout(Duration::from_secs(5), b.recv_from(Vec::with_capacity(512)))
            .await
            .unwrap();
        assert_eq!(res.unwrap().0, 10);
        assert_eq!(buf, b"still here");
    });
}

#[test]
fn echo_many_datagrams_between_tasks() {
    const N: u32 = 2_000;
    giong::block_on(async {
        let (server, server_addr) = socket();
        let echo = giong::spawn(async move {
            let mut buf = Vec::with_capacity(64);
            for _ in 0..N {
                let (res, b) = server.recv_from(buf).await;
                let (_, peer) = res.unwrap();
                let (res, b) = server.send_to(b, peer).await;
                res.unwrap();
                buf = b;
            }
        });
        let (client, _) = socket();
        client.connect(server_addr).unwrap();
        for i in 0..N {
            client.send(i.to_le_bytes().to_vec()).await.0.unwrap();
            let (res, buf) = timeout(Duration::from_secs(5), client.recv(Vec::with_capacity(64)))
                .await
                .expect("datagram lost on loopback");
            res.unwrap();
            assert_eq!(buf, i.to_le_bytes());
        }
        echo.await.unwrap();
    });
}

#[test]
fn interoperates_with_std_sockets() {
    giong::block_on(async {
        let std_socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let std_addr = std_socket.local_addr().unwrap();
        let (ours, our_addr) = socket();

        std_socket.send_to(b"from std", our_addr).unwrap();
        let (res, buf) = ours.recv_from(Vec::with_capacity(64)).await;
        assert_eq!(res.unwrap(), (8, std_addr));
        assert_eq!(buf, b"from std");

        ours.send_to(&b"from giong"[..], std_addr).await.0.unwrap();
        let mut buf = [0u8; 64];
        let (len, peer) = std_socket.recv_from(&mut buf).unwrap();
        assert_eq!((&buf[..len], peer), (&b"from giong"[..], our_addr));

        // And a std socket can be handed over.
        let adopted = UdpSocket::from_std(std_socket);
        assert_eq!(adopted.local_addr().unwrap(), std_addr);
    });
}

#[test]
fn ipv6_loopback() {
    giong::block_on(async {
        // Skip quietly on machines without IPv6.
        let Ok(a) = UdpSocket::bind("[::1]:0") else {
            return;
        };
        let b = UdpSocket::bind("[::1]:0").unwrap();
        let (a_addr, b_addr) = (a.local_addr().unwrap(), b.local_addr().unwrap());
        assert!(a_addr.is_ipv6());
        a.send_to(&b"six"[..], b_addr).await.0.unwrap();
        let (res, buf) = b.recv_from(Vec::with_capacity(16)).await;
        assert_eq!(res.unwrap(), (3, a_addr));
        assert_eq!(buf, b"six");
    });
}

#[test]
fn dropping_a_socket_closes_it() {
    giong::block_on(async {
        let (s, _) = socket();
        let fd = s.as_raw_fd();
        drop(s);
        sleep(Duration::from_millis(5)).await;
        assert_eq!(unsafe { libc::fcntl(fd, libc::F_GETFD) }, -1);
    });
}
