//! Supervisor for persisted channel loops.
//!
//! A loop mirrors the `hq loop` CLI ([`crate::cli::loop_cmd`]): it connects to
//! one or more channel publishers, merges their event streams, and runs a fresh
//! LLM chat turn per event. This module owns the live instances — [`start`]
//! spawns a background task and records it in a process-wide [`RUNNING`]
//! registry, [`stop`] aborts it, and [`respawn_all`] restarts every persisted
//! loop (e.g. on server boot).
//!
//! The spawned task has no signal handling of its own — it runs until its
//! `JoinHandle` is aborted by [`stop`] (the per-channel reader tasks then exit
//! when their send fails).

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use anyhow::Result;
use futures::StreamExt;
use futures::stream::BoxStream;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_rusqlite::Connection;

use super::db::find_all_loops;
use super::models::Loop;
use super::prompt::build_system_prompt;
use super::tools::resolve_loop_tools;
use crate::ai::chat::{
    ChatBuilder, InfiniteLoopDetector, InvisibleCharFilter, ToolSecurityMiddleware,
};
use crate::ai::tools::{ToolConfig, ToolRegistry};
use crate::channels::subscribe::subscribe;
use crate::cli::channel::validate_channel_id;
use crate::openai::{Message, Role};

/// Configuration for the loop supervisor, assembled by the caller from server
/// state and environment.
#[derive(Clone)]
pub struct LoopSupervisorConfig {
    pub db: Connection,
    pub api_base_url: String,
    pub api_hostname: String,
    pub api_key: String,
    pub model: String,
    pub storage_path: String,
    pub vapid_key_path: String,
}

/// Process-wide registry of running loops, keyed by loop id.
static RUNNING: OnceLock<Mutex<HashMap<String, JoinHandle<()>>>> = OnceLock::new();

fn running() -> &'static Mutex<HashMap<String, JoinHandle<()>>> {
    RUNNING.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Start running `record`, replacing any loop already running under the same id.
///
/// Every channel is validated and subscribed up front so a missing publisher
/// fails fast (before any task is spawned). All `.await`s happen before the
/// [`RUNNING`] lock is touched.
pub async fn start(record: Loop, config: LoopSupervisorConfig) -> Result<()> {
    let id = record.id.clone();

    let mut streams = Vec::with_capacity(record.channels.len());
    for channel in &record.channels {
        validate_channel_id(channel)?;
        let stream = subscribe(&config.db, &config.storage_path, channel).await?;
        streams.push((channel.clone(), stream));
    }

    // Dedupe: abort any previous task for this id before replacing it.
    stop(&id);

    let handle = tokio::spawn(run_loop(record, config, streams));
    running().lock().unwrap().insert(id, handle);
    Ok(())
}

/// Stop the loop with `id`. Returns `true` if it was running.
pub fn stop(id: &str) -> bool {
    match running().lock().unwrap().remove(id) {
        Some(handle) => {
            handle.abort();
            true
        }
        None => false,
    }
}

/// Ids of loops whose tasks are still alive.
///
/// A task that exits on its own cannot remove its own entry, so finished
/// handles are filtered out to avoid stale "running" reports.
pub fn running_ids() -> Vec<String> {
    running()
        .lock()
        .unwrap()
        .iter()
        .filter(|(_, handle)| !handle.is_finished())
        .map(|(id, _)| id.clone())
        .collect()
}

/// Start every persisted loop. Failures for individual loops are logged and
/// skipped; an `Err` is returned only if the loop list itself cannot be read.
pub async fn respawn_all(config: LoopSupervisorConfig) -> Result<()> {
    let records = find_all_loops(&config.db).await?;
    for record in records {
        let id = record.id.clone();
        if let Err(e) = start(record, config.clone()).await {
            tracing::warn!("failed to respawn loop '{}': {}", id, e);
        }
    }
    Ok(())
}

