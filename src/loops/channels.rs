//! Default channels available to loops.
//!
//! These are server-owned channels: instead of a CLI publisher reading stdin,
//! the server publishes an event on a fixed interval through the in-process
//! [`registry`](super::registry). Loops subscribe to them like any other
//! channel, receiving events tagged `[timer-1h] tick <timestamp>`.
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

use super::db::{ensure_channels_with_source, find_owned_server_channels};
use super::registry;

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
/// Each publisher is registered in the in-process [`registry`] before this
/// returns, so a caller that respawns loops afterwards can rely on the channels
/// being live. Re-registering replaces any prior publisher (respawn on boot).
pub async fn start_default_channels(db: &Connection) -> Result<()> {
    ensure_default_channels(db).await?;
    for &(id, interval) in DEFAULT_TIMER_CHANNELS {
        spawn_timer_channel(id, interval);
        tracing::info!("started default timer channel '{}'", id);
    }
    Ok(())
}

/// Re-register in-process publishers for agent-created server channels.
///
/// A server channel's publisher lives only in the in-memory [`registry`], so it
/// is lost on restart. Agent-created channels are otherwise registered lazily by
/// their owner, which is too late for a loop subscribed to one: the loop's
/// subscription fails fast and it never respawns. Run this at boot before
/// respawning loops.
pub async fn register_owned_channels(db: &Connection) -> Result<()> {
    for id in find_owned_server_channels(db).await? {
        registry::ensure_registered(&id);
        tracing::info!("registered owned channel '{}'", id);
    }
    Ok(())
}

/// Register `id` and spawn a task publishing a tick every `interval`.
///
/// Returns immediately; the publisher task runs until the process exits.
fn spawn_timer_channel(id: &str, interval: Duration) -> JoinHandle<()> {
    let publisher = registry::register(id);
    let id = id.to_string();
    tokio::spawn(async move {
        if let Err(e) = publisher.serve(timer_events(interval)).await {
            tracing::warn!("timer channel '{}' stopped: {}", id, e);
        }
    })
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
         across server restarts. Use `create_channel` to create a channel you own and \
         `emit_channel` to publish messages to it; only the agent that created a channel \
         can emit to it.\n\n\
         The following timer channels are always available; each publishes a `tick` event \
         with the current UTC time on a fixed interval:\n\
         {channels}\n\n\
         To run something on a schedule, create a loop subscribed to the matching timer channel."
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::db::{async_db, initialize_db};
    use crate::loops::db::insert_channel;
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
        let id = format!("timer-test-{}", Uuid::new_v4());

        let _handle = spawn_timer_channel(&id, Duration::from_millis(20));

        let rx = registry::subscribe(&id).expect("timer channel is registered");
        let mut events = registry::broadcast_stream(rx);
        let event = tokio::time::timeout(Duration::from_secs(5), events.next())
            .await
            .expect("timed out waiting for tick")
            .unwrap();

        assert!(event.starts_with("tick "), "unexpected event: {event}");
    }

    #[tokio::test]
    async fn register_owned_channels_registers_only_owned_server_channels() {
        let (_dir, db) = setup_db().await;
        let owned = format!("owned-{}", Uuid::new_v4());
        let socket = format!("sock-{}", Uuid::new_v4());

        insert_channel(&db, &owned, "server", Some("agent-a"))
            .await
            .unwrap();
        insert_channel(&db, &socket, "socket", Some("agent-a"))
            .await
            .unwrap();

        register_owned_channels(&db).await.unwrap();

        assert!(registry::subscribe(&owned).is_some());
        assert!(registry::subscribe(&socket).is_none());
    }
}
