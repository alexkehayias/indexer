//! Router for the loops API

use std::sync::{Arc, RwLock};

use axum::{
    Router,
    extract::State,
    response::{IntoResponse, Json},
};

use super::public::{LoopListResponse, LoopSummaryResponse};
use crate::api::public::ApiError;
use crate::api::state::AppState;
use crate::loops::db::find_all_loops;
use crate::loops::supervisor;

type SharedState = Arc<RwLock<AppState>>;

/// List every loop with its subscribed channels and running status.
async fn list_loops(State(state): State<SharedState>) -> Result<impl IntoResponse, ApiError> {
    let db = state
        .read()
        .expect("Unable to read shared state")
        .db
        .clone();

    let loops = find_all_loops(&db).await?;
    let running = supervisor::running_ids();

    let loops = loops
        .into_iter()
        .map(|record| LoopSummaryResponse {
            running: running.iter().any(|id| id == &record.id),
            id: record.id,
            channels: record.channels,
            system_prompt: record.system_prompt,
            tools: record.tools,
            created_at: record.created_at,
        })
        .collect();

    Ok(Json(LoopListResponse { loops }))
}

/// Create the loops router.
pub fn router() -> Router<SharedState> {
    Router::new().route("/", axum::routing::get(list_loops))
}
