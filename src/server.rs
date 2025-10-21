use std::collections::HashMap;
use std::env;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};

use axum::extract::Query;
use axum::middleware;
use axum::{
    Router,
    extract::{Path, Request, State},
    response::{Json, Response},
    routing::{get, post},
};
use http::{HeaderValue, header};
use rusqlite::Connection;
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
use crate::chat::chat;
use crate::indexing::index_all;
use crate::openai::{BoxedToolCall, Message, Role};
use crate::tool::{NoteSearchTool, SearxSearchTool, EmailUnreadTool};

use super::db::vector_db;
use super::git::{diff_last_commit_files, maybe_pull_and_reset_repo};
use super::notification::{PushSubscription, send_push_notification};
use super::search::{SearchResult, search_notes};
use crate::gmail::{Thread, extract_body, fetch_thread, list_unread_messages};
use crate::oauth::refresh_access_token;

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
    pub gmail_api_client_id: String,
    pub gmail_api_client_secret: String,
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
    db: Mutex<Connection>,
    config: AppConfig,
    chat_sessions: ChatSessions,
}

impl AppState {
    pub fn new(db: Connection, config: AppConfig) -> Self {
        Self {
            latest_selection: None,
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

    let mut transcript = {
        let mut sessions = state.write().unwrap().chat_sessions.clone();

        let session = sessions
            .entry(payload.session_id.clone())
            .and_modify(|v| v.transcript.push(user_msg.clone()))
            .or_insert(ChatSession {
                session_id: payload.session_id.clone(),
                transcript: vec![
                    Message::new(Role::System, "You are a helpful assistant."),
                    user_msg,
                ],
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
async fn push_subscription(
    State(state): State<SharedState>,
    Json(subscription): Json<PushSubscriptionRequest>,
) {
    let shared_state = state.read().unwrap();
    let db = shared_state.db.lock().unwrap_or_else(|e| e.into_inner());

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

    let mut subscription_stmt = db.prepare(
        "REPLACE INTO push_subscription(endpoint, p256dh, auth) VALUES (?, ?, ?)",
    ).unwrap();
    subscription_stmt
        .execute(rusqlite::params![
            subscription.endpoint,
            p256dh,
            auth,
        ])
        .expect("Note meta upsert failed");
    db.execute("DELETE FROM vec_items", [])
        .expect("Failed to delete vec_items data");
}

async fn search(
    State(state): State<SharedState>,
    Query(params): Query<HashMap<String, String>>,
) -> Json<SearchResponse> {
    let raw_query = params.get("query").expect("Missing query param");
    let query = aql::parse_query(raw_query).expect("Parsing AQL failed");
    let shared_state = state.read().unwrap();
    let index_path = &shared_state.config.index_path;
    // Ignoring any previous panics since we are trying to get the
    // db connection and it's probably fine
    let db = shared_state.db.lock().unwrap_or_else(|e| e.into_inner());

    let include_similarity = params.contains_key("include_similarity")
        && params.get("include_similarity").unwrap() == "true";
    let results = search_notes(index_path, &db, include_similarity, &query, 20);

    let resp = SearchResponse {
        raw_query: raw_query.to_string(),
        parsed_query: format!("{:?}", query),
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
) -> Json<ViewNoteResult> {
    let shared_state = state.read().expect("Unable to read share state");

    let db = shared_state.db.lock().unwrap_or_else(|e| e.into_inner());

    let result = db
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

    Json(result)
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

    let vapid_key_path = state
        .read()
        .expect("Unable to read share state")
        .config
        .vapid_key_path
        .clone();

    let subscriptions = {
        let shared_state = state.read().expect("Unable to read share state");
        let db = shared_state.db.lock().unwrap_or_else(|e| e.into_inner());

        let mut stmt = db
            .prepare(
                r"
          SELECT
            endpoint,
            p256dh,
            auth
          FROM push_subscription
        ")
        .expect("Failed to prepare sql statement");

        let results = stmt.query_map([], |i| {
            Ok(PushSubscription {
                endpoint: i.get(0)?,
                p256dh: i.get(1)?,
                auth: i.get(2)?,
            })
        }).unwrap();

        let mut subs: Vec<PushSubscription> = Vec::new();
        for s in results {
            subs.push(s.unwrap());
        }
        subs
    };

    let mut tasks = JoinSet::new();
    for sub in subscriptions {
        tasks.spawn(send_push_notification(
            vapid_key_path.clone(),
            sub.endpoint,
            sub.p256dh,
            sub.auth,
            payload.message.clone(),
        ));
    }
    tasks.join_all().await;

    let resp = json!({
        "success": true,
    });
    Json(resp)
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

pub async fn email_unread_handler(
    State(state): State<SharedState>,
    Query(params): Query<EmailUnreadQuery>,
) -> Json<Value> {
    let refresh_token: String = {
        let shared_state = state.read().expect("Unable to read share state");

        let db = shared_state.db.lock().unwrap_or_else(|e| e.into_inner());

        match db
            .prepare("SELECT refresh_token FROM auth WHERE id = ?1")
            .and_then(|mut stmt| stmt.query_row([&params.email], |row| row.get(0)))
        {
            Ok(token) => token,
            Err(_) => return Json(serde_json::Value::from("")),
        }
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
    let refresh_resp = refresh_access_token(&client_id, &client_secret, &refresh_token).await;
    let oauth = match refresh_resp {
        Ok(token) => token,
        Err(e) => {
            tracing::error!("OAuth error: {}", e);
            return Json(serde_json::Value::from(""));
        }
    };
    let access_token = oauth.access_token;
    let limit = params.limit.unwrap_or(7); // Default 7 days if not specified

    // Query Gmail for unread messages
    let messages = match list_unread_messages(&access_token, limit).await {
        Ok(x) => x,
        Err(e) => {
            tracing::error!("Gmail API error: {}", e);
            return Json(serde_json::Value::from(""));
        }
    };

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

    Json(json!(threads))
}

async fn set_static_cache_control(request: Request, next: middleware::Next) -> Response {
    let mut response = next.run(request).await;
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    response
}

pub fn app(app_state: AppState) -> Router {
    let shared_state = SharedState::new(RwLock::new(app_state));
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
    let db = vector_db(&vec_db_path).expect("Failed to connect to db");
    let app_config = AppConfig {
        notes_path,
        index_path,
        deploy_key_path,
        vapid_key_path,
        note_search_api_url,
        searxng_api_url,
        gmail_api_client_id,
        gmail_api_client_secret,
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
