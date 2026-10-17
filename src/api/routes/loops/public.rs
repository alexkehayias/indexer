//! Public types for the loops API

use serde::Serialize;

#[derive(Serialize)]
pub struct LoopListResponse {
    pub loops: Vec<LoopSummaryResponse>,
}

#[derive(Serialize)]
pub struct LoopSummaryResponse {
    pub id: String,
    pub channels: Vec<String>,
    pub system_prompt: Option<String>,
    pub tools: Vec<String>,
    pub created_at: String,
    pub running: bool,
}
