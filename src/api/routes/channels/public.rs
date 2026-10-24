//! Public types for the channels API

use serde::Serialize;

#[derive(Serialize)]
pub struct ChannelListResponse {
    pub channels: Vec<ChannelSummaryResponse>,
}

#[derive(Serialize)]
pub struct ChannelSummaryResponse {
    pub id: String,
    /// How the channel is served: `server` or `socket`.
    pub source: String,
    /// The agent session that created the channel, if any; `null` for server-
    /// and socket-defined channels. Only this owner may emit to the channel.
    pub created_by: Option<String>,
    pub debounce_ms: i64,
    pub created_at: String,
}
