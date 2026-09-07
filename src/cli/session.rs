use std::path::PathBuf;

use anyhow::{Context, Result, bail};

use crate::ai::chat::db::{delete_chat_session, find_chat_session_by_id};
use crate::ai::chat::summarize::generate_and_update_session_info;
use crate::core::db::async_db;
use crate::openai::Message;
use crate::search::delete_chat_session_index;
use tokio_rusqlite::Connection;

/// Delete a chat session, its messages, its tantivy search index entries,
/// and its workspace directory.
///
/// Order of operations:
/// 1. Open the DB; bail if the session doesn't exist in `session` (matches
///    the `tasks::run_delete` "error on missing" pattern in `src/cli/tasks.rs:191`).
/// 2. Delete the session's messages from the tantivy full-text search index
///    (needs DB rows to look up message IDs — must run BEFORE `delete_chat_session`).
/// 3. Delete the DB rows: chat_message, session_tag links, and the session row
///    (in a transaction, in dependency order).
/// 4. Remove the workspace directory at `{storage_path}/workspace/{session_id}`.
///
/// If tantivy deletion fails (e.g., index directory missing), we error out
/// before touching DB rows — no half-state. If workspace deletion fails after
/// a successful DB/index cleanup, the session is still effectively "deleted"
/// from the user's perspective; we report the failure as an error so they
/// can manually clean up the directory.
pub async fn run_delete(
    vec_db_path: &str,
    index_dir_path: &str,
    storage_path: &str,
    session_id: &str,
) -> Result<()> {
    let db = async_db(vec_db_path)
        .await
        .context("Failed to connect to database")?;

    // Check if the session exists. If not, bail — matches `tasks::run_delete`
    // behavior where `find_task(...)?` errors when the task ID doesn't exist.
    let s_id_check = session_id.to_string();
    let exists: bool = db
        .call(move |conn| {
            let count: i64 = conn.query_row(
                "SELECT COUNT(*) FROM session WHERE id = ?",
                rusqlite::params![s_id_check],
                |row| row.get(0),
            )?;
            Ok(count > 0)
        })
        .await
        .context("Failed to check session existence")?;

    if !exists {
        bail!("Chat session {session_id} not found");
    }

    // 1. Tantivy index cleanup (needs DB rows to exist for message ID lookup)
    delete_chat_session_index(&db, index_dir_path, session_id)
        .await
        .context("Failed to delete chat session from search index")?;

    // 2. DB cleanup
    let result = delete_chat_session(&db, session_id)
        .await
        .context("Failed to delete chat session from database")?;

    // 3. Workspace directory cleanup
    let workspace_path = PathBuf::from(format!("{}/workspace/{}", storage_path, session_id));
    let workspace_existed = workspace_path.exists();
    if workspace_existed {
        tokio::fs::remove_dir_all(&workspace_path)
            .await
            .with_context(|| {
                format!(
                    "Failed to remove workspace directory: {}",
                    workspace_path.display()
                )
            })?;
    }

    println!(
        "Deleted chat session {} ({} messages removed, search index updated, workspace dir {})",
        session_id,
        result.messages_deleted,
        if workspace_existed { "removed" } else { "not found" }
    );

    Ok(())
}

/// Generate (or regenerate) a title and summary for a chat session by sending
/// its transcript to the LLM, then persist the result to the `session` table.
///
/// Always regenerates, even if the session already has a title/summary (unlike
/// the background `GenerateSessionTitles` job, which only processes sessions
/// missing both). Bails if the session doesn't exist or has no messages.
pub async fn run_summarize(
    db: Connection,
    api_hostname: &str,
    api_key: &str,
    model: &str,
    session_id: &str,
) -> Result<()> {
    // Check if the session exists — same check as `run_delete`.
    let s_id_check = session_id.to_string();
    let exists: bool = db
        .call(move |conn| {
            let count: i64 = conn.query_row(
                "SELECT COUNT(*) FROM session WHERE id = ?",
                rusqlite::params![s_id_check],
                |row| row.get(0),
            )?;
            Ok(count > 0)
        })
        .await
        .context("Failed to check session existence")?;

    if !exists {
        bail!("Chat session {session_id} not found");
    }

    let transcript: Vec<Message> = find_chat_session_by_id(&db, session_id)
        .await
        .context("Failed to load chat session transcript")?
        .into_iter()
        .map(|(_, msg)| msg)
        .collect();

    if transcript.is_empty() {
        bail!("Chat session {session_id} has no messages to summarize");
    }

    match generate_and_update_session_info(
        &db,
        api_hostname,
        api_key,
        model,
        session_id,
        &transcript,
    )
    .await?
    {
        Some((title, summary)) => {
            println!("Summarized session {session_id}:\n  title: {title}\n  summary: {summary}");
        }
        None => {
            println!(
                "Session {session_id}: could not parse LLM response; session left unchanged"
            );
        }
    }

    Ok(())
}

