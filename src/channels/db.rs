use anyhow::Result;
use tokio_rusqlite::Connection;

use super::models::{Channel, ChannelSource};

/// List every channel, ordered by id.
pub async fn find_all_channels(db: &Connection) -> Result<Vec<Channel>> {
    let channels = db.call(|conn| {
        let mut stmt = conn.prepare(
            "SELECT id, source, debounce_ms, created_by, created_at FROM channel ORDER BY id",
        )?;
        let channels = stmt
            .query_map([], |i| {
                Ok(Channel {
                    id: i.get(0)?,
                    source: ChannelSource::from_column(&i.get::<_, String>(1)?),
                    debounce_ms: i.get(2)?,
                    created_by: i.get(3)?,
                    created_at: i.get(4)?,
                })
            })?
            .filter_map(Result::ok)
            .collect::<Vec<Channel>>();
        Ok(channels)
    });
    Ok(channels.await?)
}

/// Find a single channel by id.
pub async fn find_channel(db: &Connection, id: &str) -> Result<Option<Channel>> {
    let id = id.to_string();
    let channel = db.call(move |conn| {
        let mut stmt = conn.prepare(
            "SELECT id, source, debounce_ms, created_by, created_at FROM channel WHERE id = ?1",
        )?;
        let mut rows = stmt.query_map([&id], |i| {
            Ok(Channel {
                id: i.get(0)?,
                source: ChannelSource::from_column(&i.get::<_, String>(1)?),
                debounce_ms: i.get(2)?,
                created_by: i.get(3)?,
                created_at: i.get(4)?,
            })
        })?;
        Ok(rows.next().transpose()?)
    });
    Ok(channel.await?)
}

/// Create a channel row. Fails if the id already exists.
///
/// `created_by` is the agent session that owns the channel, if any; see
/// [`Channel::created_by`].
pub async fn insert_channel(
    db: &Connection,
    id: &str,
    source: &ChannelSource,
    created_by: Option<&str>,
) -> Result<()> {
    let id = id.to_string();
    let source = source.as_str().to_string();
    let created_by = created_by.map(str::to_string);
    db.call(move |conn| {
        conn.execute(
            "INSERT INTO channel (id, source, created_by) VALUES (?1, ?2, ?3)",
            rusqlite::params![id, source, created_by],
        )?;
        Ok(())
    })
    .await
    .map_err(anyhow::Error::new)
}

/// Ids of agent-created channels (those with a `created_by` owner). Their
/// in-process publishers must be re-registered on boot; see
/// [`crate::channels::default::register_agent_channels`].
pub async fn find_agent_channels(db: &Connection) -> Result<Vec<String>> {
    let ids = db.call(|conn| {
        let mut stmt =
            conn.prepare("SELECT id FROM channel WHERE created_by IS NOT NULL ORDER BY id")?;
        let ids = stmt
            .query_map([], |i| i.get::<_, String>(0))?
            .filter_map(Result::ok)
            .collect::<Vec<String>>();
        Ok(ids)
    });
    Ok(ids.await?)
}

/// Ensure a `channel` row exists for each id with the given source so a loop's
/// `loop_channel` links satisfy the foreign key. Existing rows are left
/// untouched, preserving a channel's stored source and debounce.
///
/// Uses `ON CONFLICT(id) DO NOTHING` rather than `INSERT OR IGNORE` so only
/// the duplicate-id conflict is suppressed; any other constraint failure
/// still surfaces as an error.
pub async fn ensure_channels(
    db: &Connection,
    channels: &[String],
    source: &ChannelSource,
) -> Result<()> {
    let channels = channels.to_vec();
    let source = source.as_str().to_string();
    let inserted = db.call(move |conn| {
        let mut stmt = conn.prepare(
            "INSERT INTO channel (id, source) VALUES (?1, ?2)
             ON CONFLICT(id) DO NOTHING",
        )?;
        for channel in &channels {
            stmt.execute(rusqlite::params![channel, source])?;
        }
        Ok(())
    });
    Ok(inserted.await?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::db::{async_db, initialize_db};

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

    /// Insert a channel row directly, bypassing the helpers.
    async fn insert_raw_channel(db: &Connection, id: &str, source: &str) {
        let id = id.to_string();
        let source = source.to_string();
        db.call(move |conn| {
            conn.execute(
                "INSERT INTO channel (id, source) VALUES (?1, ?2)",
                rusqlite::params![id, source],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn find_all_channels_returns_rows_ordered_by_id() {
        let (_dir, db) = setup_db().await;
        insert_raw_channel(&db, "beta", "server").await;
        insert_raw_channel(&db, "alpha", "socket").await;

        let channels = find_all_channels(&db).await.unwrap();

        let ids: Vec<&str> = channels.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, vec!["alpha", "beta"]);
        assert_eq!(channels[0].source, ChannelSource::Socket);
        assert_eq!(channels[0].created_by, None);
        assert_eq!(channels[0].debounce_ms, 250);
        assert!(!channels[0].created_at.is_empty());
        assert_eq!(channels[1].source, ChannelSource::Server);
    }

    #[tokio::test]
    async fn find_all_channels_empty_table_returns_empty() {
        let (_dir, db) = setup_db().await;
        assert!(find_all_channels(&db).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn insert_and_find_channel_round_trip() {
        let (_dir, db) = setup_db().await;

        insert_channel(&db, "owned", &ChannelSource::Server, Some("agent-a"))
            .await
            .unwrap();

        let found = find_channel(&db, "owned").await.unwrap().unwrap();
        assert_eq!(found.source, ChannelSource::Server);
        assert_eq!(found.created_by.as_deref(), Some("agent-a"));
        assert!(find_channel(&db, "missing").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn insert_channel_rejects_duplicate_id() {
        let (_dir, db) = setup_db().await;
        insert_channel(&db, "dup", &ChannelSource::Socket, None)
            .await
            .unwrap();
        assert!(
            insert_channel(&db, "dup", &ChannelSource::Socket, None)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn find_agent_channels_returns_only_owned() {
        let (_dir, db) = setup_db().await;
        insert_raw_channel(&db, "timer", "server").await; // server, no owner
        insert_raw_channel(&db, "sock", "socket").await; // socket
        insert_channel(&db, "owned", &ChannelSource::Server, Some("agent-a"))
            .await
            .unwrap();

        let ids = find_agent_channels(&db).await.unwrap();
        assert_eq!(ids, vec!["owned"]);
    }

    #[tokio::test]
    async fn ensure_channels_creates_rows_and_preserves_existing() {
        let (_dir, db) = setup_db().await;

        // A channel that already exists with a different source.
        insert_raw_channel(&db, "gamma", "server").await;

        ensure_channels(
            &db,
            &["alpha".to_string(), "beta".to_string(), "gamma".to_string()],
            &ChannelSource::Socket,
        )
        .await
        .unwrap();

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
                ("alpha".to_string(), "socket".to_string()),
                ("beta".to_string(), "socket".to_string()),
                ("gamma".to_string(), "server".to_string()),
            ]
        );
    }
}
