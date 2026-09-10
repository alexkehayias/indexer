use crate::api::public::notes::SearchResponse;
use crate::openai::{Function, Parameters, ToolCall, ToolType};
use anyhow::{Error, Result};
use async_trait::async_trait;
use chrono::Utc;
use super::registry::{Tool, ToolContext};
use serde::{Deserialize, Serialize};

#[derive(Serialize)]
pub struct TasksDueTodayProps {}

#[derive(Deserialize)]
pub struct TasksDueTodayArgs {}

#[derive(Serialize)]
pub struct TasksDueTodayTool {
    pub r#type: ToolType,
    pub function: Function<TasksDueTodayProps>,
    api_base_url: String,
}

#[async_trait]
impl ToolCall for TasksDueTodayTool {
    async fn call(&self, _args: &str) -> Result<String, Error> {
        let today = Utc::now().format("%Y-%m-%d").to_string();

        // Build query: deadline:<TODAY> -status:done -status:canceled -title:journal
        let query = format!("deadline:<={} -status:done -status:canceled", today);

        let mut url = reqwest::Url::parse(&format!("{}/api/notes/search", self.api_base_url))
            .expect("Invalid URL");
        url.query_pairs_mut()
            .append_pair("query", &query)
            .append_pair("include_similarity", "false");

        let search_resp: SearchResponse = reqwest::Client::new()
            .get(url.as_str())
            .header("Content-Type", "application/json")
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;

        if search_resp.results.is_empty() {
            return Ok("No results found".to_string());
        }

        let mut accum = vec![];
        for r in search_resp.results.iter() {
            accum.push(format!("## {}\n{}\n{}", r.title, r.id, r.body))
        }

        Ok(accum.join("\n\n"))
    }

    fn function_name(&self) -> String {
        self.function.name.clone()
    }
}

impl Tool for TasksDueTodayTool {
    fn from_context(ctx: &ToolContext) -> Result<Self> {
        Ok(Self::new(&ctx.api_base_url))
    }
}

