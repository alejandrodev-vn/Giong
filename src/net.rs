//! TCP on io_uring.
//!
//! * [`TcpListener::accept`] uses a **multishot accept**: one submission keeps
//!   producing connections.
//! * [`TcpStream::recv`] uses a **multishot receive with provided buffers**:
//!   one submission per connection, and the kernel picks a buffer from the
//!   worker's shared pool only when data actually arrives — idle connections
//!   pin no memory.
//! * Writes take ownership of the buffer and return it with the result.
//! * [`UdpSocket`] sends and receives datagrams, connected or not.
//! * Descriptors are closed through the ring, batched with everything else.

use std::cell::Cell;
use std::future::poll_fn;
use std::io;
use std::marker::PhantomData;
use std::mem::ManuallyDrop;
use std::net::{
    Ipv4Addr, Ipv6Addr, Shutdown, SocketAddr, SocketAddrV4, SocketAddrV6, ToSocketAddrs,
};
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, RawFd};
use std::task::{Context, Poll};

use io_uring::{cqueue, opcode, types};
use socket2::{Domain, Protocol, SockAddr, Socket, Type};

use crate::buf::{IoBuf, IoBufMut, RecvBuf};
use crate::driver::{MultiKind, MultiPoll, BGID};
use crate::op::Op;
use crate::runtime::{self, Worker};

fn os_err(res: i32) -> io::Error {
    io::Error::from_raw_os_error(-res)
}

/// A multishot operation slot owned by a socket: the driver key plus the uid
/// of the worker whose driver it belongs to.
struct MultiSlot {
    kind: MultiKind,
    key: Cell<Option<(usize, usize)>>,
}

impl MultiSlot {
    fn new(kind: MultiKind) -> Self {
        Self {
            kind,
            key: Cell::new(None),
        }
    }

    /// The driver key on the current worker, creating the slot on first use.
    fn key(&self, w: &Worker) -> usize {
        match self.key.get() {
            Some((key, uid)) if uid == w.shared.uid => key,
            // First use — or the runtime that owned the old key is gone, in
            // which case its teardown already cancelled the operation.
            _ => {
                let key = w.driver.borrow_mut().new_multi(self.kind);
                self.key.set(Some((key, w.shared.uid)));
                key
            }
        }
    }

    /// `true` if a multishot operation has been set up on the current worker.
    fn is_active(&self, w: &Worker) -> bool {
        matches!(self.key.get(), Some((_, uid)) if uid == w.shared.uid)
    }

    /// Releases the slot. Returns `false` if the driver could not be reached.
    fn release(&self, w: &Worker) -> bool {
        match self.key.take() {
            Some((key, uid)) if uid == w.shared.uid => match w.driver.try_borrow_mut() {
                Ok(mut driver) => {
                    driver.drop_multi(key);
                    true
                }
                Err(_) => false,
            },
            _ => true,
        }
    }
}

/// Closes `sock` through the current worker's ring, or directly if there is none.
fn close_socket(sock: &mut ManuallyDrop<Socket>, slot: Option<&MultiSlot>) {
    let fd = sock.as_raw_fd();
    let closed = runtime::try_with_worker(|w| {
        // Cancel the multishot operation first: it holds a reference to the
        // socket, which would otherwise stay open after the close.
        if let Some(slot) = slot {
            slot.release(w);
        }
        match w.driver.try_borrow_mut() {
            Ok(mut driver) => {
                driver.close_fd(fd);
                true
            }
            Err(_) => false,
        }
    })
    .unwrap_or(false);
    if !closed {
        // SAFETY: `sock` is dropped exactly once, here.
        unsafe { ManuallyDrop::drop(sock) };
    }
}

// -------------------------------------------------------------------- listener

/// A TCP socket listening for connections.
pub struct TcpListener {
    sock: ManuallyDrop<Socket>,
    accept: MultiSlot,
    _not_send: PhantomData<*const ()>,
}

