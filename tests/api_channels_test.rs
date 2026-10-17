//! Integration tests for the channels API endpoint

mod test_utils;

#[cfg(test)]
mod tests {
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use serial_test::serial;
    use tower::util::ServiceExt;

    use crate::test_utils::{body_to_string, test_app, test_app_with_state};

    /// Tests that listing channels returns an empty collection initially.
    #[tokio::test]
    #[serial]
    async fn it_gets_empty_channels() {
        let app = test_app().await;

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/channels")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body = body_to_string(response.into_body()).await;
        assert!(body.contains("\"channels\""));
        assert!(body.contains("[]"));
    }

    /// Tests that a persisted channel is returned with its source and debounce.
    #[tokio::test]
    #[serial]
    async fn it_lists_channels() {
        let (app, state) = test_app_with_state().await;
        let db = state.db.clone();

        db.call(|conn| {
            conn.execute(
                "INSERT INTO channel (id, source, debounce_ms) VALUES ('alpha', 'socket', 500)",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/channels")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body = body_to_string(response.into_body()).await;
        assert!(body.contains("\"alpha\""));
        assert!(body.contains("\"socket\""));
        assert!(body.contains("500"));
    }
}
