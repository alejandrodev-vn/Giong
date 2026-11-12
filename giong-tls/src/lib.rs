//! # giong-tls
//!
//! Server-side TLS for the [giong] thread-per-core runtime, built on
//! [rustls] (TLS 1.2 and 1.3, `ring` for cryptography).
//!
//! ```no_run
//! use giong::net::TcpListener;
//! use giong_tls::TlsAcceptor;
//!
//! # fn main() -> std::io::Result<()> {
//! let cert = std::fs::read("cert.pem")?;
//! let key = std::fs::read("key.pem")?;
//! let acceptor = TlsAcceptor::from_pem(&cert, &key)?;
//!
//! giong::block_on(async move {
//!     let listener = TcpListener::bind("0.0.0.0:8443")?;
//!     loop {
//!         let tcp = listener.accept().await?;
//!         let acceptor = acceptor.clone();
//!         giong::spawn(async move {
//!             let Ok(tls) = acceptor.accept(tcp).await else { return };
//!             while let Ok(data) = tls.recv().await {
//!                 if data.is_empty() {
//!                     break; // closed by the peer
//!                 }
//!                 if tls.write_all(data).await.0.is_err() {
//!                     break;
//!                 }
//!             }
//!             let _ = tls.shutdown().await;
//!         });
//!     }
//! })
//! # }
//! ```
//!
//! The API mirrors [`giong::net::TcpStream`]: [`TlsStream::recv`] returns the
//! next piece of decrypted data and [`TlsStream::write_all`] takes a buffer by
//! value and hands it back.

#![warn(missing_docs)]

use std::cell::{Cell, RefCell};
use std::future::poll_fn;
use std::io::{self, Read, Write};
use std::mem;
use std::net::Shutdown;
use std::sync::Arc;
use std::task::{Poll, Waker};
use std::time::Duration;

use giong::net::TcpStream;
use giong::time::timeout;
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::{ServerConfig, ServerConnection};

pub use rustls;

fn invalid(e: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, e.to_string())
}

/// Turns accepted TCP connections into TLS connections.
///
/// Cheap to clone; share one between all workers.
#[derive(Clone)]
pub struct TlsAcceptor {
    config: Arc<ServerConfig>,
    handshake_timeout: Duration,
}

impl TlsAcceptor {
    /// An acceptor using an existing rustls configuration.
    pub fn new(config: Arc<ServerConfig>) -> Self {
        Self {
            config,
            handshake_timeout: Duration::from_secs(10),
        }
    }

    /// An acceptor for one certificate chain and its private key, both in PEM
    /// format. TLS 1.2 and 1.3 with rustls' default cipher suites, no client
    /// authentication, and `http/1.1` offered through ALPN.
    pub fn from_pem(cert_chain: &[u8], private_key: &[u8]) -> io::Result<Self> {
        let certs = CertificateDer::pem_slice_iter(cert_chain)
            .collect::<Result<Vec<_>, _>>()
            .map_err(invalid)?;
        if certs.is_empty() {
            return Err(invalid("no certificate found in the PEM data"));
        }
        let key = PrivateKeyDer::from_pem_slice(private_key).map_err(invalid)?;
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let mut config = ServerConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map_err(invalid)?
            .with_no_client_auth()
            .with_single_cert(certs, key)
            .map_err(invalid)?;
        config.alpn_protocols = vec![b"http/1.1".to_vec()];
        Ok(Self::new(Arc::new(config)))
    }

    /// How long a client may take to complete the TLS handshake (default:
    /// 10 s).
    pub fn handshake_timeout(mut self, timeout: Duration) -> Self {
        self.handshake_timeout = timeout;
        self
    }

