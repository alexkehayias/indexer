use std::env;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use axum::extract::Query;
use axum::middleware;
use axum::{
    Router,
    extract::{Path, Request, State},
    http::StatusCode,
    response::sse::{Event, KeepAlive, Sse},
    response::{IntoResponse, Json, Response},
    routing::{get, post},
};
use http::{HeaderValue, header};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{convert::Infallible, time::Duration};
use tantivy::doc;
use tokio::sync::{broadcast, mpsc};

use tokio::task::JoinSet;
use tokio_rusqlite::Connection;
use tokio_stream::StreamExt as _;
use tokio_stream::wrappers::UnboundedReceiverStream;
use tower::ServiceBuilder;
use tower_http::cors::CorsLayer;
use tower_http::services::ServeDir;
use tower_http::trace::TraceLayer;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use crate::aql;
use crate::chat::{
    chat_session_count, chat_session_list, chat_stream, find_chat_session_by_id,
    get_or_create_session, insert_chat_message,
};
use crate::config::AppConfig;
use crate::gcal::list_events;
use crate::indexing::index_all;
use crate::jobs::{GenerateSessionTitles, ResearchMeetingAttendees, spawn_periodic_job};
use crate::notification::find_all_notification_subscriptions;
use crate::openai::{BoxedToolCall, Message, Role};
use crate::public::{self};
use crate::tools::{
    CalendarTool, EmailUnreadTool, NoteSearchTool, SearxSearchTool, WebsiteViewTool,
};
use crate::utils::DetectDisconnect;

use super::db::async_db;
use super::git::{diff_last_commit_files, maybe_pull_and_reset_repo};
use super::notification::{PushNotificationPayload, PushSubscription, broadcast_push_notification};
use super::search::search_notes;
use crate::gmail::{Thread, extract_body, fetch_thread, list_unread_messages};
use crate::oauth::refresh_access_token;

type SharedState = Arc<RwLock<AppState>>;

#[derive(Debug, Deserialize)]
struct LastSelection {
    id: String,
    title: String,
    file_name: String,
}

pub struct AppState {
    // Stores the latest search hit selected by the user
    latest_selection: Option<LastSelection>,
    db: Connection,
    config: AppConfig,
}

impl AppState {
    pub fn new(db: Connection, config: AppConfig) -> Self {
        Self {
            latest_selection: None,
            db,
            config,
        }
    }
}

async fn chat_session(
    State(state): State<SharedState>,
    // This is the session ID of the chat
    Path(id): Path<String>,
) -> Result<impl IntoResponse, public::ApiError> {
    let db = state.read().expect("Unable to read share state").db.clone();
    let transcript = find_chat_session_by_id(&db, &id).await?.to_owned();

    if transcript.is_empty() {
        return Ok((
            StatusCode::NOT_FOUND,
            format!("Chat session {} not found", id),
        )
            .into_response());
    }

    Ok(Json(public::ChatTranscriptResponse { transcript }).into_response())
}

/// Get a list of all chat sessions
async fn chat_list(
    State(state): State<SharedState>,
    Query(params): Query<public::ChatSessionsQuery>,
) -> Result<Json<public::ChatSessionsResponse>, public::ApiError> {
    let db = state.read().expect("Unable to read share state").db.clone();
    let page = params.page.unwrap_or(1);
    let limit = params.limit.unwrap_or(20);
    let offset = (page - 1) * limit;
    let tags = params.tags.unwrap_or(vec![]);
    let total_sessions = chat_session_count(&db, &tags).await?;
    let sessions = chat_session_list(&db, &tags, limit, offset).await?;
    let total_pages = (total_sessions as f64 / limit as f64).ceil() as i64;

    Ok(Json(public::ChatSessionsResponse {
        sessions,
        page,
        limit,
        total_sessions,
        total_pages,
    }))
}

