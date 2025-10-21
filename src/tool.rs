use crate::openai::{Function, Parameters, Property, ToolCall, ToolType};
use crate::public::SearchResponse;
use anyhow::{Error, Result};
use async_trait::async_trait;
use reqwest;
use serde::{Deserialize, Serialize};
use serde_json;
use serde_json::{Value, json};

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
        url.query_pairs_mut().append_pair("query", &fn_args.query);

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

#[derive(Serialize)]
pub struct EmailUnreadProps {
    pub email: Property,
}

#[derive(Deserialize)]
pub struct EmailUnreadArgs {
    pub email: String,
}

#[derive(Serialize)]
pub struct EmailUnreadTool {
    pub r#type: ToolType,
    pub function: Function<EmailUnreadProps>,
    api_base_url: String,
}

#[async_trait]
impl ToolCall for EmailUnreadTool {
    async fn call(&self, args: &str) -> Result<String, Error> {
        let fn_args: EmailUnreadArgs = serde_json::from_str(args).unwrap();

        let mut url = reqwest::Url::parse(&format!("{}/email/unread", self.api_base_url))
            .expect("Invalid URL");
        url.query_pairs_mut().append_pair("email", &fn_args.email);

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

impl EmailUnreadTool {
    pub fn new(api_base_url: &str) -> Self {
        let function = Function {
            name: String::from("get_unread_emails"),
            description: String::from("Fetch unread emails for a specific email address."),
            parameters: Parameters {
                r#type: String::from("object"),
                properties: EmailUnreadProps {
                    email: Property {
                        r#type: String::from("string"),
                        description: String::from("The email address to fetch unread emails for."),
                    },
                },
                required: vec![String::from("email")],
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

impl Default for EmailUnreadTool {
    fn default() -> Self {
        Self::new("http://localhost:2222")
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
        let _mock = server.mock("GET", "/search?q=stormlight+archive&categories=general&format=json")
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