/// List all chat sessions as a table of session ID and title (if set).
pub async fn run_list(db: Connection) -> Result<()> {
    let sessions: Vec<(String, Option<String>)> = db
        .call(|conn| {
            let mut stmt = conn.prepare(
                "SELECT id, title FROM session ORDER BY created_at DESC",
            )?;
            let rows = stmt.query_map([], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .await
        .context("Failed to list chat sessions")?;

    if sessions.is_empty() {
        println!("No chat sessions found.");
        return Ok(());
    }

    println!("{:<40} {}", "ID", "Title");
    println!("{}", "-".repeat(80));
    for (id, title) in sessions {
        let title = title.unwrap_or_else(|| "—".to_string());
        println!("{id:<40} {title}");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::chat::db::{get_or_create_session, insert_chat_message};
    use crate::ai::chat::models::SessionMode;
    use crate::core::db::{async_db, initialize_db};
    use crate::openai::{Message, Role};
    use tempfile::TempDir;

    /// Set up a test DB at `{dir}/db` with chat schema initialized.
    async fn test_db(dir: &std::path::Path) -> String {
        let db_path = dir.join("db");
        std::fs::create_dir_all(&db_path).unwrap();
        let db_path_str = db_path.to_str().unwrap().to_string();
        let db = async_db(&db_path_str).await.unwrap();
        db.call(|conn| {
            initialize_db(conn).unwrap();
            Ok(())
        })
        .await
        .unwrap();
        db_path_str
    }

    /// Create a temp storage directory with `db/`, `index/`, and `workspace/`
    /// subdirectories, returning the root path string.
    fn setup_storage_dir() -> TempDir {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join("db")).unwrap();
        std::fs::create_dir_all(dir.path().join("index")).unwrap();
        std::fs::create_dir_all(dir.path().join("workspace")).unwrap();
        dir
    }

    #[tokio::test]
    async fn test_run_delete_with_existing_session_and_workspace() {
        let storage = setup_storage_dir();
        let storage_path = storage.path().to_str().unwrap().to_string();
        let db_path = test_db(storage.path()).await;
        let index_dir = storage.path().join("index").to_str().unwrap().to_string();

        // Create a session with messages
        let db = async_db(&db_path).await.unwrap();
        get_or_create_session(&db, "test-session-1", &[], SessionMode::Chat)
            .await
            .unwrap();
        let msg = Message::new(Role::User, "Hello");
        insert_chat_message(&db, "test-session-1", &msg).await.unwrap();

        // Create a workspace directory with a sentinel file
        let workspace_path = storage.path().join("workspace").join("test-session-1");
        std::fs::create_dir_all(&workspace_path).unwrap();
        std::fs::write(workspace_path.join("sentinel.txt"), "test").unwrap();

        // Run delete
        let result = run_delete(&db_path, &index_dir, &storage_path, "test-session-1").await;

        assert!(result.is_ok(), "expected Ok, got {result:?}");

        // DB rows are gone
        let remaining_session: i64 = db
            .call(|conn| {
                Ok(conn.query_row(
                    "SELECT COUNT(*) FROM session WHERE id = ?",
                    ["test-session-1"],
                    |row| row.get(0),
                )?)
            })
            .await
            .unwrap();
        assert_eq!(remaining_session, 0);

        let remaining_msgs: i64 = db
            .call(|conn| {
                Ok(conn.query_row(
                    "SELECT COUNT(*) FROM chat_message WHERE session_id = ?",
                    ["test-session-1"],
                    |row| row.get(0),
                )?)
            })
            .await
            .unwrap();
        assert_eq!(remaining_msgs, 0);

        // Workspace dir is gone
        assert!(!workspace_path.exists(), "workspace dir should be removed");
    }

    #[tokio::test]
    async fn test_run_delete_nonexistent_session() {
        let storage = setup_storage_dir();
        let storage_path = storage.path().to_str().unwrap().to_string();
        let db_path = test_db(storage.path()).await;
        let index_dir = storage.path().join("index").to_str().unwrap().to_string();

        // Run delete on a session that doesn't exist
        let result = run_delete(&db_path, &index_dir, &storage_path, "nonexistent").await;

        assert!(
            result.is_err(),
            "expected error for nonexistent session, got {result:?}"
        );
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("not found"),
            "error should mention 'not found', got: {err}"
        );
    }

    #[tokio::test]
    async fn test_run_delete_missing_workspace_dir() {
        let storage = setup_storage_dir();
        let storage_path = storage.path().to_str().unwrap().to_string();
        let db_path = test_db(storage.path()).await;
        let index_dir = storage.path().join("index").to_str().unwrap().to_string();

        // Create a session but NO workspace directory
        let db = async_db(&db_path).await.unwrap();
        get_or_create_session(&db, "test-session-no-workspace", &[], SessionMode::Chat)
            .await
            .unwrap();
        let msg = Message::new(Role::User, "Hello");
        insert_chat_message(&db, "test-session-no-workspace", &msg)
            .await
            .unwrap();

        // Verify workspace dir does NOT exist before delete
        let workspace_path = storage
            .path()
            .join("workspace")
            .join("test-session-no-workspace");
        assert!(!workspace_path.exists());

        // Delete should still succeed
        let result = run_delete(
            &db_path,
            &index_dir,
            &storage_path,
            "test-session-no-workspace",
        )
        .await;

        assert!(result.is_ok(), "expected Ok, got {result:?}");

        // DB rows are gone
        let remaining: i64 = db
            .call(|conn| {
                Ok(conn.query_row(
                    "SELECT COUNT(*) FROM session WHERE id = ?",
                    ["test-session-no-workspace"],
                    |row| row.get(0),
                )?)
            })
            .await
            .unwrap();
        assert_eq!(remaining, 0);
    }

    #[tokio::test]
    async fn test_run_summarize_nonexistent_session() {
        let storage = setup_storage_dir();
        let db_path = test_db(storage.path()).await;
        let db = async_db(&db_path).await.unwrap();

        let result = run_summarize(db, "host", "key", "model", "nonexistent").await;

        assert!(
            result.is_err(),
            "expected error for nonexistent session, got {result:?}"
        );
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("not found"),
            "error should mention 'not found', got: {err}"
        );
    }

    #[tokio::test]
    async fn test_run_summarize_empty_session() {
        let storage = setup_storage_dir();
        let db_path = test_db(storage.path()).await;

        // Session exists but has no messages
        let db = async_db(&db_path).await.unwrap();
        get_or_create_session(&db, "empty-session", &[], SessionMode::Chat)
            .await
            .unwrap();

        let result = run_summarize(db, "host", "key", "model", "empty-session").await;

        assert!(
            result.is_err(),
            "expected error for session with no messages, got {result:?}"
        );
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("no messages"),
            "error should mention 'no messages', got: {err}"
        );
    }

    #[tokio::test]
    async fn test_run_list_empty() {
        let storage = setup_storage_dir();
        let db_path = test_db(storage.path()).await;
        let db = async_db(&db_path).await.unwrap();

        let result = run_list(db).await;
        assert!(result.is_ok(), "expected Ok, got {result:?}");
    }

    #[tokio::test]
    async fn test_run_list_with_sessions() {
        let storage = setup_storage_dir();
        let db_path = test_db(storage.path()).await;
        let db = async_db(&db_path).await.unwrap();

        // One session with a title, one without
        db.call(|conn| {
            conn.execute(
                "INSERT INTO session (id, title) VALUES (?1, ?2)",
                rusqlite::params!["with-title", "My session"],
            )?;
            conn.execute(
                "INSERT INTO session (id) VALUES (?1)",
                rusqlite::params!["no-title"],
            )?;
            Ok(())
        })
        .await
        .unwrap();

        let result = run_list(db).await;
        assert!(result.is_ok(), "expected Ok, got {result:?}");
    }
}
