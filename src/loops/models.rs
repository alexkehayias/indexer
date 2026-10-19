use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Loop {
    pub id: String,
    pub channels: Vec<String>,
    pub system_prompt: String,
    pub tools: Vec<String>,
    pub created_at: String,
}

/// An event source that loops can subscribe to.
#[derive(Debug, Clone, Serialize)]
pub struct Channel {
    pub id: String,
    /// Where events come from: 'socket', 'server', or 'webhook'.
    pub source: String,
    /// Debounce window (ms) coalescing a channel's line burst into one event.
    pub debounce_ms: i64,
    /// Session/agent that created an agent-owned channel, if any. Only this
    /// agent may emit to the channel.
    pub created_by: Option<String>,
    pub created_at: String,
}
