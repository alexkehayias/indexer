use std::collections::HashMap;

use anyhow::Result;
use tokio_rusqlite::Connection;

use super::models::{Channel, Loop};

pub async fn find_all_loops(db: &Connection) -> Result<Vec<Loop>> {
    let loops = db.call(|conn| {
        let mut stmt = conn.prepare("SELECT id, system_prompt, tools, created_at FROM loop")?;
        let mut loops = stmt
            .query_map([], |i| {
                let tools: String = i.get(2)?;
                Ok(Loop {
                    id: i.get(0)?,
                    system_prompt: i.get(1)?,
                    tools: serde_json::from_str(&tools).unwrap_or_default(),
                    created_at: i.get(3)?,
                    channels: Vec::new(),
                })
            })?
            .filter_map(Result::ok)
            .collect::<Vec<Loop>>();

        // Channel ids per loop, ordered by subscription position.
        let mut ch_stmt = conn
            .prepare("SELECT loop_id, channel_id FROM loop_channel ORDER BY loop_id, position")?;
        let mut channels: HashMap<String, Vec<String>> = HashMap::new();
        let rows =
            ch_stmt.query_map([], |i| Ok((i.get::<_, String>(0)?, i.get::<_, String>(1)?)))?;
        for row in rows.filter_map(Result::ok) {
            channels.entry(row.0).or_default().push(row.1);
        }

        for l in loops.iter_mut() {
            if let Some(ch) = channels.remove(&l.id) {
                l.channels = ch;
            }
        }
        Ok(loops)
    });
    Ok(loops.await?)
}

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
                    source: i.get(1)?,
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
                source: i.get(1)?,
                debounce_ms: i.get(2)?,
                created_by: i.get(3)?,
                created_at: i.get(4)?,
            })
        })?;
        Ok(rows.next().transpose()?)
    });
    Ok(channel.await?)
}

/// Create a channel row owned by `created_by`. Fails if the id already exists.
pub async fn insert_channel(
    db: &Connection,
    id: &str,
    source: &str,
    created_by: Option<&str>,
) -> Result<()> {
    let id = id.to_string();
    let source = source.to_string();
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

/// Ids of agent-created server channels (those with a `created_by`). Their
/// in-process publishers must be re-registered on boot; see
/// [`crate::loops::channels::register_owned_channels`].
pub async fn find_owned_server_channels(db: &Connection) -> Result<Vec<String>> {
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

pub async fn find_loop(db: &Connection, id: &str) -> Result<Option<Loop>> {
    let id = id.to_string();
    let record = db.call(move |conn| {
        let mut record = {
            let mut stmt = conn
                .prepare("SELECT id, system_prompt, tools, created_at FROM loop WHERE id = ?1")?;
            let mut rows = stmt.query_map([&id], |i| {
                let tools: String = i.get(2)?;
                Ok(Loop {
                    id: i.get(0)?,
                    system_prompt: i.get(1)?,
                    tools: serde_json::from_str(&tools).unwrap_or_default(),
                    created_at: i.get(3)?,
                    channels: Vec::new(),
                })
            })?;
            match rows.next().transpose()? {
                Some(record) => record,
                None => return Ok(None),
            }
        };

        let mut ch_stmt = conn
            .prepare("SELECT channel_id FROM loop_channel WHERE loop_id = ?1 ORDER BY position")?;
        record.channels = ch_stmt
            .query_map([&id], |i| i.get::<_, String>(0))?
            .filter_map(Result::ok)
            .collect();

        Ok(Some(record))
    });
    Ok(record.await?)
}

pub async fn insert_loop(db: &Connection, record: &Loop) -> Result<()> {
    let id = record.id.clone();
    let system_prompt = record.system_prompt.clone();
    let tools = serde_json::to_string(&record.tools)?;
    let channels = record.channels.clone();
    db.call(move |conn| {
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO loop (id, system_prompt, tools) VALUES (?1, ?2, ?3)",
            rusqlite::params![id, system_prompt, tools],
        )?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO loop_channel (loop_id, channel_id, position) VALUES (?1, ?2, ?3)",
            )?;
            for (position, channel_id) in channels.iter().enumerate() {
                stmt.execute(rusqlite::params![id, channel_id, position as i64])?;
            }
        }
        tx.commit()?;
        Ok(())
    })
    .await
    .map_err(anyhow::Error::new)
}

