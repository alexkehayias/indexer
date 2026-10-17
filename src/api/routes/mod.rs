//! API routes module

pub mod calendar;
pub mod channels;
pub mod chat;
pub mod email;
pub mod files;
mod kv;
pub mod loops;
pub mod metrics;
pub mod notes;
pub mod push;
pub mod skills;
pub mod web;
pub mod webhook;

use std::sync::{Arc, RwLock};

use crate::api::state::AppState;
use axum::Router;

type SharedState = Arc<RwLock<AppState>>;

/// Create the combined API router
pub fn router() -> Router<SharedState> {
    Router::new()
        // Notes routes
        .nest("/notes", notes::router())
        // Chat routes
        .nest("/chat", chat::router())
        // File upload routes
        .nest("/files", files::router())
        // KV routes (for latest selection)
        .nest("/notes/search", kv::router())
        // Push notification routes
        .nest("/push", push::router())
        // Email routes
        .nest("/email", email::router())
        // Calendar routes
        .nest("/calendar", calendar::router())
        // Web search routes
        .nest("/web", web::router())
        // Metrics routes
        .nest("/metrics", metrics::router())
        // Skills routes
        .nest("/skills", skills::router())
        // Channel loop routes
        .nest("/loops", loops::router())
        // Channels routes
        .nest("/channels", channels::router())
        // Webhook routes
        .nest("/webhook", webhook::router())
}
