//! Minimal single-threaded abortable future — no futures crate. The wasm
//! layer wraps every submitted pipeline in one of these; Ctrl-C (and
//! `dispose()`) abort via the handle, which also wakes the task so it
//! settles promptly.

use std::cell::{Cell, RefCell};
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::task::{Context, Poll, Waker};

#[derive(Default)]
struct AbortState {
    aborted: Cell<bool>,
    waker: RefCell<Option<Waker>>,
}

#[derive(Clone)]
pub struct AbortHandle(Rc<AbortState>);

impl AbortHandle {
    pub fn abort(&self) {
        self.0.aborted.set(true);
        if let Some(w) = self.0.waker.borrow_mut().take() {
            w.wake();
        }
    }

    pub fn is_aborted(&self) -> bool {
        self.0.aborted.get()
    }
}

/// The inner future's output, or `Err(Aborted)` if the handle fired first.
/// The abort flag is checked *before* polling the inner future, so an
/// aborted task never resumes its body (and never touches engine state
/// again).
pub struct Aborted;

pub struct Abortable<F> {
    inner: F,
    state: Rc<AbortState>,
}

impl<F: Future> Abortable<F> {
    pub fn wrap(inner: F) -> (Self, AbortHandle) {
        let state = Rc::new(AbortState::default());
        (Abortable { inner, state: state.clone() }, AbortHandle(state))
    }
}

impl<F: Future> Future for Abortable<F> {
    type Output = Result<F::Output, Aborted>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        // SAFETY: standard pin projection — `inner` is never moved out.
        let this = unsafe { self.get_unchecked_mut() };
        if this.state.aborted.get() {
            return Poll::Ready(Err(Aborted));
        }
        let inner = unsafe { Pin::new_unchecked(&mut this.inner) };
        match inner.poll(cx) {
            Poll::Ready(v) => Poll::Ready(Ok(v)),
            Poll::Pending => {
                *this.state.waker.borrow_mut() = Some(cx.waker().clone());
                Poll::Pending
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::task::{RawWaker, RawWakerVTable};

    fn noop_waker() -> Waker {
        fn raw() -> RawWaker {
            fn clone(_: *const ()) -> RawWaker {
                raw()
            }
            fn noop(_: *const ()) {}
            RawWaker::new(std::ptr::null(), &RawWakerVTable::new(clone, noop, noop, noop))
        }
        // SAFETY: all vtable fns are no-ops.
        unsafe { Waker::from_raw(raw()) }
    }

    #[test]
    fn completes_normally_when_not_aborted() {
        let (fut, _handle) = Abortable::wrap(std::future::ready(7));
        let waker = noop_waker();
        let mut cx = Context::from_waker(&waker);
        let mut fut = Box::pin(fut);
        match fut.as_mut().poll(&mut cx) {
            Poll::Ready(Ok(7)) => {}
            _ => panic!("expected Ready(Ok(7))"),
        }
    }

    #[test]
    fn abort_before_poll_never_runs_body() {
        let ran = Rc::new(Cell::new(false));
        let ran2 = ran.clone();
        let (fut, handle) = Abortable::wrap(async move {
            ran2.set(true);
        });
        handle.abort();
        let waker = noop_waker();
        let mut cx = Context::from_waker(&waker);
        let mut fut = Box::pin(fut);
        match fut.as_mut().poll(&mut cx) {
            Poll::Ready(Err(Aborted)) => {}
            _ => panic!("expected aborted"),
        }
        assert!(!ran.get(), "aborted future must not run");
    }

    #[test]
    fn abort_mid_pending_settles_aborted() {
        // A future that is pending once, then would return 1.
        struct PendingOnce(bool);
        impl Future for PendingOnce {
            type Output = i32;
            fn poll(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<i32> {
                if self.0 {
                    Poll::Ready(1)
                } else {
                    self.0 = true;
                    Poll::Pending
                }
            }
        }
        let (fut, handle) = Abortable::wrap(PendingOnce(false));
        let waker = noop_waker();
        let mut cx = Context::from_waker(&waker);
        let mut fut = Box::pin(fut);
        assert!(matches!(fut.as_mut().poll(&mut cx), Poll::Pending));
        handle.abort();
        assert!(handle.is_aborted());
        match fut.as_mut().poll(&mut cx) {
            Poll::Ready(Err(Aborted)) => {}
            _ => panic!("expected aborted after handle.abort()"),
        }
    }
}
