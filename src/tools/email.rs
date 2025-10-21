use crate::openai::{Function, Parameters, Property, ToolCall, ToolType};
use anyhow::{Error, Result};
use async_trait::async_trait;
use reqwest;
use serde::{Deserialize, Serialize};
use serde_json;
use serde_json::{Value, json};

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