/// Initiate or add to a chat session and stream the response using
/// OpenAI's API streaming scheme using server sent events (SSE). The
/// user's message and the assistant's message(s) are stored in the
/// database.
///
/// If the user disconnects before the streaming response completes,
/// the result is still processed and the messages are stored in the
/// database.
///
/// If inference fails, neither the user or assistant's
/// messages are stored in the DB. This is on purpose so the user can
/// try again while keeping the transcript clean.
async fn chat_handler(
    State(state): State<SharedState>,
    Json(payload): Json<public::ChatRequest>,
) -> Result<impl IntoResponse, public::ApiError> {
    let session_id = payload.session_id;
    let (tx, rx) = mpsc::unbounded_channel::<String>();

    let sse_stream = UnboundedReceiverStream::new(rx)
        .map(|chunk| Ok::<Event, Infallible>(Event::default().data(chunk)));
    let (disconnect_notifier, mut disconnect_receiver) = broadcast::channel::<()>(1);
    let wrapped_sse_stream = DetectDisconnect::new(sse_stream, disconnect_notifier);

    let (
        note_search_tool,
        searx_search_tool,
        email_unread_tool,
        calendar_tool,
        website_view_tool,
        openai_api_hostname,
        openai_api_key,
        openai_model,
        vapid_key_path,
    ) = {
        let shared_state = state.read().expect("Unable to read share state");
        let AppConfig {
            note_search_api_url,
            searxng_api_url,
            openai_api_hostname,
            openai_api_key,
            openai_model,
            vapid_key_path,
            ..
        } = &shared_state.config;
        (
            NoteSearchTool::new(note_search_api_url),
            SearxSearchTool::new(searxng_api_url),
            EmailUnreadTool::new(note_search_api_url),
            CalendarTool::new(note_search_api_url),
            WebsiteViewTool::new(),
            openai_api_hostname.clone(),
            openai_api_key.clone(),
            openai_model.clone(),
            vapid_key_path.clone(),
        )
    };

    let tools: Option<Vec<BoxedToolCall>> = Some(vec![
        Box::new(note_search_tool),
        Box::new(searx_search_tool),
        Box::new(email_unread_tool),
        Box::new(calendar_tool),
        Box::new(website_view_tool),
    ]);
    let user_msg = Message::new(Role::User, &payload.message);

    let db = state.read().expect("Unable to read share state").db.clone();

    // Create session in database if it doesn't already exist
    get_or_create_session(&db, &session_id, &[]).await?;

    // Try to fetch the session from the db. If it doesn't exist then
    // initialize the transcript with a system message and the user's
    // first message
    let mut transcript = find_chat_session_by_id(&db, &session_id).await?;

    // Initialize a new transcript
    if transcript.is_empty() {
        let shared_state = state.read().expect("Unable to read share state");
        let default_system_msg = Message::new(Role::System, &shared_state.config.system_message);
        transcript.push(default_system_msg.clone());
    }

    // Add the new message to the transcript
    transcript.push(user_msg.clone());

    // Get the next response
    tokio::spawn(async move {
        let result = chat_stream(
            tx.clone(),
            &tools,
            &transcript,
            &openai_api_hostname,
            &openai_api_key,
            &openai_model,
        )
        .await;

        match result {
            Ok(messages) => {
                // Write the user's message to the DB
                insert_chat_message(&db, &session_id, &user_msg).await?;
                // Write new messages that were generated by the chat
                for m in messages {
                    insert_chat_message(&db, &session_id, &m).await?;
                }
                // Send a notification if the client disconnected
                // before the response completed.
                // Handles the following scenarios:
                // - [no notification] The SSE stream completes and
                //   the client is still there
                // - [notification] The SSE stream completes but the
                //   client is no longer there
                if tx.is_closed() {
                    // The client disconnects when the SSE stream
                    // completes OR if the user closes their browser
                    let _ = disconnect_receiver.recv().await.map(async |()| {
                        tracing::info!("Sending notification!");
                        // Broadcast push notification to all subscribers, using a new read lock for DB/config each time
                        let payload = PushNotificationPayload::new(
                            "New chat response",
                            "New response after you disconnected.",
                            Some(&format!("/chat/?session_id={session_id}")),
                            None,
                            None,
                        );
                        let subscriptions = find_all_notification_subscriptions(&db).await.unwrap();
                        broadcast_push_notification(subscriptions, vapid_key_path.to_string(), payload).await;
                    })?.await;
                };
            }
            Err(e) => {
                tracing::error!("Chat handler error: {}. Root cause: {}", e, e.root_cause());

                // Stream back a default message as a response letting
                // the user know what happened
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
                tx.send(completion_chunk)?;
            }
        }

        Ok::<(), anyhow::Error>(())
    });

    let resp = Sse::new(wrapped_sse_stream)
        .keep_alive(
            KeepAlive::default()
                .text("keep-alive")
                .interval(Duration::from_millis(100)),
        )
        .into_response();

    Ok(resp)
}