impl TcpListener {
    /// Binds to `addr` with `SO_REUSEADDR` and `SO_REUSEPORT` set, so every
    /// worker can bind its own listener to the same port and let the kernel
    /// spread incoming connections across cores.
    pub fn bind<A: ToSocketAddrs>(addr: A) -> io::Result<Self> {
        let mut last_err = None;
        for addr in addr.to_socket_addrs()? {
            match Self::bind_addr(addr) {
                Ok(listener) => return Ok(listener),
                Err(e) => last_err = Some(e),
            }
        }
        Err(last_err.unwrap_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "could not resolve to any address",
            )
        }))
    }

    fn bind_addr(addr: SocketAddr) -> io::Result<Self> {
        let sock = Socket::new(Domain::for_address(addr), Type::STREAM, Some(Protocol::TCP))?;
        sock.set_reuse_address(true)?;
        sock.set_reuse_port(true)?;
        sock.bind(&addr.into())?;
        sock.listen(4096)?;
        Ok(Self::from_socket(sock))
    }

    /// Wraps a listener created by the standard library.
    pub fn from_std(listener: std::net::TcpListener) -> Self {
        // SAFETY: we take over the descriptor that `listener` owned.
        Self::from_socket(unsafe { Socket::from_raw_fd(listener.into_raw_fd()) })
    }

    fn from_socket(sock: Socket) -> Self {
        Self {
            sock: ManuallyDrop::new(sock),
            accept: MultiSlot::new(MultiKind::Accept),
            _not_send: PhantomData,
        }
    }

    /// The address this listener is bound to.
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.sock
            .local_addr()?
            .as_socket()
            .ok_or_else(|| io::ErrorKind::AddrNotAvailable.into())
    }

    /// Waits for the next incoming connection.
    ///
    /// Cancel safe: dropping the future loses no connection — already accepted
    /// sockets stay queued for the next call.
    ///
    /// Use [`TcpStream::peer_addr`] if you need the remote address. On a
    /// persistent error such as `EMFILE` this returns the error on every call,
    /// so back off (e.g. [`sleep`](crate::time::sleep)) before retrying.
    pub async fn accept(&self) -> io::Result<TcpStream> {
        poll_fn(|cx| self.poll_accept(cx)).await
    }

    fn poll_accept(&self, cx: &mut Context<'_>) -> Poll<io::Result<TcpStream>> {
        runtime::with_worker(|w| {
            let key = self.accept.key(w);
            let mut driver = w.driver.borrow_mut();
            loop {
                match driver.poll_multi(key, cx.waker()) {
                    MultiPoll::Item(res, _) if res >= 0 => {
                        // SAFETY: a fresh descriptor returned by accept.
                        let sock = unsafe { Socket::from_raw_fd(res) };
                        return Poll::Ready(Ok(TcpStream::from_socket(sock)));
                    }
                    MultiPoll::Item(res, _) => return Poll::Ready(Err(os_err(res))),
                    MultiPoll::Heap(_) => unreachable!("accept never queues heap data"),
                    MultiPoll::NeedArm => {
                        let sqe = opcode::AcceptMulti::new(types::Fd(self.sock.as_raw_fd()))
                            .flags(libc::SOCK_CLOEXEC)
                            .build();
                        driver.arm_multi(key, sqe);
                    }
                    MultiPoll::Pending => return Poll::Pending,
                }
            }
        })
    }
}

impl AsRawFd for TcpListener {
    fn as_raw_fd(&self) -> RawFd {
        self.sock.as_raw_fd()
    }
}

impl Drop for TcpListener {
    fn drop(&mut self) {
        close_socket(&mut self.sock, Some(&self.accept));
    }
}

impl std::fmt::Debug for TcpListener {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TcpListener")
            .field("fd", &self.as_raw_fd())
            .finish()
    }
}

// ---------------------------------------------------------------------- stream

/// A TCP connection.
///
/// All methods take `&self`, so a stream can be shared (e.g. in an `Rc`)
/// between one reading task and one writing task. Only one task may read and
/// only one may write at any given time.
pub struct TcpStream {
    sock: ManuallyDrop<Socket>,
    recv: MultiSlot,
    _not_send: PhantomData<*const ()>,
}

