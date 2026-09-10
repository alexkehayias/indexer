use crate::ai::prompt::{self, Prompt};
use crate::api::public;
use crate::openai::{Function, Parameters, Property, ToolCall, ToolType, parse_tool_args};
use anyhow::{Error, Result};
use async_trait::async_trait;
use super::registry::{Tool, ToolContext};
use serde::{Deserialize, Serialize};
use serde_json::json;

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
    #[serde(skip_serializing)]
    api_base_url: String,
}

#[async_trait]
impl ToolCall for EmailUnreadTool {
    async fn call(&self, args: &str) -> Result<String, Error> {
        let fn_args: EmailUnreadArgs = parse_tool_args(args)?;

        let mut url = reqwest::Url::parse(&format!("{}/api/email/unread", self.api_base_url))
            .expect("Invalid URL");
        url.query_pairs_mut().append_pair("email", &fn_args.email);

        let resp = match reqwest::Client::new()
            .get(url.as_str())
            .header("Content-Type", "application/json")
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                return Ok(format!("Error fetching unread emails: {e}"))
            }
        };

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            let detail = if body.trim().is_empty() {
                status.to_string()
            } else {
                format!("{status}: {}", body.trim())
            };
            return Ok(format!(
                "Error fetching unread emails: the hq server returned an error ({detail})"
            ));
        }

        let email_threads: Vec<public::email::EmailThread> = match resp.json().await {
            Ok(threads) => threads,
            Err(e) => {
                return Ok(format!(
                    "Error fetching unread emails: failed to parse response as JSON: {e}"
                ))
            }
        };

        let templates = prompt::templates();
        let content = templates.render(
            &Prompt::UnreadEmails.to_string(),
            &json!({"email_threads": email_threads}),
        )?;

        Ok(content.trim().to_string())
    }

    fn function_name(&self) -> String {
        self.function.name.clone()
    }
}

impl Tool for EmailUnreadTool {
    fn from_context(ctx: &ToolContext) -> Result<Self> {
        Ok(Self::new(&ctx.api_base_url))
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
                        r#enum: None,
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

#[derive(Serialize)]
pub struct EmailSearchProps {
    pub email: Property,
    pub query: Property,
}

#[derive(Deserialize)]
pub struct EmailSearchArgs {
    pub email: String,
    pub query: String,
}

#[derive(Serialize)]
pub struct EmailSearchTool {
    pub r#type: ToolType,
    pub function: Function<EmailSearchProps>,
    #[serde(skip_serializing)]
    api_base_url: String,
}

#[async_trait]
impl ToolCall for EmailSearchTool {
    async fn call(&self, args: &str) -> Result<String, Error> {
        let fn_args: EmailSearchArgs = parse_tool_args(args)?;

        let mut url = reqwest::Url::parse(&format!("{}/api/email/search", self.api_base_url))
            .expect("Invalid URL");
        url.query_pairs_mut()
            .append_pair("email", &fn_args.email)
            .append_pair("query", &fn_args.query);

        let resp = match reqwest::Client::new()
            .get(url.as_str())
            .header("Content-Type", "application/json")
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => return Ok(format!("Error searching emails: {e}")),
        };

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            let detail = if body.trim().is_empty() {
                status.to_string()
            } else {
                format!("{status}: {}", body.trim())
            };
            return Ok(format!(
                "Error searching emails: the hq server returned an error ({detail})"
            ));
        }

        let email_threads: Vec<public::email::EmailThread> = match resp.json().await {
            Ok(threads) => threads,
            Err(e) => {
                return Ok(format!(
                    "Error searching emails: failed to parse response as JSON: {e}"
                ))
            }
        };

        let templates = prompt::templates();
        let content = templates.render(
            &Prompt::EmailSearch.to_string(),
            &json!({"email_threads": email_threads}),
        )?;

        Ok(content.trim().to_string())
    }

    fn function_name(&self) -> String {
        self.function.name.clone()
    }
}

impl Tool for EmailSearchTool {
    fn from_context(ctx: &ToolContext) -> Result<Self> {
        Ok(Self::new(&ctx.api_base_url))
    }
}