async fn kv_get(State(state): State<SharedState>) -> Json<Option<Value>> {
    if let Some(LastSelection {
        id,
        file_name,
        title,
    }) = &state.read().unwrap().latest_selection
    {
        let resp = json!({
            "id": id,
            "file_name": file_name,
            "title": title,
        });
        Json(Some(resp))
    } else {
        Json(None)
    }
}

async fn kv_set(State(state): State<SharedState>, Json(data): Json<LastSelection>) {
    state.write().unwrap().latest_selection = Some(LastSelection {
        id: data.id,
        file_name: data.file_name,
        title: data.title,
    });
}

// Register a client for push notifications
async fn push_subscription(
    State(state): State<SharedState>,
    Json(subscription): Json<public::PushSubscriptionRequest>,
) -> Result<Json<Value>, public::ApiError> {
    let p256dh = subscription
        .keys
        .get("p256dh")
        .expect("Missing p256dh key")
        .clone();
    let auth = subscription
        .keys
        .get("auth")
        .expect("Missing auth key")
        .clone();

    {
        let db = state.read().unwrap().db.clone();
        db.call(move |conn| {
            let mut subscription_stmt = conn.prepare(
                "REPLACE INTO push_subscription(endpoint, p256dh, auth) VALUES (?, ?, ?)",
            )?;
            subscription_stmt.execute(tokio_rusqlite::params![
                subscription.endpoint,
                p256dh,
                auth,
            ])?;
            conn.execute("DELETE FROM vec_items", [])?;
            Ok(())
        })
        .await?;
    }

    Ok(Json(json!({"success": true})))
}

async fn search(
    State(state): State<SharedState>,
    Query(params): Query<public::SearchRequest>,
) -> Result<Json<public::SearchResponse>, public::ApiError> {
    let raw_query = params.query;
    let query = aql::parse_query(&raw_query).expect("Parsing AQL failed");
    let (db, index_path) = {
        let shared_state = state.read().unwrap();
        (
            shared_state.db.clone(),
            shared_state.config.index_path.clone(),
        )
    };

    let results = search_notes(
        &index_path,
        &db,
        params.include_similarity,
        params.truncate,
        &query,
        params.limit,
    )
    .await?;

    let resp = public::SearchResponse {
        raw_query: raw_query.to_string(),
        parsed_query: format!("{:?}", query),
        results,
    };

    Ok(Json(resp))
}

// Build the index for all notes
async fn index_notes(State(state): State<SharedState>) -> Result<Json<Value>, public::ApiError> {
    // Clone all of these vars so we don't pass data across await
    // calls below
    let (a_db, index_path, notes_path, deploy_key_path) = {
        let shared_state = state.read().expect("Unable to read share state");
        (
            shared_state.db.clone(),
            shared_state.config.index_path.clone(),
            shared_state.config.notes_path.clone(),
            shared_state.config.deploy_key_path.clone(),
        )
    };
    tokio::spawn(async move {
        maybe_pull_and_reset_repo(&deploy_key_path, &notes_path);
        let diff = diff_last_commit_files(&deploy_key_path, &notes_path);
        let paths: Vec<PathBuf> = diff
            .iter()
            .map(|f| PathBuf::from(format!("{}/{}", &notes_path, f)))
            .collect();
        let filter_paths = if paths.is_empty() { None } else { Some(paths) };
        index_all(&a_db, &index_path, &notes_path, true, true, filter_paths)
            .await
            .unwrap();
    });
    Ok(Json(json!({ "success": true })))
}