impl TcpStream {
    /// Opens a connection to `addr`.
    pub async fn connect<A: ToSocketAddrs>(addr: A) -> io::Result<Self> {
        let mut last_err = None;
        for addr in addr.to_socket_addrs()? {
            match Self::connect_addr(addr).await {
                Ok(stream) => return Ok(stream),
                Err(e) => last_err = Some(e),
            }
        }
        Err(last_err.unwrap_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "could not resolve to any address",
            )
        }))
    }

    async fn connect_addr(addr: SocketAddr) -> io::Result<Self> {
        let sock = Socket::new(Domain::for_address(addr), Type::STREAM, Some(Protocol::TCP))?;
        // Boxed so the address the SQE points at does not move with the future.
        let addr: Box<SockAddr> = Box::new(addr.into());
        let sqe = opcode::Connect::new(
            types::Fd(sock.as_raw_fd()),
            addr.as_ptr() as *const libc::sockaddr,
            addr.len(),
        )
        .build();
        let (res, _, _addr) = Op::new(sqe, addr).await;
        if res < 0 {
            return Err(os_err(res));
        }
        Ok(Self::from_socket(sock))
    }

    /// Wraps a connected stream created by the standard library.
    pub fn from_std(stream: std::net::TcpStream) -> Self {
        // SAFETY: we take over the descriptor that `stream` owned.
        Self::from_socket(unsafe { Socket::from_raw_fd(stream.into_raw_fd()) })
    }

    fn from_socket(sock: Socket) -> Self {
        Self {
            sock: ManuallyDrop::new(sock),
            recv: MultiSlot::new(MultiKind::Recv),
            _not_send: PhantomData,
        }
    }

    /// Receives the next chunk of data.
    ///
    /// The returned [`RecvBuf`] borrows a buffer from the worker's pool; drop
    /// it as soon as you are done with the bytes. **An empty buffer means the
    /// peer closed the connection.**
    ///
    /// Cancel safe: dropping the future (for instance through
    /// [`timeout`](crate::time::timeout)) loses no data — whatever arrives in
    /// the meantime is returned by the next call.
    pub async fn recv(&self) -> io::Result<RecvBuf> {
        poll_fn(|cx| self.poll_recv(cx)).await
    }

    fn poll_recv(&self, cx: &mut Context<'_>) -> Poll<io::Result<RecvBuf>> {
        runtime::with_worker(|w| {
            let key = self.recv.key(w);
            let mut driver = w.driver.borrow_mut();
            loop {
                match driver.poll_multi(key, cx.waker()) {
                    MultiPoll::Item(res, flags) => {
                        let bid = cqueue::buffer_select(flags);
                        return Poll::Ready(match (res, bid) {
                            (n, Some(bid)) if n > 0 => {
                                Ok(RecvBuf::pooled(driver.pool().clone(), bid, n as usize))
                            }
                            (n, bid) => {
                                // No payload: hand the buffer (if any) straight back.
                                if let Some(bid) = bid {
                                    driver.pool().recycle(bid);
                                }
                                match n {
                                    0 => Ok(RecvBuf::eof()),
                                    n if n < 0 => Err(os_err(n)),
                                    _ => Err(io::Error::other(
                                        "io_uring returned data without a buffer",
                                    )),
                                }
                            }
                        });
                    }
                    MultiPoll::Heap(data) => return Poll::Ready(Ok(RecvBuf::heap(data))),
                    MultiPoll::NeedArm => {
                        if driver.pool().free() == 0 {
                            // The pool is exhausted. Reclaim buffers that hold
                            // unread data of other streams ...
                            driver.evacuate();
                        }
                        if driver.pool().free() == 0 {
                            // ... and if user code holds all the rest, wait for
                            // one to come back instead of spinning on ENOBUFS.
                            driver.pool().wait(cx.waker());
                            return Poll::Pending;
                        }
                        let sqe =
                            opcode::RecvMulti::new(types::Fd(self.sock.as_raw_fd()), BGID).build();
                        driver.arm_multi(key, sqe);
                    }
                    MultiPoll::Pending => return Poll::Pending,
                }
            }
        })
    }

    /// Reads into `buf` (from its start, up to its capacity) with a one-off
    /// receive, returning the buffer with its length set to the bytes read.
    /// `Ok(0)` means the peer closed the connection.
    ///
    /// Prefer [`recv`](Self::recv) for servers. The two must not be mixed on
    /// one stream: once `recv` has been used, `read` fails with `InvalidInput`.
    pub async fn read<B: IoBufMut>(&self, mut buf: B) -> (io::Result<usize>, B) {
        if runtime::with_worker(|w| self.recv.is_active(w)) {
            let err = io::Error::new(
                io::ErrorKind::InvalidInput,
                "TcpStream::read cannot be used after TcpStream::recv on the same stream",
            );
            return (Err(err), buf);
        }
        let len = buf.capacity().min(u32::MAX as usize) as u32;
        let sqe =
            opcode::Recv::new(types::Fd(self.sock.as_raw_fd()), buf.as_mut_ptr(), len).build();
        let (res, _, mut buf) = Op::new(sqe, buf).await;
        if res < 0 {
            return (Err(os_err(res)), buf);
        }
        // SAFETY: the kernel wrote `res` bytes at the start of the buffer.
        unsafe { buf.set_init(res as usize) };
        (Ok(res as usize), buf)
    }

    /// Writes some bytes of `buf`, returning how many were written.
    pub async fn write<B: IoBuf>(&self, buf: B) -> (io::Result<usize>, B) {
        let (res, buf) = self.send_from(buf, 0).await;
        (res, buf)
    }

    /// Writes all of `buf`. The buffer is handed back so it can be reused.
    pub async fn write_all<B: IoBuf>(&self, mut buf: B) -> (io::Result<()>, B) {
        let mut written = 0;
        while written < buf.len() {
            let (res, b) = self.send_from(buf, written).await;
            buf = b;
            match res {
                Ok(0) => return (Err(io::ErrorKind::WriteZero.into()), buf),
                Ok(n) => written += n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return (Err(e), buf),
            }
        }
        (Ok(()), buf)
    }

    async fn send_from<B: IoBuf>(&self, buf: B, offset: usize) -> (io::Result<usize>, B) {
        debug_assert!(offset <= buf.len());
        let len = (buf.len() - offset).min(u32::MAX as usize) as u32;
        // SAFETY: `offset <= len`, so the pointer stays within the buffer.
        let ptr = unsafe { buf.as_ptr().add(offset) };
        let sqe = opcode::Send::new(types::Fd(self.sock.as_raw_fd()), ptr, len)
            .flags(libc::MSG_NOSIGNAL)
            .build();
        let (res, _, buf) = Op::new(sqe, buf).await;
        if res < 0 {
            (Err(os_err(res)), buf)
        } else {
            (Ok(res as usize), buf)
        }
    }

    /// Shuts down the read half, the write half, or both.
    pub fn shutdown(&self, how: Shutdown) -> io::Result<()> {
        self.sock.shutdown(how)
    }

    /// Enables or disables Nagle's algorithm (`TCP_NODELAY`).
    pub fn set_nodelay(&self, nodelay: bool) -> io::Result<()> {
        self.sock.set_tcp_nodelay(nodelay)
    }

    /// The remote address of this connection.
    pub fn peer_addr(&self) -> io::Result<SocketAddr> {
        self.sock
            .peer_addr()?
            .as_socket()
            .ok_or_else(|| io::ErrorKind::AddrNotAvailable.into())
    }

    /// The local address of this connection.
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.sock
            .local_addr()?
            .as_socket()
            .ok_or_else(|| io::ErrorKind::AddrNotAvailable.into())
    }
}