impl TasksDueTodayTool {
    pub fn new(api_base_url: &str) -> Self {
        let function = Function {
            name: String::from("tasks_due_today"),
            description: String::from(
                "Get a list of tasks that are due today, excluding done and canceled tasks.",
            ),
            parameters: Parameters {
                r#type: String::from("object"),
                properties: TasksDueTodayProps {},
                required: vec![],
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

impl Default for TasksDueTodayTool {
    fn default() -> Self {
        Self::new("http://localhost:2222")
    }
}

#[derive(Serialize)]
pub struct TasksScheduledTodayProps {}

#[derive(Deserialize)]
pub struct TasksScheduledTodayArgs {}

#[derive(Serialize)]
pub struct TasksScheduledTodayTool {
    pub r#type: ToolType,
    pub function: Function<TasksScheduledTodayProps>,
    api_base_url: String,
}

#[async_trait]
impl ToolCall for TasksScheduledTodayTool {
    async fn call(&self, _args: &str) -> Result<String, Error> {
        let today = Utc::now().format("%Y-%m-%d").to_string();

        // Build query: scheduled:<TODAY> -status:done -status:canceled -title:journal
        let query = format!("scheduled:<={} -status:done -status:canceled", today);

        let mut url = reqwest::Url::parse(&format!("{}/api/notes/search", self.api_base_url))
            .expect("Invalid URL");
        url.query_pairs_mut()
            .append_pair("query", &query)
            .append_pair("include_similarity", "false");

        let resp = reqwest::Client::new()
            .get(url.as_str())
            .header("Content-Type", "application/json")
            .send()
            .await?
            .error_for_status()?;

        let search_resp: SearchResponse = resp.json().await?;

        if search_resp.results.is_empty() {
            return Ok("No results found".to_string());
        }

        let mut accum = vec![];
        for r in search_resp.results.iter() {
            accum.push(format!("## {}\n{}\n{}", r.title, r.id, r.body))
        }

        Ok(accum.join("\n\n"))
    }

    fn function_name(&self) -> String {
        self.function.name.clone()
    }
}

impl Tool for TasksScheduledTodayTool {
    fn from_context(ctx: &ToolContext) -> Result<Self> {
        Ok(Self::new(&ctx.api_base_url))
    }
}

impl TasksScheduledTodayTool {
    pub fn new(api_base_url: &str) -> Self {
        let function = Function {
            name: String::from("tasks_scheduled_today"),
            description: String::from(
                "Get a list of tasks that are scheduled for today, excluding done and canceled tasks.",
            ),
            parameters: Parameters {
                r#type: String::from("object"),
                properties: TasksScheduledTodayProps {},
                required: vec![],
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

impl Default for TasksScheduledTodayTool {
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
    async fn it_gets_tasks_due_today() -> Result<()> {
        let mut server = mockito::Server::new_async().await;
        let url = server.url();

        let mock_resp = fs::read_to_string("./tests/data/tasks_search_response.json").unwrap();
        // The query includes today's date, so we need to match the pattern with regex
        let _mock = server
            .mock("GET", mockito::Matcher::Regex(r"/api/notes/search\?query=deadline%3A%3C%3D\d{4}-\d{2}-\d{2}\+-status%3Adone\+-status%3Acanceled&include_similarity=false".into()))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(mock_resp)
            .create();

        let tool = TasksDueTodayTool::new(&url);
        let result = tool.call("{}").await;
        assert!(result.is_ok());

        let output = result.unwrap();
        assert!(output.contains("## Complete project report"));
        assert!(output.contains("note-123"));
        assert!(output.contains("Write the final report for the quarterly project review"));
        assert!(output.contains("## Review pull requests"));
        assert!(output.contains("note-456"));

        Ok(())
    }

    #[tokio::test]
    async fn it_gets_tasks_scheduled_today() -> Result<()> {
        let mut server = mockito::Server::new_async().await;
        let url = server.url();

        let mock_resp = fs::read_to_string("./tests/data/tasks_search_response.json").unwrap();
        // The query includes today's date, so we need to match the pattern with regex
        let _mock = server
            .mock("GET", mockito::Matcher::Regex(r"/api/notes/search\?query=scheduled%3A%3C%3D\d{4}-\d{2}-\d{2}\+-status%3Adone\+-status%3Acanceled&include_similarity=false".into()))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(mock_resp)
            .create();

        let tool = TasksScheduledTodayTool::new(&url);
        let result = tool.call("{}").await;
        assert!(result.is_ok());

        let output = result.unwrap();
        assert!(output.contains("## Complete project report"));
        assert!(output.contains("note-123"));
        assert!(output.contains("Write the final report for the quarterly project review"));
        assert!(output.contains("## Review pull requests"));
        assert!(output.contains("note-456"));

        Ok(())
    }

    #[tokio::test]
    async fn it_handles_no_tasks_due_today() -> Result<()> {
        let mut server = mockito::Server::new_async().await;
        let url = server.url();

        let empty_resp = r#"{"raw_query": "", "parsed_query": "", "results": []}"#;
        let _mock = server
            .mock("GET", mockito::Matcher::Regex(r"/api/notes/search\?query=deadline%3A%3C%3D\d{4}-\d{2}-\d{2}\+-status%3Adone\+-status%3Acanceled&include_similarity=false".into()))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(empty_resp)
            .create();

        let tool = TasksDueTodayTool::new(&url);
        let result = tool.call("{}").await;
        assert!(result.is_ok());

        let output = result.unwrap();
        assert_eq!(output, "No results found");

        Ok(())
    }

    #[tokio::test]
    async fn it_handles_no_tasks_scheduled_today() -> Result<()> {
        let mut server = mockito::Server::new_async().await;
        let url = server.url();

        let empty_resp = r#"{"raw_query": "", "parsed_query": "", "results": []}"#;
        let _mock = server
            .mock("GET", mockito::Matcher::Regex(r"/api/notes/search\?query=scheduled%3A%3C%3D\d{4}-\d{2}-\d{2}\+-status%3Adone\+-status%3Acanceled&include_similarity=false".into()))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(empty_resp)
            .create();

        let tool = TasksScheduledTodayTool::new(&url);
        let result = tool.call("{}").await;
        assert!(result.is_ok());

        let output = result.unwrap();
        assert_eq!(output, "No results found");

        Ok(())
    }

    #[test]
    fn test_tasks_due_today_default() {
        let tool = TasksDueTodayTool::default();
        assert_eq!(tool.api_base_url, "http://localhost:2222");
        assert_eq!(tool.function_name(), "tasks_due_today");
    }

    #[test]
    fn test_tasks_scheduled_today_default() {
        let tool = TasksScheduledTodayTool::default();
        assert_eq!(tool.api_base_url, "http://localhost:2222");
        assert_eq!(tool.function_name(), "tasks_scheduled_today");
    }
}