// Render a note in org-mode format by ID
// Fetch the contents of the note by ID using the DB
async fn view_note(
    State(state): State<SharedState>,
    // This is the org-id of the note
    Path(id): Path<String>,
) -> Result<Json<public::ViewNoteResponse>, public::ApiError> {
    let db = state.read().unwrap().db.clone();

    let note_result = db
        .call(move |conn| {
            let result = conn
                .prepare(
                    r"
          SELECT
            id,
            title,
            body,
            tags
          FROM note_meta
          WHERE id = ?
          LIMIT 1
        ",
                )
                .expect("Failed to prepare sql statement")
                .query_map([id], |i| {
                    Ok(public::ViewNoteResponse {
                        id: i.get(0)?,
                        title: i.get(1)?,
                        body: i.get(2)?,
                        tags: i.get(3)?,
                    })
                })
                // lol wat?
                .unwrap()
                .last()
                .unwrap()
                .unwrap();
            Ok(result)
        })
        .await?;

    Ok(Json(note_result))
}

// Endpoint to send push notification to all subscriptions
async fn send_notification(
    State(state): State<SharedState>,
    Json(payload): Json<public::NotificationRequest>,
) -> Result<Json<Value>, public::ApiError> {
    let vapid_key_path = state
        .read()
        .expect("Unable to read share state")
        .config
        .vapid_key_path
        .clone();

    let subscriptions = {
        let db = state.read().unwrap().db.clone();
        db.call(move |conn| {
            let mut stmt = conn.prepare("SELECT endpoint, p256dh, auth FROM push_subscription")?;
            let result = stmt
                .query_map([], |i| {
                    Ok(PushSubscription {
                        endpoint: i.get(0)?,
                        p256dh: i.get(1)?,
                        auth: i.get(2)?,
                    })
                })?
                .filter_map(Result::ok)
                .collect::<Vec<_>>();
            Ok(result)
        })
        .await?
    };

    let payload = PushNotificationPayload::new(
        "Notification",
        &payload.message,
        None,
        None,
        Some("indexer_updated"),
    );
    broadcast_push_notification(subscriptions, vapid_key_path, payload).await;

    Ok(Json(json!({ "success": true })))
}

async fn email_unread_handler(
    State(state): State<SharedState>,
    Query(params): Query<public::EmailUnreadQuery>,
) -> Result<Json<Value>, public::ApiError> {
    let refresh_token: String = {
        let db = state.read().unwrap().db.clone();

        db.call(move |conn| {
            let result = conn
                .prepare("SELECT refresh_token FROM auth WHERE id = ?1")
                .and_then(|mut stmt| stmt.query_row([&params.email], |row| row.get(0)))?;
            Ok(result)
        })
        .await?
    };

    // Pull the config values out before the async call so that we
    // don't get an error for holding the lock across awaits.
    let (client_id, client_secret) = {
        let shared_state = state.read().expect("Unable to read share state");
        let AppConfig {
            gmail_api_client_id,
            gmail_api_client_secret,
            ..
        } = &shared_state.config;
        (gmail_api_client_id.clone(), gmail_api_client_secret.clone())
    };
    let oauth = refresh_access_token(&client_id, &client_secret, &refresh_token).await?;
    let access_token = oauth.access_token;
    let limit = params.limit.unwrap_or(7); // Default 7 days if not specified

    // Query Gmail for unread messages
    let messages = list_unread_messages(&access_token, limit).await?;

    // Fetch each thread concurrently
    let mut tasks = JoinSet::new();
    for message in messages.into_iter() {
        let access_token = access_token.clone();
        let thread_id = message.thread_id;
        tasks.spawn(fetch_thread(access_token, thread_id));
    }
    let results: Vec<Thread> = tasks
        .join_all()
        .await
        .into_iter()
        .map(|i| i.unwrap())
        .collect();

    // Transform the threads and messages into a simpler format
    let mut threads: Vec<public::EmailThread> = Vec::new();
    for t in results {
        let mut messages: Vec<public::EmailMessage> = Vec::new();
        for m in t.messages {
            let body = extract_body(&m);
            if body == "Failed to decode" {
                tracing::error!("Decode error: {:?}", m.payload);
            }
            let payload = m.payload.unwrap();
            let headers = payload.headers.unwrap();

            // Each of these headers are required to be here or it's not a valid email
            let from = headers
                .iter()
                .find(|h| h.name == "From")
                .map(|h| h.value.clone())
                .unwrap();
            let to = headers
                .iter()
                .find(|h| h.name == "To")
                .map(|h| h.value.clone())
                .unwrap();
            let subject = headers
                .iter()
                .find(|h| h.name == "Subject")
                .map(|h| h.value.clone())
                .unwrap();

            messages.push(public::EmailMessage {
                id: m.id,
                thread_id: m.thread_id,
                received: m.internal_date,
                from,
                to,
                subject,
                body,
            })
        }

        // It's guaranteed there is at least one message per thread
        let latest_msg = messages[0].clone();

        threads.push(public::EmailThread {
            id: t.id,
            received: latest_msg.received,
            subject: latest_msg.subject,
            from: latest_msg.from,
            to: latest_msg.to,
            messages,
        });
    }

    // Order of threads isn't guaranteed because we fetch them
    // concurrently
    threads.sort_by_key(|i| std::cmp::Reverse(i.received.clone()));

    Ok(Json(json!(threads)))
}

