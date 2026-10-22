//! Source-aware channel subscription for in-process consumers.
//!
//! A subscriber connects to a channel according to its [`ChannelSource`]:
//! [`Socket`](ChannelSource::Socket) channels are read from their Unix socket
//! publisher (coalescing line bursts with the channel's debounce);
//! [`Server`](ChannelSource::Server) channels — including agent-created ones,
//! which are stored as server-sourced — from the in-process [`registry`]. Both
//! yield a [`BoxStream`] of events so the caller (the loop supervisor) treats
//! them uniformly.

use std::time::Duration;

use anyhow::{Result, anyhow};
use futures::stream::BoxStream;
use tokio::io::BufReader;
use tokio::net::UnixStream;
use tokio_rusqlite::Connection;

use super::models::ChannelSource;
use super::registry;
use crate::cli::channel::{event_stream_from_reader, socket_path};

/// Debounce window used when a channel has no `channel` row.
pub const DEFAULT_DEBOUNCE_MS: u64 = 250;

/// Read a channel's source and debounce window from its `channel` row.
///
/// Falls back to [`ChannelSource::Socket`] / [`DEFAULT_DEBOUNCE_MS`] when the
/// channel has no row, preserving the behavior for channels whose publisher has
/// not registered yet.
pub async fn channel_meta(db: &Connection, id: &str) -> Result<(ChannelSource, u64)> {
    let id = id.to_string();
    let row = db
        .call(move |conn| {
            let mut stmt = conn.prepare("SELECT source, debounce_ms FROM channel WHERE id = ?1")?;
            let mut rows = stmt.query_map([&id], |i| {
                Ok((
                    ChannelSource::from_column(&i.get::<_, String>(0)?),
                    i.get::<_, i64>(1)?,
                ))
            })?;
            Ok(rows.next().transpose()?)
        })
        .await?;

    Ok(match row {
        Some((source, debounce_ms)) => (source, debounce_ms.max(0) as u64),
        None => (ChannelSource::Socket, DEFAULT_DEBOUNCE_MS),
    })
}

/// Subscribe to channel `id`, dispatching on its source.
///
/// Fails fast (before any events flow) if a socket publisher is not reachable
/// or a server channel is not registered.
pub async fn subscribe(
    db: &Connection,
    storage_path: &str,
    id: &str,
) -> Result<BoxStream<'static, String>> {
    match channel_meta(db, id).await? {
        (ChannelSource::Server, _) => {
            let rx = registry::subscribe(id).ok_or_else(|| {
                anyhow!("server channel '{id}' not found — is the publisher running?")
            })?;
            Ok(registry::broadcast_stream(rx))
        }
        (ChannelSource::Socket, debounce_ms) => {
            let path = socket_path(storage_path, id)?;
            let stream = UnixStream::connect(&path)
                .await
                .map_err(|_| anyhow!("channel '{id}' not found — is the publisher running?"))?;
            Ok(event_stream_from_reader(
                BufReader::new(stream),
                Duration::from_millis(debounce_ms),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::channel::Publisher;
    use crate::core::db::{async_db, initialize_db};
    use async_stream::stream;
    use futures::StreamExt;
    use uuid::Uuid;

    async fn setup_db() -> (tempfile::TempDir, Connection) {
        let dir = tempfile::tempdir().unwrap();
        let db = async_db(dir.path().to_str().unwrap()).await.unwrap();
        db.call(|conn| {
            initialize_db(conn)?;
            Ok(())
        })
        .await
        .unwrap();
        (dir, db)
    }

    async fn insert_channel(db: &Connection, id: &str, source: &str, debounce_ms: i64) {
        let id = id.to_string();
        let source = source.to_string();
        db.call(move |conn| {
            conn.execute(
                "INSERT INTO channel (id, source, debounce_ms) VALUES (?1, ?2, ?3)",
                rusqlite::params![id, source, debounce_ms],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn channel_meta_reads_rows_and_defaults_when_absent() {
        let (_dir, db) = setup_db().await;

        assert_eq!(
            channel_meta(&db, "absent").await.unwrap(),
            (ChannelSource::Socket, DEFAULT_DEBOUNCE_MS)
        );

        insert_channel(&db, "socket-chan", "socket", 1000).await;
        insert_channel(&db, "server-chan", "server", 5).await;

        assert_eq!(
            channel_meta(&db, "socket-chan").await.unwrap(),
            (ChannelSource::Socket, 1000)
        );
        assert_eq!(
            channel_meta(&db, "server-chan").await.unwrap(),
            (ChannelSource::Server, 5)
        );
    }

    #[tokio::test]
    async fn subscribe_server_source_reads_from_registry() {
        let (_dir, db) = setup_db().await;
        let id = format!("srv-{}", Uuid::new_v4());
        insert_channel(&db, &id, "server", 250).await;

        let publisher = registry::register(&id);
        // Create the receiver before publishing so the event is not dropped.
        let mut events = subscribe(&db, "/tmp/ignored", &id).await.unwrap();
        tokio::spawn(async move {
            let s = stream! {
                yield "tick".to_string();
                std::future::pending::<()>().await;
            };
            let _ = publisher.serve(Box::pin(s)).await;
        });

        let event = tokio::time::timeout(Duration::from_secs(5), events.next())
            .await
            .expect("timed out waiting for event")
            .unwrap();
        assert_eq!(event, "tick");
    }

    #[tokio::test]
    async fn subscribe_agent_created_channel_reads_from_registry() {
        let (_dir, db) = setup_db().await;
        let id = format!("agent-{}", Uuid::new_v4());
        // Agent-created channels are stored as server-sourced with an owner.
        crate::channels::db::insert_channel(&db, &id, &ChannelSource::Server, Some("sess"))
            .await
            .unwrap();

        let publisher = registry::register(&id);
        let mut events = subscribe(&db, "/tmp/ignored", &id).await.unwrap();
        tokio::spawn(async move {
            let s = stream! {
                yield "hello".to_string();
                std::future::pending::<()>().await;
            };
            let _ = publisher.serve(Box::pin(s)).await;
        });

        let event = tokio::time::timeout(Duration::from_secs(5), events.next())
            .await
            .expect("timed out waiting for event")
            .unwrap();
        assert_eq!(event, "hello");
    }

    #[tokio::test]
    async fn subscribe_server_source_without_publisher_errors() {
        let (_dir, db) = setup_db().await;
        let id = format!("srv-{}", Uuid::new_v4());
        insert_channel(&db, &id, "server", 250).await;

        let err = subscribe(&db, "/tmp/ignored", &id)
            .await
            .err()
            .expect("expected an error");
        assert!(err.to_string().contains("not found"), "{err}");
    }

    #[tokio::test]
    async fn subscribe_socket_source_reads_from_publisher() {
        let (dir, db) = setup_db().await;
        let storage = dir.path().to_str().unwrap().to_string();
        let id = "socket-chan";
        insert_channel(&db, id, "socket", 10).await;

        let publisher = Publisher::bind(&storage, id).await.unwrap();
        tokio::spawn(async move {
            let s = stream! {
                loop {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    yield "hello".to_string();
                }
            };
            let _ = publisher.serve(Box::pin(s)).await;
        });

        let mut events = subscribe(&db, &storage, id).await.unwrap();
        let event = tokio::time::timeout(Duration::from_secs(5), events.next())
            .await
            .expect("timed out waiting for event")
            .unwrap();
        assert_eq!(event, "hello");
    }
}
