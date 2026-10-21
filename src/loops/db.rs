use std::collections::HashMap;

use anyhow::Result;
use tokio_rusqlite::Connection;

use super::models::Loop;

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
}
