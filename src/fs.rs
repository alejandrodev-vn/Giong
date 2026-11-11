//! Files on io_uring.
//!
//! Every operation — open, read, write, sync, close — goes through the ring,
//! so file I/O never blocks the worker thread and needs no helper thread pool
//! in user space.
//!
//! Reads and writes are **positional** (`read_at`, `write_at`): a file has no
//! implicit cursor, so one `File` can serve many tasks at once.
//!
//! ```no_run
//! use giong::fs::File;
//!
//! # async fn demo() -> std::io::Result<()> {
//! let file = File::create("hello.txt").await?;
//! let (res, _buf) = file.write_all_at(b"hello".to_vec(), 0).await;
//! res?;
//! file.sync_all().await?;
//!
//! let file = File::open("hello.txt").await?;
//! let (res, buf) = file.read_at(Vec::with_capacity(64), 0).await;
//! assert_eq!(&buf[..res?], b"hello");
//! # Ok(())
//! # }
//! ```

use std::ffi::CString;
use std::future::Future;
use std::io;
use std::marker::PhantomData;
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::pin::Pin;
use std::task::{Context, Poll};

use io_uring::{opcode, types};

use crate::buf::{IoBuf, IoBufMut};
use crate::op::Op;
use crate::runtime;

fn os_err(res: i32) -> io::Error {
    io::Error::from_raw_os_error(-res)
}

/// Options for opening a file, mirroring [`std::fs::OpenOptions`].
#[derive(Clone, Debug)]
pub struct OpenOptions {
    read: bool,
    write: bool,
    append: bool,
    truncate: bool,
    create: bool,
    create_new: bool,
    mode: u32,
}

impl Default for OpenOptions {
    fn default() -> Self {
        Self::new()
    }
}

impl OpenOptions {
    /// All options off; files are created with mode `0o666` (before umask).
    pub fn new() -> Self {
        Self {
            read: false,
            write: false,
            append: false,
            truncate: false,
            create: false,
            create_new: false,
            mode: 0o666,
        }
    }

    /// Open for reading.
    pub fn read(mut self, read: bool) -> Self {
        self.read = read;
        self
    }

    /// Open for writing.
    pub fn write(mut self, write: bool) -> Self {
        self.write = write;
        self
    }

    /// Open for appending: every write goes to the end of the file,
    /// whatever offset is passed to `write_at`.
    pub fn append(mut self, append: bool) -> Self {
        self.append = append;
        self
    }

    /// Truncate the file to zero length when opening it.
    pub fn truncate(mut self, truncate: bool) -> Self {
        self.truncate = truncate;
        self
    }

    /// Create the file if it does not exist.
    pub fn create(mut self, create: bool) -> Self {
        self.create = create;
        self
    }

    /// Create the file, failing if it already exists.
    pub fn create_new(mut self, create_new: bool) -> Self {
        self.create_new = create_new;
        self
    }

    /// Permission bits used when a file is created.
    pub fn mode(mut self, mode: u32) -> Self {
        self.mode = mode;
        self
    }

    fn flags(&self) -> io::Result<i32> {
        let invalid = || io::Error::from(io::ErrorKind::InvalidInput);
        let access = match (self.read, self.write || self.append) {
            (true, false) => libc::O_RDONLY,
            (false, true) => libc::O_WRONLY,
            (true, true) => libc::O_RDWR,
            (false, false) => return Err(invalid()),
        };
        let writable = self.write || self.append;
        if !writable && (self.truncate || self.create || self.create_new) {
            return Err(invalid());
        }
        if self.append && self.truncate {
            return Err(invalid());
        }
        let mut flags = access | libc::O_CLOEXEC;
        if self.append {
            flags |= libc::O_APPEND;
        }
        if self.truncate {
            flags |= libc::O_TRUNC;
        }
        if self.create_new {
            flags |= libc::O_CREAT | libc::O_EXCL;
        } else if self.create {
            flags |= libc::O_CREAT;
        }
        Ok(flags)
    }

    /// Opens the file at `path` with these options.
    pub async fn open(&self, path: impl AsRef<Path>) -> io::Result<File> {
        let flags = self.flags()?;
        // The kernel reads the path when the operation runs, so it must stay
        // alive (and in place) until then: a `CString` keeps it on the heap.
        let path = CString::new(path.as_ref().as_os_str().as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains a NUL byte"))?;
        let sqe = opcode::OpenAt::new(types::Fd(libc::AT_FDCWD), path.as_ptr())
            .flags(flags)
            .mode(self.mode as libc::mode_t)
            .build();
        let (res, _, _path) = Op::new(sqe, path).await;
        if res < 0 {
            return Err(os_err(res));
        }
        Ok(File {
            fd: res,
            _not_send: PhantomData,
        })
    }
}

/// An open file.
///
/// Dropping a `File` closes it in the background. Use [`File::close`] to
/// learn whether the close succeeded.
pub struct File {
    fd: RawFd,
    _not_send: PhantomData<*const ()>,
}

impl File {
    /// Opens an existing file for reading.
    pub async fn open(path: impl AsRef<Path>) -> io::Result<File> {
        OpenOptions::new().read(true).open(path).await
    }

    /// Opens a file for writing, creating it if needed and truncating it if
    /// it exists.
    pub async fn create(path: impl AsRef<Path>) -> io::Result<File> {
        OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)
            .await
    }

    /// A fresh set of [`OpenOptions`].
    pub fn options() -> OpenOptions {
        OpenOptions::new()
    }

    /// Wraps a file opened by the standard library.
    pub fn from_std(file: std::fs::File) -> File {
        File {
            fd: file.into_raw_fd(),
            _not_send: PhantomData,
        }
    }