impl AsRawFd for TcpStream {
    fn as_raw_fd(&self) -> RawFd {
        self.sock.as_raw_fd()
    }
}

impl Drop for TcpStream {
    fn drop(&mut self) {
        close_socket(&mut self.sock, Some(&self.recv));
    }
}

impl std::fmt::Debug for TcpStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TcpStream")
            .field("fd", &self.as_raw_fd())
            .finish()
    }
}

// ------------------------------------------------------------------------- udp

/// Everything a `sendmsg` / `recvmsg` operation points at. Boxed, so the
/// pointers inside `hdr` stay valid while the value is moved around.
struct Msg<B> {
    hdr: libc::msghdr,
    iov: libc::iovec,
    addr: libc::sockaddr_storage,
    buf: B,
}

fn socket_addr_from_storage(
    storage: &libc::sockaddr_storage,
    len: usize,
) -> io::Result<SocketAddr> {
    let invalid = || {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "unsupported or truncated socket address",
        )
    };
    match storage.ss_family as libc::c_int {
        libc::AF_INET if len >= std::mem::size_of::<libc::sockaddr_in>() => {
            // SAFETY: the family says this is a `sockaddr_in`, the kernel wrote
            // at least that many bytes, and `sockaddr_storage` is suitably aligned.
            let a =
                unsafe { &*(storage as *const libc::sockaddr_storage as *const libc::sockaddr_in) };
            let ip = Ipv4Addr::from(u32::from_be(a.sin_addr.s_addr));
            Ok(SocketAddr::V4(SocketAddrV4::new(
                ip,
                u16::from_be(a.sin_port),
            )))
        }
        libc::AF_INET6 if len >= std::mem::size_of::<libc::sockaddr_in6>() => {
            // SAFETY: as above, for `sockaddr_in6`.
            let a = unsafe {
                &*(storage as *const libc::sockaddr_storage as *const libc::sockaddr_in6)
            };
            let ip = Ipv6Addr::from(a.sin6_addr.s6_addr);
            Ok(SocketAddr::V6(SocketAddrV6::new(
                ip,
                u16::from_be(a.sin6_port),
                a.sin6_flowinfo,
                a.sin6_scope_id,
            )))
        }
        _ => Err(invalid()),
    }
}

