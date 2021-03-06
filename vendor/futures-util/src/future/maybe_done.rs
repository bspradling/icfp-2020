//! Definition of the MaybeDone combinator

use core::mem;
use core::pin::Pin;
use futures_core::future::{FusedFuture, Future};
use futures_core::task::{Context, Poll};
use pin_project::{pin_project, project};

/// A future that may have completed.
///
/// This is created by the [`maybe_done()`] function.
#[pin_project]
#[derive(Debug)]
pub enum MaybeDone<Fut: Future> {
    /// A not-yet-completed future
    Future(#[pin] Fut),
    /// The output of the completed future
    Done(Fut::Output),
    /// The empty variant after the result of a [`MaybeDone`] has been
    /// taken using the [`take_output`](MaybeDone::take_output) method.
    Gone,
}

/// Wraps a future into a `MaybeDone`
///
/// # Examples
///
/// ```
/// # futures::executor::block_on(async {
/// use futures::future;
/// use futures::pin_mut;
///
/// let future = future::maybe_done(async { 5 });
/// pin_mut!(future);
/// assert_eq!(future.as_mut().take_output(), None);
/// let () = future.as_mut().await;
/// assert_eq!(future.as_mut().take_output(), Some(5));
/// assert_eq!(future.as_mut().take_output(), None);
/// # });
/// ```
pub fn maybe_done<Fut: Future>(future: Fut) -> MaybeDone<Fut> {
    MaybeDone::Future(future)
}

impl<Fut: Future> MaybeDone<Fut> {
    /// Returns an [`Option`] containing a mutable reference to the output of the future.
    /// The output of this method will be [`Some`] if and only if the inner
    /// future has been completed and [`take_output`](MaybeDone::take_output)
    /// has not yet been called.
    #[project]
    #[inline]
    pub fn output_mut(self: Pin<&mut Self>) -> Option<&mut Fut::Output> {
        #[project]
        match self.project() {
            MaybeDone::Done(res) => Some(res),
            _ => None,
        }
    }

    /// Attempt to take the output of a `MaybeDone` without driving it
    /// towards completion.
    #[inline]
    pub fn take_output(self: Pin<&mut Self>) -> Option<Fut::Output> {
        // Safety: we return immediately unless we are in the `Done`
        // state, which does not have any pinning guarantees to uphold.
        //
        // Hopefully `pin_project` will support this safely soon:
        // https://github.com/taiki-e/pin-project/issues/184
        unsafe {
            let this = self.get_unchecked_mut();
            match this {
                MaybeDone::Done(_) => {},
                MaybeDone::Future(_) | MaybeDone::Gone => return None,
            };
            if let MaybeDone::Done(output) = mem::replace(this, MaybeDone::Gone) {
                Some(output)
            } else {
                unreachable!()
            }
        }
    }
}

impl<Fut: Future> FusedFuture for MaybeDone<Fut> {
    fn is_terminated(&self) -> bool {
        match self {
            MaybeDone::Future(_) => false,
            MaybeDone::Done(_) | MaybeDone::Gone => true,
        }
    }
}

impl<Fut: Future> Future for MaybeDone<Fut> {
    type Output = ();

    #[project]
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        #[project]
        match self.as_mut().project() {
            MaybeDone::Future(f) => {
                let res = ready!(f.poll(cx));
                self.set(MaybeDone::Done(res));
            },
            MaybeDone::Done(_) => {},
            MaybeDone::Gone => panic!("MaybeDone polled after value taken"),
        }
        Poll::Ready(())
    }
}
