//! Public API types

use axum::response::{IntoResponse, Response};
use http::StatusCode;

// Errors
pub struct ApiError(anyhow::Error);

/// Convert `AppError` into an Axum compatible response.
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        // Always log the error
        tracing::error!("{}\n{}", self.0, self.0.backtrace());

        // Respond with an error status
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

// Re-export public types from each route

pub mod calendar {
    pub use crate::api::routes::calendar::public::*;
}

pub mod channels {
    pub use crate::api::routes::channels::public::*;
}

pub mod email {
    pub use crate::api::routes::email::public::*;
}

pub mod files {
    pub use crate::api::routes::files::public::*;
}

pub mod loops {
    pub use crate::api::routes::loops::public::*;
}

pub mod metrics {
    pub use crate::api::routes::metrics::public::*;
}

pub mod notes {
    pub use crate::api::routes::notes::public::*;
}

pub mod push {
    pub use crate::api::routes::push::public::*;
}

pub mod webhook {
    pub use crate::api::routes::webhook::public::*;
}

pub mod skills {
    pub use crate::api::routes::skills::public::*;
}

pub mod web {
    pub use crate::api::routes::web::public::*;
}
