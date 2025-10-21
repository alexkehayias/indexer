use crate::openai::{Function, Parameters, Property, ToolCall, ToolType};
use anyhow::{Error, Result};
use async_trait::async_trait;
use reqwest;
use serde::{Deserialize, Serialize};
use serde_json;

#[derive(Serialize)]
pub struct SearxSearchProps {
    pub query: Property,
    pub categories: Property,
}

#[derive(Deserialize)]
pub struct SearxSearchArgs {
    pub query: String,
    pub categories: String,
}

#[derive(Serialize)]
pub struct SearxSearchTool {
    pub r#type: ToolType,
    pub function: Function<SearxSearchProps>,
    api_base_url: String,
}

#[async_trait]
impl ToolCall for SearxSearchTool {
    async fn call(&self, args: &str) -> Result<String, Error> {
        let fn_args: SearxSearchArgs = serde_json::from_str(args).unwrap();
        let query_url = reqwest::Url::parse_with_params(
            &format!("{}/search", self.api_base_url),
            &[
                ("q", fn_args.query),
                ("categories", fn_args.categories),
                ("format", "json".to_string()),
            ],
        )?;

        let resp = reqwest::get(query_url).await?;
        let json_resp = resp.json::<serde_json::Value>().await?;

        // Reduce the size of the search output by removing unused
        // fields and shortening snippets
        // TODO: Handle if results are empty
        // TODO: Parse into a struct
        let results = json_resp["results"].as_array().unwrap();

        let mut accum = vec![];
        for r in results {
            let url = r["url"].as_str().unwrap();
            let title = r["title"].as_str().unwrap();
            let content = r["content"].as_str().unwrap();
            // TODO: Check if content is too long
            accum.push(format!("## {}\n{}\n{}", title, url, content))
        }

        let out = accum.join("\n\n");
        Ok(out)
    }

    fn function_name(&self) -> String {
        self.function.name.clone()
    }
}

impl SearxSearchTool {
    pub fn new(api_base_url: &str) -> Self {
        let function = Function {
            name: String::from("search_searx"),
            description: String::from("Perform a search using the SearxNG API."),
            parameters: Parameters {
                r#type: String::from("object"),
                properties: SearxSearchProps {
                    query: Property {
                        r#type: String::from("string"),
                        description: String::from(
                            "The search query string. Allowed categories include general, images, videos, \
                            news, map, music, it, science, files, social media.",
                        ),
                    },
                    categories: Property {
                        r#type: String::from("string"),
                        description: String::from("Optional categories for filtering the search."),
                    },
                },
                required: vec!["query".into(), "categories".into()],
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

impl Default for SearxSearchTool {
    fn default() -> Self {
        Self::new("http://localhost:8080")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;
    use std::fs;

    #[tokio::test]
    async fn it_searches_searxng() -> Result<()> {
        let mut server = mockito::Server::new_async().await;
        let url = server.url();

        let mock_resp = fs::read_to_string("./tests/data/searxng_search_results.json").unwrap();
        let _mock = server
            .mock(
                "GET",
                "/search?q=stormlight+archive&categories=general&format=json",
            )
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(mock_resp)
            .create();

        let tool = SearxSearchTool::new(&url);
        let args = r#"{"query": "stormlight archive", "categories": "general"}"#;
        let result = tool.call(args).await;
        assert!(result.is_ok() || result.is_err());

        let output = result.unwrap();
        assert!(output.starts_with("## The Stormlight Archive - Wikipedia\nhttps://en.wikipedia.org/wiki/The_Stormlight_Archive\n2 days ago - The Stormlight Archive is a high fantasy novel series written by American author Brandon Sanderson, planned to consist of ten novels. As of 2024, the series comprises five published novels and two novellas, set within his broader Cosmere universe. The first novel, The Way of Kings, was published ...\n\n"));

        Ok(())
    }
}
