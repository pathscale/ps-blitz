//! Reactive cancellation of an owned operation.

use blitz_traits::net::AbortSignal;
use std::{future::Future, task::Poll};

pub(super) async fn until_cancelled<F: Future>(
    signal: Option<AbortSignal>,
    future: F,
) -> Option<F::Output> {
    let Some(signal) = signal else {
        return Some(future.await);
    };
    let mut cancelled = std::pin::pin!(signal.cancelled());
    let mut future = std::pin::pin!(future);
    std::future::poll_fn(|cx| {
        if cancelled.as_mut().poll(cx).is_ready() {
            return Poll::Ready(None);
        }
        match future.as_mut().poll(cx) {
            Poll::Ready(_) if signal.aborted() => Poll::Ready(None),
            Poll::Ready(output) => Poll::Ready(Some(output)),
            Poll::Pending => Poll::Pending,
        }
    })
    .await
}
