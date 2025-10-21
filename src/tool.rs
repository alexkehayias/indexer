use async_trait::async_trait;
use anyhow::{Error, Result};
use crate::openai::{Function, Parameters, Property, ToolCall, ToolType};
use serde::{Deserialize, Serialize};
use serde_json;
use serde_json::{json, Value};
use reqwest;


#[derive(Serialize)]
pub struct NoteSearchProps {
    pub query: Property,
}

#[derive(Deserialize)]
pub struct NoteSearchArgs {
    pub query: String,
}

#[derive(Serialize)]
pub struct NoteSearchTool {
    pub r#type: ToolType,
    pub function: Function<NoteSearchProps>,
    api_base_url: String,
}

#[async_trait]
impl ToolCall for NoteSearchTool {
    async fn call(&self, args: &str) -> Result<String, Error> {
        let fn_args: NoteSearchArgs = serde_json::from_str(args).unwrap();

        // Construct the URL with query parameters encoded
        let mut url = reqwest::Url::parse(
            &format!("{}/notes/search", self.api_base_url)
        ).expect("Invalid URL");
        url.query_pairs_mut().append_pair("query", &fn_args.query);

        let resp: Value = reqwest::Client::new()
            .get(url.as_str())
            .header("Content-Type", "application/json")
            .send()
            .await?
            .json()
            .await?;
        // TODO: Process the results into something cleaner than raw
        // json like markdown.
        let result = json!(resp).to_string();
        Ok(result)
    }

    fn function_name(&self) -> String {
        self.function.name.clone()
    }
}

impl NoteSearchTool {
    pub fn new(api_base_url: &str) -> Self {
        let function = Function {
            name: String::from("search_notes"),
            description: String::from("Find notes the user has written about."),
            parameters: Parameters {
                r#type: String::from("object"),
                properties: NoteSearchProps {
                    query: Property {
                        r#type: String::from("string"),
                        description: String::from("The query to use for searching notes that should be short and optimized for search.")
                    }
                },
                required: vec![String::from("query")],
                additional_properties: false,
            },
            strict: true,
        };
        Self {
            r#type: ToolType::Function,
            function,
            api_base_url: api_base_url.to_string(),
        }
    }
}

impl Default for NoteSearchTool {
    fn default() -> Self {
        Self::new("http://localhost:2222")
    }
}
