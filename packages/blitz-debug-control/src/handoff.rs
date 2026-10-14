use std::future::{Future, poll_fn};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use super::{ControlRequest, ControlResponse};

/// A command was not immediately available.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TryRecvError {
    Empty,
    Disconnected,
}

impl std::fmt::Display for TryRecvError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Empty => "renderer command queue is empty",
            Self::Disconnected => "renderer command queue is closed",
        })
    }
}

impl std::error::Error for TryRecvError {}

/// A blocking command receive outlived its deadline or server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecvTimeoutError {
    Timeout,
    Disconnected,
}

impl std::fmt::Display for RecvTimeoutError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Timeout => "renderer command receive timed out",
            Self::Disconnected => "renderer command queue is closed",
        })
    }
}

impl std::error::Error for RecvTimeoutError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecvError;

impl std::fmt::Display for RecvError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("renderer command queue is closed")
    }
}

impl std::error::Error for RecvError {}

/// Value-delivery adapter over the consumer-owned typed command ring.
pub struct ValueReceiver(pub(crate) CommandReceiver);

impl ValueReceiver {
    pub fn try_recv(&self) -> Result<ControlRequest, TryRecvError> {
        self.0.try_recv().map(Self::into_value)
    }

    pub fn recv(&self) -> Result<ControlRequest, RecvError> {
        nagoya::block_on(self.0.recv())
            .map(Self::into_value)
            .map_err(|_| RecvError)
    }

    pub fn recv_timeout(&self, timeout: Duration) -> Result<ControlRequest, RecvTimeoutError> {
        self.0.recv_timeout(timeout).map(Self::into_value)
    }

    pub fn iter(&self) -> ValueIter<'_> {
        ValueIter(self)
    }

    pub fn try_iter(&self) -> TryValueIter<'_> {
        TryValueIter(self)
    }

    fn into_value(request: Arc<ControlRequest>) -> ControlRequest {
        Arc::try_unwrap(request).expect("queued request has one consumer")
    }
}

pub struct ValueIter<'a>(&'a ValueReceiver);

impl Iterator for ValueIter<'_> {
    type Item = ControlRequest;

    fn next(&mut self) -> Option<Self::Item> {
        self.0.recv().ok()
    }
}

pub struct TryValueIter<'a>(&'a ValueReceiver);

impl Iterator for TryValueIter<'_> {
    type Item = ControlRequest;

    fn next(&mut self) -> Option<Self::Item> {
        self.0.try_recv().ok()
    }
}

impl<'a> IntoIterator for &'a ValueReceiver {
    type Item = ControlRequest;
    type IntoIter = ValueIter<'a>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

pub struct IntoValueIter(ValueReceiver);

impl Iterator for IntoValueIter {
    type Item = ControlRequest;

    fn next(&mut self) -> Option<Self::Item> {
        self.0.recv().ok()
    }
}

impl IntoIterator for ValueReceiver {
    type Item = ControlRequest;
    type IntoIter = IntoValueIter;

    fn into_iter(self) -> Self::IntoIter {
        IntoValueIter(self)
    }
}

struct Ring<T> {
    producer: spsc::Producer<Arc<T>>,
    consumer: spsc::Consumer<Arc<T>>,
    waiter: Option<Waker>,
    closed: bool,
}

pub(crate) struct CommandQueue(Mutex<Ring<ControlRequest>>);

pub(crate) enum QueueError {
    Full,
    Closed,
}

impl CommandQueue {
    pub(crate) fn new(capacity: usize) -> Arc<Self> {
        let (producer, consumer) = spsc::make(capacity);
        Arc::new(Self(Mutex::new(Ring {
            producer,
            consumer,
            waiter: None,
            closed: false,
        })))
    }

    pub(crate) fn enqueue(&self, request: Arc<ControlRequest>) -> Result<(), QueueError> {
        // All producers share this lock. The ring has one producer lane and
        // one consumer lane even when reactor tasks move between workers.
        let wake = {
            let mut ring = self.0.lock().unwrap();
            if ring.closed {
                return Err(QueueError::Closed);
            }
            if ring.producer.try_push(request).is_some() {
                return Err(QueueError::Full);
            }
            ring.waiter.take()
        };
        if let Some(wake) = wake {
            wake.wake();
        }
        Ok(())
    }

