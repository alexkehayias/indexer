use crate::openai::{Function, Parameters, Property, ToolCall, ToolType};
use anyhow::{Context, Error, Result};
use async_trait::async_trait;
use htmd::HtmlToMarkdown;
use http::StatusCode;
use reqwest;
use serde::{Deserialize, Serialize};

#[derive(Serialize)]
pub struct WebsiteViewProps {
    pub url: Property,
}

#[derive(Deserialize)]
pub struct WebsiteViewArgs {
    pub url: String,
}

#[derive(Serialize)]
pub struct WebsiteViewTool {
    pub r#type: ToolType,
    pub function: Function<WebsiteViewProps>,
}

#[async_trait]
impl ToolCall for WebsiteViewTool {
    async fn call(&self, args: &str) -> Result<String, Error> {
        let fn_args: WebsiteViewArgs = serde_json::from_str(args).unwrap();
        // let url = fn_args.url;

        // Clean the URL, stripping away unnecessary URL params like
        // UTM codes. This breaks sites that rely on query params for
        // viewing the content but that's a fair tradeoff to prevent
        // accidental data leakage.
        let url = reqwest::Url::parse(fn_args.url.trim())
            .context(fn_args.url)
            .expect("Invalid URL");
        let clean_url = format!(
            "{}://{}{}",
            url.scheme(),
            url.host_str().expect("Missing host"),
            url.path()
        );

        // TODO: Rewrite URLs based on rules. For example, use mirrors
        // or archives for certain sites.

        // TODO: Validate the URL is acceptable to view given the AI
        // agent's context. This partially mitigates prompt injection
        // attacks by constraining the set of possible websites that
        // can be requested.
        // Does this matter if we only allow GET requests and no
        // params?

        // Fetch the HTML content from the URL
        let response = reqwest::Client::new()
            .get(&clean_url)
            .send()
            .await;

        // Handle request errors like timeouts
        let content = match response {
            Ok(resp) => {
                // Convert HTML to markdown
                let html_content = resp.text().await?;
                let converter = HtmlToMarkdown::builder()
                    .skip_tags(vec!["script", "style", "footer", "img", "svg"])
                    .build();
                converter.convert(&html_content)?
            }
            Err(e) => {
                // If the request failed, provide a default answer so we
                // don't crash the whole chat. For example: "Fetching the link
                // failed and due to a 500 status code"
                match e {
                    i if i.is_timeout() => {
                        tracing::warn!("Website view failed due to timeout.");
                        String::from("Request timed out.")
                    },
                    i if i.is_request() => {
                        tracing::warn!("Website view failed due to request sending error.");
                        String::from("Request was not able to be sent. Do not retry.")
                    }
                    i if i.is_status() => {
                        if let Some(status) = i.status() {
                            match status {
                                StatusCode::BAD_REQUEST => {
                                    tracing::warn!("Website view failed due to bad request.");
                                    String::from("Website view failed due to bad request.")
                                }
                                StatusCode::NOT_FOUND => {
                                    tracing::warn!("Website view failed because the page was not found.");
                                    String::from("Website view failed because the page was not found.")
                                }
                                _ => {
                                    tracing::warn!("Website view failed with HTTP status code {}.", status);
                                    format!("Website view failed with HTTP status code {}", status)
                                }
                            }
                        } else {
                            // Not sure how we could end up here with an error
                            // that doesn't have a status code, but need to
                            // make the compiler happy
                            anyhow::bail!("Website view failed: {}", i)
                        }
                    },
                    _ => anyhow::bail!("Website view failed: {}", e)
                }
            }
        };

        // TODO: Limit the amount of content returned to avoid filling
        // the context window with noise.
        Ok(content)
    }

    fn function_name(&self) -> String {
        self.function.name.clone()
    }
}

impl WebsiteViewTool {
    pub fn new() -> Self {
        let function = Function {
            name: String::from("view_website"),
            description: String::from(
                "Fetch and convert a website's content to markdown for viewing.",
            ),
            parameters: Parameters {
                r#type: String::from("object"),
                properties: WebsiteViewProps {
                    url: Property {
                        r#type: String::from("string"),
                        description: String::from(
                            "The URL of the website to fetch and convert to markdown.",
                        ),
                    },
                },
                required: vec![String::from("url")],
                additional_properties: false,
            },
            strict: true,
        };
        Self {
            r#type: ToolType::Function,
            function,
        }
    }
}

impl Default for WebsiteViewTool {
    fn default() -> Self {
        Self::new()
    }
}
