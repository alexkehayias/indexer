use crate::openai::{Function, Parameters, Property, ToolCall, ToolType};
use crate::public::SearchResponse;
use anyhow::{Error, Result};
use async_trait::async_trait;
use reqwest;
use serde::{Deserialize, Serialize};
use serde_json;

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

        let mut url = reqwest::Url::parse(&format!("{}/notes/search", self.api_base_url))
            .expect("Invalid URL");

        // By default, only include search results from notes. This
        // avoids low quality content like tasks, meetings, and
        // journal entries from polluting the results.
        let query = format!(
            "{} type:note -tags:project -title:journal -category:work -category:personal",
            &fn_args.query
        );
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
                        description: String::from(
                            "The query to use for searching notes that should be short and optimized for search.",
                        ),
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

impl Default for NoteSearchTool {
    fn default() -> Self {
        Self::new("http://localhost:2222")
    }
}