    /// Performs the TLS handshake on an accepted connection.
    pub async fn accept(&self, tcp: TcpStream) -> io::Result<TlsStream> {
        let conn = ServerConnection::new(self.config.clone()).map_err(invalid)?;
        let stream = TlsStream {
            tcp,
            conn: RefCell::new(conn),
            plain: RefCell::new(Vec::new()),
            wire: RefCell::new(Vec::new()),
            eof: Cell::new(false),
            sending: Cell::new(false),
            broken: Cell::new(false),
            failed: Cell::new(None),
            waiters: RefCell::new(Vec::new()),
        };
        let handshake = async {
            while stream.conn.borrow().is_handshaking() {
                stream.flush_tls().await?;
                if !stream.conn.borrow().is_handshaking() {
                    break;
                }
                if stream.eof.get() {
                    return Err(io::ErrorKind::UnexpectedEof.into());
                }
                stream.pump().await?;
            }
            stream.flush_tls().await
        };
        match timeout(self.handshake_timeout, handshake).await {
            Ok(Ok(())) => Ok(stream),
            Ok(Err(e)) => Err(e),
            Err(_) => Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "TLS handshake timed out",
            )),
        }
    }
}

impl std::fmt::Debug for TlsAcceptor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TlsAcceptor")
            .field("handshake_timeout", &self.handshake_timeout)
            .finish_non_exhaustive()
    }
}

/// A TLS connection over a [`TcpStream`].
pub struct TlsStream {
    tcp: TcpStream,
    conn: RefCell<ServerConnection>,
    /// Decrypted data not handed out yet.
    plain: RefCell<Vec<u8>>,
    /// Scratch buffer for outgoing TLS records, reused between writes.
    wire: RefCell<Vec<u8>>,
    /// The peer closed the connection (with or without `close_notify`).
    eof: Cell<bool>,
    /// A task is writing TLS records to the socket. Records must not
    /// interleave, so other tasks wait for it in `waiters`.
    sending: Cell<bool>,
    /// A write to the socket failed or was cancelled half way: the record
    /// stream is cut in the middle and nothing more can be sent.
    broken: Cell<bool>,
    /// A receive error held back until the data before it has been handed out.
    failed: Cell<Option<io::Error>>,
    /// Tasks waiting for `sending` to clear.
    waiters: RefCell<Vec<Waker>>,
}

/// Held while TLS records are being written to the socket.
struct Sending<'a> {
    stream: &'a TlsStream,
    complete: bool,
}

impl Drop for Sending<'_> {
    fn drop(&mut self) {
        if !self.complete {
            self.stream.broken.set(true);
        }
        self.stream.sending.set(false);
        let waiters = mem::take(&mut *self.stream.waiters.borrow_mut());
        for waker in waiters {
            waker.wake();
        }
    }
}

