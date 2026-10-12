//! Default channels available to loops.
//!
//! These are server-owned channels: instead of a CLI publisher reading stdin,
//! the server binds the channel socket and emits an event on a fixed interval.
//! Loops subscribe to them exactly like any other channel, receiving events
//! tagged `[timer-1h] tick <timestamp>`.
//!
//! [`start_default_channels`] ensures a `channel` row exists for each default
//! channel (`source = 'server'`) and starts its publisher, so a persisted loop
//! that subscribes to one reconnects on boot.

use std::time::Duration;

use anyhow::Result;
use async_stream::stream;
use futures::stream::BoxStream;
use tokio::task::JoinHandle;
use tokio::time::{Instant, interval_at};
use tokio_rusqlite::Connection;

use super::db::ensure_channels_with_source;
use crate::cli::channel::Publisher;

/// Default timer channels as `(id, interval)`.
pub const DEFAULT_TIMER_CHANNELS: &[(&str, Duration)] = &[
    ("timer-30m", Duration::from_secs(30 * 60)),
    ("timer-1h", Duration::from_secs(60 * 60)),
    ("timer-4h", Duration::from_secs(4 * 60 * 60)),
    ("timer-12h", Duration::from_secs(12 * 60 * 60)),
];

/// Channel ids of the default timer channels.
fn default_channel_ids() -> Vec<String> {
    DEFAULT_TIMER_CHANNELS
        .iter()
        .map(|(id, _)| id.to_string())
        .collect()
}

/// Ensure a `channel` row exists for each default channel (`source = 'server'`).
/// Existing rows are left untouched.
pub async fn ensure_default_channels(db: &Connection) -> Result<()> {
    ensure_channels_with_source(db, &default_channel_ids(), "server").await
}

/// Ensure the default channel rows exist and start a publisher for each.
///
/// Each publisher binds its socket before this returns, so a caller that
/// respawns loops afterwards can rely on the sockets being live. A channel
/// whose socket is already owned by another live publisher is logged and
/// skipped, never blocking the rest.
pub async fn start_default_channels(db: &Connection, storage_path: &str) -> Result<()> {
    ensure_default_channels(db).await?;
    for &(id, interval) in DEFAULT_TIMER_CHANNELS {
        match spawn_timer_channel(storage_path, id, interval).await {
            Ok(_) => tracing::info!("started default timer channel '{}'", id),
            Err(e) => tracing::warn!("failed to start default timer channel '{}': {}", id, e),
        }
    }
    Ok(())
}

/// Bind the socket for `id` and spawn a task publishing a tick every
/// `interval`. Returns once the socket is bound.
async fn spawn_timer_channel(
    storage_path: &str,
    id: &str,
    interval: Duration,
) -> Result<JoinHandle<()>> {
    let publisher = Publisher::bind(storage_path, id).await?;
    let id = id.to_string();
    let handle = tokio::spawn(async move {
        if let Err(e) = publisher.serve(timer_events(interval)).await {
            tracing::warn!("timer channel '{}' stopped: {}", id, e);
        }
    });
    Ok(handle)
}

/// An infinite stream of tick events, one every `interval`.
///
/// The first tick fires after one full interval (not immediately), so a 12h
/// channel does not fire at boot.
fn timer_events(interval: Duration) -> BoxStream<'static, String> {
    let s = stream! {
        let mut ticker = interval_at(Instant::now() + interval, interval);
        loop {
            ticker.tick().await;
            yield format!("tick {}", utc_timestamp());
        }
    };
    Box::pin(s)
}

/// Current UTC time as an RFC 3339 string with second precision (e.g.
/// `2026-10-11T15:00:00Z`).
fn utc_timestamp() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// Human-readable label for a timer interval (e.g. `every 30 minutes`).
fn interval_label(interval: Duration) -> String {
    let secs = interval.as_secs();
    if secs.is_multiple_of(3600) {
        let hours = secs / 3600;
        format!("every {hours} hour{}", if hours == 1 { "" } else { "s" })
    } else if secs.is_multiple_of(60) {
        format!("every {} minutes", secs / 60)
    } else {
        format!("every {secs} seconds")
    }
}

