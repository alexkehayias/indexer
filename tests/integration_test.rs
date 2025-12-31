#[cfg(test)]
mod tests {
    use std::env;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::{Arc, RwLock};
    use std::time::SystemTime;

    use anyhow::{Error, Result};
    use async_trait::async_trait;
    use axum::{
        Router,
        body::Body,
        http::{Request, StatusCode},
    };
    use tokio::sync::mpsc;
    use tower::util::ServiceExt;

    use indexer::config::AppConfig;
    use indexer::db::async_db;
    use indexer::db::initialize_db;
    use indexer::indexing::index_all;
    use indexer::openai;
    use indexer::openai::BoxedToolCall;
    use indexer::prompt::{self, Prompt};
    use indexer::server::{AppState, app};
    use serde::Serialize;
    use serde_json::json;
    use serial_test::serial;

    async fn body_to_string(body: Body) -> String {
        let bytes = axum::body::to_bytes(body, 8192usize).await.unwrap();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    /// Anything that uses this fixture can not be run in parallel due
    /// to a lock held by `tantivy` during index writing so add a
    /// `#[serial]` to the test function or run `cargo test --
    /// --test-threads=1`.
    async fn test_app() -> Router {
        // Create a unique directory for the test with a randomly
        // generated name using a timestamp to avoid collisions and
        // vulnerabilities
        let temp_dir = env::temp_dir();
        let ts = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            .to_string();
        let dir = temp_dir.join(ts);
        fs::create_dir_all(&dir).expect("Failed to create base directory");

        // Create the directory from each path
        let notes_path = dir.join("notes");
        let index_path = dir.join("index");
        let vec_db_path = dir.join("db");
        fs::create_dir_all(&notes_path).expect("Failed to create notes directory");
        fs::create_dir_all(&index_path).expect("Failed to create index directory");
        fs::create_dir_all(&vec_db_path).expect("Failed to create db directory");

        let db_path_str = dir.join(&vec_db_path);
        let db_path_str = db_path_str.to_str().unwrap();

        let db = async_db(db_path_str)
            .await
            .expect("Failed to connect to async db");
        db.call(|conn| {
            initialize_db(conn).expect("Failed to migrate db");
            Ok(())
        })
        .await
        .unwrap();

        index_dummy_notes_async(&db, dir.clone()).await;

        let app_config = AppConfig {
            notes_path: notes_path.display().to_string(),
            index_path: index_path.display().to_string(),
            deploy_key_path: String::from("test_deploy_key_path"),
            vapid_key_path: String::from("test_vapid_key_path"),
            note_search_api_url: String::from("http://localhost:2222"),
            searxng_api_url: String::from("http://localhost:8080"),
            gmail_api_client_id: String::from("test_client_id"),
            gmail_api_client_secret: String::from("test_client_secret"),
            openai_model: String::from("gpt-4o"),
            openai_api_hostname: String::from("https://api.openai.com"),
            openai_api_key: String::from("test-api-key"),
            system_message: String::from("You are a helpful assistant."),
            calendar_email: None,
        };
        let app_state = AppState::new(db, app_config);
        app(Arc::new(RwLock::new(app_state)))
    }

    async fn index_dummy_notes_async(db: &tokio_rusqlite::Connection, temp_dir: PathBuf) {
        let index_dir = temp_dir.join("index");
        let index_dir_path = index_dir.to_str().unwrap();
        fs::create_dir_all(index_dir_path).expect("Failed to create directory");

        let notes_dir = temp_dir.join("notes");
        let notes_dir_path = notes_dir.to_str().unwrap();
        fs::create_dir_all(notes_dir_path).expect("Failed to create directory");

        let test_note_path = notes_dir.join("test.org");
        let paths = vec![test_note_path.clone()];

        fs::write(
            test_note_path,
            r#":PROPERTIES:
:ID:       6A503659-15E4-4427-835F-7873F8FF8ECF
:END:
#+TITLE: this is a test
#+DATE: 2025-01-28
"#,
        )
        .unwrap();

        index_all(db, index_dir_path, notes_dir_path, true, true, Some(paths))
            .await
            .unwrap();
    }

    #[tokio::test]
    #[serial]
    async fn it_serves_web_ui() {
        let app = test_app().await;

        let response = app
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body = body_to_string(response.into_body()).await;
        assert!(body.contains("Welcome Alex"));
    }

    #[tokio::test]
    #[serial]
    async fn it_searches_full_text() {
        let app = test_app().await;

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/notes/search?query=test")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }

    #[derive(Serialize)]
    pub struct DummyProps {
        dummy_arg: openai::Property,
    }

    #[derive(Serialize)]
    pub struct DummyTool {
        pub r#type: openai::ToolType,
        pub function: openai::Function<DummyProps>,
    }

    #[async_trait]
    impl openai::ToolCall for DummyTool {
        async fn call(&self, _args: &str) -> Result<String, Error> {
            Ok(String::from("DummyTool called!"))
        }

        fn function_name(&self) -> String {
            String::from("dummy_tool")
        }
    }

    #[derive(Serialize)]
    pub struct DummyProps2 {
        dummy_arg: openai::Property,
    }

    #[derive(Serialize)]
    pub struct DummyTool2 {
        pub r#type: openai::ToolType,
        pub function: openai::Function<DummyProps2>,
    }

    #[async_trait]
    impl openai::ToolCall for DummyTool2 {
        async fn call(&self, _args: &str) -> Result<String, Error> {
            Ok(String::from("DummyTool2 called!"))
        }

        fn function_name(&self) -> String {
            String::from("dummy_tool_2")
        }
    }

    #[tokio::test]
    #[ignore]
    async fn it_makes_openai_request() {
        let messages = vec![
            openai::Message::new(openai::Role::System, "You are a helpful assistant."),
            openai::Message::new(
                openai::Role::User,
                "Write a haiku that explains the concept of recursion.",
            ),
        ];
        let tools = None;
        let response = openai::completion(
            &messages,
            &tools,
            "https://api.openai.com",
            "test-api-key",
            "gpt-4o",
        )
        .await;
        assert!(response.is_ok());
    }

    #[tokio::test]
    #[ignore]
    async fn it_makes_openai_streaming_request() {
        let (tx, _rx) = mpsc::unbounded_channel::<String>();
        let messages = vec![
            openai::Message::new(openai::Role::System, "You are a helpful assistant."),
            openai::Message::new(
                openai::Role::User,
                "Write a haiku that explains the concept of recursion.",
            ),
        ];
        let tools = None;
        let response = openai::completion_stream(
            tx,
            &messages,
            &tools,
            "https://api.openai.com",
            "test-api-key",
            "gpt-4o",
        )
        .await;

        assert!(response.is_ok());
        assert_eq!(response.unwrap(), "Testing");
    }

    #[tokio::test]
    #[ignore]
    async fn it_makes_openai_tool_calls() {
        let messages = vec![
            openai::Message::new(openai::Role::System, "You are a helpful assistant."),
            openai::Message::new(openai::Role::User, "What's the weather in New York?"),
        ];
        let function = openai::Function {
            name: String::from("get_weather"),
            description: String::from("Retrieves current weather for the given location."),
            parameters: openai::Parameters {
                r#type: String::from("object"),
                properties: DummyProps {
                    dummy_arg: openai::Property {
                        r#type: String::from("string"),
                        description: String::from("Location of the weather requested"),
                    },
                },
                required: vec![String::from("dummy_arg")],
                additional_properties: false,
            },
            strict: true,
        };
        let dummy_tool = DummyTool {
            r#type: openai::ToolType::Function,
            function,
        };

        let function2 = openai::Function {
            name: String::from("get_notes"),
            description: String::from("Retrieves notes the user asks about."),
            parameters: openai::Parameters {
                r#type: String::from("object"),
                properties: DummyProps2 {
                    dummy_arg: openai::Property {
                        r#type: String::from("string"),
                        description: String::from("Some dummy arg"),
                    },
                },
                required: vec![String::from("dummy_arg")],
                additional_properties: false,
            },
            strict: true,
        };
        let dummy_tool_2 = DummyTool2 {
            r#type: openai::ToolType::Function,
            function: function2,
        };
        let tools: Option<Vec<BoxedToolCall>> =
            Some(vec![Box::new(dummy_tool), Box::new(dummy_tool_2)]);
        let response = openai::completion(
            &messages,
            &tools,
            "https://api.openai.com",
            "test-api-key",
            "gpt-4o",
        )
        .await
        .unwrap();
        let tool_calls = response["choices"][0]["message"]["tool_calls"]
            .as_array()
            .unwrap();
        assert!(!tool_calls.is_empty());
    }

    #[tokio::test]
    async fn it_renders_a_prompt() -> Result<(), Error> {
        let templates = prompt::templates();
        let actual = templates.render(
            &Prompt::NoteSummary.to_string(),
            &json!({"context": "test test"}),
        )?;
        assert!(actual.contains("CONTEXT:\ntest test"));
        Ok(())
    }

    #[tokio::test]
    #[serial]
    async fn it_gets_chat_sessions() {
        let app = test_app().await;

        // First create some chat sessions by making a request to the chat endpoint
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/notes/chat")
                    .method("POST")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "session_id": "test-session-1",
                            "message": "Hello"
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        // Now test the sessions endpoint
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/notes/chat/sessions")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body = body_to_string(response.into_body()).await;
        // Verify we get valid JSON response
        assert!(body.contains("\"sessions\""));
        assert!(body.contains("\"page\""));
        assert!(body.contains("\"limit\""));
        assert!(body.contains("\"total_sessions\""));
        assert!(body.contains("\"total_pages\""));
    }

    #[tokio::test]
    #[serial]
    async fn it_gets_chat_sessions_with_pagination() {
        let app = test_app().await;

        // Create multiple chat sessions to test pagination
        for i in 1..=5 {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri("/notes/chat")
                        .method("POST")
                        .header("content-type", "application/json")
                        .body(Body::from(
                            json!({
                                "session_id": format!("test-session-{}", i),
                                "message": format!("Message {}", i)
                            })
                            .to_string(),
                        ))
                        .unwrap(),
                )
                .await
                .unwrap();

            assert_eq!(response.status(), StatusCode::OK);
        }

        // Test pagination with limit=2, page=1
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/notes/chat/sessions?page=1&limit=2")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body = body_to_string(response.into_body()).await;
        // Verify we get valid JSON response with pagination data
        assert!(body.contains("\"sessions\""));
        // Just check that the response has the basic structure we expect
        assert!(body.contains("\"page\""));
        assert!(body.contains("\"limit\""));
        assert!(body.contains("\"total_sessions\""));
        assert!(body.contains("\"total_pages\""));

        // Test pagination with limit=2, page=2
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/notes/chat/sessions?page=2&limit=2")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body = body_to_string(response.into_body()).await;
        // Verify we get valid JSON response with pagination data for second page
        assert!(body.contains("\"page\""));
        assert!(body.contains("\"limit\""));
        assert!(body.contains("\"total_sessions\""));
        assert!(body.contains("\"total_pages\""));
    }

    #[tokio::test]
    async fn it_records_metric() {
        let app = test_app().await;

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/metrics")
                    .method("POST")
                    .header("content-type", "application/json")
                    .body(Body::from(
                        json!({
                            "name": "token-count",
                            "value": 20,
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }
}