    /// Reads up to `buf.capacity()` bytes starting at `offset`, into the start
    /// of `buf`, and returns the buffer with its length set to the bytes read.
    /// `Ok(0)` means `offset` is at or past the end of the file.
    pub async fn read_at<B: IoBufMut>(&self, mut buf: B, offset: u64) -> (io::Result<usize>, B) {
        let len = buf.capacity().min(u32::MAX as usize) as u32;
        let sqe = opcode::Read::new(types::Fd(self.fd), buf.as_mut_ptr(), len)
            .offset(offset)
            .build();
        let (res, _, mut buf) = Op::new(sqe, buf).await;
        if res < 0 {
            return (Err(os_err(res)), buf);
        }
        // SAFETY: the kernel wrote `res` bytes at the start of the buffer.
        unsafe { buf.set_init(res as usize) };
        (Ok(res as usize), buf)
    }

    /// Writes some bytes of `buf` at `offset`, returning how many were written.
    ///
    /// If the returned future is dropped before it completes, the write may
    /// still take place.
    pub async fn write_at<B: IoBuf>(&self, buf: B, offset: u64) -> (io::Result<usize>, B) {
        self.write_from(buf, 0, offset).await
    }

    /// Writes all of `buf` starting at `offset`.
    pub async fn write_all_at<B: IoBuf>(&self, mut buf: B, offset: u64) -> (io::Result<()>, B) {
        let mut written = 0;
        while written < buf.len() {
            let (res, b) = self.write_from(buf, written, offset + written as u64).await;
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

    async fn write_from<B: IoBuf>(
        &self,
        buf: B,
        skip: usize,
        offset: u64,
    ) -> (io::Result<usize>, B) {
        debug_assert!(skip <= buf.len());
        let len = (buf.len() - skip).min(u32::MAX as usize) as u32;
        // SAFETY: `skip <= len`, so the pointer stays within the buffer.
        let ptr = unsafe { buf.as_ptr().add(skip) };
        let sqe = opcode::Write::new(types::Fd(self.fd), ptr, len)
            .offset(offset)
            .build();
        let (res, _, buf) = Op::new(sqe, buf).await;
        if res < 0 {
            (Err(os_err(res)), buf)
        } else {
            (Ok(res as usize), buf)
        }
    }

    /// Flushes file data and metadata to the storage device (`fsync`).
    pub async fn sync_all(&self) -> io::Result<()> {
        self.fsync(types::FsyncFlags::empty()).await
    }

    /// Flushes file data, but not necessarily metadata such as timestamps
    /// (`fdatasync`).
    pub async fn sync_data(&self) -> io::Result<()> {
        self.fsync(types::FsyncFlags::DATASYNC).await
    }

    async fn fsync(&self, flags: types::FsyncFlags) -> io::Result<()> {
        let sqe = opcode::Fsync::new(types::Fd(self.fd)).flags(flags).build();
        let (res, _, ()) = Op::new(sqe, ()).await;
        if res < 0 {
            Err(os_err(res))
        } else {
            Ok(())
        }
    }

    /// The size of the file in bytes.
    pub fn len(&self) -> io::Result<u64> {
        // SAFETY: borrows the descriptor for the duration of the call; the
        // `ManuallyDrop` keeps the temporary `std::fs::File` from closing it.
        let file = std::mem::ManuallyDrop::new(unsafe { std::fs::File::from_raw_fd(self.fd) });
        Ok(file.metadata()?.len())
    }

    /// `true` if the file is empty.
    pub fn is_empty(&self) -> io::Result<bool> {
        Ok(self.len()? == 0)
    }

    /// Closes the file and reports whether that succeeded.
    pub async fn close(self) -> io::Result<()> {
        let fd = self.fd;
        // From here on the `Close` future owns the descriptor.
        std::mem::forget(self);
        let sqe = opcode::Close::new(types::Fd(fd)).build();
        let res = Close {
            op: Op::new(sqe, ()),
            fd,
            submitted: false,
        }
        .await;
        if res < 0 {
            Err(os_err(res))
        } else {
            Ok(())
        }
    }
}

/// The future behind [`File::close`]. Makes sure the descriptor is closed
/// exactly once, whenever the future is dropped.
struct Close {
    op: Op<()>,
    fd: RawFd,
    /// The close has been handed to the driver, which will carry it out even
    /// if this future goes away.
    submitted: bool,
}

impl Future for Close {
    type Output = i32;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<i32> {
        // The first poll of an `Op` always submits it.
        self.submitted = true;
        Pin::new(&mut self.op).poll(cx).map(|(res, _, ())| res)
    }
}

impl Drop for Close {
    fn drop(&mut self) {
        if !self.submitted {
            // SAFETY: we own the descriptor and nobody else will close it.
            drop(unsafe { OwnedFd::from_raw_fd(self.fd) });
        }
    }
}

impl AsRawFd for File {
    fn as_raw_fd(&self) -> RawFd {
        self.fd
    }
}

impl Drop for File {
    fn drop(&mut self) {
        let fd = self.fd;
        let queued = runtime::try_with_worker(|w| match w.driver.try_borrow_mut() {
            Ok(mut driver) => {
                driver.close_fd(fd);
                true
            }
            Err(_) => false,
        })
        .unwrap_or(false);
        if !queued {
            // SAFETY: we own the descriptor and close it exactly once.
            drop(unsafe { OwnedFd::from_raw_fd(fd) });
        }
    }
}

impl std::fmt::Debug for File {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("File").field("fd", &self.fd).finish()
    }
}