/// The spawned loop task: fan channel events into one receiver and run a fresh
/// chat turn per event.
async fn run_loop(
    record: Loop,
    config: LoopSupervisorConfig,
    streams: Vec<(String, BoxStream<'static, String>)>,
) {
    // Merge each channel's event stream into one receiver. The streams are
    // already coalesced by source (socket debounce, or one event per server
    // publish).
    let (event_tx, mut event_rx) = mpsc::channel::<(String, String)>(100);

    for (channel_id, stream) in streams {
        let tx = event_tx.clone();
        tokio::spawn(async move {
            let mut events = stream;
            while let Some(event) = events.next().await {
                if tx.send((channel_id.clone(), event)).await.is_err() {
                    return; // main loop exited
                }
            }
        });
    }
    drop(event_tx); // close our sender so event_rx drains then returns None

    let system_prompt = build_system_prompt(&record.system_prompt);

    // session_id is the loop id, fixed for the lifetime of the loop so every
    // BashTool mounts the same workspace directory across events and restarts.
    let context = ToolConfig {
        db: config.db.clone(),
        api_base_url: config.api_base_url.clone(),
        storage_path: config.storage_path.clone(),
        vapid_key_path: config.vapid_key_path.clone(),
        session_id: record.id.clone(),
        skill_registry: None,
        api_hostname: config.api_hostname.clone(),
        api_key: config.api_key.clone(),
        model: config.model.clone(),
    };
    let registry = ToolRegistry::builtin(context);
    let tool_names = resolve_loop_tools(&record.tools);

    // Each event is processed with only the system prompt and that event — no
    // accumulated transcript. Tools and chat are rebuilt per event.
    while let Some((channel_id, event)) = event_rx.recv().await {
        let user_msg = format!("[{}] {}", channel_id, event);

        let tools = match registry.from_list(&tool_names) {
            Ok(tools) => tools,
            Err(e) => {
                tracing::warn!("loop '{}': failed to build tools: {}", record.id, e);
                continue;
            }
        };

        let mut chat = ChatBuilder::new(&config.api_hostname, &config.api_key, &config.model)
            .transcript(vec![Message::new(Role::System, &system_prompt)])
            .tools(tools)
            .middleware(vec![
                Box::new(InfiniteLoopDetector::new(3)),
                Box::new(ToolSecurityMiddleware::default()),
                Box::new(InvisibleCharFilter),
            ])
            .build();

        match chat.next_msg(Message::new(Role::User, &user_msg)).await {
            Ok(resp) => {
                if let Some(content) = resp.last().and_then(|msg| msg.text()) {
                    tracing::info!("loop '{}': {}", record.id, content);
                }
            }
            Err(e) => tracing::warn!("loop '{}': chat error: {}", record.id, e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channels::registry;
    use crate::core::db::{async_db, initialize_db};
    use uuid::Uuid;

    async fn setup() -> (tempfile::TempDir, LoopSupervisorConfig) {
        let dir = tempfile::tempdir().unwrap();
        let db = async_db(dir.path().to_str().unwrap()).await.unwrap();
        db.call(|conn| {
            initialize_db(conn)?;
            Ok(())
        })
        .await
        .unwrap();

        let storage_path = dir.path().to_str().unwrap().to_string();
        let config = LoopSupervisorConfig {
            db,
            api_base_url: "http://localhost".to_string(),
            api_hostname: "localhost".to_string(),
            api_key: "test-key".to_string(),
            model: "test-model".to_string(),
            storage_path,
            vapid_key_path: dir.path().join("vapid").to_str().unwrap().to_string(),
        };
        (dir, config)
    }

    fn loop_record() -> Loop {
        Loop {
            id: "loop-test".to_string(),
            channels: vec!["nonexistent-channel".to_string()],
            system_prompt: "You are a loop.".to_string(),
            tools: vec![],
            created_at: String::new(),
        }
    }

    #[tokio::test]
    async fn start_with_registered_server_channel_runs() {
        let (_dir, config) = setup().await;
        let channel = format!("srv-{}", Uuid::new_v4());
        let channel_for_db = channel.clone();
        config
            .db
            .call(move |conn| {
                conn.execute(
                    "INSERT INTO channel (id, source) VALUES (?1, 'server')",
                    [&channel_for_db],
                )?;
                Ok(())
            })
            .await
            .unwrap();
        let _publisher = registry::register(&channel);

        let record = Loop {
            id: "loop-server".to_string(),
            channels: vec![channel],
            system_prompt: "You are a loop.".to_string(),
            tools: vec![],
            created_at: String::new(),
        };

        start(record, config).await.unwrap();
        assert!(running_ids().iter().any(|id| id == "loop-server"));
        stop("loop-server");
    }

    #[tokio::test]
    async fn start_with_missing_channel_errors_and_running_ids_stays_empty() {
        let (_dir, config) = setup().await;
        let err = start(loop_record(), config).await.unwrap_err();
        assert!(err.to_string().contains("not found"), "{err}");
        assert!(running_ids().is_empty());
    }

    #[tokio::test]
    async fn failed_start_leaves_no_entry_in_running() {
        let (_dir, config) = setup().await;
        assert!(start(loop_record(), config).await.is_err());
        assert!(running().lock().unwrap().is_empty());
    }

    #[test]
    fn stop_unknown_id_returns_false() {
        assert!(!stop("definitely-not-running"));
    }
}
