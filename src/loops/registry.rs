//! In-process bus for server-owned channels.
//!
//! A `source = 'server'` channel never crosses a process boundary — the server
//! is both publisher and subscriber — so it uses this in-process broadcast
//! instead of a Unix socket. [`register`] returns a [`ServerPublisher`] that
//! forwards an event stream to every [`subscribe`]d receiver.
//!
//! The registry is process-wide and lives for the server's lifetime; entries
//! are never removed, and re-[`register`]ing an id replaces its publisher (used
//! to respawn default channels on boot).

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use anyhow::Result;
use async_stream::stream;
use futures::StreamExt;
use futures::stream::BoxStream;
use tokio::sync::broadcast;

/// Capacity of each server channel's broadcast buffer. Matches the socket
/// publisher's buffer; server channels publish sparsely.
const BUFFER: usize = 128;

static SERVER_CHANNELS: OnceLock<Mutex<HashMap<String, broadcast::Sender<String>>>> =
    OnceLock::new();

fn channels() -> &'static Mutex<HashMap<String, broadcast::Sender<String>>> {
    SERVER_CHANNELS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Publisher handle for a server channel.
///
/// Dropping the handle leaves the channel registered (the server owns channels
/// for its lifetime); a later [`register`] for the same id replaces it.
pub struct ServerPublisher {
    tx: broadcast::Sender<String>,
}

impl ServerPublisher {
    /// Forward each event from `events` to every current and future subscriber
    /// until the stream ends.
    ///
    /// Sends are non-blocking: with no subscribers the event is dropped, and a
    /// lagging subscriber misses events rather than stalling the publisher.
    pub async fn serve(self, mut events: BoxStream<'static, String>) -> Result<()> {
        while let Some(event) = events.next().await {
            let _ = self.tx.send(event);
        }
        Ok(())
    }
}

/// Register `id` as a server channel and return a publisher for it.
///
/// Replaces any existing publisher for the same id.
pub fn register(id: &str) -> ServerPublisher {
    let (tx, _) = broadcast::channel(BUFFER);
    channels()
        .lock()
        .unwrap()
        .insert(id.to_string(), tx.clone());
    ServerPublisher { tx }
}

/// Register `id` with a fresh publisher if it has none; a no-op otherwise.
///
/// Unlike [`register`], an existing entry is left in place, so current
/// subscribers keep their receiver. Used to bring an agent-created channel live
/// without disturbing anyone already listening.
pub fn ensure_registered(id: &str) {
    channels()
        .lock()
        .unwrap()
        .entry(id.to_string())
        .or_insert_with(|| broadcast::channel(BUFFER).0);
}

/// Publish `event` to the registered server channel `id`.
///
/// Returns `false` if the channel has no publisher (so the caller can report
/// it as not running). Like [`ServerPublisher::serve`], the send is
/// non-blocking: with no subscribers the event is dropped.
pub fn publish(id: &str, event: String) -> bool {
    match channels().lock().unwrap().get(id) {
        Some(tx) => {
            let _ = tx.send(event);
            true
        }
        None => false,
    }
}

/// Subscribe to a registered server channel, or `None` if it is not running.
pub fn subscribe(id: &str) -> Option<broadcast::Receiver<String>> {
    channels().lock().unwrap().get(id).map(|tx| tx.subscribe())
}

/// Stream the events published to a server channel.
///
/// Each published message is yielded as one event. A lagging subscriber logs
/// and skips missed events; the stream ends when every publisher for the
/// channel is dropped.
pub fn broadcast_stream(rx: broadcast::Receiver<String>) -> BoxStream<'static, String> {
    let s = stream! {
        let mut rx = rx;
        loop {
            match rx.recv().await {
                Ok(event) => yield event,
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!("server channel subscriber lagged by {} events", n);
                    continue;
                }
                Err(broadcast::error::RecvError::Closed) => return,
            }
        }
    };
    Box::pin(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn unique_id() -> String {
        format!("test-{}", Uuid::new_v4())
    }

    #[tokio::test]
    async fn register_subscribe_and_publish_round_trip() {
        let id = unique_id();
        let publisher = register(&id);
        let mut rx = subscribe(&id).expect("channel is registered");

        publisher.tx.send("hello".to_string()).unwrap();
        assert_eq!(rx.recv().await.unwrap(), "hello");
    }

    #[tokio::test]
    async fn subscribe_unknown_channel_returns_none() {
        assert!(subscribe(&unique_id()).is_none());
    }

    #[tokio::test]
    async fn broadcast_stream_yields_published_events() {
        let id = unique_id();
        let publisher = register(&id);
        let mut events = broadcast_stream(subscribe(&id).unwrap());

        publisher.tx.send("a".to_string()).unwrap();
        publisher.tx.send("b".to_string()).unwrap();
        assert_eq!(events.next().await.unwrap(), "a");
        assert_eq!(events.next().await.unwrap(), "b");
    }

    #[tokio::test]
    async fn reregister_replaces_the_publisher() {
        let id = unique_id();
        let first = register(&id);
        let mut old_rx = subscribe(&id).unwrap();

        // A second registration replaces the map's sender; once the first
        // publisher is dropped its receivers see the channel close.
        let _second = register(&id);
        drop(first);
        assert!(matches!(
            old_rx.recv().await,
            Err(broadcast::error::RecvError::Closed)
        ));
    }

    #[tokio::test]
    async fn publish_delivers_to_subscribers_and_reports_missing() {
        let id = unique_id();
        register(&id);
        let mut rx = subscribe(&id).unwrap();

        assert!(publish(&id, "hello".to_string()));
        assert_eq!(rx.recv().await.unwrap(), "hello");

        assert!(!publish(&unique_id(), "nobody".to_string()));
    }

    #[tokio::test]
    async fn ensure_registered_is_idempotent_and_preserves_subscribers() {
        let id = unique_id();
        ensure_registered(&id);
        let mut rx = subscribe(&id).unwrap();

        // A second call must not swap the sender out from under `rx`.
        ensure_registered(&id);
        assert!(publish(&id, "kept".to_string()));
        assert_eq!(rx.recv().await.unwrap(), "kept");
    }
}
