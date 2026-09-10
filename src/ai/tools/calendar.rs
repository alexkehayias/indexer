use crate::api::public::calendar::CalendarResponse;
use crate::openai::{Function, Parameters, Property, ToolCall, ToolType, parse_tool_args};
use anyhow::{Error, Result};
use async_trait::async_trait;
use super::registry::{Tool, ToolContext};
use serde::{Deserialize, Serialize};
use tokio_rusqlite::Connection;

#[derive(Serialize)]
pub struct CalendarProps {
    pub days_ahead: Property,
    pub calendar_id: Property,
}

#[derive(Deserialize)]
pub struct CalendarArgs {
    pub days_ahead: Option<i64>,
    pub calendar_id: Option<String>,
}

#[derive(Serialize)]
pub struct CalendarTool {
    pub r#type: ToolType,
    pub function: Function<CalendarProps>,
    #[serde(skip)]
    api_base_url: String,
    #[serde(skip)]
    db: Connection,
}

#[async_trait]
impl ToolCall for CalendarTool {
    async fn call(&self, args: &str) -> Result<String, Error> {
        let fn_args: CalendarArgs = parse_tool_args(args)?;

        // Get all authorized email addresses from the database
        let emails: Vec<String> = self
            .db
            .call(|conn| {
                let mut stmt = conn.prepare("SELECT id FROM auth WHERE service = 'gmail'")?;
                let rows = stmt.query_map([], |row| row.get(0))?;
                let mut emails = Vec::new();
                for email in rows {
                    emails.push(email?);
                }
                Ok(emails)
            })
            .await?;

        if emails.is_empty() {
            return Ok("No authorized calendar accounts found.".to_string());
        }

        let mut all_events = vec![];

        for email in emails {
            // Build URL for this email
            let mut url = reqwest::Url::parse(&format!("{}/api/calendar", self.api_base_url))
                .expect("Invalid URL");

            url.query_pairs_mut().append_pair("email", &email);

            if let Some(days_ahead) = fn_args.days_ahead {
                url.query_pairs_mut()
                    .append_pair("days_ahead", &days_ahead.to_string());
            }

            if let Some(calendar_id) = fn_args.calendar_id.clone() {
                url.query_pairs_mut()
                    .append_pair("calendar_id", &calendar_id);
            }

            let resp = match reqwest::Client::new()
                .get(url.as_str())
                .header("Content-Type", "application/json")
                .send()
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    return Ok(format!("Error fetching calendar events: {e}"))
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
                    "Error fetching calendar events: the hq server returned an error ({detail})"
                ));
            }

            let calendar_resp: Vec<CalendarResponse> = match resp.json().await {
                Ok(events) => events,
                Err(e) => {
                    return Ok(format!(
                        "Error fetching calendar events: failed to parse response as JSON: {e}"
                    ))
                }
            };

            for event in calendar_resp {
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

                all_events.push(format!(
                    "## {}\nStart: {}\nEnd: {}\n{}\n",
                    event.summary, event.start, event.end, attendees_str
                ))
            }
        }

        let out = all_events.join("\n\n");
        Ok(out)
    }

    fn function_name(&self) -> String {
        self.function.name.clone()
    }
}

impl Tool for CalendarTool {
    fn from_context(ctx: &ToolContext) -> Result<Self> {
        Ok(Self::new(ctx.db.clone(), &ctx.api_base_url))
    }
}

impl CalendarTool {
    pub fn new(db: Connection, api_base_url: &str) -> Self {
        let function = Function {
            name: String::from("get_calendar_events"),
            description: String::from(
                "Fetch upcoming calendar events for all authorized accounts.",
            ),
            parameters: Parameters {
                r#type: String::from("object"),
                properties: CalendarProps {
                    days_ahead: Property {
                        r#type: String::from("integer"),
                        description: String::from(
                            "Number of days ahead to fetch events for (default is 7).",
                        ),
                        r#enum: None,
                    },
                    calendar_id: Property {
                        r#type: String::from("string"),
                        description: String::from(
                            "The calendar ID to fetch events from (default is 'primary').",
                        ),
                        r#enum: None,
                    },
                },
                required: vec![],
                additional_properties: false,
            },
            strict: true,
        };

        Self {
            r#type: ToolType::Function,
            function,
            api_base_url: api_base_url.to_string(),
            db,
        }
    }
}
