//! Public API types

use std::collections::HashMap;

use crate::openai::Message;
use axum::response::{IntoResponse, Response};
use http::StatusCode;
use serde::{Deserialize, Serialize};

// Errors

pub struct ApiError(anyhow::Error);

/// Convert `AppError` into an Axum compatible response.
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Something went wrong: {}", self.0),
        )
            .into_response()
    }
}

/// Enables using `?` on functions that return `Result<_,
/// anyhow::Error>` to turn them into `Result<_, AppError>`
impl<E> From<E> for ApiError
where
    E: Into<anyhow::Error>,
{
    fn from(err: E) -> Self {
        Self(err.into())
    }
}

// Search

fn default_limit() -> usize {
    20
}

fn default_as_true() -> bool {
    true
}

fn default_as_false() -> bool {
    false
}

#[derive(Deserialize)]
pub struct SearchRequest {
    pub query: String,
    #[serde(default = "default_as_false")]
    pub include_similarity: bool,
    #[serde(default = "default_limit")]
    pub limit: usize,
    #[serde(default = "default_as_true")]
    pub truncate: bool,
}

#[derive(Serialize, Deserialize)]
pub struct SearchResult {
    pub id: String,
    pub r#type: String,
    pub title: String,
    pub category: String,
    pub file_name: String,
    pub tags: Option<String>,
    pub is_task: bool,
    pub task_status: Option<String>,
    pub task_scheduled: Option<String>,
    pub task_deadline: Option<String>,
    pub task_closed: Option<String>,
    pub meeting_date: Option<String>,
    pub body: String,
}

#[derive(Serialize, Deserialize)]
pub struct SearchResponse {
    pub raw_query: String,
    pub parsed_query: String,
    pub results: Vec<SearchResult>,
}

// Note

#[derive(Serialize)]
pub struct ViewNoteResponse {
    pub id: String,
    pub title: String,
    pub body: String,
    pub tags: Option<String>,
}

// Chat

#[derive(Deserialize)]
pub struct ChatRequest {
    pub session_id: String,
    pub message: String,
}

#[derive(Deserialize)]
pub struct ChatSessionsQuery {
    pub page: Option<usize>,
    pub limit: Option<usize>,
    // Use HTML form syntax "?tags=t1&tags=t2"
    pub tags: Option<Vec<String>>,
    // Exclude sessions containing any of these tags
    pub exclude_tags: Option<Vec<String>>,
}

#[derive(Serialize)]
pub struct ChatSessionsResponse {
    pub sessions: Vec<ChatSession>,
    pub page: usize,
    pub limit: usize,
    pub total_sessions: i64,
    pub total_pages: i64,
}

#[derive(Serialize)]
pub struct ChatResponse {
    message: String,
}

impl ChatResponse {
    pub fn new(message: &str) -> Self {
        Self {
            message: message.into(),
        }
    }
}

#[derive(Serialize)]
pub struct ChatTranscriptResponse {
    pub transcript: Vec<Message>,
}

// Notifications

#[derive(Deserialize)]
pub struct PushSubscriptionRequest {
    pub endpoint: String,
    pub keys: HashMap<String, String>,
}

#[derive(Deserialize)]
pub struct NotificationRequest {
    pub message: String,
}

// Email

#[derive(Deserialize)]
pub struct EmailUnreadQuery {
    pub email: String,
    pub limit: Option<i64>,
}

#[derive(Clone, Serialize)]
pub struct EmailMessage {
    pub id: String,
    pub thread_id: String,
    pub from: String,
    pub to: String,
    pub received: String,
    pub subject: String,
    pub body: String,
}

#[derive(Clone, Serialize)]
pub struct EmailThread {
    pub id: String,
    pub received: String,
    pub from: String,
    pub to: String,
    pub subject: String,
    pub messages: Vec<EmailMessage>,
}

#[derive(Deserialize)]
pub struct CalendarQuery {
    pub email: String,
    pub days_ahead: Option<i64>,
    pub calendar_id: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct CalendarAttendee {
    pub email: String,
    pub display_name: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct CalendarResponse {
    pub id: String,
    pub summary: String,
    pub start: String, // Using String for datetime to maintain compatibility
    pub end: String,   // Using String for datetime to maintain compatibility
    pub attendees: Option<Vec<CalendarAttendee>>,
}

// Web search request/response structures
#[derive(Deserialize)]
pub struct WebSearchParams {
    pub query: String,
    #[serde(default = "default_web_limit")]
    pub limit: u8,
}

fn default_web_limit() -> u8 { 3 }

#[derive(Serialize, Deserialize)]
pub struct WebSearchResult {
    pub title: String,
    pub link: String,
    pub snippet: String,
}

#[derive(Serialize, Deserialize)]
pub struct WebSearchResponse {
    pub query: String,
    pub results: Vec<WebSearchResult>,
}

#[derive(Serialize, Clone)]
pub struct ChatSession {
    pub id: String,
    pub title: Option<String>,
    pub summary: Option<String>,
    pub tags: Vec<String>,
}

// Metrics
#[derive(Serialize, Deserialize)]
pub enum MetricName {
    #[serde(rename = "token-count")]
    TokenCount
}

/// Request to record a metric event
#[derive(Deserialize)]
pub struct MetricRequest {
    pub name: MetricName,
    pub value: i64,
}