async fn calendar_handler(
    State(state): State<SharedState>,
    Query(params): Query<public::CalendarQuery>,
) -> Result<Json<Vec<public::CalendarResponse>>, public::ApiError> {
    let refresh_token: String = {
        let db = state.read().unwrap().db.clone();

        db.call(move |conn| {
            let result = conn
                .prepare("SELECT refresh_token FROM auth WHERE id = ?1")
                .and_then(|mut stmt| stmt.query_row([&params.email], |row| row.get(0)))?;
            Ok(result)
        })
        .await?
    };

    // Pull the config values out before the async call so that we
    // don't get an error for holding the lock across awaits.
    let (client_id, client_secret) = {
        let shared_state = state.read().expect("Unable to read share state");
        let AppConfig {
            gmail_api_client_id,
            gmail_api_client_secret,
            ..
        } = &shared_state.config;
        (gmail_api_client_id.clone(), gmail_api_client_secret.clone())
    };
    let oauth = refresh_access_token(&client_id, &client_secret, &refresh_token).await?;
    let access_token = oauth.access_token;

    // Default to 7 days ahead if not specified
    let days_ahead = params.days_ahead.unwrap_or(7);

    // Default to primary calendar if not specified
    let calendar_id = params
        .calendar_id
        .clone()
        .unwrap_or_else(|| "primary".to_string());

    // Get the current time and calculate the end time
    let now = chrono::Utc::now();
    let end_time = now + chrono::Duration::days(days_ahead);

    // Fetch upcoming events
    let events = list_events(&access_token, &calendar_id, now, end_time).await?;

    // Transform events to a simpler format for the API response
    let resp = events
        .into_iter()
        .map(|event| {
            let summary = event.summary.unwrap_or_else(|| "No title".to_string());
            public::CalendarResponse {
                id: event.id,
                summary,
                start: event.start.to_rfc3339(),
                end: event.end.to_rfc3339(),
                attendees: event.attendees.map(|attendees| {
                    attendees
                        .into_iter()
                        .map(|attendee| public::CalendarAttendee {
                            email: attendee.email,
                            display_name: attendee.display_name,
                        })
                        .collect::<Vec<_>>()
                }),
            }
        })
        .collect();

    Ok(Json(resp))
}

async fn sse_handler() -> impl IntoResponse {
    let (tx, rx) = mpsc::unbounded_channel::<String>();

    let sse_stream = UnboundedReceiverStream::new(rx)
        .map(|chunk| Ok::<Event, Infallible>(Event::default().data(chunk)));

    tx.send(String::from("Testing")).unwrap();

    Sse::new(sse_stream)
        .keep_alive(
            KeepAlive::default()
                .text("keep-alive")
                .interval(Duration::from_millis(100)),
        )
        .into_response()
}

async fn set_static_cache_control(request: Request, next: middleware::Next) -> Response {
    let mut response = next.run(request).await;
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    response
}

