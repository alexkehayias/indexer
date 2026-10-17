//! Integration tests for the loops API endpoint

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

    /// Tests that listing loops returns an empty collection initially.
    #[tokio::test]
    #[serial]
    async fn it_gets_empty_loops() {
        let app = test_app().await;

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/loops")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body = body_to_string(response.into_body()).await;
        assert!(body.contains("\"loops\""));
        assert!(body.contains("[]"));
    }

    /// Tests that a persisted loop is returned with its channels and status.
    #[tokio::test]
    #[serial]
    async fn it_lists_loops_with_channels() {
        let (app, state) = test_app_with_state().await;
        let db = state.db.clone();

        db.call(|conn| {
            conn.execute(
                "INSERT INTO channel (id, source) VALUES ('alpha', 'socket')",
                [],
            )?;
            conn.execute(
                "INSERT INTO loop (id, tools) VALUES ('loop-1', '[\"bash\"]')",
                [],
            )?;
            conn.execute(
                "INSERT INTO loop_channel (loop_id, channel_id, position)
                 VALUES ('loop-1', 'alpha', 0)",
                [],
            )?;
            Ok(())
        })
        .await
        .unwrap();

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/loops")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body = body_to_string(response.into_body()).await;
        assert!(body.contains("\"loop-1\""));
        assert!(body.contains("\"alpha\""));
        assert!(body.contains("\"running\":false"));
    }
}
