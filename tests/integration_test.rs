#[cfg(test)]
mod tests {
    use std::env;
    use std::fs;
    use std::time::SystemTime;

    use indexer::server::{app, AppState, AppConfig};
    use indexer::db::vector_db;
    use axum::{
        Router,
        body::Body,
        http::{Request, StatusCode},
    };
    use tower::util::ServiceExt; // for `call`, `oneshot`, and `ready`

    async fn body_to_string(body: Body) -> String {
        let bytes = axum::body::to_bytes(body, 4096usize).await.unwrap();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    fn test_app() -> Router {
        // Create a unique directory for the test with a randomly
        // generated name using a timestamp to avoid collisions and
        // vulnerabilities
        let temp_dir = env::temp_dir();
        let ts = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap().as_secs().to_string();
        let dir = temp_dir.join(ts);
        fs::create_dir_all(&dir).expect("Failed to create base directory");

        // Create the directory from each path
        let notes_path = dir.join("notes");
        let index_path = dir.join("index");
        let vec_db_path = dir.join("db");
        fs::create_dir_all(&notes_path).expect("Failed to create notes directory");
        fs::create_dir_all(&index_path).expect("Failed to create index directory");
        fs::create_dir_all(&vec_db_path).expect("Failed to create db directory");

        let db = vector_db(dir.join(&vec_db_path).to_str().unwrap()).expect("Failed to connect to db");
        let app_config = AppConfig {
            notes_path: notes_path.display().to_string(),
            index_path: index_path.display().to_string(),
        };
        let app_state = AppState::new(db, app_config);
        app(app_state)
    }

    #[tokio::test]
    async fn it_serves_web_ui() {
        let app = test_app();

        let response = app
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body = body_to_string(response.into_body()).await;
        assert!(body.contains("input id=\"search\""));
    }
}
