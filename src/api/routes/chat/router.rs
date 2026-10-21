//! Router for the chat API

use std::convert::Infallible;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use axum::Json;
use axum::response::Response;
use axum::{
    Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, sse::Event, sse::KeepAlive, sse::Sse},
    routing::{get, post},
};
use axum_extra::extract::Query;
use serde_json::json;
use tokio::sync::{broadcast, mpsc};
use tokio_stream::StreamExt;
use tokio_stream::wrappers::UnboundedReceiverStream;
use uuid::Uuid;

use super::db::{chat_session_count, chat_session_list};
use super::stream::{self, SessionStream};
use crate::ai::chat::commands::{SlashCommand, get_help_text};
use crate::ai::chat::models::SessionMode;
use crate::ai::chat::{
    ChatBuilder, InfiniteLoopDetector, InvisibleCharFilter, ToolSecurityMiddleware,
    find_chat_session_by_id, get_or_create_session, insert_chat_message, set_session_mode,
    user_message_with_attachments,
};
use crate::ai::tools::{
    BashTool, CalendarTool, CreateChannelTool, CreateLoopTool, DateTimeTool, DeleteLoopTool,
    EmailSearchTool, EmailUnreadTool, EmitChannelTool, IterateTool, ListLoopsTool,
    MeetingSearchTool, MemoryTool, NoteSearchTool, NotifyTool, PythonTool, TasksDueTodayTool,
    TasksScheduledTodayTool, WebSearchTool, WebsiteViewTool, run_in_sandbox,
};
use crate::anthropic::claude::{ClaudeCodeSession, Delta, StreamEvent};
use crate::api::state::AppState;
use crate::api::utils::DetectDisconnect;
use crate::channels::prompt::system_prompt_section;
use crate::core::AppConfig;
use crate::loops::supervisor::LoopSupervisorConfig;
use crate::notify::{
    PushNotificationPayload, broadcast_push_notification, find_all_notification_subscriptions,
    mark_push_subscription_invalid,
};
use crate::openai::{BoxedToolCall, Message, Role};
use crate::search::index_chat_messages;
// Re-export public types for this module
pub use super::public::{
    ChatRequest, ChatSessionsQuery, ChatSessionsResponse, ChatStreamQuery, ChatTranscriptResponse,
};

type SharedState = Arc<RwLock<AppState>>;

/// Maximum number of image attachments accepted on a single chat message.
/// Bounds the size of the multimodal request sent to the model.
const MAX_ATTACHMENTS_PER_MESSAGE: usize = 20;

