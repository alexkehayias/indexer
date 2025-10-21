use crate::openai::{Function, Parameters, Property, ToolCall, ToolType};
use crate::public::CalendarResponse;
use anyhow::{Error, Result};
use async_trait::async_trait;
use reqwest;
use serde::{Deserialize, Serialize};
use serde_json;

#[derive(Serialize)]
pub struct CalendarProps {
    pub email: Property,
    pub days_ahead: Property,
    pub calendar_id: Property,
}

#[derive(Deserialize)]
pub struct CalendarArgs {
    pub email: String,
    pub days_ahead: Option<i64>,
    pub calendar_id: Option<String>,
}

#[derive(Serialize)]
pub struct CalendarTool {
    pub r#type: ToolType,
    pub function: Function<CalendarProps>,
    api_base_url: String,
}

#[async_trait]
impl ToolCall for CalendarTool {
    async fn call(&self, args: &str) -> Result<String, Error> {
        let fn_args: CalendarArgs = serde_json::from_str(args).unwrap();

        let mut url =
            reqwest::Url::parse(&format!("{}/calendar", self.api_base_url)).expect("Invalid URL");

        url.query_pairs_mut().append_pair("email", &fn_args.email);

        if let Some(days_ahead) = fn_args.days_ahead {
            url.query_pairs_mut()
                .append_pair("days_ahead", &days_ahead.to_string());
        }

        if let Some(calendar_id) = fn_args.calendar_id {
            url.query_pairs_mut()
                .append_pair("calendar_id", &calendar_id);
        }

        let resp = reqwest::Client::new()
            .get(url.as_str())
            .header("Content-Type", "application/json")
            .send()
            .await?
            .error_for_status()?;

        let calendar_resp: Vec<CalendarResponse> = resp.json().await?;

        let mut accum = vec![];
        for event in calendar_resp.iter() {
            let attendees_str = if let Some(attendees) = &event.attendees {
                let attendee_list: Vec<String> = attendees
                    .iter()
                    .map(|a| {
                        format!(
                            "{} <{}>",
                            a.display_name.clone().unwrap_or("No name".to_string()),
                            a.email
                        )
                    })
                    .collect();
                if attendee_list.is_empty() {
                    "No attendees".to_string()
                } else {
                    format!("Attendees: {}", attendee_list.join(", "))
                }
            } else {
                "No attendees".to_string()
            };

            accum.push(format!(
                "## {}\nStart: {}\nEnd: {}\n{}\n",
                event.summary, event.start, event.end, attendees_str
            ))
        }

        let out = accum.join("\n\n");
        Ok(out)
    }

    fn function_name(&self) -> String {
        self.function.name.clone()
    }
}

impl CalendarTool {
    pub fn new(api_base_url: &str) -> Self {
        let function = Function {
            name: String::from("get_calendar_events"),
            description: String::from("Fetch upcoming calendar events for a user."),
            parameters: Parameters {
                r#type: String::from("object"),
                properties: CalendarProps {
                    email: Property {
                        r#type: String::from("string"),
                        description: String::from(
                            "The email address associated with the Google account to fetch calendar events from.",
                        ),
                    },
                    days_ahead: Property {
                        r#type: String::from("integer"),
                        description: String::from(
                            "Number of days ahead to fetch events for (default is 7).",
                        ),
                    },
                    calendar_id: Property {
                        r#type: String::from("string"),
                        description: String::from(
                            "The calendar ID to fetch events from (default is 'primary').",
                        ),
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

impl Default for CalendarTool {
    fn default() -> Self {
        Self::new("http://localhost:2222")
    }
}
