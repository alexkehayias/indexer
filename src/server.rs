use std::collections::HashMap;
use std::env;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use axum::extract::Query;
use axum::middleware;
use axum::debug_handler;
use axum::{
    Router,
    extract::{Path, Request, State},
    http::StatusCode,
    response::{Json, Response, IntoResponse},
    routing::{get, post},
};
use http::{HeaderValue, header};
use tokio_rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tantivy::doc;
use tokio::task::JoinSet;
use tower::ServiceBuilder;
use tower_http::cors::CorsLayer;
use tower_http::services::ServeDir;
use tower_http::trace::TraceLayer;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use crate::aql;
use crate::config::AppConfig;
use crate::chat::{chat, find_chat_session_by_id, insert_chat_message};
use crate::indexing::index_all;
use crate::jobs::{
    spawn_periodic_job,
    ProcessEmail,
};
use crate::openai::{BoxedToolCall, Message, Role};
use crate::tool::{NoteSearchTool, SearxSearchTool, EmailUnreadTool};

use super::db::async_db;
use super::git::{diff_last_commit_files, maybe_pull_and_reset_repo};
use super::notification::{PushSubscription, broadcast_push_notification};
use super::search::{SearchResult, search_notes};
use crate::gmail::{Thread, extract_body, fetch_thread, list_unread_messages};
use crate::oauth::refresh_access_token;


// Top level API error
pub struct ApiError(anyhow::Error);

/// Convert `AppError` into an Axum compatible response.
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Something went wrong: {}", self.0),
        )
            .into_response()
    }
}

/// Enables using `?` on functions that return `Result<_,
/// anyhow::Error>` to turn them into `Result<_, AppError>`
impl<E> From<E> for ApiError
where
    E: Into<anyhow::Error>,
{
    fn from(err: E) -> Self {
        Self(err.into())
    }
}

type SharedState = Arc<RwLock<AppState>>;

#[derive(Deserialize)]
struct ChatRequest {
    session_id: String,
    message: String,
}

#[derive(Serialize)]
struct ChatResponse {
    message: String,
}

impl ChatResponse {
    fn new(message: &str) -> Self {
        Self {
            message: message.into(),
        }
    }
}

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
    config: AppConfig
}

impl AppState {
    pub fn new(db: Connection, config: AppConfig) -> Self {
        Self {
            latest_selection: None,
            db,
            config
        }
    }
}

#[derive(Serialize)]
struct ChatTranscriptResponse {
    transcript: Vec<Message>,
}

#[debug_handler]
async fn chat_session(
    State(state): State<SharedState>,
    // This is the session ID of the chat
    Path(id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    let db = state.read().expect("Unable to read share state").db.clone();
    let transcript = find_chat_session_by_id(&db, &id).await?.to_owned();

    if transcript.is_empty() {
        return Ok(
            (
                StatusCode::NOT_FOUND,
                format!("Chat session {} not found", id)
            ).into_response()
        );
    }

    Ok(Json(ChatTranscriptResponse {transcript}).into_response())
}

#[debug_handler]
async fn chat_handler(
    State(state): State<SharedState>,
    Json(payload): Json<ChatRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let (note_search_tool, searx_search_tool, email_unread_tool) = {
        let shared_state = state.read().expect("Unable to read share state");
        let AppConfig {
            note_search_api_url,
            searxng_api_url,
            ..
        } = &shared_state.config;
        (
            NoteSearchTool::new(note_search_api_url),
            SearxSearchTool::new(searxng_api_url),
            EmailUnreadTool::new(note_search_api_url),
        )
    };

    let tools: Option<Vec<BoxedToolCall>> = Some(vec![
        Box::new(note_search_tool),
        Box::new(searx_search_tool),
        Box::new(email_unread_tool),
    ]);
    let user_msg = Message::new(Role::User, &payload.message);
    let mut accum_new: Vec<Message> = vec![];

    let session_id = &payload.session_id;
    let db = state.read().expect("Unable to read share state").db.clone();

    // Try to fetch the session from the db. If it doesn't exist then
    // initialize the transcript with a system message and the user's
    // first message
    let mut transcript = find_chat_session_by_id(&db, session_id).await?;

    // Initialize a new transcript
    if transcript.is_empty() {
        let default_system_msg = Message::new(Role::System, "You are a helpful assistant.");
        transcript.push(default_system_msg.clone());
        accum_new.push(default_system_msg);
    }

    // Add the new message to the transcript
    transcript.push(user_msg.clone());
    accum_new.push(user_msg);

    // Get the next response
    chat(&tools, &mut transcript, &mut accum_new).await;

    // Write new messages that were generated by the chat
    for m in accum_new {
        insert_chat_message(&db, session_id, &m).await?;
    }

    let assistant_msg = transcript.last().expect("Transcript was empty").to_owned();
    let resp = ChatResponse::new(&assistant_msg.content.unwrap());

    Ok(Json(resp).into_response())
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

#[derive(Serialize)]
struct SearchResponse {
    raw_query: String,
    parsed_query: String,
    results: Vec<SearchResult>,
}


#[derive(Deserialize)]
pub struct PushSubscriptionRequest {
    pub endpoint: String,
    pub keys: HashMap<String, String>,
}

// Register a client for push notifications
#[debug_handler]
async fn push_subscription(
    State(state): State<SharedState>,
    Json(subscription): Json<PushSubscriptionRequest>,
) -> Result<Json<Value>, ApiError> {
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
            subscription_stmt.execute(rusqlite::params![
                subscription.endpoint,
                p256dh,
                auth,
            ])?;
            conn.execute("DELETE FROM vec_items", [])?;
            Ok(())
        }).await?;
    }

    Ok(Json(json!({"success": true})))
}

