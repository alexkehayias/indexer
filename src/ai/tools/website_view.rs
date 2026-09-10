use std::path::PathBuf;
use std::time::Duration;

use crate::core::http::html_to_markdown;
use crate::openai::{Function, Parameters, Property, RecoverableToolError, ToolCall, ToolType, parse_tool_args};
use anyhow::{Context, Error, Result};
use async_trait::async_trait;
use super::registry::{Tool, ToolContext};
use serde::{Deserialize, Serialize};
use tokio::fs;

/// Maximum estimated token count before we truncate the output.
/// Using a rough estimate of ~4 characters per token.
const MAX_OUTPUT_TOKENS: usize = 10_000;
const CHARS_PER_TOKEN: usize = 4;
const MAX_OUTPUT_CHARS: usize = MAX_OUTPUT_TOKENS * CHARS_PER_TOKEN;

/// How many characters to keep in the truncated response returned to the AI.
const TRUNCATED_PREFIX_CHARS: usize = 7_500 * CHARS_PER_TOKEN;

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
    #[serde(skip_serializing)]
    workspace_path: PathBuf,
}

#[async_trait]
impl ToolCall for WebsiteViewTool {
    async fn call(&self, args: &str) -> Result<String, Error> {
        let fn_args: WebsiteViewArgs = parse_tool_args(args)?;
        // let url = fn_args.url;

        // Clean the URL, stripping away unnecessary URL params like
        // UTM codes. This breaks sites that rely on query params for
        // viewing the content but that's a fair tradeoff to prevent
        // accidental data leakage.
        let url = reqwest::Url::parse(fn_args.url.trim())
            .context(fn_args.url)
            .expect("Invalid URL");
        let host = url.host_str().expect("Missing host");
        let port = url
            .port()
            .map(|p| format!(":{}", p))
            .unwrap_or_default();
        let clean_url = format!("{}://{}{}{}", url.scheme(), host, port, url.path());

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
            .timeout(Duration::from_secs(30))
            .send()
            .await;

        // Handle request errors like timeouts
        let content = match response {
            Ok(resp) => {
                // Check for HTTP-level errors before processing the
                // body. reqwest does not treat non-2xx as errors by
                // default, so we need to check explicitly.
                let status = resp.status();
                if status.is_server_error() {
                    tracing::warn!("Website view failed with HTTP {}.", status);
                    return Err(RecoverableToolError::new(
                        &format!(
                            "Website view failed with HTTP {} (server error). The server may be temporarily unavailable — try again.",
                            status,
                        ),
                    )
                    .into());
                }
                if status.is_client_error() {
                    tracing::warn!("Website view failed with HTTP {}.", status);
                    return Ok(format!(
                        "Website view failed with HTTP status code {}",
                        status,
                    ));
                }

                // Convert HTML to markdown
                let html_content = resp.text().await?;
                html_to_markdown(&html_content)?
            }
            Err(e) => {
                // If the request failed, provide a default answer so we
                // don't crash the whole chat. For example: "Fetching the link
                // failed and due to a 500 status code"
                match e {
                    i if i.is_timeout() => {
                        tracing::warn!("Website view failed due to timeout.");
                        return Err(RecoverableToolError::new(
                            "Request timed out. The server may be slow or unavailable — try again or use a different source.",
                        )
                        .into());
                    }
                    i if i.is_request() => {
                        tracing::warn!("Website view failed due to request sending error.");
                        String::from("Request was not able to be sent. Do not retry.")
                    }
                    i if i.is_status() => {
                        // HTTP status errors are handled in the Ok(resp)
                        // branch above. This path is a safety net in case
                        // reqwest is configured to treat non-2xx as errors.
                        let msg = i
                            .status()
                            .map(|s| format!("Website view failed with HTTP status code {}", s))
                            .unwrap_or_else(|| format!("Website view failed: {}", i));
                        tracing::warn!("{}", msg);
                        String::from(msg)
                    }
                    _ => anyhow::bail!("Website view failed: {}", e),
                }
            }
        };

        // Truncate the content if it exceeds the maximum token count
        // to avoid filling the context window with noise. The full
        // content is written to a file in the session workspace so the
        // agent can read specific sections via the bash tool.
        if content.len() > MAX_OUTPUT_CHARS {
            // Ensure the workspace directory exists
            if !self.workspace_path.exists() {
                fs::create_dir_all(&self.workspace_path).await?;
            }

            let timestamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let file_path = self.workspace_path.join(format!("website_content_{}.md", timestamp));
            fs::write(&file_path, &content).await?;

            let truncated: String = content.chars().take(TRUNCATED_PREFIX_CHARS).collect();
            let estimated_total_tokens = content.len() / CHARS_PER_TOKEN;
            Ok(format!(
                "{}\n\n---\n**Results truncated.** Full content (est. ~{} tokens) written to `{}`. \
                 Use the bash tool to read specific sections from the file.",
                truncated, estimated_total_tokens, file_path.display(),
            ))
        } else {
            Ok(content)
        }
    }

