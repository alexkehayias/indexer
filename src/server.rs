use std::collections::HashMap;
use std::env;
use std::sync::{Arc, RwLock};

use tantivy::doc;

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

use crate::indexing::index_notes_all;

use super::search::search_notes;
use super::git::maybe_pull_and_reset_repo;

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

// Build the index for all notes
async fn index_notes() -> Json<Value> {
    let notes_path = "./notes";
    let deploy_key_path = env::var("INDEXER_NOTES_DEPLOY_KEY_PATH")
        .expect("Missing env var INDEXER_NOTES_DEPLOY_KEY_PATH");
    maybe_pull_and_reset_repo(notes_path, deploy_key_path);
    index_notes_all();
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
