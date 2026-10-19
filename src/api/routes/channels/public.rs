//! Public types for the channels API

use serde::Serialize;

#[derive(Serialize)]
pub struct ChannelListResponse {
    pub channels: Vec<ChannelSummaryResponse>,
}

#[derive(Serialize)]
pub struct ChannelSummaryResponse {
    pub id: String,
    pub source: String,
    pub debounce_ms: i64,
    /// Session/agent that created an agent-owned channel, if any.
    pub created_by: Option<String>,
    pub created_at: String,
}