#[axum::debug_handler]
async fn search(
    State(state): State<SharedState>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<SearchResponse>, ApiError> {
    let raw_query = params.get("query").expect("Missing query param");
    let query = aql::parse_query(raw_query).expect("Parsing AQL failed");
    let (db, index_path) = {
        let shared_state = state.read().unwrap();
        (shared_state.db.clone(), shared_state.config.index_path.clone())
    };

    let include_similarity = params.contains_key("include_similarity")
        && params.get("include_similarity").unwrap() == "true";
    let results = search_notes(&index_path, &db, include_similarity, &query, 20)
        .await?;

    let resp = SearchResponse {
        raw_query: raw_query.to_string(),
        parsed_query: format!("{:?}", query),
        results,
    };

    Ok(Json(resp))
}

// Build the index for all notes
async fn index_notes(State(state): State<SharedState>) -> Result<Json<Value>, ApiError> {
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
        let paths: Vec<PathBuf> = diff.iter().map(|f| PathBuf::from(format!("{}/{}", &notes_path, f))).collect();
        let filter_paths = if paths.is_empty() { None } else { Some(paths) };
        index_all(&a_db, &index_path, &notes_path, true, true, filter_paths).await.unwrap();
    });
    Ok(Json(json!({ "success": true })))
}

#[derive(Serialize)]
struct ViewNoteResult {
    id: String,
    title: String,
    body: String,
    tags: Option<String>,
}

// Render a note in org-mode format by ID
// Fetch the contents of the note by ID using the DB
async fn view_note(
    State(state): State<SharedState>,
    // This is the org-id of the note
    Path(id): Path<String>,
) -> Result<Json<ViewNoteResult>, ApiError> {
    let db = state.read().unwrap().db.clone();

    let note_result = db.call(move |conn| {
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
                Ok(ViewNoteResult {
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
    }).await?;

    Ok(Json(note_result))
}

#[derive(Deserialize)]
struct NotificationPayload {
    message: String,
}

// Endpoint to send push notification to all subscriptions
async fn send_notification(
    State(state): State<SharedState>,
    Json(payload): Json<NotificationPayload>,
) -> Result<Json<Value>, ApiError> {
    let vapid_key_path = state
        .read()
        .expect("Unable to read share state")
        .config
        .vapid_key_path
        .clone();

    let subscriptions = {
        let db = state.read().unwrap().db.clone();
        db.call(move |conn| {
            let mut stmt = conn.prepare(
                "SELECT endpoint, p256dh, auth FROM push_subscription"
            )?;
            let result = stmt.query_map([], |i| {
                Ok(PushSubscription {
                    endpoint: i.get(0)?,
                    p256dh: i.get(1)?,
                    auth: i.get(2)?,
                })
            })?
                .filter_map(Result::ok)
                .collect::<Vec<_>>();
            Ok(result)
        }).await?
    };

    broadcast_push_notification(
        subscriptions,
        vapid_key_path,
        payload.message.clone(),
    ).await;

    Ok(Json(json!({ "success": true })))
}

#[derive(Deserialize)]
pub struct EmailUnreadQuery {
    email: String,
    limit: Option<i64>,
}

#[derive(Clone, Serialize)]
pub struct EmailMessage {
    id: String,
    thread_id: String,
    from: String,
    to: String,
    received: String,
    subject: String,
    body: String,
}

#[derive(Clone, Serialize)]
pub struct EmailThread {
    id: String,
    received: String,
    from: String,
    to: String,
    subject: String,
    messages: Vec<EmailMessage>,
}

async fn email_unread_handler(
    State(state): State<SharedState>,
    Query(params): Query<EmailUnreadQuery>,
) -> Result<Json<Value>, ApiError> {
    let refresh_token: String = {
        let db = state.read().unwrap().db.clone();

        db.call(move |conn| {
            let result = conn.prepare("SELECT refresh_token FROM auth WHERE id = ?1")
                .and_then(|mut stmt| stmt.query_row([&params.email], |row| row.get(0)))?;
            Ok(result)
        }).await?
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
    let mut threads: Vec<EmailThread> = Vec::new();
    for t in results {
        let mut messages: Vec<EmailMessage> = Vec::new();
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

            messages.push(EmailMessage {
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

        threads.push(EmailThread {
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
        // Storage for push subscriptions
        .route("/push/subscribe", post(push_subscription))
        .route("/push/notification", post(send_notification))
        // Get a list of unread emails
        .route("/email/unread", get(email_unread_handler))
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

    let db = async_db(&vec_db_path).await.expect("Failed to connect to async db");

    let app_config = AppConfig {
        notes_path,
        index_path,
        deploy_key_path,
        vapid_key_path,
        note_search_api_url: note_search_api_url.clone(),
        searxng_api_url,
        gmail_api_client_id,
        gmail_api_client_secret,
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
    // spawn_periodic_job(app_config, db, ProcessEmail);

    axum::serve(listener, app).await.unwrap();
}