/// A UDP socket.
///
/// Datagram operations are single-shot and take the buffer by value, like
/// [`TcpStream::read`] and [`TcpStream::write`]. All methods take `&self`;
/// any number of tasks may send at once, and any number may wait to receive
/// (each datagram is delivered to exactly one of them).
///
/// ```no_run
/// use giong::net::UdpSocket;
///
/// # async fn demo() -> std::io::Result<()> {
/// let socket = UdpSocket::bind("127.0.0.1:9000")?;
/// loop {
///     let (res, buf) = socket.recv_from(Vec::with_capacity(1500)).await;
///     let (_len, peer) = res?;
///     socket.send_to(buf, peer).await.0?; // echo it back
/// }
/// # }
/// ```
pub struct UdpSocket {
    sock: ManuallyDrop<Socket>,
    _not_send: PhantomData<*const ()>,
}

impl UdpSocket {
    /// Creates a socket bound to `addr`.
    pub fn bind<A: ToSocketAddrs>(addr: A) -> io::Result<Self> {
        let mut last_err = None;
        for addr in addr.to_socket_addrs()? {
            let bound = Socket::new(Domain::for_address(addr), Type::DGRAM, Some(Protocol::UDP))
                .and_then(|sock| sock.bind(&addr.into()).map(|()| sock));
            match bound {
                Ok(sock) => return Ok(Self::from_socket(sock)),
                Err(e) => last_err = Some(e),
            }
        }
        Err(last_err.unwrap_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "could not resolve to any address",
            )
        }))
    }

    /// Wraps a socket created by the standard library.
    pub fn from_std(socket: std::net::UdpSocket) -> Self {
        // SAFETY: we take over the descriptor that `socket` owned.
        Self::from_socket(unsafe { Socket::from_raw_fd(socket.into_raw_fd()) })
    }

    fn from_socket(sock: Socket) -> Self {
        Self {
            sock: ManuallyDrop::new(sock),
            _not_send: PhantomData,
        }
    }

    /// Sets the default destination for [`send`](Self::send) and restricts
    /// [`recv`](Self::recv) to datagrams from that address.
    pub fn connect<A: ToSocketAddrs>(&self, addr: A) -> io::Result<()> {
        let mut last_err = None;
        for addr in addr.to_socket_addrs()? {
            // Connecting a datagram socket only records the peer; it never blocks.
            match self.sock.connect(&addr.into()) {
                Ok(()) => return Ok(()),
                Err(e) => last_err = Some(e),
            }
        }
        Err(last_err.unwrap_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "could not resolve to any address",
            )
        }))
    }

    /// The address this socket is bound to.
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.sock
            .local_addr()?
            .as_socket()
            .ok_or_else(|| io::ErrorKind::AddrNotAvailable.into())
    }

    /// The address this socket is connected to.
    pub fn peer_addr(&self) -> io::Result<SocketAddr> {
        self.sock
            .peer_addr()?
            .as_socket()
            .ok_or_else(|| io::ErrorKind::AddrNotAvailable.into())
    }

    /// Sends `buf` as one datagram to the connected peer.
    pub async fn send<B: IoBuf>(&self, buf: B) -> (io::Result<usize>, B) {
        let len = buf.len().min(u32::MAX as usize) as u32;
        let sqe = opcode::Send::new(types::Fd(self.sock.as_raw_fd()), buf.as_ptr(), len).build();
        let (res, _, buf) = Op::new(sqe, buf).await;
        if res < 0 {
            (Err(os_err(res)), buf)
        } else {
            (Ok(res as usize), buf)
        }
    }

    /// Receives one datagram from the connected peer into the start of `buf`,
    /// whose length is set to the bytes received. A datagram longer than
    /// `buf.capacity()` is truncated.
    ///
    /// If the returned future is dropped before it completes, a datagram that
    /// arrives at that very moment may be lost.
    pub async fn recv<B: IoBufMut>(&self, mut buf: B) -> (io::Result<usize>, B) {
        let len = buf.capacity().min(u32::MAX as usize) as u32;
        let sqe =
            opcode::Recv::new(types::Fd(self.sock.as_raw_fd()), buf.as_mut_ptr(), len).build();
        let (res, _, mut buf) = Op::new(sqe, buf).await;
        if res < 0 {
            return (Err(os_err(res)), buf);
        }
        // SAFETY: the kernel wrote `res` bytes at the start of the buffer.
        unsafe { buf.set_init(res as usize) };
        (Ok(res as usize), buf)
    }

    /// Sends `buf` as one datagram to `addr`.
    pub async fn send_to<B: IoBuf>(&self, buf: B, addr: SocketAddr) -> (io::Result<usize>, B) {
        let addr = SockAddr::from(addr);
        // SAFETY: an all-zero `msghdr` / `sockaddr_storage` is a valid value.
        let mut msg = Box::new(Msg {
            hdr: unsafe { std::mem::zeroed::<libc::msghdr>() },
            iov: libc::iovec {
                iov_base: buf.as_ptr() as *mut libc::c_void,
                iov_len: buf.len(),
            },
            addr: unsafe { std::mem::zeroed::<libc::sockaddr_storage>() },
            buf,
        });
        // SAFETY: `addr.len()` bytes are valid at `addr.as_ptr()` and fit in a
        // `sockaddr_storage`, which is at least as large as any address.
        unsafe {
            std::ptr::copy_nonoverlapping(
                addr.as_ptr() as *const u8,
                &mut msg.addr as *mut libc::sockaddr_storage as *mut u8,
                addr.len() as usize,
            );
        }
        msg.hdr.msg_name = &mut msg.addr as *mut libc::sockaddr_storage as *mut libc::c_void;
        msg.hdr.msg_namelen = addr.len();
        msg.hdr.msg_iov = &mut msg.iov;
        msg.hdr.msg_iovlen = 1;

        let sqe = opcode::SendMsg::new(types::Fd(self.sock.as_raw_fd()), &msg.hdr).build();
        let (res, _, msg) = Op::new(sqe, msg).await;
        let buf = msg.buf;
        if res < 0 {
            (Err(os_err(res)), buf)
        } else {
            (Ok(res as usize), buf)
        }
    }

    /// Receives one datagram into the start of `buf` and reports who sent it.
    /// The buffer's length is set to the bytes received; a datagram longer
    /// than `buf.capacity()` is truncated.
    ///
    /// If the returned future is dropped before it completes, a datagram that
    /// arrives at that very moment may be lost.
    pub async fn recv_from<B: IoBufMut>(&self, mut buf: B) -> (io::Result<(usize, SocketAddr)>, B) {
        let iov = libc::iovec {
            iov_base: buf.as_mut_ptr() as *mut libc::c_void,
            iov_len: buf.capacity(),
        };
        // SAFETY: an all-zero `msghdr` / `sockaddr_storage` is a valid value.
        let mut msg = Box::new(Msg {
            hdr: unsafe { std::mem::zeroed::<libc::msghdr>() },
            iov,
            addr: unsafe { std::mem::zeroed::<libc::sockaddr_storage>() },
            buf,
        });
        msg.hdr.msg_name = &mut msg.addr as *mut libc::sockaddr_storage as *mut libc::c_void;
        msg.hdr.msg_namelen = std::mem::size_of::<libc::sockaddr_storage>() as libc::socklen_t;
        msg.hdr.msg_iov = &mut msg.iov;
        msg.hdr.msg_iovlen = 1;

        let sqe = opcode::RecvMsg::new(types::Fd(self.sock.as_raw_fd()), &mut msg.hdr).build();
        let (res, _, msg) = Op::new(sqe, msg).await;
        let Msg {
            hdr, addr, mut buf, ..
        } = *msg;
        if res < 0 {
            return (Err(os_err(res)), buf);
        }
        // SAFETY: the kernel wrote `res` bytes at the start of the buffer.
        unsafe { buf.set_init(res as usize) };
        match socket_addr_from_storage(&addr, hdr.msg_namelen as usize) {
            Ok(peer) => (Ok((res as usize, peer)), buf),
            Err(e) => (Err(e), buf),
        }
    }
}

impl AsRawFd for UdpSocket {
    fn as_raw_fd(&self) -> RawFd {
        self.sock.as_raw_fd()
    }
}

impl Drop for UdpSocket {
    fn drop(&mut self) {
        close_socket(&mut self.sock, None);
    }
}

impl std::fmt::Debug for UdpSocket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UdpSocket")
            .field("fd", &self.as_raw_fd())
            .finish()
    }
}