    pub(crate) fn close(&self) {
        let wake = {
            let mut ring = self.0.lock().unwrap();
            ring.closed = true;
            ring.waiter.take()
        };
        if let Some(wake) = wake {
            wake.wake();
        }
    }
}

/// Single renderer consumer of typed request pointers.
///
/// Every returned Arc is the pointer enqueued by the transport. Receiving
/// does not copy the method, path or JSON body.
pub struct CommandReceiver(pub(crate) Arc<CommandQueue>);

impl CommandReceiver {
    /// Take a queued request without waiting.
    pub fn try_recv(&self) -> Result<Arc<ControlRequest>, TryRecvError> {
        let ring = self.0.0.lock().unwrap();
        match ring.consumer.try_pop() {
            Some(request) => Ok(request),
            None if ring.closed => Err(TryRecvError::Disconnected),
            None => Err(TryRecvError::Empty),
        }
    }

    /// Await one request. Only one receive future may wait at a time.
    pub async fn recv(&self) -> Result<Arc<ControlRequest>, TryRecvError> {
        poll_fn(|cx| {
            let mut ring = self.0.0.lock().unwrap();
            if let Some(request) = ring.consumer.try_pop() {
                Poll::Ready(Ok(request))
            } else if ring.closed {
                Poll::Ready(Err(TryRecvError::Disconnected))
            } else {
                ring.waiter = Some(cx.waker().clone());
                Poll::Pending
            }
        })
        .await
    }

    /// Blocking adapter for an existing headless renderer entry point.
    pub fn recv_timeout(&self, timeout: Duration) -> Result<Arc<ControlRequest>, RecvTimeoutError> {
        match nagoya::block_on(nagoya::time::timeout(timeout, self.recv())) {
            Ok(Ok(request)) => Ok(request),
            Ok(Err(_)) => Err(RecvTimeoutError::Disconnected),
            Err(_) => Err(RecvTimeoutError::Timeout),
        }
    }
}

impl Drop for CommandReceiver {
    fn drop(&mut self) {
        self.0.close();
        // Release requests that nobody can now take, waking response waiters.
        while self.0.0.lock().unwrap().consumer.try_pop().is_some() {}
    }
}

#[derive(Debug)]
struct ReplyState {
    response: Option<Arc<ControlResponse>>,
    waiter: Option<Waker>,
    completed: bool,
    sender_closed: bool,
    receiver_alive: bool,
}

#[derive(Debug)]
pub(crate) struct Reply(Arc<Mutex<ReplyState>>);

impl Reply {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self(Arc::new(Mutex::new(ReplyState {
            response: None,
            waiter: None,
            completed: false,
            sender_closed: false,
            receiver_alive: true,
        }))))
    }

    pub(crate) fn complete(
        &self,
        response: Arc<ControlResponse>,
    ) -> Result<(), Arc<ControlResponse>> {
        let wake = {
            let mut state = self.0.lock().unwrap();
            if !state.receiver_alive || state.completed || state.sender_closed {
                return Err(response);
            }
            state.response = Some(response);
            state.completed = true;
            state.waiter.take()
        };
        if let Some(wake) = wake {
            wake.wake();
        }
        Ok(())
    }

    pub(crate) fn close_sender(&self) {
        let wake = {
            let mut state = self.0.lock().unwrap();
            state.sender_closed = true;
            state.waiter.take()
        };
        if let Some(wake) = wake {
            wake.wake();
        }
    }
}

impl Drop for Reply {
    fn drop(&mut self) {
        self.close_sender();
    }
}

pub(crate) struct ResponseWait(Arc<Mutex<ReplyState>>);

impl ResponseWait {
    pub(crate) fn new(reply: &Arc<Reply>) -> Self {
        // Own completion state without keeping the request's sender alive.
        Self(Arc::clone(&reply.0))
    }
}

impl Future for ResponseWait {
    type Output = Result<Arc<ControlResponse>, ()>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut state = self.0.lock().unwrap();
        if let Some(response) = state.response.take() {
            Poll::Ready(Ok(response))
        } else if state.sender_closed {
            Poll::Ready(Err(()))
        } else {
            state.waiter = Some(cx.waker().clone());
            Poll::Pending
        }
    }
}

impl Drop for ResponseWait {
    fn drop(&mut self) {
        let mut state = self.0.lock().unwrap();
        state.receiver_alive = false;
        state.waiter = None;
        state.response = None;
    }
}
