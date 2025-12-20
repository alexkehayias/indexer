use crate::openai::{Function, Parameters, Property, ToolCall, ToolType};
use anyhow::{Error, Result};
use async_trait::async_trait;
use reqwest;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Serialize)]
pub struct WebSearchProps {
    /// The search term to query.
    pub query: Property,
    /// Maximum number of results to return.
    pub limit: Property,
}

#[derive(Deserialize)]
pub struct WebSearchArgs {
    pub query: String,
    pub limit: u32,
}

#[derive(Serialize)]
pub struct WebSearchTool {
    pub r#type: ToolType,
    pub function: Function<WebSearchProps>,
    api_base_url: String,
}

#[async_trait]
impl ToolCall for WebSearchTool {
    async fn call(&self, args: &str) -> Result<String, Error> {
        let fn_args: WebSearchArgs = serde_json::from_str(args).unwrap();

        let url = reqwest::Url::parse_with_params(
            &format!("{}/web/search", self.api_base_url),
            &[
                ("query", &fn_args.query),
                ("limit", &fn_args.limit.to_string()),
            ],
        ).expect("Invalid URL");

        let resp: Value = reqwest::Client::new()
            .get(url.as_str())
            .header("Content-Type", "application/json")
            .send()
            .await?
            .json()
            .await?;

        let result = json!(resp).to_string();
        Ok(result)
    }

    fn function_name(&self) -> String {
        self.function.name.clone()
    }
}

impl WebSearchTool {
    pub fn new(api_base_url: &str) -> Self {
        let function = Function {
            name: String::from("web_search"),
            description: String::from(
                "Search the web for a term and return up to `limit` results."
            ),
            parameters: Parameters {
                r#type: String::from("object"),
                properties: WebSearchProps {
                    query: Property {
                        r#type: String::from("string"),
                        description: String::from("The search query term."),
                    },
                    limit: Property {
                        r#type: String::from("integer"),
                        description: String::from(
                            "Maximum number of results to return (default 10)."
                        ),
                    },
                },
                required: vec![String::from("query"), String::from("limit")],
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

impl Default for WebSearchTool {
    fn default() -> Self {
        Self::new("http://localhost:2222")
    }
}
