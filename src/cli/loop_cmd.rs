//! Loop: subscribe to one or more channels and run an LLM chat on events.
//!
//! ## What it does
//!
//! `hq loop --channel <name> [--channel <other>]...` connects to one or more
//! channel publishers (see [`crate::cli::channel`]), merges their event streams,
//! and feeds each event into a fresh LLM chat. The chat has access to the
//! **bash tool** (sandboxed to a fresh workspace per loop invocation) and the
//! **notify tool** (push notifications) — not to the full tool set used by
//! `hq chat`.
//!
//! ## Event format
//!
//! Each event is tagged with its source channel: `[channel-id] event`. Every
//! event is processed independently: the LLM sees only the system prompt and
//! that single event — the chat (and its transcript) is rebuilt per event, so
//! there is no accumulated context across channels. The bash tool mounts the
//! same workspace directory for the whole loop, so files written in one event
//! remain accessible in later events.
//!
//! ## Trust boundary
//!
//! Same as the channel publisher: any process owned by the same user can
//! publish. The loop subscriber trusts events from channels it connects to —
//! do not subscribe to untrusted publishers if the LLM's bash workspace must
//! stay isolated.

use anyhow::{anyhow, Result};
use futures::StreamExt;
use std::time::Duration;
use tokio::io::BufReader;
use tokio::net::UnixStream;
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::ai::chat::{
    ChatBuilder, InfiniteLoopDetector, InvisibleCharFilter, ToolSecurityMiddleware,
};
use crate::ai::tools::{ToolContext, ToolRegistry};
use crate::cli::channel::{event_stream_from_reader, sigterm, socket_path};
use crate::openai::{Message, Role};
use tokio_rusqlite::Connection;

/// Run the loop: subscribe to `channels`, feed events into an LLM chat.
///
/// One reader task per channel merges events into a single mpsc receiver. The
/// main loop reads `(channel_id, event)` pairs and sends `"[channel-id] event"`
/// as a user message to the chat. The LLM's response is printed to stdout.
///
/// Incoming lines from each channel are coalesced into events using the same
/// debounce window as the publisher, so a multi-line burst is delivered to the
/// chat as one event rather than one per line.
///
/// All config (storage path, LLM endpoint/key/model) is passed in by the
/// caller (`mod.rs` run_dispatch); this module does not parse env vars.
pub async fn run(
    db: Connection,
    storage_path: &str,
    api_hostname: &str,
    api_key: &str,
    model: &str,
    vapid_key_path: &str,
    channels: &[String],
    debounce: Duration,
    tools: &[String],
    system_prompt: Option<&str>,
) -> Result<()> {
    if channels.is_empty() {
        return Err(anyhow!("at least one --channel is required"));
    }

    // Merge event streams from all channels into one receiver.
    let (event_tx, mut event_rx) = mpsc::channel::<(String, String)>(100);

    for channel_id in channels {
        let path = socket_path(storage_path, channel_id)?;
        let stream = UnixStream::connect(&path)
            .await
            .map_err(|_| anyhow!("channel '{}' not found — is the publisher running?", channel_id))?;
        println!("Subscribed to channel '{}'", channel_id);

        let ch_id = channel_id.clone();
        let tx = event_tx.clone();
        tokio::spawn(async move {
            let reader = BufReader::new(stream);
            let mut events = event_stream_from_reader(reader, debounce);
            while let Some(event) = events.next().await {
                if tx.send((ch_id.clone(), event)).await.is_err() {
                    return; // main loop exited
                }
            }
        });
    }
    drop(event_tx); // close our sender so event_rx drains then returns None

    let default_prompt = "You are a helpful assistant. You receive events from one or more \
         channels, each tagged as [channel-name] event. Respond to each \
         event appropriately. You have access to a bash tool for running commands \
         and a notify tool for sending push notifications.";
    let system_prompt = system_prompt.unwrap_or(default_prompt);

    // session_id is fixed for the lifetime of the loop so every BashTool mounts
    // the same workspace directory. Files written in one turn are visible in
    // later turns even though the LLM transcript is rebuilt fresh each turn.
    let session_id = Uuid::new_v4().to_string();

    // Build the tool registry once. The registry owns the context (including the
    // fixed session_id), so tools rebuilt per event stay rooted in the same
    // workspace. Defaults to bash+notify, preserving the original behavior.
    let context = ToolContext {
        db: db.clone(),
        api_base_url: "http://localhost:2222".to_string(),
        storage_path: storage_path.to_string(),
        vapid_key_path: vapid_key_path.to_string(),
        session_id: session_id.clone(),
        skill_registry: None,
    };
    let registry = ToolRegistry::builtin(context);
    let tool_names: Vec<String> = if tools.is_empty() {
        vec!["bash".to_string(), "notify".to_string()]
    } else {
        tools.to_vec()
    };

    // Main loop: read merged events, send to chat, print responses.
    // Ctrl-C or SIGTERM breaks out and exits cleanly.
    //
    // Tools and chat are reconstructed per event. Rebuilding the chat means
    // each event is processed with only the system prompt and that event — no
    // accumulated transcript. Rebuilding the tools is harmless (bash mounts the
    // same session_id directory; notify is stateless).
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                eprintln!("\nShutting down...");
                return Ok(());
            }
            _ = sigterm() => {
                eprintln!("\nShutting down (SIGTERM)...");
                return Ok(());
            }
            event = event_rx.recv() => {
                let Some((channel_id, event)) = event else { break; };
                let user_msg = format!("[{}] {}", channel_id, event);

                let tools = registry.from_list(&tool_names)?;

                let mut chat = ChatBuilder::new(api_hostname, api_key, model)
                    .transcript(vec![Message::new(Role::System, system_prompt)])
                    .tools(tools)
                    .middleware(vec![
                        Box::new(InfiniteLoopDetector::new(3)),
                        Box::new(ToolSecurityMiddleware::default()),
                        Box::new(InvisibleCharFilter),
                    ])
                    .build();

                match chat.next_msg(Message::new(Role::User, &user_msg)).await {
                    Ok(resp) => {
                        if let Some(msg) = resp.last() {
                            if let Some(content) = &msg.content {
                                println!("{}", content);
                            }
                        }
                    }
                    Err(e) => eprintln!("chat error: {}", e),
                }
            }
        }
    }

    Ok(())
}