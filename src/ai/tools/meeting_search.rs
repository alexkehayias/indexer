use crate::api::public::notes::SearchResponse;
use crate::openai::{Function, Parameters, Property, ToolCall, ToolType, parse_tool_args};
use anyhow::{Error, Result};
use async_trait::async_trait;
use super::registry::{Tool, ToolContext};
use serde::{Deserialize, Serialize};

#[derive(Serialize)]
pub struct MeetingSearchProps {
    pub query: Property,
}

#[derive(Deserialize)]
pub struct MeetingSearchArgs {
    pub query: String,
}

#[derive(Serialize)]
pub struct MeetingSearchTool {
    pub r#type: ToolType,
    pub function: Function<MeetingSearchProps>,
    api_base_url: String,
}

#[async_trait]
impl ToolCall for MeetingSearchTool {
    async fn call(&self, args: &str) -> Result<String, Error> {
        let fn_args: MeetingSearchArgs = parse_tool_args(args)?;

        let mut url = reqwest::Url::parse(&format!("{}/api/notes/search", self.api_base_url))
            .expect("Invalid URL");

        // Search for notes with the "meeting" tag
        let query = format!("tags:meeting {}", &fn_args.query);
        url.query_pairs_mut().append_pair("query", &query);

        let resp = reqwest::Client::new()
            .get(url.as_str())
            .header("Content-Type", "application/json")
            .send()
            .await?
            .error_for_status()?;

        let search_resp: SearchResponse = resp.json().await?;

        let mut accum = vec![];
        for r in search_resp.results.iter() {
            accum.push(format!("## {}\n{}\n{}", r.title, r.id, r.body))
        }

        let out = accum.join("\n\n");
        Ok(out)
    }

    fn function_name(&self) -> String {
        self.function.name.clone()
    }
}

impl Tool for MeetingSearchTool {
    fn from_context(ctx: &ToolContext) -> Result<Self> {
        Ok(Self::new(&ctx.api_base_url))
    }
}

impl MeetingSearchTool {
    pub fn new(api_base_url: &str) -> Self {
        let function = Function {
            name: String::from("search_meetings"),
            description: String::from("Find meeting notes the user has written about."),
            parameters: Parameters {
                r#type: String::from("object"),
                properties: MeetingSearchProps {
                    query: Property {
                        r#type: String::from("string"),
                        description: String::from(
                            "The query to use for searching meeting notes that should be short and optimized for search.",
                        ),
                        r#enum: None,
                    },
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

impl Default for MeetingSearchTool {
    fn default() -> Self {
        Self::new("http://localhost:2222")
    }
}
