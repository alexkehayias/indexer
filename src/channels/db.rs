use anyhow::Result;
use tokio_rusqlite::Connection;

use super::models::{Channel, ChannelOwner};

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
                    owner: ChannelOwner::from_columns(&i.get::<_, String>(1)?, i.get(3)?),
                    debounce_ms: i.get(2)?,
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
                owner: ChannelOwner::from_columns(&i.get::<_, String>(1)?, i.get(3)?),
                debounce_ms: i.get(2)?,
                created_at: i.get(4)?,
            })
        })?;
        Ok(rows.next().transpose()?)
    });
    Ok(channel.await?)
}

/// Create a channel row with the given owner. Fails if the id already exists.
pub async fn insert_channel(db: &Connection, id: &str, owner: &ChannelOwner) -> Result<()> {
    let id = id.to_string();
    let source = owner.source_str().to_string();
    let created_by = owner.agent_id().map(str::to_string);
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

/// Ids of agent-owned channels. Their in-process publishers must be
/// re-registered on boot; see
/// [`crate::channels::default::register_owned_channels`].
pub async fn find_agent_channels(db: &Connection) -> Result<Vec<String>> {
    let ids = db.call(|conn| {
        let mut stmt = conn.prepare(
            "SELECT id FROM channel WHERE source = 'server' AND created_by IS NOT NULL ORDER BY id",
        )?;
        let ids = stmt
            .query_map([], |i| i.get::<_, String>(0))?
            .filter_map(Result::ok)
            .collect::<Vec<String>>();
        Ok(ids)
    });
    Ok(ids.await?)
}

/// Ensure a `channel` row exists for each id with the given owner so a loop's
/// `loop_channel` links satisfy the foreign key. Existing rows are left
/// untouched, preserving a channel's registered owner and debounce.
///
/// Uses `ON CONFLICT(id) DO NOTHING` rather than `INSERT OR IGNORE` so only
/// the duplicate-id conflict is suppressed; any other constraint failure
/// still surfaces as an error.
pub async fn ensure_channels(
    db: &Connection,
    channels: &[String],
    owner: &ChannelOwner,
) -> Result<()> {
    let channels = channels.to_vec();
    let source = owner.source_str().to_string();
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
        assert_eq!(channels[0].owner, ChannelOwner::Socket);
        assert_eq!(channels[0].debounce_ms, 250);
        assert!(!channels[0].created_at.is_empty());
        assert_eq!(channels[1].owner, ChannelOwner::Server);
    }

    #[tokio::test]
    async fn find_all_channels_empty_table_returns_empty() {
        let (_dir, db) = setup_db().await;
        assert!(find_all_channels(&db).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn insert_and_find_channel_round_trip() {
        let (_dir, db) = setup_db().await;

        insert_channel(&db, "owned", &ChannelOwner::Agent("agent-a".to_string()))
            .await
            .unwrap();

        let found = find_channel(&db, "owned").await.unwrap().unwrap();
        assert_eq!(found.owner, ChannelOwner::Agent("agent-a".to_string()));
        assert!(find_channel(&db, "missing").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn insert_channel_rejects_duplicate_id() {
        let (_dir, db) = setup_db().await;
        let owner = ChannelOwner::Agent("agent-a".to_string());
        insert_channel(&db, "dup", &owner).await.unwrap();
        assert!(insert_channel(&db, "dup", &owner).await.is_err());
    }

    #[tokio::test]
    async fn find_agent_channels_excludes_unowned_and_socket() {
        let (_dir, db) = setup_db().await;
        insert_raw_channel(&db, "timer", "server").await; // server, no owner
        insert_raw_channel(&db, "sock", "socket").await; // socket
        insert_channel(&db, "owned", &ChannelOwner::Agent("agent-a".to_string()))
            .await
            .unwrap();

        let ids = find_agent_channels(&db).await.unwrap();
        assert_eq!(ids, vec!["owned"]);
    }

    #[tokio::test]
    async fn ensure_channels_creates_rows_and_preserves_existing() {
        let (_dir, db) = setup_db().await;

        // A channel that already exists with a non-socket owner.
        insert_raw_channel(&db, "gamma", "server").await;

        ensure_channels(
            &db,
            &["alpha".to_string(), "beta".to_string(), "gamma".to_string()],
            &ChannelOwner::Socket,
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