impl TlsStream {
    /// Sends every TLS record rustls has queued.
    async fn flush_tls(&self) -> io::Result<()> {
        loop {
            if !self.conn.borrow().wants_write() {
                return Ok(());
            }
            if self.broken.get() {
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "a TLS write failed or was cancelled; nothing more can be sent",
                ));
            }
            if self.sending.get() {
                // Another task is sending. Wait for it, then send what is
                // left (it may already have sent this task's records too).
                poll_fn(|cx| {
                    if !self.sending.get() {
                        return Poll::Ready(());
                    }
                    self.waiters.borrow_mut().push(cx.waker().clone());
                    Poll::Pending
                })
                .await;
                continue;
            }
            let mut wire = mem::take(&mut *self.wire.borrow_mut());
            {
                let mut conn = self.conn.borrow_mut();
                while conn.wants_write() {
                    conn.write_tls(&mut wire)?;
                }
            }
            self.sending.set(true);
            let mut sending = Sending {
                stream: self,
                complete: false,
            };
            let (res, mut wire) = self.tcp.write_all(wire).await;
            res?;
            sending.complete = true;
            drop(sending);
            wire.clear();
            *self.wire.borrow_mut() = wire;
        }
    }

    /// Receives one chunk of TLS data, decrypts it into `plain`, and sends
    /// whatever the protocol wants to send in response.
    async fn pump(&self) -> io::Result<()> {
        // Cancelling here loses nothing: `recv` is cancel safe.
        let chunk = self.tcp.recv().await?;
        if chunk.is_empty() {
            self.eof.set(true);
            return Ok(());
        }
        let result = {
            let mut conn = self.conn.borrow_mut();
            let mut plain = self.plain.borrow_mut();
            let mut input = &chunk[..];
            let mut result = Ok(());
            while !input.is_empty() {
                // rustls buffers a bounded amount of plaintext, so it has to
                // be drained as the records are fed in.
                match conn.read_tls(&mut input) {
                    Ok(0) => {
                        result = Err(invalid("rustls accepted no more TLS data"));
                        break;
                    }
                    Ok(_) => {}
                    Err(e) => {
                        result = Err(e);
                        break;
                    }
                }
                match conn.process_new_packets() {
                    Ok(state) => {
                        let n = state.plaintext_bytes_to_read();
                        if n > 0 {
                            let at = plain.len();
                            plain.resize(at + n, 0);
                            if let Err(e) = conn.reader().read_exact(&mut plain[at..]) {
                                plain.truncate(at);
                                result = Err(e);
                                break;
                            }
                        }
                        if state.peer_has_closed() {
                            self.eof.set(true);
                            break;
                        }
                    }
                    Err(e) => {
                        result = Err(invalid(e));
                        break;
                    }
                }
            }
            result
        };
        drop(chunk);
        // Handshake messages, key updates, or the alert describing `result`.
        // Not being able to send them is no reason to withhold what was
        // received, if sending has failed before.
        if self.broken.get() {
            return result;
        }
        let flushed = self.flush_tls().await;
        result.and(flushed)
    }

    /// Receives decrypted data. An empty buffer means that the peer has closed
    /// the connection, with a `close_notify` alert or by closing the socket.
    ///
    /// Cancel safe: dropping the future loses no data. (If it is dropped at
    /// the rare moment a TLS protocol message is being sent, sending fails
    /// from then on instead of continuing on a broken stream.)
    ///
    /// One task may receive while another one sends.
    pub async fn recv(&self) -> io::Result<Vec<u8>> {
        loop {
            {
                let mut plain = self.plain.borrow_mut();
                if !plain.is_empty() {
                    return Ok(mem::take(&mut *plain));
                }
            }
            if let Some(e) = self.failed.take() {
                return Err(e);
            }
            if self.eof.get() {
                return Ok(Vec::new());
            }
            if let Err(e) = self.pump().await {
                // Data that arrived intact before the failure comes first.
                if self.plain.borrow().is_empty() {
                    return Err(e);
                }
                self.failed.set(Some(e));
            }
        }
    }

    /// Encrypts and sends all of `buf`, then hands the buffer back.
    ///
    /// Not cancel safe: if the future is dropped, part of `buf` may have been
    /// sent, and if that happened in the middle of a TLS record the
    /// connection cannot send any more: later writes return an error, while
    /// data the peer already sent can still be received.
    pub async fn write_all(&self, buf: Vec<u8>) -> (io::Result<()>, Vec<u8>) {
        let mut sent = 0;
        while sent < buf.len() {
            // rustls accepts a bounded amount per call; send as it fills up.
            let accepted = self.conn.borrow_mut().writer().write(&buf[sent..]);
            match accepted {
                Ok(n) => sent += n,
                Err(e) => return (Err(e), buf),
            }
            if let Err(e) = self.flush_tls().await {
                return (Err(e), buf);
            }
        }
        (Ok(()), buf)
    }

    /// Sends the TLS `close_notify` alert and closes the sending direction.
    pub async fn shutdown(&self) -> io::Result<()> {
        self.conn.borrow_mut().send_close_notify();
        self.flush_tls().await?;
        self.tcp.shutdown(Shutdown::Write)
    }

    /// The protocol agreed on through ALPN, if any (for example `http/1.1`).
    pub fn alpn_protocol(&self) -> Option<Vec<u8>> {
        self.conn.borrow().alpn_protocol().map(<[u8]>::to_vec)
    }

    /// The underlying TCP connection.
    pub fn tcp(&self) -> &TcpStream {
        &self.tcp
    }
}

impl std::fmt::Debug for TlsStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TlsStream")
            .field("tcp", &self.tcp)
            .finish_non_exhaustive()
    }
}