pub async fn delete_loop(db: &Connection, id: &str) -> Result<usize> {
    let id = id.to_string();
    let deleted = db.call(move |conn| {
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM loop_channel WHERE loop_id = ?1", [&id])?;
        let count = tx.execute("DELETE FROM loop WHERE id = ?1", [&id])?;
        tx.commit()?;
        Ok(count)
    });
    Ok(deleted.await?)
}

/// Ensure a `channel` row exists for each id with the given source so a loop's
/// `loop_channel` links satisfy the foreign key. Existing rows are left
/// untouched, preserving a channel's registered source and debounce.
///
/// Uses `ON CONFLICT(id) DO NOTHING` rather than `INSERT OR IGNORE` so only
/// the duplicate-id conflict is suppressed; any other constraint failure
/// still surfaces as an error.
pub async fn ensure_channels_with_source(
    db: &Connection,
    channels: &[String],
    source: &str,
) -> Result<()> {
    let channels = channels.to_vec();
    let source = source.to_string();
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

/// Ensure a `channel` row exists for each id as a `socket` channel (the default
/// for a loop's own subscriptions). See [`ensure_channels_with_source`].
pub async fn ensure_channels(db: &Connection, channels: &[String]) -> Result<()> {
    ensure_channels_with_source(db, channels, "socket").await
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

    fn sample_loop() -> Loop {
        Loop {
            id: "loop-1".to_string(),
            channels: vec!["alpha".to_string(), "beta".to_string()],
            system_prompt: "You are a loop.".to_string(),
            tools: vec!["bash".to_string(), "note_search".to_string()],
            created_at: String::new(),
        }
    }

    #[tokio::test]
    async fn initialize_db_creates_loop_tables() {
        let (_dir, db) = setup_db().await;
        let tables = db
            .call(|conn| {
                let mut stmt = conn.prepare(
                    "SELECT name FROM sqlite_master WHERE type = 'table'
                    AND name IN ('channel', 'loop', 'loop_channel')",
                )?;
                let names = stmt
                    .query_map([], |row| row.get::<_, String>(0))?
                    .filter_map(Result::ok)
                    .collect::<Vec<String>>();
                Ok(names)
            })
            .await
            .unwrap();
        assert!(tables.contains(&"channel".to_string()));
        assert!(tables.contains(&"loop".to_string()));
        assert!(tables.contains(&"loop_channel".to_string()));
    }

    #[tokio::test]
    async fn insert_find_and_delete_round_trip() {
        let (_dir, db) = setup_db().await;
        insert_raw_channel(&db, "alpha", "socket").await;
        insert_raw_channel(&db, "beta", "server").await;
        let record = sample_loop();

        insert_loop(&db, &record).await.unwrap();

        let all = find_all_loops(&db).await.unwrap();
        assert_eq!(all.len(), 1);
        let found = &all[0];
        assert_eq!(found.id, record.id);
        assert_eq!(found.channels, record.channels);
        assert_eq!(found.tools, record.tools);
        assert_eq!(found.system_prompt, record.system_prompt);
        assert!(!found.created_at.is_empty());

        let deleted = delete_loop(&db, &record.id).await.unwrap();
        assert_eq!(deleted, 1);

        let all = find_all_loops(&db).await.unwrap();
        assert!(all.is_empty());
    }

    #[tokio::test]
    async fn delete_loop_removes_channel_links() {
        let (_dir, db) = setup_db().await;
        insert_raw_channel(&db, "alpha", "socket").await;
        insert_raw_channel(&db, "beta", "server").await;
        insert_loop(&db, &sample_loop()).await.unwrap();

        delete_loop(&db, "loop-1").await.unwrap();

        let remaining = db
            .call(|conn| {
                let count: i64 = conn.query_row(
                    "SELECT COUNT(*) FROM loop_channel WHERE loop_id = ?1",
                    ["loop-1"],
                    |row| row.get(0),
                )?;
                Ok(count)
            })
            .await
            .unwrap();
        assert_eq!(remaining, 0);
    }

    #[tokio::test]
    async fn find_all_channels_returns_rows_ordered_by_id() {
        let (_dir, db) = setup_db().await;
        insert_raw_channel(&db, "beta", "server").await;
        insert_raw_channel(&db, "alpha", "socket").await;

        let channels = find_all_channels(&db).await.unwrap();

        let ids: Vec<&str> = channels.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, vec!["alpha", "beta"]);
        assert_eq!(channels[0].source, "socket");
        assert_eq!(channels[0].debounce_ms, 250);
        assert!(!channels[0].created_at.is_empty());
        assert_eq!(channels[1].source, "server");
    }

    #[tokio::test]
    async fn find_all_channels_empty_table_returns_empty() {
        let (_dir, db) = setup_db().await;
        assert!(find_all_channels(&db).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn insert_and_find_channel_round_trip() {
        let (_dir, db) = setup_db().await;

        insert_channel(&db, "owned", "server", Some("agent-a"))
            .await
            .unwrap();

        let found = find_channel(&db, "owned").await.unwrap().unwrap();
        assert_eq!(found.source, "server");
        assert_eq!(found.created_by.as_deref(), Some("agent-a"));
        assert!(find_channel(&db, "missing").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn insert_channel_rejects_duplicate_id() {
        let (_dir, db) = setup_db().await;
        insert_channel(&db, "dup", "server", Some("agent-a"))
            .await
            .unwrap();
        assert!(
            insert_channel(&db, "dup", "server", Some("agent-a"))
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn find_owned_server_channels_excludes_unowned_and_socket() {
        let (_dir, db) = setup_db().await;
        insert_raw_channel(&db, "timer", "server").await; // server, no owner
        insert_raw_channel(&db, "sock", "socket").await; // socket
        insert_channel(&db, "owned", "server", Some("agent-a"))
            .await
            .unwrap();

        let ids = find_owned_server_channels(&db).await.unwrap();
        assert_eq!(ids, vec!["owned"]);
    }

    #[tokio::test]
    async fn find_loop_returns_some_for_present_and_none_for_missing() {
        let (_dir, db) = setup_db().await;
        insert_raw_channel(&db, "alpha", "socket").await;
        insert_raw_channel(&db, "beta", "server").await;
        let record = sample_loop();
        insert_loop(&db, &record).await.unwrap();

        let found = find_loop(&db, &record.id).await.unwrap();
        assert!(found.is_some());
        let found = found.unwrap();
        assert_eq!(found.id, record.id);
        assert_eq!(found.channels, record.channels);

        let missing = find_loop(&db, "does-not-exist").await.unwrap();
        assert!(missing.is_none());
    }

    #[tokio::test]
    async fn delete_loop_missing_id_returns_zero() {
        let (_dir, db) = setup_db().await;
        let deleted = delete_loop(&db, "does-not-exist").await.unwrap();
        assert_eq!(deleted, 0);
    }

    #[tokio::test]
    async fn ensure_channels_creates_rows_and_preserves_existing() {
        let (_dir, db) = setup_db().await;

        // A channel that already exists with a non-socket source.
        insert_raw_channel(&db, "gamma", "server").await;

        ensure_channels(
            &db,
            &["alpha".to_string(), "beta".to_string(), "gamma".to_string()],
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
