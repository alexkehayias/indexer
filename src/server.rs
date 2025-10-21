use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};

use anyhow::Result;
use axum::response::Html;
use tantivy::doc;

use axum::extract::Query;
use axum::{
    extract::{Path, State},
    response::Json,
    routing::{get, post},
    Router,
};
use orgize::ParseConfig;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::task::JoinSet;
use tower_http::cors::CorsLayer;
use tower_http::services::ServeDir;
use tower_http::trace::TraceLayer;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use crate::chat::chat;
use crate::indexing::index_all;
use crate::openai::{BoxedToolCall, Message, Role};
use crate::tool::{NoteSearchTool, SearxSearchTool};

use super::db::vector_db;
use super::git::{diff_last_commit_files, maybe_pull_and_reset_repo};
use super::notification::{send_push_notification, PushSubscription};
use super::search::{search_notes, SearchResult};

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

#[derive(Clone, Debug)]
struct ChatSession {
    #[allow(unused)]
    session_id: String,
    transcript: Vec<Message>,
}

type ChatSessions = HashMap<String, ChatSession>;

pub struct AppConfig {
    pub notes_path: String,
    pub index_path: String,
    pub deploy_key_path: String,
    pub vapid_key_path: String,
    pub note_search_api_url: String,
    pub searxng_api_url: String,
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
    // Stores the push subscriptions
    push_subscriptions: Vec<PushSubscription>,
    db: Mutex<Connection>,
    config: AppConfig,
    chat_sessions: ChatSessions,
}

impl AppState {
    pub fn new(db: Connection, config: AppConfig) -> Self {
        Self {
            latest_selection: None,
            push_subscriptions: Vec::new(),
            db: Mutex::new(db),
            config,
            chat_sessions: HashMap::new(),
        }
    }
}

#[derive(Serialize)]
struct ChatTranscriptResponse {
    transcript: Vec<Message>,
}

async fn chat_session(
    State(state): State<SharedState>,
    // This is the session ID of the chat
    Path(id): Path<String>,
) -> Json<ChatTranscriptResponse> {
    let session = {
        let sessions = state
            .read()
            .expect("Unable to read share state")
            .chat_sessions
            .clone();
        sessions.get(&id).cloned()
    };
    // FIX: Yuck! Figure out how to return a result that maps to a 404
    // or a response.
    if let Some(s) = session {
        Json(ChatTranscriptResponse {
            transcript: s.transcript,
        })
    } else {
        Json(ChatTranscriptResponse {
            transcript: Vec::new(),
        })
    }
}

async fn chat_handler(
    State(state): State<SharedState>,
    Json(payload): Json<ChatRequest>,
) -> Json<ChatResponse> {
    let (note_search_tool, searx_search_tool) = {
        let shared_state = state.read().expect("Unable to read share state");
        let AppConfig {
            note_search_api_url,
            searxng_api_url,
            ..
        } = &shared_state.config;
        (
            NoteSearchTool::new(note_search_api_url),
            SearxSearchTool::new(searxng_api_url),
        )
    };

    let tools: Option<Vec<BoxedToolCall>> = Some(vec![
        Box::new(note_search_tool),
        Box::new(searx_search_tool),
    ]);
    let user_msg = Message::new(Role::User, &payload.message);

    let mut transcript = {
        let mut sessions = state.write().unwrap().chat_sessions.clone();

        let session = sessions
            .entry(payload.session_id.clone())
            .and_modify(|v| v.transcript.push(user_msg.clone()))
            .or_insert(ChatSession {
                session_id: payload.session_id.clone(),
                transcript: vec![Message::new(Role::System, "You are a helpful assistant."), user_msg],
            });

        // Take the entire transcript so we don't hold the lock across .await
        std::mem::take(&mut session.transcript)
    };

    chat(&mut transcript, &tools).await;

    // Re-acquire the lock and write the transcript back into the session
    let assistant_msg = transcript.last().expect("Transcript was empty").clone();

    state
        .write()
        .unwrap()
        .chat_sessions
        .entry(payload.session_id.clone())
        .and_modify(|v| v.transcript = transcript.clone())
        .or_insert(ChatSession {
            session_id: payload.session_id.clone(),
            transcript,
        });

    let resp = ChatResponse::new(&assistant_msg.content.unwrap());

    Json(resp)
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
    query: Option<String>,
    results: Vec<SearchResult>,
}

// Register a client for push notifications
async fn push_subscription(
    State(state): State<SharedState>,
    Json(subscription): Json<PushSubscription>,
) {
    state.write().unwrap().push_subscriptions.push(subscription);
}

async fn search(
    State(state): State<SharedState>,
    Query(params): Query<HashMap<String, String>>,
) -> Json<SearchResponse> {
    let query = params.get("query");
    let shared_state = state.read().unwrap();
    let index_path = &shared_state.config.index_path;
    // Ignoring any previous panics since we are trying to get the
    // db connection and it's probably fine
    let db = shared_state.db.lock().unwrap_or_else(|e| e.into_inner());

    let results = if let Some(query) = query {
        let include_similarity = params.contains_key("include_similarity")
            && params.get("include_similarity").unwrap() == "true";
        search_notes(index_path, &db, include_similarity, query, 20)
    } else {
        Vec::new()
    };

    let resp = SearchResponse {
        query: query.map(|s| s.to_string()),
        results,
    };

    Json(resp)
}