pub fn app(shared_state: Arc<RwLock<AppState>>) -> Router {
    let cors = CorsLayer::permissive();

    Router::new()
        // Search API endpoint
        .route("/notes/search", get(search))
        // Storage for selected search hits
        .route("/notes/search/latest", get(kv_get).post(kv_set))
        // Index content endpoint
        .route("/notes/index", post(index_notes))
        // View a specific note
        .route("/notes/{id}/view", get(view_note))
        // Chat with notes
        .route("/notes/chat", post(chat_handler))
        // Retrieve a past chat session
        .route("/notes/chat/{id}", get(chat_session))
        // Get list of chat sessions
        .route("/notes/chat/sessions", get(chat_list))
        // Storage for push subscriptions
        .route("/push/subscribe", post(push_subscription))
        .route("/push/notification", post(send_notification))
        // Get a list of unread emails
        .route("/email/unread", get(email_unread_handler))
        // Get list of calender events
        .route("/calendar", get(calendar_handler))
        // Server sent events (SSE) example
        .route("/sse", get(sse_handler))
        // Static server of assets in ./web-ui
        .fallback_service(
            ServiceBuilder::new()
                .layer(middleware::from_fn(set_static_cache_control))
                .service(
                    ServeDir::new("./web-ui/src")
                        .precompressed_br()
                        .precompressed_gzip(),
                ),
        )
        .layer(TraceLayer::new_for_http())
        .layer(cors)
        .with_state(Arc::clone(&shared_state))
}

// Run the server
#[allow(clippy::too_many_arguments)]
pub async fn serve(
    host: String,
    port: String,
    notes_path: String,
    index_path: String,
    vec_db_path: String,
    deploy_key_path: String,
    vapid_key_path: String,
    note_search_api_url: String,
    searxng_api_url: String,
    gmail_api_client_id: String,
    gmail_api_client_secret: String,
    calendar_email: Option<String>,
) {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                // axum logs rejections from built-in extractors with the `axum::rejection`
                // target, at `TRACE` level. `axum::rejection=trace` enables showing those events
                format! {
                    "{}=debug,tower_http=debug,axum::rejection=trace",
                    env!("CARGO_CRATE_NAME")
                }
                .into()
            }),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let db = async_db(&vec_db_path)
        .await
        .expect("Failed to connect to async db");

    // Get OpenAI API configuration from environment variables or use defaults
    let openai_api_hostname =
        env::var("INDEXER_LOCAL_LLM_HOST").unwrap_or_else(|_| "https://api.openai.com".to_string());
    let openai_api_key =
        env::var("OPENAI_API_KEY").unwrap_or_else(|_| "thiswontworkforopenai".to_string());
    let openai_model =
        env::var("INDEXER_LOCAL_LLM_MODEL").unwrap_or_else(|_| "gpt-4.1-mini".to_string());

    let system_message = env::var("INDEXER_SYSTEM_MESSAGE")
        .unwrap_or_else(|_| "You are a helpful assistant.".to_string());

    let app_config = AppConfig {
        notes_path,
        index_path,
        deploy_key_path,
        vapid_key_path,
        note_search_api_url: note_search_api_url.clone(),
        searxng_api_url,
        gmail_api_client_id,
        gmail_api_client_secret,
        openai_api_hostname,
        openai_api_key,
        openai_model,
        system_message,
        calendar_email,
    };
    let app_state = AppState::new(db.clone(), app_config.clone());
    let shared_state = Arc::new(RwLock::new(app_state));
    let app = app(Arc::clone(&shared_state));

    let listener = tokio::net::TcpListener::bind(format!("{}:{}", host, port))
        .await
        .unwrap();

    tracing::debug!(
        "Server started. Listening on {}",
        listener.local_addr().unwrap()
    );

    // Run background jobs. Each job is spawned in it's own tokio task
    // in a loop.
    spawn_periodic_job(app_config.clone(), db.clone(), ResearchMeetingAttendees);
    spawn_periodic_job(app_config, db, GenerateSessionTitles);

    axum::serve(listener, app).await.unwrap();
}
