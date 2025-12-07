use std::{
    convert::Infallible,
    pin::Pin,
    task::{Context, Poll},
};
use futures_util::Stream;
use axum::response::sse::Event;

use tokio::sync::broadcast;

pub fn truncate(s: &str, max_chars: usize) -> &str {
    match s.char_indices().nth(max_chars) {
        None => s,
        Some((idx, _)) => &s[..idx],
    }
}

/// Wrapper for an axum SSE stream to detect when a client disconnects
pub struct DetectDisconnect<S> {
    /// The actual stream of SSE events.
    inner: S,

    /// When this `Sender` gets closed we know the client vanished.
    /// (You can also store a `oneshot::Sender<()>` if you only need a
    /// single notification.)
    disconnect_notifier: broadcast::Sender<()>,
}

impl<S> DetectDisconnect<S> {
    pub fn new(inner: S, disconnect_notifier: broadcast::Sender<()>) -> Self {
        Self {
            inner,
            disconnect_notifier,
        }
    }
}

impl<S> Stream for DetectDisconnect<S>
where
    S: Stream<Item = Result<axum::response::sse::Event, Infallible>> + Unpin,
{
    type Item = Result<Event, Infallible>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        // Forward the inner stream unchanged
        let this = self.get_mut();
        match Pin::new(&mut this.inner).poll_next(cx) {
            Poll::Ready(Some(item)) => Poll::Ready(Some(item)),
            // When the upstream finishes (or is cancelled), forward
            // the termination
            other => other,
        }
    }
}

impl<S> Drop for DetectDisconnect<S> {
    fn drop(&mut self) {
        // The response body has been dropped → client disconnected or
        // request was aborted.
        //
        // `broadcast::Sender::send` never fails unless there are no
        // receivers, which is fine.
        let _ = self.disconnect_notifier.send(());
        tracing::info!("SSE client disconnected");
    }
}