/// Get a single chat session by ID
async fn chat_session(
    State(state): State<SharedState>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, crate::api::public::ApiError> {
    let db = state.read().expect("Unable to read share state").db.clone();
    let transcript_with_ids = find_chat_session_by_id(&db, &id).await?;
    let transcript: Vec<Message> = transcript_with_ids
        .into_iter()
        .map(|(_, msg)| msg)
        .collect();

    if transcript.is_empty() {
        return Ok((
            StatusCode::NOT_FOUND,
            format!("Chat session {} not found", id),
        )
            .into_response());
    }

    let streams = state
        .read()
        .expect("Unable to read share state")
        .streams
        .clone();
    let in_progress = stream::get(&streams, &id).is_some_and(|stream| !stream.is_done());

    Ok(axum::Json(ChatTranscriptResponse {
        transcript,
        in_progress,
    })
    .into_response())
}

/// Build an SSE response that replays a session's buffered events after
/// `after`, then tails live until the turn finishes.
fn sse_from_stream(stream: Arc<SessionStream>, after: u64) -> Response {
    Sse::new(stream.subscribe(after))
        .keep_alive(
            KeepAlive::default()
                .text("keep-alive")
                .interval(Duration::from_millis(100)),
        )
        .into_response()
}

/// Attach to a session's in-flight response stream, resuming after the
/// `Last-Event-ID` header (or `?after=` query). Returns 204 when there is no
/// active stream, which also tells `EventSource` to stop reconnecting.
async fn chat_stream_handler(
    State(state): State<SharedState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Query(params): Query<ChatStreamQuery>,
) -> Result<Response, crate::api::public::ApiError> {
    let streams = state
        .read()
        .expect("Unable to read share state")
        .streams
        .clone();

    let Some(stream) = stream::get(&streams, &id) else {
        return Ok(StatusCode::NO_CONTENT.into_response());
    };

    let after = headers
        .get("Last-Event-ID")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .or(params.after)
        .unwrap_or(0);

    Ok(sse_from_stream(stream, after))
}

/// Get a list of all chat sessions
async fn chat_list(
    State(state): State<SharedState>,
    Query(params): Query<ChatSessionsQuery>,
) -> Result<Json<ChatSessionsResponse>, crate::api::public::ApiError> {
    let db = state.read().expect("Unable to read share state").db.clone();
    let page = params.page.unwrap_or(1);
    let limit = params.limit.unwrap_or(20);
    let offset = (page - 1) * limit;
    let include_tags = params.tags.unwrap_or(vec![]);
    let exclude_tags = params.exclude_tags.unwrap_or(vec![]);
    let total_sessions = chat_session_count(&db, &include_tags, &exclude_tags).await?;
    let paged_sessions =
        chat_session_list(&db, &include_tags, &exclude_tags, limit, offset).await?;
    let total_pages = (total_sessions as f64 / limit as f64).ceil() as i64;

    Ok(axum::Json(ChatSessionsResponse {
        sessions: paged_sessions,
        page,
        limit,
        total_sessions,
        total_pages,
    }))
}

/// Handle messages in agent mode using Claude Code
async fn handle_agent_mode(
    state: SharedState,
    session_id: String,
    user_msg: Message,
    _tx: mpsc::UnboundedSender<String>,
    mut disconnect_receiver: broadcast::Receiver<()>,
    resume: bool,
) -> Result<Response, crate::api::public::ApiError> {
    let db = state.read().expect("Unable to read share state").db.clone();

    // Create UUID from session_id for Claude Code
    let uuid = Uuid::parse_str(&session_id).unwrap_or_else(|_| Uuid::new_v4());

    // Create Claude Code session with default tools (Read, Edit, Bash)
    let claude_session = ClaudeCodeSession::with_default_tools(uuid);

    // Start the agent conversation
    let mut events = if resume {
        claude_session.resume(user_msg.text().unwrap_or(""))
    } else {
        claude_session.start(user_msg.text().unwrap_or(""))
    };

    let (sse_tx, sse_rx) = mpsc::unbounded_channel::<String>();
    let (disconnect_notifier, _) = broadcast::channel::<()>(1);

    // Spawn task to process events and forward to SSE channel
    tokio::spawn(async move {
        let mut full_response = String::new();

        while let Some(event_result) = tokio_stream::StreamExt::next(&mut events).await {
            match event_result {
                Ok(StreamEvent::ContentBlockDelta { delta }) => {
                    if let Delta::TextDelta { text } = delta {
                        full_response.push_str(&text);
                        // Send in same format as chat API
                        let chunk = json!({
                            "choices": [{
                                "delta": { "content": text }
                            }]
                        });
                        // Skip sending if client disconnected
                        if !sse_tx.is_closed() {
                            let _ = sse_tx.send(chunk.to_string());
                        } else {
                            break;
                        }
                    }
                }
                Ok(StreamEvent::MessageStop) => {
                    // Store assistant response in transcript
                    let assistant_msg = Message::new(Role::Assistant, &full_response);
                    // Store user message in transcript
                    insert_chat_message(&db, &session_id, &user_msg)
                        .await
                        .expect("Inserting user message failed");
                    insert_chat_message(&db, &session_id, &assistant_msg)
                        .await
                        .expect("Inserting assistant message failed");
                }
                Ok(_) => {
                    // Other events we don't need to forward
                }
                Err(e) => {
                    tracing::error!("Claude Code error: {}", e);
                    let err_chunk = json!({
                        "choices": [{
                            "finish_reason": "error",
                            "delta": { "content": format!("Error: {}", e) }
                        }]
                    });
                    // Skip sending if client disconnected
                    if !sse_tx.is_closed() {
                        let _ = sse_tx.send(err_chunk.to_string());
                    }
                }
            }
        }

        // Send notification if client disconnected
        if sse_tx.is_closed() {
            let _ = disconnect_receiver.recv().await.map(|()| {
                tracing::info!("Sending notification for agent response!");
            });
        }
    });

    let sse_stream = tokio_stream::StreamExt::map(UnboundedReceiverStream::new(sse_rx), |chunk| {
        Ok::<Event, Infallible>(Event::default().data(chunk))
    });
    let wrapped_sse_stream = DetectDisconnect::new(sse_stream, disconnect_notifier);

    Ok(Sse::new(wrapped_sse_stream)
        .keep_alive(
            KeepAlive::default()
                .text("keep-alive")
                .interval(Duration::from_millis(100)),
        )
        .into_response())
}

/// Initiate or add to a chat session and stream the response
async fn chat_handler(
    State(state): State<SharedState>,
    Json(payload): Json<ChatRequest>,
) -> Result<impl IntoResponse, crate::api::public::ApiError> {
    let session_id = payload.session_id;
    let (tx, rx) = mpsc::unbounded_channel::<String>();
    let (disconnect_notifier, disconnect_receiver) = broadcast::channel::<()>(1);

    let db = state.read().expect("Unable to read share state").db.clone();

    let (
        note_search_tool,
        meeting_search_tool,
        web_search_tool,
        email_unread_tool,
        email_search_tool,
        calendar_tool,
        website_view_tool,
        tasks_due_today_tool,
        tasks_scheduled_today_tool,
        memory_tool,
        datetime_tool,
        bash_tool,
        python_tool,
        notify_tool,
        iterate_tool,
        create_loop_tool,
        list_loops_tool,
        delete_loop_tool,
        create_channel_tool,
        emit_channel_tool,
        skill_registry,
        openai_api_hostname,
        openai_api_key,
        openai_model,
        vapid_key_path,
        index_dir_path,
        storage_path_owned,
    ) = {
        let shared_state = state.read().expect("Unable to read share state");
        let AppConfig {
            note_search_api_url,
            storage_path,
            index_path,
            openai_api_hostname,
            openai_api_key,
            openai_model,
            vapid_key_path,
            ..
        } = &shared_state.config;

        let loop_config = LoopSupervisorConfig {
            db: db.clone(),
            api_base_url: note_search_api_url.clone(),
            api_hostname: openai_api_hostname.clone(),
            api_key: openai_api_key.clone(),
            model: openai_model.clone(),
            storage_path: storage_path.clone(),
            vapid_key_path: vapid_key_path.clone(),
        };

        (
            NoteSearchTool::new(note_search_api_url),
            MeetingSearchTool::new(note_search_api_url),
            WebSearchTool::new(note_search_api_url),
            EmailUnreadTool::new(note_search_api_url),
            EmailSearchTool::new(note_search_api_url),
            CalendarTool::new(db.clone(), note_search_api_url),
            WebsiteViewTool::new(storage_path, &session_id),
            TasksDueTodayTool::new(note_search_api_url),
            TasksScheduledTodayTool::new(note_search_api_url),
            MemoryTool::new(storage_path),
            DateTimeTool::new(),
            BashTool::new(storage_path, &session_id),
            PythonTool::new(storage_path, &session_id),
            NotifyTool::new(db.clone(), vapid_key_path),
            IterateTool::new(openai_api_hostname, openai_api_key, openai_model),
            CreateLoopTool::new(loop_config.clone()),
            ListLoopsTool::new(loop_config.clone()),
            DeleteLoopTool::new(loop_config),
            CreateChannelTool::new(db.clone(), &session_id),
            EmitChannelTool::new(db.clone(), &session_id),
            shared_state.skill_registry.clone(),
            openai_api_hostname.clone(),
            openai_api_key.clone(),
            openai_model.clone(),
            vapid_key_path.clone(),
            index_path.clone(),
            storage_path.clone(),
        )
    };

    let all_tools: Vec<BoxedToolCall> = vec![
        Box::new(note_search_tool),
        Box::new(meeting_search_tool),
        Box::new(web_search_tool),
        Box::new(email_unread_tool),
        Box::new(email_search_tool),
        Box::new(calendar_tool),
        Box::new(website_view_tool),
        Box::new(tasks_due_today_tool),
        Box::new(tasks_scheduled_today_tool),
        #[cfg(debug_assertions)]
        Box::new(memory_tool),
        Box::new(datetime_tool),
        Box::new(bash_tool),
        Box::new(python_tool),
        Box::new(notify_tool),
        Box::new(iterate_tool),
        Box::new(create_loop_tool),
        Box::new(list_loops_tool),
        Box::new(delete_loop_tool),
        Box::new(create_channel_tool),
        Box::new(emit_channel_tool),
    ];

    let tools = all_tools;

    // Only images are sent to the model; the upload API already restricts
    // uploads to image types.
    let mut attachment_filenames: Vec<String> = payload
        .attachments
        .iter()
        .filter(|a| {
            a.content_type
                .as_deref()
                .is_none_or(|content_type| content_type.starts_with("image/"))
        })
        .map(|a| a.filename.clone())
        .collect();
    if attachment_filenames.len() > MAX_ATTACHMENTS_PER_MESSAGE {
        tracing::warn!(
            "Truncating {} image attachments to {}",
            attachment_filenames.len(),
            MAX_ATTACHMENTS_PER_MESSAGE
        );
        attachment_filenames.truncate(MAX_ATTACHMENTS_PER_MESSAGE);
    }
    let user_msg = user_message_with_attachments(&payload.message, &attachment_filenames);

    let db = state.read().expect("Unable to read share state").db.clone();

    // Parse message using slash command system
    let slash_cmd = payload.message.parse()?;
    let session = get_or_create_session(&db, &session_id, &[], SessionMode::Chat).await?;
    let current_mode = session.mode;

    // Handle mode transitions
    match (&current_mode, &slash_cmd) {
        (SessionMode::Chat, SlashCommand::Code { prompt }) => {
            // Transition from chat to agent mode
            set_session_mode(&db, &session_id, SessionMode::Code).await?;
            let user_msg = Message::new(Role::User, prompt);
            return handle_agent_mode(state, session_id, user_msg, tx, disconnect_receiver, false)
                .await;
        }
        (SessionMode::Code, SlashCommand::Exit) => {
            // Exit agent mode back to chat
            set_session_mode(&db, &session_id, SessionMode::Chat).await?;

            // Store the user's /exit message
            let user_msg = Message::new(Role::User, "/exit");
            insert_chat_message(&db, &session_id, &user_msg).await?;

            // Store the assistant's exit response
            let exit_response = "Exited agent mode. How else can I help?";
            let assistant_msg = Message::new(Role::Assistant, exit_response);
            insert_chat_message(&db, &session_id, &assistant_msg).await?;

            let _ = tx.send(
                json!({
                    "choices": [{
                        "delta": { "content": exit_response }
                    }]
                })
                .to_string(),
            );

            // Return early to avoid falling through to chat logic
            let sse_stream = StreamExt::map(UnboundedReceiverStream::new(rx), |chunk| {
                Ok::<Event, Infallible>(Event::default().data(chunk))
            });
            let wrapped_sse_stream = DetectDisconnect::new(sse_stream, disconnect_notifier);
            return Ok(Sse::new(wrapped_sse_stream)
                .keep_alive(
                    KeepAlive::default()
                        .text("keep-alive")
                        .interval(Duration::from_millis(100)),
                )
                .into_response());
        }
        (SessionMode::Code, SlashCommand::None(msg)) => {
            // Continue in agent mode
            let user_msg = Message::new(Role::User, msg);
            return handle_agent_mode(state, session_id, user_msg, tx, disconnect_receiver, true)
                .await;
        }
        (SessionMode::Code, SlashCommand::Code { prompt }) => {
            // Already in agent mode but got another /code command
            let user_msg = Message::new(Role::User, prompt);
            return handle_agent_mode(state, session_id, user_msg, tx, disconnect_receiver, true)
                .await;
        }
        (SessionMode::Chat, SlashCommand::Exit) => {
            // Already in chat mode, /exit is a no-op - store messages for consistency
            let exit_msg = "Already in chat mode. How can I help?";
            let _ = tx.send(
                json!({
                    "choices": [{
                        "delta": { "content": exit_msg }
                    }]
                })
                .to_string(),
            );
            // Return early - don't fall through to chat logic
            let sse_stream =
                tokio_stream::StreamExt::map(UnboundedReceiverStream::new(rx), |chunk| {
                    Ok::<Event, Infallible>(Event::default().data(chunk))
                });
            let wrapped_sse_stream = DetectDisconnect::new(sse_stream, disconnect_notifier);
            return Ok(Sse::new(wrapped_sse_stream)
                .keep_alive(
                    KeepAlive::default()
                        .text("keep-alive")
                        .interval(Duration::from_millis(100)),
                )
                .into_response());
        }
        (SessionMode::Code, SlashCommand::Skill { name: _name }) => {
            tracing::warn!(
                "Attempted to activate a skill within coding agent session mode which is not allowed."
            );
        }
        (SessionMode::Code, SlashCommand::Bash { .. }) => {
            tracing::warn!(
                "Attempted to run /bash within coding agent session mode which is not allowed."
            );
        }
        (SessionMode::Chat, SlashCommand::Skill { name }) => {
            // List all skills or show a specific skill's content
            let (response, persist_skill_msg) = if let Some(skill_name) = name {
                let registry = skill_registry
                    .read()
                    .expect("Unable to read skill registry")
                    .clone();

                match registry.load_skill(skill_name).await {
                    Ok(skill) => {
                        let skill_msg = format!(
                            "<skill>\n<name>{name}</name>\n<path>{path}</path>\n{content}</skill>",
                            name = skill_name,
                            path = skill.path.display(),
                            content = skill.full_content(),
                        );

                        (skill_msg, true)
                    }
                    Err(e) => (e.to_string(), false),
                }
            } else {
                let registry = skill_registry
                    .read()
                    .expect("Unable to read skill registry");
                let skills = registry.list_skills();
                if skills.is_empty() {
                    ("No skills available.".to_string(), false)
                } else {
                    let skill_list: Vec<String> = skills
                        .iter()
                        .map(|s| format!("- **{}**: {}", s.name, s.description))
                        .collect();
                    (
                        format!(
                            "Available skills:\n\n{}\n\nUse `/skills <name>` to view a specific skill.",
                            skill_list.join("\n")
                        ),
                        false,
                    )
                }
            };

            if persist_skill_msg {
                let user_msg = Message::new(Role::User, &response);
                insert_chat_message(&db, &session_id, &user_msg).await?;
            }
            let _ = tx.send(
                json!({
                    "choices": [{
                        "delta": { "content": response }
                    }]
                })
                .to_string(),
            );
            // Return early - don't fall through
            let sse_stream =
                tokio_stream::StreamExt::map(UnboundedReceiverStream::new(rx), |chunk| {
                    Ok::<Event, Infallible>(Event::default().data(chunk))
                });
            let wrapped_sse_stream = DetectDisconnect::new(sse_stream, disconnect_notifier);
            return Ok(Sse::new(wrapped_sse_stream)
                .keep_alive(
                    KeepAlive::default()
                        .text("keep-alive")
                        .interval(Duration::from_millis(100)),
                )
                .into_response());
        }
        (SessionMode::Chat, SlashCommand::Bash { command }) => {
            // Run a command in the bashkit sandbox and return the output
            let workspace_path =
                PathBuf::from(format!("{}/workspace/{}", storage_path_owned, session_id));
            let response = match run_in_sandbox(command, &workspace_path).await {
                Ok(output) => {
                    let mut parts = Vec::new();
                    parts.push("```".to_string());
                    parts.push(format!("$ {}\n", command));
                    if !output.stdout.is_empty() {
                        parts.push(output.stdout);
                    }
                    if !output.stderr.is_empty() {
                        parts.push(format!("stderr:\n{}", output.stderr));
                    }
                    parts.push(format!("---\nExit code: {}", output.exit_code));
                    if output.truncated {
                        parts.push("*Output was truncated due to size limits.*".to_string());
                    }
                    parts.push("```".to_string());
                    parts.join("\n")
                }
                Err(e) => format!("```\nError running command: {}\n```", e),
            };
            let _ = tx.send(
                json!({
                    "choices": [{
                        "delta": { "content": response }
                    }]
                })
                .to_string(),
            );
            // Return early - don't fall through
            let sse_stream =
                tokio_stream::StreamExt::map(UnboundedReceiverStream::new(rx), |chunk| {
                    Ok::<Event, Infallible>(Event::default().data(chunk))
                });
            let wrapped_sse_stream = DetectDisconnect::new(sse_stream, disconnect_notifier);
            return Ok(Sse::new(wrapped_sse_stream)
                .keep_alive(
                    KeepAlive::default()
                        .text("keep-alive")
                        .interval(Duration::from_millis(100)),
                )
                .into_response());
        }
        (SessionMode::Chat, SlashCommand::None(_)) => {
            // Continue in chat mode - fall through to existing logic
        }
        (_, SlashCommand::Error(err_msg)) => {
            let _ = tx.send(
                json!({
                    "choices": [{
                        "delta": { "content": err_msg }
                    }]
                })
                .to_string(),
            );
            // Return early - don't fall through
            let sse_stream =
                tokio_stream::StreamExt::map(UnboundedReceiverStream::new(rx), |chunk| {
                    Ok::<Event, Infallible>(Event::default().data(chunk))
                });
            let wrapped_sse_stream = DetectDisconnect::new(sse_stream, disconnect_notifier);
            return Ok(Sse::new(wrapped_sse_stream)
                .keep_alive(
                    KeepAlive::default()
                        .text("keep-alive")
                        .interval(Duration::from_millis(100)),
                )
                .into_response());
        }
        (_, SlashCommand::Help) => {
            // Show help text - same in both modes
            let help_msg = get_help_text();
            let _ = tx.send(
                json!({
                    "choices": [{
                        "delta": { "content": help_msg }
                    }]
                })
                .to_string(),
            );
            // Return early - don't fall through
            let sse_stream =
                tokio_stream::StreamExt::map(UnboundedReceiverStream::new(rx), |chunk| {
                    Ok::<Event, Infallible>(Event::default().data(chunk))
                });
            let wrapped_sse_stream = DetectDisconnect::new(sse_stream, disconnect_notifier);
            return Ok(Sse::new(wrapped_sse_stream)
                .keep_alive(
                    KeepAlive::default()
                        .text("keep-alive")
                        .interval(Duration::from_millis(100)),
                )
                .into_response());
        }
    }

    // Register a resumable stream for this turn. The POST response below tails
    // it from the start; a reconnecting client can attach via
    // GET /api/chat/{id}/stream.
    let streams = state
        .read()
        .expect("Unable to read share state")
        .streams
        .clone();
    let stream = stream::register(&streams, &session_id);

    // Forward model chunks into the resumable stream. This task owns `finish()`
    // so the terminal event can never precede the last buffered chunk (all `tx`
    // clones drop only after the turn task below ends).
    let forward_stream = Arc::clone(&stream);
    let forward_registry = Arc::clone(&streams);
    let forward_session_id = session_id.clone();
    tokio::spawn(async move {
        let mut rx = rx;
        while let Some(chunk) = rx.recv().await {
            forward_stream.publish(chunk);
        }
        forward_stream.finish();

        // Keep the finished stream around briefly so a late reload can still
        // replay the whole turn, then evict it to bound memory. `remove_if_same`
        // guards against deleting a newer turn's stream for the same session.
        tokio::time::sleep(Duration::from_secs(300)).await;
        stream::remove_if_same(&forward_registry, &forward_session_id, &forward_stream);
    });

    // Create session in database if it doesn't already exist

    // Try to fetch the session from the db
    let transcript_with_ids = find_chat_session_by_id(&db, &session_id).await?;
    let mut transcript: Vec<Message> = transcript_with_ids
        .into_iter()
        .map(|(_, msg)| msg)
        .collect();

    // Initialize a new transcript
    if transcript.is_empty() {
        // NOTE: The system message is effectively immutable. Adding
        // new skills and continuing a chat session may cause it to
        // not be found.
        let mut system_content = state
            .read()
            .expect("Unable to read share state")
            .config
            .system_message
            .clone();

        // Append skill instructions if there are skills in the registry
        {
            let registry = skill_registry
                .read()
                .expect("Unable to read skill registry");
            let skill_count = registry.count();
            if skill_count > 0 {
                let skill_names: Vec<String> = registry
                    .list_skills()
                    .iter()
                    .map(|s| format!("{}: {}", s.name, s.description))
                    .collect();

                let skills_section = format!(
                    "\n\n## Available Skills\n\
                    You have access to the following skills. **Always use relevant skills first before using tools.**\n\
                    - {}\n\
                    \nTo use a skill, first load it using the `load_skill` tool. Loaded skills appear in the transcript enclosed in a `<skill>` tag so you can refer to them. You don't need to load a skill more than once. Skills are just instructions, they may reference other tools but you can't call a skill as a tool. Follow loaded skill instructions very carefully.",
                    skill_names.join("\n- ")
                );
                system_content.push_str(&skills_section);
            }
        }

        // Describe channels and loops, including the default timer channels.
        system_content.push_str(&system_prompt_section());

        tracing::debug!("System message: {}", &system_content);

        // Save the system message and add it as the first message in
        // the transcript
        let system_msg = Message::new(Role::System, &system_content);
        insert_chat_message(&db, &session_id, &system_msg).await?;
        transcript.push(system_msg);
    }

    let workspace_path = PathBuf::from(format!("{}/workspace/{}", storage_path_owned, session_id));

    let mut chat = ChatBuilder::new(&openai_api_hostname, &openai_api_key, &openai_model)
        .database(&db, Some(&session_id), None)
        .transcript(transcript)
        .workspace_path(&workspace_path)
        .tools(tools)
        .streaming(tx.clone())
        .middleware(vec![
            Box::new(InfiniteLoopDetector::new(3)),
            Box::new(ToolSecurityMiddleware::default()),
            Box::new(InvisibleCharFilter),
        ]);

    // Add skill management tools if the registry is available
    chat = chat.skills(skill_registry.clone(), &storage_path_owned, &session_id);

    let mut chat = chat.build();

    let notify_stream = Arc::clone(&stream);
    tokio::spawn(async move {
        let result = chat.next_msg(user_msg.clone()).await;

        match result {
            Ok(messages) => {
                // Index new chat messages for full-text search (batch, single writer)
                let db_clone = db.clone();
                let index_dir_path_clone = index_dir_path.clone();
                let session_id_clone = session_id.clone();
                tokio::spawn(async move {
                    if let Err(e) = index_chat_messages(
                        &db_clone,
                        &index_dir_path_clone,
                        &session_id_clone,
                        messages.clone(),
                    )
                    .await
                    {
                        tracing::error!("Failed to index chat messages: {}", e);
                    }
                });

                // Send a notification if nobody is watching the stream
                if notify_stream.subscriber_count() == 0 {
                    tracing::info!("Sending notification!");
                    let db_for_notify = db.clone();
                    let vapid_key_path_for_notify = vapid_key_path.to_string();
                    let session_id_for_notify = session_id.clone();
                    tokio::spawn(async move {
                        let payload = PushNotificationPayload::new(
                            "New chat response",
                            "New response after you disconnected.",
                            Some(&format!("/chat/?session_id={session_id_for_notify}")),
                            None,
                            None,
                        );
                        let subscriptions = find_all_notification_subscriptions(&db_for_notify)
                            .await
                            .unwrap();
                        let failed_subscriptions = broadcast_push_notification(
                            subscriptions,
                            vapid_key_path_for_notify,
                            payload,
                        )
                        .await;
                        for sub in failed_subscriptions {
                            let _ =
                                mark_push_subscription_invalid(&db_for_notify, &sub.endpoint).await;
                        }
                    });
                };
            }
            Err(e) => {
                tracing::error!("Chat handler error: {}. Root cause: {}", e, e.root_cause());

                let err_msg = format!("Something went wrong: {}", e);
                let completion_chunk = json!({
                    "id": "error",
                    "choices": [
                        {
                            "finish_reason": "error",
                            "delta": { "content": err_msg }
                        }
                    ]
                })
                .to_string();
                // Skip sending if client disconnected
                if !tx.is_closed() {
                    let _ = tx.send(completion_chunk);
                }
            }
        }
        Ok::<(), anyhow::Error>(())
    });

    Ok(sse_from_stream(stream, 0))
}

/// Create the chat router
pub fn router() -> Router<SharedState> {
    Router::new()
        .route("/", post(chat_handler))
        .route("/{id}", get(chat_session))
        .route("/{id}/stream", get(chat_stream_handler))
        .route("/sessions", get(chat_list))
}
