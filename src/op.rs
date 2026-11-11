//! A single-shot io_uring operation as a future.

use std::any::Any;
use std::future::Future;
use std::marker::PhantomData;
use std::mem;
use std::pin::Pin;
use std::task::{Context, Poll};

use io_uring::squeue;

use crate::runtime;

/// One SQE → one CQE.
///
/// `T` are the resources the SQE points at (typically the buffer). They are
/// owned by the future while the operation is in flight and returned together
/// with the result. If the future is dropped first, ownership moves to the
/// driver until the kernel reports completion (see `driver` module docs).
pub(crate) struct Op<T: 'static> {
    sqe: Option<squeue::Entry>,
    /// Driver key while in flight.
    key: Option<usize>,
    data: Option<T>,
    /// The worker whose driver owns `key`.
    worker_uid: usize,
    /// Operations are tied to one worker thread.
    _not_send: PhantomData<*const ()>,
}

// `Op` never hands out a pinned reference to `T`; it only moves it in and out.
impl<T: 'static> Unpin for Op<T> {}

impl<T: 'static> Op<T> {
    /// # Safety contract (upheld by callers in this crate)
    ///
    /// Every pointer in `sqe` must point into memory owned by `data` (or into
    /// `'static` memory) at an address that does not change when `data` moves.
    pub(crate) fn new(sqe: squeue::Entry, data: T) -> Self {
        Self {
            sqe: Some(sqe),
            key: None,
            data: Some(data),
            worker_uid: 0,
            _not_send: PhantomData,
        }
    }
}

impl<T: 'static> Future for Op<T> {
    /// `(result, cqe flags, resources)`.
    type Output = (i32, u32, T);

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = &mut *self;
        runtime::with_worker(|w| {
            let mut driver = w.driver.borrow_mut();
            match this.key {
                None => {
                    let sqe = this
                        .sqe
                        .take()
                        .expect("giong: operation polled after completion");
                    this.key = Some(driver.submit_op(sqe, cx.waker()));
                    this.worker_uid = w.shared.uid;
                    Poll::Pending
                }
                Some(key) => {
                    assert_eq!(
                        this.worker_uid, w.shared.uid,
                        "giong: operation polled on a different runtime"
                    );
                    match driver.poll_op(key, cx.waker()) {
                        Poll::Ready((res, flags)) => {
                            this.key = None;
                            let data = this.data.take().expect("operation resources");
                            Poll::Ready((res, flags, data))
                        }
                        Poll::Pending => Poll::Pending,
                    }
                }
            }
        })
    }
}

impl<T: 'static> Drop for Op<T> {
    fn drop(&mut self) {
        // Never submitted, or already completed: nothing is in flight.
        let Some(key) = self.key.take() else { return };
        let mut data = self.data.take();
        let handed_over = runtime::try_with_worker(|w| {
            if w.shared.uid != self.worker_uid {
                return false;
            }
            let Ok(mut driver) = w.driver.try_borrow_mut() else {
                return false;
            };
            driver.cancel_op(key, data.take().map(|d| Box::new(d) as Box<dyn Any>));
            true
        })
        .unwrap_or(false);
        if !handed_over {
            // The driver is unreachable, so we cannot know when the kernel is
            // done with this memory. Leaking it is the only safe option.
            mem::forget(data);
        }
    }
}