/// System-message section describing channels, loops, and the default timer
/// channels. Appended to the main chat's system prompt so the model knows how
/// to schedule work and which channels are available.
pub fn system_prompt_section() -> String {
    let channels = DEFAULT_TIMER_CHANNELS
        .iter()
        .map(|(id, interval)| format!("- {id}: {}", interval_label(*interval)))
        .collect::<Vec<_>>()
        .join("\n");

    format!(
        "\n\n## Channels and Loops\n\
         Channels are streams of events. A loop is a persistent subscriber: each event \
         published to a channel runs an LLM turn with the loop's system prompt and tools. \
         Use the `create_loop` tool to create a loop subscribed to one or more channels, \
         `list_loops` to inspect them, and `delete_loop` to remove one. Loops keep running \
         across server restarts.\n\n\
         The following timer channels are always available; each publishes a `tick` event \
         with the current UTC time on a fixed interval:\n\
         {channels}\n\n\
         To run something on a schedule, create a loop subscribed to the matching timer channel."
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::channel::socket_path;
    use crate::core::db::{async_db, initialize_db};
    use tokio::io::{AsyncBufReadExt, BufReader};

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

    #[tokio::test]
    async fn ensure_default_channels_registers_server_channels() {
        let (_dir, db) = setup_db().await;
        ensure_default_channels(&db).await.unwrap();

        let rows = db
            .call(|conn| {
                let mut stmt = conn.prepare("SELECT id, source FROM channel ORDER BY id")?;
                let rows = stmt
                    .query_map([], |i| Ok((i.get::<_, String>(0)?, i.get::<_, String>(1)?)))?
                    .filter_map(Result::ok)
                    .collect::<Vec<(String, String)>>();
                Ok(rows)
            })
            .await
            .unwrap();

        assert_eq!(
            rows,
            vec![
                ("timer-12h".to_string(), "server".to_string()),
                ("timer-1h".to_string(), "server".to_string()),
                ("timer-30m".to_string(), "server".to_string()),
                ("timer-4h".to_string(), "server".to_string()),
            ]
        );
    }

    #[tokio::test]
    async fn ensure_default_channels_preserves_existing_rows() {
        let (_dir, db) = setup_db().await;
        db.call(|conn| {
            conn.execute(
                "INSERT INTO channel (id, source, debounce_ms) VALUES ('timer-1h', 'socket', 999)",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();

        ensure_default_channels(&db).await.unwrap();

        let (source, debounce) = db
            .call(|conn| {
                Ok(conn.query_row(
                    "SELECT source, debounce_ms FROM channel WHERE id = 'timer-1h'",
                    [],
                    |i| Ok((i.get::<_, String>(0)?, i.get::<_, i64>(1)?)),
                )?)
            })
            .await
            .unwrap();
        assert_eq!(source, "socket");
        assert_eq!(debounce, 999);
    }

    #[test]
    fn system_prompt_section_lists_default_channels() {
        let section = system_prompt_section();
        assert!(section.contains("## Channels and Loops"));
        assert!(section.contains("create_loop"));
        for (id, _) in DEFAULT_TIMER_CHANNELS {
            assert!(section.contains(id), "missing channel {id}");
        }
        assert!(section.contains("every 30 minutes"), "{section}");
        assert!(section.contains("every 1 hour"), "{section}");
        assert!(section.contains("every 4 hours"), "{section}");
        assert!(section.contains("every 12 hours"), "{section}");
    }

    #[tokio::test]
    async fn timer_channel_publishes_ticks_to_subscribers() {
        let dir = tempfile::tempdir().unwrap();
        let storage = dir.path().to_str().unwrap().to_string();

        let _handle = spawn_timer_channel(&storage, "timer-test", Duration::from_millis(20))
            .await
            .unwrap();

        let path = socket_path(&storage, "timer-test").unwrap();
        let stream = tokio::net::UnixStream::connect(&path).await.unwrap();
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        tokio::time::timeout(Duration::from_secs(5), reader.read_line(&mut line))
            .await
            .expect("timed out waiting for tick")
            .unwrap();

        assert!(line.trim().starts_with("tick "), "unexpected event: {line}");
    }
}
