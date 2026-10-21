//! Public types for the channels API

use serde::Serialize;

#[derive(Serialize)]
pub struct ChannelListResponse {
    pub channels: Vec<ChannelSummaryResponse>,
}

#[derive(Serialize)]
pub struct ChannelSummaryResponse {
    pub id: String,
    /// The channel's owner: `server`, `socket`, or `agent`.
    pub source: String,
    pub debounce_ms: i64,
    pub created_at: String,
}
