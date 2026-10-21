//! Router for the channels API

use std::sync::{Arc, RwLock};

use axum::{
    Router,
    extract::State,
    response::{IntoResponse, Json},
};

use super::public::{ChannelListResponse, ChannelSummaryResponse};
use crate::api::public::ApiError;
use crate::api::state::AppState;
use crate::channels::db::find_all_channels;

type SharedState = Arc<RwLock<AppState>>;

/// List every channel loops can subscribe to.
async fn list_channels(State(state): State<SharedState>) -> Result<impl IntoResponse, ApiError> {
    let db = state
        .read()
        .expect("Unable to read shared state")
        .db
        .clone();

    let channels = find_all_channels(&db)
        .await?
        .into_iter()
        .map(|channel| ChannelSummaryResponse {
            id: channel.id,
            source: channel.owner.as_str().to_string(),
            debounce_ms: channel.debounce_ms,
            created_at: channel.created_at,
        })
        .collect();

    Ok(Json(ChannelListResponse { channels }))
}

/// Create the channels router.
pub fn router() -> Router<SharedState> {
    Router::new().route("/", axum::routing::get(list_channels))
}
