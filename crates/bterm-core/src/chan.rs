//! A bounded, single-producer single-consumer async channel.
//!
//! Hand-rolled because the workspace deliberately avoids the `futures`
//! crate — it is the only dependency that would pull in a scheduler we do
//! not need, and binary size is a standing constraint.
//!
//! The bound is the point: it is what makes "a pipeline never holds more
//! than N items in flight" true regardless of how fast a producer runs.

use crate::registry::PipelineData;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::future::poll_fn;
use std::rc::Rc;
use std::task::{Poll, Waker};

/// The receiver went away, so nothing will read what you are sending.
/// A producing command treats this as "stop", not as an error to report.
#[derive(Debug, PartialEq, Eq)]
pub struct Closed;

struct Inner {
    buffer: VecDeque<PipelineData>,
    capacity: usize,
    sender_gone: bool,
    receiver_gone: bool,
    /// Woken when the buffer gains an item or the sender goes away.
    recv_waker: Option<Waker>,
    /// Woken when the buffer frees a slot or the receiver goes away.
    send_waker: Option<Waker>,
}

pub fn channel(capacity: usize) -> (Sender, Receiver) {
    debug_assert!(capacity > 0, "a zero-capacity channel can never accept an item");
    let inner = Rc::new(RefCell::new(Inner {
        buffer: VecDeque::new(),
        capacity,
        sender_gone: false,
        receiver_gone: false,
        recv_waker: None,
        send_waker: None,
    }));
    (Sender { inner: inner.clone() }, Receiver { inner })
}

pub struct Sender {
    inner: Rc<RefCell<Inner>>,
}

impl Sender {
    /// Resolves once the item is buffered. Pending while the buffer is full —
    /// this is the backpressure. `Err(Closed)` means the receiver is gone.
    pub async fn send(&self, item: PipelineData) -> Result<(), Closed> {
        let mut slot = Some(item);
        poll_fn(|cx| {
            let mut inner = self.inner.borrow_mut();
            if inner.receiver_gone {
                return Poll::Ready(Err(Closed));
            }
            if inner.buffer.len() < inner.capacity {
                if let Some(item) = slot.take() {
                    inner.buffer.push_back(item);
                }
                let waker = inner.recv_waker.take();
                drop(inner);
                // Wake outside the borrow: a waker may re-enter this channel.
                if let Some(w) = waker {
                    w.wake();
                }
                return Poll::Ready(Ok(()));
            }
            inner.send_waker = Some(cx.waker().clone());
            Poll::Pending
        })
        .await
    }

    /// Items currently buffered. For tests asserting the bound holds.
    pub fn len(&self) -> usize {
        self.inner.borrow().buffer.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// True once the receiver has gone away.
    pub fn is_closed(&self) -> bool {
        self.inner.borrow().receiver_gone
    }
}

impl Drop for Sender {
    fn drop(&mut self) {
        let mut inner = self.inner.borrow_mut();
        inner.sender_gone = true;
        let waker = inner.recv_waker.take();
        drop(inner);
        if let Some(w) = waker {
            w.wake();
        }
    }
}

pub struct Receiver {
    inner: Rc<RefCell<Inner>>,
}

impl Receiver {
    /// `None` means end of stream: the sender is gone and the buffer is
    /// drained. Pending means more may yet arrive.
    pub async fn recv(&mut self) -> Option<PipelineData> {
        poll_fn(|cx| {
            let mut inner = self.inner.borrow_mut();
            if let Some(item) = inner.buffer.pop_front() {
                let waker = inner.send_waker.take();
                drop(inner);
                if let Some(w) = waker {
                    w.wake();
                }
                return Poll::Ready(Some(item));
            }
            if inner.sender_gone {
                return Poll::Ready(None);
            }
            inner.recv_waker = Some(cx.waker().clone());
            Poll::Pending
        })
        .await
    }
}

impl Drop for Receiver {
    fn drop(&mut self) {
        let mut inner = self.inner.borrow_mut();
        inner.receiver_gone = true;
        let waker = inner.send_waker.take();
        drop(inner);
        if let Some(w) = waker {
            w.wake();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::block_on;
    use crate::registry::PipelineData;
    use crate::value::Value;

    fn item(n: i64) -> PipelineData {
        PipelineData::Value(Value::Int(n))
    }

    #[test]
    fn items_arrive_in_order_then_the_stream_ends() {
        let (tx, mut rx) = channel(4);
        block_on(async {
            tx.send(item(1)).await.expect("send 1");
            tx.send(item(2)).await.expect("send 2");
            drop(tx);
            assert_eq!(rx.recv().await, Some(item(1)));
            assert_eq!(rx.recv().await, Some(item(2)));
            // Sender dropped and buffer drained: end of stream, not pending.
            assert_eq!(rx.recv().await, None);
        });
    }

    #[test]
    fn capacity_is_a_real_bound() {
        let (tx, mut rx) = channel(2);
        block_on(async {
            tx.send(item(1)).await.expect("send 1");
            tx.send(item(2)).await.expect("send 2");
            // The buffer holds exactly `capacity`; this is the bounded-memory
            // guarantee, so assert on it rather than trusting the constant.
            assert_eq!(tx.len(), 2);
            assert_eq!(rx.recv().await, Some(item(1)));
            assert_eq!(tx.len(), 1);
        });
    }
}
