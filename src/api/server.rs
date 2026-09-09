use std::sync::{Arc, RwLock};

use axum::Router;
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use super::{routes, assets};
use crate::ai::skills::SkillRegistry;
use crate::api::state::AppState;
use crate::core::{AppConfig, db::async_db};
use crate::jobs::{DailyAgenda, GenerateSessionTitles, GitSync, spawn_periodic_job};

pub fn app(shared_state: Arc<RwLock<AppState>>) -> Router {
    let cors = CorsLayer::permissive();

    let router = Router::new()
        // API routes
        .nest("/api", routes::router());

    // Static assets: embedded in the binary (prod), or served from disk (dev).
    let router = assets::attach_assets(router);

    router
        .layer(TraceLayer::new_for_http())
        .layer(cors)
        .with_state(Arc::clone(&shared_state))
}

// Run the server
#[allow(clippy::too_many_arguments)]
pub async fn serve(host: String, port: String, config: AppConfig) {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                // axum logs rejections from built-in extractors with the `axum::rejection`
                // target, at `TRACE` level. `axum::rejection=trace` enables showing those events
                format! {
                    "{}=debug,tower_http=debug,axum::rejection=trace",
                    env!("CARGO_CRATE_NAME")
                }
                .into()
            }),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let db = async_db(&config.vec_db_path)
        .await
        .expect("Failed to connect to async db");

    let skill_registry = SkillRegistry::new(&config.skills_path)
        .await
        .expect("Failed to create skill registry");
    let app_state = AppState::new(db.clone(), config.clone(), skill_registry);
    let shared_state = Arc::new(RwLock::new(app_state));
    let app = app(Arc::clone(&shared_state));

    let listener = tokio::net::TcpListener::bind(format!("{}:{}", host, port))
        .await
        .unwrap();

    tracing::debug!(
        "Server started. Listening on {}",
        listener.local_addr().unwrap()
    );

    // Run background jobs. Each job is spawned in it's own tokio task
    // in a loop.
    spawn_periodic_job(config.clone(), db.clone(), DailyAgenda);
    spawn_periodic_job(config.clone(), db.clone(), GitSync);
    spawn_periodic_job(config, db, GenerateSessionTitles);

    axum::serve(listener, app).await.unwrap();
}
