use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, RwLock};

use tantivy::{doc, Index, IndexWriter};

use axum::extract::Query;
use axum::{
    extract::State,
    response::Json,
    routing::{get, post},
    Router,
};
use serde::Deserialize;
use serde_json::{json, Value};
use tower_http::cors::CorsLayer;
use tower_http::services::ServeDir;
use tower_http::trace::TraceLayer;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use super::schema::note_schema;
use super::search::search_notes;
use super::indexing::index_note;
use super::source::notes;

type SharedState = Arc<RwLock<AppState>>;

#[derive(Default)]
struct AppState {
    // Stores the latest search hit selected by the user
    latest_selection: HashMap<String, String>,
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
        "file_name": state.read().unwrap().latest_selection.get("id"),
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
async fn search(Query(params): Query<HashMap<String, String>>) -> Json<Value> {
    let results = if let Some(query) = params.get("query") {
        search_notes(query)
    } else {
        Vec::new()
    };

    let resp = json!({
        "query": params.get("query"),
        "results": results,
    });
    Json(resp)
}

// Pull and reset to origin main branch
fn maybe_pull_and_reset_repo(deploy_key_path: String) {
    let git_clone = Command::new("sh")
        .arg("-c")
        .arg(format!("cd ./notes && GIT_SSH_COMMAND='ssh -i {} -o IdentitiesOnly=yes' git fetch origin && git reset --hard origin/main", deploy_key_path))
        .output()
        .expect("failed to execute process");

    let stdout = std::str::from_utf8(&git_clone.stdout).expect("Failed to parse stdout");
    let stderr = std::str::from_utf8(&git_clone.stderr).expect("Failed to parse stderr");
    tracing::debug!("stdout: {}\nstderr: {}", stdout, stderr);
}

// Build the index for all notes
async fn index_notes() -> Json<Value> {
    let deploy_key_path = env::var("INDEXER_NOTES_DEPLOY_KEY_PATH")
        .expect("Missing env var INDEXER_NOTES_DEPLOY_KEY_PATH");
    maybe_pull_and_reset_repo(deploy_key_path);

    let index_path = "./.index";
    fs::remove_dir_all(index_path).expect("Failed to remove index directory");
    fs::create_dir(index_path).expect("Failed to recreate index directory");

    let index_path = tantivy::directory::MmapDirectory::open(index_path).expect("Index not found");
    let schema = note_schema();
    let idx =
        Index::open_or_create(index_path, schema.clone()).expect("Unable to open or create index");
    let mut index_writer: IndexWriter = idx
        .writer(50_000_000)
        .expect("Index writer failed to initialize");

    let notes_path = "./notes";
    for note in notes(notes_path) {
        let _ = index_note(&mut index_writer, &schema, note);
    }

    index_writer.commit().expect("Index write failed");

    let resp = json!({
        "success": true,
    });
    Json(resp)
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

    let shared_state = SharedState::default();
    let cors = CorsLayer::permissive();
    let serve_dir = ServeDir::new("./web-ui/src");

    let app = Router::new()
        // Search API endpoint
        .route("/notes/search", get(search))
        // Storage for selected search hits
        .route("/notes/search/latest", get(kv_get).post(kv_set))
        // Search API endpoint
        .route("/notes/index", post(index_notes))
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
