use std::fs;
use std::collections::HashMap;
use std::env;
use std::sync::{Arc, Mutex, RwLock};

use axum::response::Html;
use tantivy::doc;

use axum::extract::Query;
use axum::{
    extract::{State, Path},
    response::Json,
    routing::{get, post},
    Router,
};
use rusqlite::Connection;
use serde::Deserialize;
use serde_json::{json, Value};
use tower_http::cors::CorsLayer;
use tower_http::services::ServeDir;
use tower_http::trace::TraceLayer;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
use orgize::ParseConfig;

use crate::indexing::index_notes_all;

use super::db::vector_db;
use super::git::maybe_pull_and_reset_repo;
use super::search::search_notes;

type SharedState = Arc<RwLock<AppState>>;

struct AppConfig {
    notes_path: &'static str,
    index_path: &'static str,
}

struct AppState {
    // Stores the latest search hit selected by the user
    latest_selection: HashMap<String, String>,
    db: Mutex<Connection>,
    config: AppConfig,
}

impl AppState {
    fn new(db: Connection, config: AppConfig) -> Self {
        Self {
            latest_selection: HashMap::new(),
            db: Mutex::new(db),
            config
        }
    }
}

#[derive(Debug, Deserialize)]
struct SetLatest {
    id: String,
    title: String,
    file_name: String,
}

async fn kv_get(State(state): State<SharedState>) -> Json<Value> {
    let resp = json!({
        "id": state.read().unwrap().latest_selection.get("id"),
        "file_name": state.read().unwrap().latest_selection.get("file_name"),
        "title": state.read().unwrap().latest_selection.get("title"),
    });
    Json(resp)
}

async fn kv_set(State(state): State<SharedState>, Json(data): Json<SetLatest>) {
    state
        .write()
        .unwrap()
        .latest_selection
        .insert(String::from("id"), data.id);
    state
        .write()
        .unwrap()
        .latest_selection
        .insert(String::from("file_name"), data.file_name);
    state
        .write()
        .unwrap()
        .latest_selection
        .insert(String::from("title"), data.title);
}

// Fulltext search of all notes
async fn search(
    State(state): State<SharedState>,
    Query(params): Query<HashMap<String, String>>,
) -> Json<Value> {
    let query = params.get("query");
    let results = if let Some(query) = query {
        let shared_state = state.read().unwrap();
        let index_path = shared_state.config.index_path;
        let db = shared_state
            .db
            .lock()
        // Ignoring any previous panics since we are trying to get the
        // db connection and it's probably fine
            .unwrap_or_else(|e| e.into_inner());

        let include_similarity = params.contains_key("include_similarity") && params.get("include_similarity").unwrap() == "true";
        if include_similarity {
            // TODO: Handle search query arguments like this default
            // argument that comes from the search UI
            let similarity_query = query.replace("-title:journal ", "");
            search_notes(index_path, &db, &similarity_query, include_similarity)
        } else {
            search_notes(index_path, &db, query, include_similarity)
        }
    } else {
        Vec::new()
    };

    let resp = json!({
        "query": query,
        "results": results,
    });
    Json(resp)
}

// Build the index for all notes
async fn index_notes(State(state): State<SharedState>) -> Json<Value> {
    let AppConfig {index_path, notes_path} = &state.read().expect("Failed to read state").config;
    let deploy_key_path = env::var("INDEXER_NOTES_DEPLOY_KEY_PATH")
        .expect("Missing env var INDEXER_NOTES_DEPLOY_KEY_PATH");
    maybe_pull_and_reset_repo(notes_path, deploy_key_path);

    index_notes_all(index_path, notes_path);

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
    Path(id): Path<String>
) -> Html<String> {
    let shared_state = state.read().expect("Unable to read share state");
    let notes_path = shared_state.config.notes_path;

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
        let note_path = format!("{}/{}", notes_path, f);
        let content = fs::read_to_string(&note_path).expect("Failed to get file content");

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

// Run the server
pub async fn serve(host: String, port: String) {
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

    let vec_db_path = "./db";
    let db = vector_db(vec_db_path).expect("Failed to connect to db");
    let app_config = AppConfig {
        notes_path: "./notes",
        index_path: "./.index",
    };
    let app_state = AppState::new(db, app_config);
    let shared_state = SharedState::new(RwLock::new(app_state));
    let cors = CorsLayer::permissive();
    let serve_dir = ServeDir::new("./web-ui/src");

    let app = Router::new()
        // Search API endpoint
        .route("/notes/search", get(search))
        // Storage for selected search hits
        .route("/notes/search/latest", get(kv_get).post(kv_set))
        // Index content endpoint
        .route("/notes/index", post(index_notes))
    // Static server of assets in ./web-ui
        .route("/notes/:id/view", get(view_note))
        // Static server of assets in ./web-ui
        .nest_service("/", serve_dir.clone())
        .layer(TraceLayer::new_for_http())
        .layer(cors)
        .with_state(Arc::clone(&shared_state));

    let listener = tokio::net::TcpListener::bind(format!("{}:{}", host, port))
        .await
        .unwrap();

    tracing::debug!(
        "Server started. Listening on {}",
        listener.local_addr().unwrap()
    );

    axum::serve(listener, app).await.unwrap();
}
