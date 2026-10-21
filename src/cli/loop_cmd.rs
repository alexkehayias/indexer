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

use anyhow::{Result, anyhow};
use futures::StreamExt;
use std::time::Duration;
use tokio::io::BufReader;
use tokio::net::UnixStream;
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::ai::chat::{
    ChatBuilder, InfiniteLoopDetector, InvisibleCharFilter, ToolSecurityMiddleware,
};
use crate::ai::tools::{ToolConfig, ToolRegistry};
use crate::channels::models::ChannelOwner;
use crate::channels::subscribe::channel_meta;
use crate::cli::channel::{event_stream_from_reader, sigterm, socket_path};
use crate::core::markdown_render;
use crate::loops::prompt::build_system_prompt;
use crate::loops::tools::resolve_loop_tools;
use crate::openai::{Message, Role};
use tokio_rusqlite::Connection;

/// Configuration for the event loop, assembled by `run_dispatch` from CLI
/// args and environment.
pub struct LoopConfig<'a> {
    pub api_base_url: &'a str,
    pub api_hostname: &'a str,
    pub api_key: &'a str,
    pub model: &'a str,
    pub storage_path: &'a str,
    pub vapid_key_path: &'a str,
    pub channels: &'a [String],
    pub tools: &'a [String],
    pub debounce: Duration,
    pub system_prompt: &'a str,
}

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
/// All config (LLM endpoint/key/model, channel list, storage path, db) is passed
/// in by the caller (`mod.rs` run_dispatch); this module does not parse env
/// vars. The [`ToolConfig`] is constructed here from those arguments.
pub async fn run(db: Connection, config: LoopConfig<'_>) -> Result<()> {
    let LoopConfig {
        api_base_url,
        api_hostname,
        api_key,
        model,
        storage_path,
        vapid_key_path,
        channels,
        tools,
        debounce,
        system_prompt,
    } = config;

    if channels.is_empty() {
        return Err(anyhow!("at least one --channel is required"));
    }

    // Merge event streams from all channels into one receiver.
    let (event_tx, mut event_rx) = mpsc::channel::<(String, String)>(100);

    for channel_id in channels {
        let (owner, _) = channel_meta(&db, channel_id).await?;
        if owner != ChannelOwner::Socket {
            return Err(anyhow!(
                "channel '{}' is a server channel — only loops running inside the server can subscribe to it",
                channel_id
            ));
        }

        let path = socket_path(storage_path, channel_id)?;
        let stream = UnixStream::connect(&path).await.map_err(|_| {
            anyhow!(
                "channel '{}' not found — is the publisher running?",
                channel_id
            )
        })?;
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

    let system_prompt = build_system_prompt(system_prompt);

    // session_id is fixed for the lifetime of the loop so every BashTool mounts
    // the same workspace directory. Files written in one turn are visible in
    // later turns even though the LLM transcript is rebuilt fresh each turn.
    let session_id = Uuid::new_v4().to_string();

    // Build the tool registry once. The registry owns the context (including the
    // fixed session_id), so tools rebuilt per event stay rooted in the same
    // workspace. Defaults to bash+notify, preserving the original behavior.
    let context = ToolConfig {
        db: db.clone(),
        api_base_url: api_base_url.to_string(),
        storage_path: storage_path.to_string(),
        vapid_key_path: vapid_key_path.to_string(),
        session_id: session_id.clone(),
        skill_registry: None,
        api_hostname: api_hostname.to_string(),
        api_key: api_key.to_string(),
        model: model.to_string(),
    };
    let registry = ToolRegistry::builtin(context);
    let tool_names = resolve_loop_tools(tools);

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
                            print!("{}", markdown_render::render_for_stdout(content));
                        }
                    }
                    Err(e) => eprintln!("chat error: {}", e),
                }
            }
        }
    }

    Ok(())
}
