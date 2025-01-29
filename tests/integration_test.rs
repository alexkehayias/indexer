#[cfg(test)]
mod tests {
    use std::env;
    use std::fs;
    use std::time::SystemTime;

    use anyhow::{Error, Result};
    use axum::{
        body::Body,
        http::{Request, StatusCode},
        Router,
    };
    use indexer::db::vector_db;
    use indexer::openai;
    use indexer::prompt::{self, Prompt};
    use indexer::server::{app, AppConfig, AppState};
    use serde::Serialize;
    use serde_json::json;
    use tower::util::ServiceExt;

    async fn body_to_string(body: Body) -> String {
        let bytes = axum::body::to_bytes(body, 4096usize).await.unwrap();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    fn test_app() -> Router {
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

        let db =
            vector_db(dir.join(&vec_db_path).to_str().unwrap()).expect("Failed to connect to db");
        let app_config = AppConfig {
            notes_path: notes_path.display().to_string(),
            index_path: index_path.display().to_string(),
        };
        let app_state = AppState::new(db, app_config);
        app(app_state)
    }

    // fn index_dummy_notes() {
        // This also creates the full text search index
        // TODO: Create some dummy note files
        //
        // index_all()
    // }

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

    // #[tokio::test]
    // async fn it_searches_full_text() {
    //     let app = test_app();
    //     index_dummy_notes();

    //     let response = app
    //         .oneshot(
    //             Request::builder()
    //                 .uri("/notes/search?query=test")
    //                 .body(Body::empty())
    //                 .unwrap(),
    //         )
    //         .await
    //         .unwrap();

    //     assert_eq!(response.status(), StatusCode::OK);
    // }

    #[derive(Serialize)]
    pub struct DummyProps {
        dummy_arg: openai::Property,
    }

    #[derive(Serialize)]
    pub struct DummyTool {
        pub r#type: openai::ToolType,
        pub function: openai::Function<DummyProps>,
    }

    impl openai::ToolCall for DummyTool {
        fn call(&self, _args: &str) -> String {
            String::from("DummyTool called!")
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
        pub function: openai::Function<DummyProps>,
    }

    impl openai::ToolCall for DummyTool2 {
        fn call(&self, _args: &str) -> String {
            String::from("DummyTool2 called!")
        }

        fn function_name(&self) -> String {
            String::from("dummy_tool_2")
        }
    }

    #[tokio::test]
    async fn it_makes_openai_request() {
        let messages = vec![
            openai::Message::new(openai::Role::System, "You are a helpful assistant."),
            openai::Message::new(
                openai::Role::User,
                "Write a haiku that explains the concept of recursion.",
            ),
        ];
        let tools = None;
        let response = openai::completion(&messages, &tools).await;
        assert!(response.is_ok());
    }

    #[tokio::test]
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
                properties: DummyProps {
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
        let tools: Option<Vec<Box<dyn openai::ToolCall>>> =
            Some(vec![Box::new(dummy_tool), Box::new(dummy_tool_2)]);
        let response = openai::completion(&messages, &tools).await.unwrap();
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
}