    fn function_name(&self) -> String {
        self.function.name.clone()
    }
}

impl Tool for WebsiteViewTool {
    fn from_context(ctx: &ToolContext) -> Result<Self> {
        Ok(Self::new(&ctx.storage_path, &ctx.session_id))
    }
}

impl WebsiteViewTool {
    pub fn new(storage_path: &str, session_id: &str) -> Self {
        let workspace_path =
            PathBuf::from(format!("{}/workspace/{}", storage_path, session_id));

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
                        r#enum: None,
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
            workspace_path,
        }
    }
}

impl Default for WebsiteViewTool {
    fn default() -> Self {
        Self::new("/tmp", "default")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mockito::Server;
    use uuid::Uuid;

    fn test_tool() -> WebsiteViewTool {
        WebsiteViewTool::new("/tmp", &Uuid::new_v4().to_string())
    }

    #[tokio::test]
    async fn it_returns_ok_for_successful_response() {
        let mut server = Server::new_async().await;
        let _mock = server
            .mock("GET", "/")
            .with_status(200)
            .with_header("content-type", "text/html")
            .with_body("<html><body><h1>Hello World</h1></body></html>")
            .create();

        let tool = test_tool();
        let url = format!(r#"{{"url": "{}/"}}"#, server.url());
        let result = tool.call(&url).await;

        assert!(result.is_ok());
        let content = result.unwrap();
        assert!(content.contains("Hello World"));
    }

    #[tokio::test]
    async fn it_returns_recoverable_error_for_server_error() {
        let mut server = Server::new_async().await;
        let _mock = server
            .mock("GET", "/")
            .with_status(500)
            .with_header("content-type", "text/html")
            .with_body("Internal Server Error")
            .create();

        let tool = test_tool();
        let url = format!(r#"{{"url": "{}/"}}"#, server.url());
        let result = tool.call(&url).await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        let recoverable = err.downcast_ref::<RecoverableToolError>();
        assert!(recoverable.is_some());
        assert!(recoverable
            .unwrap()
            .message
            .contains("server error"));
    }

    #[tokio::test]
    async fn it_returns_ok_string_for_not_found() {
        let mut server = Server::new_async().await;
        let _mock = server
            .mock("GET", "/")
            .with_status(404)
            .with_header("content-type", "text/html")
            .with_body("Not Found")
            .create();

        let tool = test_tool();
        let url = format!(r#"{{"url": "{}/"}}"#, server.url());
        let result = tool.call(&url).await;

        assert!(result.is_ok());
        let content = result.unwrap();
        assert!(content.contains("HTTP status code 404"));
    }

    #[tokio::test]
    async fn it_returns_recoverable_error_for_service_unavailable() {
        let mut server = Server::new_async().await;
        let _mock = server
            .mock("GET", "/")
            .with_status(503)
            .with_header("content-type", "text/html")
            .with_body("Service Unavailable")
            .create();

        let tool = test_tool();
        let url = format!(r#"{{"url": "{}/"}}"#, server.url());
        let result = tool.call(&url).await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        let recoverable = err.downcast_ref::<RecoverableToolError>();
        assert!(recoverable.is_some());
        assert!(recoverable
            .unwrap()
            .message
            .contains("server error"));
    }
}
