use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, RwLock};

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
use tower_http::cors::CorsLayer;
use tower_http::services::ServeDir;
use tower_http::trace::TraceLayer;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use crate::indexing::index_all;

use super::db::vector_db;
use super::git::{diff_last_commit_files, maybe_pull_and_reset_repo};
use super::search::{search_notes, SearchResult};

type SharedState = Arc<RwLock<AppState>>;

pub struct AppConfig {
    pub notes_path: String,
    pub index_path: String,
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
}

impl AppState {
    pub fn new(db: Connection, config: AppConfig) -> Self {
        Self {
            latest_selection: None,
            db: Mutex::new(db),
            config,
        }
    }
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

// Fulltext search of all notes
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
    } = &state.read().expect("Failed to read state").config;
    let deploy_key_path = env::var("INDEXER_NOTES_DEPLOY_KEY_PATH")
        .expect("Missing env var INDEXER_NOTES_DEPLOY_KEY_PATH");

    // Pull the latest from origin
    maybe_pull_and_reset_repo(&deploy_key_path, notes_path);

    // Determine which notes changed
    let diff = diff_last_commit_files(&deploy_key_path, notes_path);
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
        let content = fs::read_to_string(f).expect("Failed to get file content");

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
        .route("/notes/:id/view", get(view_note))
        // Static server of assets in ./web-ui
        .nest_service("/", serve_dir.clone())
        .layer(TraceLayer::new_for_http())
        .layer(cors)
        .with_state(Arc::clone(&shared_state))
}

// Run the server
pub async fn serve(
    host: String,
    port: String,
    notes_path: String,
    index_path: String,
    vec_db_path: String,
) {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                // axum logs rejections from built-in extractors with the `axum::rejection`
                // target, at `TRACE` level. `axum::rejection=trace` enables showing those events
                format!(
                    "{}=debug,tower_http=debug,axum::rejection=trace",
                    env!("CARGO_CRATE_NAME")
                )
                    .into()
            }),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();
    let db = vector_db(&vec_db_path).expect("Failed to connect to db");
    let app_config = AppConfig {
        notes_path,
        index_path,
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