impl EmailSearchTool {
    pub fn new(api_base_url: &str) -> Self {
        let function = Function {
            name: String::from("search_emails"),
            description: String::from(
                "Search emails for a specific email address using Gmail search syntax. \
                 The query supports operators like from:, to:, subject:, has:attachment, \
                 before:/after: dates, and boolean operators (AND, OR, NOT).",
            ),
            parameters: Parameters {
                r#type: String::from("object"),
                properties: EmailSearchProps {
                    email: Property {
                        r#type: String::from("string"),
                        description: String::from(
                            "The email address of the account to search.",
                        ),
                        r#enum: None,
                    },
                    query: Property {
                        r#type: String::from("string"),
                        description: String::from(
                            "Gmail search query. Examples: 'from:alice@example.com', \
                             'subject:invoice has:attachment', 'budget OR forecast'.",
                        ),
                        r#enum: None,
                    },
                },
                required: vec![String::from("email"), String::from("query")],
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

impl Default for EmailSearchTool {
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
    async fn it_fetches_unread_emails() -> Result<()> {
        let mut server = mockito::Server::new_async().await;
        let url = server.url();

        let mock_resp = fs::read_to_string("./tests/data/email_unread_response.json").unwrap();
        let _mock = server
            .mock("GET", "/api/email/unread?email=test%40example.com")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(mock_resp)
            .create();

        let tool = EmailUnreadTool::new(&url);
        let args = r#"{"email": "test@example.com"}"#;
        let actual = tool.call(args).await;
        assert!(actual.is_ok());

        let expected = "The following is a list of unread emails and their related email thread in reverse chronological order.\n\n# Unread Emails\n\n## Project kickoff meeting\n\n**ID:** thr_001\n**From:** alice@example.com\n**To:** bob@example.org\n**Subject:** Project kickoff meeting\n\n### Message 1\n\n**From:** alice@example.com\n**To:** bob@example.org\n**Date:** 2024-11-12T08:15:23Z\n**Subject:** Project kickoff meeting\n**Body:**\nHi Bob,\n\nCan we schedule a quick call tomorrow to go over the project kickoff agenda? Let me know what time works for you.\n\nThanks,\nAlice\n\n---\n\n### Message 2\n\n**From:** bob@example.org\n**To:** alice@example.com\n**Date:** 2024-11-12T09:02:10Z\n**Subject:** Re: Project kickoff meeting\n**Body:**\nHey Alice,\n\nSure thing – I’m free at 10AM PST tomorrow. Does that work?\n\nBest,\nBob\n\n---\n\n### Message 3\n\n**From:** alice@example.com\n**To:** bob@example.org\n**Date:** 2024-11-12T09:15:44Z\n**Subject:** Re: Project kickoff meeting\n**Body:**\n10AM PST works perfectly. I’ll send a calendar invite shortly.\n\nCheers,\nAlice\n\n---\n\n\n## Quarterly budget review – documents attached\n\n**ID:** thr_002\n**From:** carol@workplace.com\n**To:** dave@workplace.com, erin@workplace.com\n**Subject:** Quarterly budget review – documents attached\n\n### Message 1\n\n**From:** carol@workplace.com\n**To:** dave@workplace.com, erin@workplace.com\n**Date:** 2024-11-10T14:42:07Z\n**Subject:** Quarterly budget review – documents attached\n**Body:**\nHi team,\n\nPlease find the Q3 budget spreadsheet and the executive summary attached. Let me know if you have any questions before our meeting on Friday.\n\nThanks,\nCarol\n\n---\n\n### Message 2\n\n**From:** erin@workplace.com\n**To:** carol@workplace.com, dave@workplace.com\n**Date:** 2024-11-10T15:08:33Z\n**Subject:** Re: Quarterly budget review – documents attached\n**Body:**\nThanks Carol. I’ve reviewed the numbers and have a few comments on line 42 – can we discuss that during the call?\n\nErin\n\n---\n\n\n## Your weekly tech roundup –  Nov 1-7\n\n**ID:** thr_003\n**From:** no-reply@newsletter.com\n**To:** you@example.net\n**Subject:** Your weekly tech roundup –  Nov 1-7\n\n### Message 1\n\n**From:** no-reply@newsletter.com\n**To:** you@example.net\n**Date:** 2024-11-01T07:30:55Z\n**Subject:** Your weekly tech roundup –  Nov 1-7\n**Body:**\nHello,\n\nHere’s what happened in the world of tech this week:\n\n• Rust 2.0 beta released…\n• New AI model beats GPT-4 on benchmarks…\n• Chrome 129 ships with built-in password manager…\n\nRead more at https://newsletter.com/weekly/2024-11-01\n\nIf you’d like to unsubscribe, click here.\n\n---";
        assert_eq!(expected, actual.unwrap());

        Ok(())
    }

    #[tokio::test]
    async fn it_searches_emails() -> Result<()> {
        let mut server = mockito::Server::new_async().await;
        let url = server.url();

        let mock_resp = fs::read_to_string("./tests/data/email_search_response.json").unwrap();
        let _mock = server
            .mock("GET", "/api/email/search")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(mock_resp)
            .match_query(mockito::Matcher::Regex(
                r"email=test%40example\.com.*query=from%3Aalice".to_string(),
            ))
            .create();

        let tool = EmailSearchTool::new(&url);
        let args = r#"{"email": "test@example.com", "query": "from:alice"}"#;
        let actual = tool.call(args).await;
        assert!(actual.is_ok());

        let expected = "The following is a list of emails matching the search query and their related email threads in reverse chronological order.\n\n# Email Search Results\n\n## Project kickoff meeting\n\n**ID:** thr_001\n**From:** alice@example.com\n**To:** bob@example.org\n**Subject:** Project kickoff meeting\n\n### Message 1\n\n**From:** alice@example.com\n**To:** bob@example.org\n**Date:** 2024-11-12T08:15:23Z\n**Subject:** Project kickoff meeting\n**Body:**\nHi Bob,\n\nCan we schedule a quick call tomorrow to go over the project kickoff agenda? Let me know what time works for you.\n\nThanks,\nAlice\n\n---\n\n### Message 2\n\n**From:** bob@example.org\n**To:** alice@example.com\n**Date:** 2024-11-12T09:02:10Z\n**Subject:** Re: Project kickoff meeting\n**Body:**\nHey Alice,\n\nSure thing – I’m free at 10AM PST tomorrow. Does that work?\n\nBest,\nBob\n\n---\n\n### Message 3\n\n**From:** alice@example.com\n**To:** bob@example.org\n**Date:** 2024-11-12T09:15:44Z\n**Subject:** Re: Project kickoff meeting\n**Body:**\n10AM PST works perfectly. I’ll send a calendar invite shortly.\n\nCheers,\nAlice\n\n---\n\n\n## Quarterly budget review – documents attached\n\n**ID:** thr_002\n**From:** carol@workplace.com\n**To:** dave@workplace.com, erin@workplace.com\n**Subject:** Quarterly budget review – documents attached\n\n### Message 1\n\n**From:** carol@workplace.com\n**To:** dave@workplace.com, erin@workplace.com\n**Date:** 2024-11-10T14:42:07Z\n**Subject:** Quarterly budget review – documents attached\n**Body:**\nHi team,\n\nPlease find the Q3 budget spreadsheet and the executive summary attached. Let me know if you have any questions before our meeting on Friday.\n\nThanks,\nCarol\n\n---\n\n### Message 2\n\n**From:** erin@workplace.com\n**To:** carol@workplace.com, dave@workplace.com\n**Date:** 2024-11-10T15:08:33Z\n**Subject:** Re: Quarterly budget review – documents attached\n**Body:**\nThanks Carol. I’ve reviewed the numbers and have a few comments on line 42 – can we discuss that during the call?\n\nErin\n\n---\n\n\n## Your weekly tech roundup –  Nov 1-7\n\n**ID:** thr_003\n**From:** no-reply@newsletter.com\n**To:** you@example.net\n**Subject:** Your weekly tech roundup –  Nov 1-7\n\n### Message 1\n\n**From:** no-reply@newsletter.com\n**To:** you@example.net\n**Date:** 2024-11-01T07:30:55Z\n**Subject:** Your weekly tech roundup –  Nov 1-7\n**Body:**\nHello,\n\nHere’s what happened in the world of tech this week:\n\n• Rust 2.0 beta released…\n• New AI model beats GPT-4 on benchmarks…\n• Chrome 129 ships with built-in password manager…\n\nRead more at https://newsletter.com/weekly/2024-11-01\n\nIf you’d like to unsubscribe, click here.\n\n---";
        assert_eq!(expected, actual.unwrap());

        Ok(())
    }

    #[tokio::test]
    async fn it_returns_helpful_message_on_server_error() -> Result<()> {
        let mut server = mockito::Server::new_async().await;
        let url = server.url();

        let _mock = server
            .mock("GET", "/api/email/unread?email=test%40example.com")
            .with_status(500)
            .with_header("content-type", "application/json")
            .with_body("Something went wrong: token refresh failed")
            .create();

        let tool = EmailUnreadTool::new(&url);
        let args = r#"{"email": "test@example.com"}"#;
        let actual = tool.call(args).await;

        let msg = actual.expect("server errors should return Ok, not throw");
        assert!(
            msg.contains("token refresh failed"),
            "message should surface the server error, got: {msg}"
        );

        Ok(())
    }
}
