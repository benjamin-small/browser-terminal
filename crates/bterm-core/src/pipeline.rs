//! Drives a pipeline's stages concurrently inside a single future.
//!
//! There is deliberately no spawner. The wasm host has a real executor and
//! the native harness is a hand-rolled `block_on` that polls exactly one
//! future with a no-op waker; a spawner abstraction would mean the core's
//! own tests could not exercise concurrent stages at all. Owning the stage
//! futures here means one code path, and deterministic interleaving in
//! tests.

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll, Wake, Waker};

/// One pipeline stage: runs to completion, communicating over channels.
///
/// The lifetime is load-bearing. A bare `Pin<Box<dyn Future>>` implies
/// `+ 'static`, and stage futures borrow the parsed call, the command
/// source, the context, and the scope — all owned by the caller. Without
/// `'a` this does not compile, and the error surfaces at the call site
/// rather than here.
pub type BoxedStage<'a> = Pin<Box<dyn Future<Output = ()> + 'a>>;

/// A waker that records that it fired, then forwards to the real one.
///
/// This is what makes the driver correct under `block_on`'s no-op waker. A
/// stage made ready *during* a polling pass — because an earlier stage sent
/// to its channel — would otherwise never be re-polled: the real wake is
/// discarded natively, and the driver has already passed it this round.
/// Recording the wake lets the driver loop again immediately.
///
/// `Arc` and `AtomicBool` are required by `std::task::Wake`, not chosen —
/// nothing here crosses a thread, since wasm is single-threaded. They
/// cannot be swapped for `Rc`/`Cell`; the trait will not accept it.
struct NudgeWaker {
    woken: AtomicBool,
    /// The executor's waker, so a genuinely external wake (a JS promise
    /// resolving in the browser) still reaches the real scheduler.
    outer: Waker,
}

impl Wake for NudgeWaker {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.woken.store(true, Ordering::Relaxed);
        self.outer.wake_by_ref();
    }
}

/// Poll every stage until all complete.
///
/// Each pass polls all unfinished stages. If any woke during the pass —
/// including a wake caused by another stage in the same pass — it loops
/// again rather than returning Pending, because under a no-op waker that
/// progress would otherwise be lost.
pub async fn drive(stages: Vec<BoxedStage<'_>>) {
    let mut stages: Vec<Option<BoxedStage<'_>>> = stages.into_iter().map(Some).collect();

    std::future::poll_fn(move |cx| {
        loop {
            let nudge = Arc::new(NudgeWaker {
                woken: AtomicBool::new(false),
                outer: cx.waker().clone(),
            });
            let waker = Waker::from(nudge.clone());
            let mut inner = Context::from_waker(&waker);

            let mut remaining = 0;
            for slot in stages.iter_mut() {
                if let Some(stage) = slot {
                    match stage.as_mut().poll(&mut inner) {
                        Poll::Ready(()) => *slot = None,
                        Poll::Pending => remaining += 1,
                    }
                }
            }

            if remaining == 0 {
                return Poll::Ready(());
            }
            if !nudge.woken.load(Ordering::Relaxed) {
                // Nothing became ready this pass; each stage has registered
                // the real waker with whatever it is waiting on.
                return Poll::Pending;
            }
        }
    })
    .await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::block_on;
    use std::cell::RefCell;
    use std::rc::Rc;

    /// A future that returns Pending `n` times before completing, recording
    /// the order in which it ran. Models a stage that awaits.
    fn yielding(
        label: &'static str,
        mut n: usize,
        log: Rc<RefCell<Vec<&'static str>>>,
    ) -> BoxedStage<'static> {
        Box::pin(std::future::poll_fn(move |cx| {
            log.borrow_mut().push(label);
            if n == 0 {
                return std::task::Poll::Ready(());
            }
            n -= 1;
            // Ask to be polled again. With `block_on`'s no-op waker this
            // wake is discarded, so a driver that trusted the executor
            // would hang here — proving the driver re-polls on its own.
            cx.waker().wake_by_ref();
            std::task::Poll::Pending
        }))
    }

    #[test]
    fn every_stage_runs_to_completion() {
        let log = Rc::new(RefCell::new(Vec::new()));
        block_on(drive(vec![
            yielding("a", 2, log.clone()),
            yielding("b", 0, log.clone()),
            yielding("c", 1, log.clone()),
        ]));
        let seen = log.borrow();
        assert!(seen.iter().filter(|l| **l == "a").count() >= 3);
        assert!(seen.contains(&"b"));
        assert!(seen.contains(&"c"));
    }

    #[test]
    fn stages_interleave_rather_than_running_one_at_a_time() {
        // If the driver awaited stages sequentially, "a" would appear three
        // times before "b" ever ran. Interleaving is the whole point.
        let log = Rc::new(RefCell::new(Vec::new()));
        block_on(drive(vec![
            yielding("a", 2, log.clone()),
            yielding("b", 2, log.clone()),
        ]));
        let seen = log.borrow();
        let first_b = seen.iter().position(|l| *l == "b").expect("b ran");
        let last_a = seen.iter().rposition(|l| *l == "a").expect("a ran");
        assert!(first_b < last_a, "stages did not interleave: {seen:?}");
    }
}