// Build the index for all notes
async fn index_notes(State(state): State<SharedState>) -> Json<Value> {
    tokio::spawn(async move {
        let shared_state = state.read().expect("Unable to read share state");
        let mut db = shared_state
            .db
            .lock()
            // Ignoring any previous panics since we are trying to get the
            // db connection and it's probably fine
            .unwrap_or_else(|e| e.into_inner());

        let AppConfig {
            index_path,
            notes_path,
            deploy_key_path,
            ..
        } = &shared_state.config;

        // Pull the latest from origin
        maybe_pull_and_reset_repo(deploy_key_path, notes_path);

        // Determine which notes changed
        let diff = diff_last_commit_files(deploy_key_path, notes_path);
        // NOTE: This assumes all notes are in one directory at the root
        // of `notes_path`. This will not work if note files are in
        // different directories!
        let paths: Vec<PathBuf> = diff
            .iter()
            .map(|f| PathBuf::from(format!("{}/{}", notes_path, f)))
            .collect();
        let filter_paths = if paths.is_empty() { None } else { Some(paths) };

        // Re-index just the notes that changed
        index_all(&mut db, index_path, notes_path, true, true, filter_paths)
            .expect("Vector indexing failed");
    });

    let resp = json!({
        "success": true,
    });
    Json(resp)
}

// Render a note in org-mode format by ID
// Fetch the contents of the note by ID using the DB
async fn view_note(
    State(state): State<SharedState>,
    // This is the org-id of the note
    Path(id): Path<String>,
) -> Html<String> {
    let shared_state = state.read().expect("Unable to read share state");
    let note_path = &shared_state.config.notes_path;

    let db = shared_state
        .db
        .lock()
        // Ignoring any previous panics since we are trying to get the
        // db connection and it's probably fine
        .unwrap_or_else(|e| e.into_inner());

    let result: Vec<String> = db
        .prepare(
            r"
          SELECT
            id,
            file_name,
            title,
            tags
          FROM note_meta
          WHERE id = ?
          LIMIT 1
        ",
        )
        .expect("Failed to prepare sql statement")
        .query_map([id], |i| Ok(i.get(1).expect("Invalid row returned")))
        .expect("Query failed")
        .collect::<Result<Vec<String>, _>>()
        .expect("Query failed");
    let file_name = result.first();
    if let Some(f) = file_name {
        let file_path = PathBuf::from(format!("{}/{}", note_path, f));
        let content = fs::read_to_string(file_path).expect("Failed to get file content");

        // Render the org-mode content in HTML
        let config = ParseConfig {
            ..Default::default()
        };
        // TODO: replace org-id links with note viewer links
        let output = config.parse(content).to_html();

        Html(output)
    } else {
        // TODO replace with a 404
        Html("".to_string())
    }
}

#[derive(Deserialize)]
struct NotificationPayload {
    message: String,
}

// Endpoint to send push notification to all subscriptions
async fn send_notification(
    State(state): State<SharedState>,
    Json(payload): Json<NotificationPayload>,
) -> Json<Value> {
    // Cloning here to avoid a compile error that a mutex is being held
    // across multiple calls to await. I don't know why this needs to
    // be cloned but :shrug:
    let subscriptions = state
        .read()
        .expect("Unable to read share state")
        .push_subscriptions
        .clone();
    let vapid_key_path = state
        .read()
        .expect("Unable to read share state")
        .config
        .vapid_key_path
        .clone();

    let mut tasks = JoinSet::new();

    for subscription in subscriptions.into_iter() {
        tasks.spawn(send_push_notification(
            vapid_key_path.clone(),
            subscription,
            payload.message.clone(),
        ));
    }

    tasks.join_all().await;

    let resp = json!({
        "success": true,
    });
    Json(resp)
}

pub fn app(app_state: AppState) -> Router {
    let shared_state = SharedState::new(RwLock::new(app_state));
    let cors = CorsLayer::permissive();
    let serve_dir = ServeDir::new("./web-ui/src");

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
        // Static server of assets in ./web-ui
        .fallback_service(serve_dir)
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
    let db = vector_db(&vec_db_path).expect("Failed to connect to db");
    let app_config = AppConfig {
        notes_path,
        index_path,
        deploy_key_path,
        vapid_key_path,
        note_search_api_url,
        searxng_api_url,
    };
    let app_state = AppState::new(db, app_config);
    let app = app(app_state);

    let listener = tokio::net::TcpListener::bind(format!("{}:{}", host, port))
        .await
        .unwrap();

    tracing::debug!(
        "Server started. Listening on {}",
        listener.local_addr().unwrap()
    );

    axum::serve(listener, app).await.unwrap();
}
