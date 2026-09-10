use crate::openai::{Function, Parameters, Property, RecoverableToolError, ToolCall, ToolType, parse_tool_args};
use anyhow::{Error, Result};
use super::registry::{Tool, ToolContext};
use async_trait::async_trait;
use chrono::{Days, Months, NaiveDate, NaiveDateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum DateTimeOperation {
    CurrentTime,
    AddDuration,
    TimeUntil,
}

#[derive(Deserialize)]
struct DateTimeArgs {
    operation: DateTimeOperation,
    date: Option<String>,
    days: Option<i64>,
    months: Option<i32>,
    target_datetime: Option<String>,
}

#[derive(Serialize)]
pub struct DateTimeProps {
    pub operation: Property,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub date: Option<Property>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub days: Option<Property>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub months: Option<Property>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_datetime: Option<Property>,
}

#[derive(Serialize)]
pub struct DateTimeTool {
    pub r#type: ToolType,
    pub function: Function<DateTimeProps>,
}

#[async_trait]
impl ToolCall for DateTimeTool {
    async fn call(&self, args: &str) -> Result<String, Error> {
        let fn_args: DateTimeArgs = parse_tool_args(args)?;

        match fn_args.operation {
            DateTimeOperation::CurrentTime => Ok(current_time()),
            DateTimeOperation::AddDuration => add_duration(fn_args.date, fn_args.days, fn_args.months),
            DateTimeOperation::TimeUntil => time_until(fn_args.target_datetime, fn_args.date),
        }
    }

    fn function_name(&self) -> String {
        self.function.name.clone()
    }
}

impl Tool for DateTimeTool {
    fn from_context(_ctx: &ToolContext) -> Result<Self> {
        Ok(Self::new())
    }
}

impl DateTimeTool {
    pub fn new() -> Self {
        let function = Function {
            name: String::from("datetime"),
            description: String::from(
                "Get the current date and time, perform calendar math (add days/months to a date), or calculate the duration until a target date/time."
            ),
            parameters: Parameters {
                r#type: String::from("object"),
                properties: DateTimeProps {
                    operation: Property {
                        r#type: String::from("string"),
                        description: String::from(
                            "The operation to perform: 'current_time' (get current datetime), 'add_duration' (add days/months to a date), or 'time_until' (calculate time until a target)."
                        ),
                        r#enum: Some(vec![
                            String::from("current_time"),
                            String::from("add_duration"),
                            String::from("time_until"),
                        ]),
                    },
                    date: Some(Property {
                        r#type: String::from("string"),
                        description: String::from(
                            "Base date in ISO 8601 format (e.g. '2026-05-23'). Defaults to today. Used by 'add_duration' and 'time_until'."
                        ),
                        r#enum: None,
                    }),
                    days: Some(Property {
                        r#type: String::from("integer"),
                        description: String::from("Number of days to add (for 'add_duration' operation). Can be negative to subtract days."),
                        r#enum: None,
                    }),
                    months: Some(Property {
                        r#type: String::from("integer"),
                        description: String::from("Number of months to add (for 'add_duration' operation). Can be negative to subtract months."),
                        r#enum: None,
                    }),
                    target_datetime: Some(Property {
                        r#type: String::from("string"),
                        description: String::from(
                            "Target date or datetime in ISO 8601 format (e.g. '2026-12-25' or '2026-12-25T15:30:00'). Used by 'time_until' operation."
                        ),
                        r#enum: None,
                    }),
                },
                required: vec![String::from("operation")],
                additional_properties: false,
            },
            strict: false,
        };
        Self {
            r#type: ToolType::Function,
            function,
        }
    }
}

impl Default for DateTimeTool {
    fn default() -> Self {
        Self::new()
    }
}

fn current_time() -> String {
    let now = Utc::now();
    let day_of_week = now.format("%A").to_string();
    let iso = now.format("%Y-%m-%dT%H:%M:%SZ").to_string();
    format!(
        "Current time: {}\nDay of week: {}\nUnix timestamp: {}",
        iso,
        day_of_week,
        now.timestamp()
    )
}

fn add_duration(date: Option<String>, days: Option<i64>, months: Option<i32>) -> Result<String, Error> {
    let base_date = parse_date_or_today(date)?;

    let after_months = match months {
        Some(m) if m > 0 => base_date.checked_add_months(Months::new(m as u32)),
        Some(m) => base_date.checked_sub_months(Months::new(m.unsigned_abs())),
        None => Some(base_date),
    }
    .ok_or_else(|| anyhow::Error::from(RecoverableToolError::new("Date overflow when adding months")))?;

    let result = match days {
        Some(d) if d >= 0 => after_months
            .checked_add_days(Days::new(d as u64))
            .ok_or_else(|| anyhow::Error::from(RecoverableToolError::new("Date overflow when adding days"))),
        Some(d) => after_months
            .checked_sub_days(Days::new(d.unsigned_abs()))
            .ok_or_else(|| anyhow::Error::from(RecoverableToolError::new("Date overflow when subtracting days"))),
        None => Ok(after_months),
    }?;

    Ok(format!("Result: {}", result.format("%Y-%m-%d")))
}

fn time_until(target: Option<String>, date: Option<String>) -> Result<String, Error> {
    let target_str = target.ok_or_else(|| {
        anyhow::Error::from(RecoverableToolError::new(
            "target_datetime is required for 'time_until' operation",
        ))
    })?;

    let target_dt = parse_datetime(&target_str)?;
    let now = match date {
        Some(d) => {
            let base = parse_date_or_today(Some(d))?;
            base.and_hms_opt(0, 0, 0)
                .ok_or_else(|| {
                    anyhow::Error::from(RecoverableToolError::new("Invalid time conversion"))
                })?
        }
        None => Utc::now().naive_utc(),
    };

    let (diff, preposition) = if target_dt > now {
        (target_dt - now, "until")
    } else {
        (now - target_dt, "since")
    };

    let total_days = diff.num_days();
    let remaining_hours = diff.num_hours() - total_days * 24;
    let remaining_minutes = diff.num_minutes() - diff.num_hours() * 60;

    Ok(format!(
        "{} days, {} hours, {} minutes {} {}",
        total_days, remaining_hours, remaining_minutes, preposition, target_str
    ))
}

fn parse_date_or_today(date: Option<String>) -> Result<NaiveDate, Error> {
    match date {
        Some(d) => {
            let d = d.trim();
            NaiveDate::parse_from_str(d, "%Y-%m-%d").map_err(|_| {
                anyhow::Error::from(RecoverableToolError::new(&format!(
                    "Invalid date format '{}'. Expected ISO 8601 format: YYYY-MM-DD (e.g. 2026-05-23)",
                    d
                )))
            })
        }
        None => Ok(Utc::now().date_naive()),
    }
}

fn parse_datetime(s: &str) -> Result<NaiveDateTime, Error> {
    let s = s.trim();
    // Try full datetime format first
    if let Ok(dt) = NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S") {
        return Ok(dt);
    }
    // Try date-only format (assume midnight)
    if let Ok(d) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        return d.and_hms_opt(0, 0, 0).ok_or_else(|| {
            anyhow::Error::from(RecoverableToolError::new("Invalid date conversion"))
        });
    }
    Err(anyhow::Error::from(RecoverableToolError::new(&format!(
        "Invalid datetime format '{}'. Expected ISO 8601: YYYY-MM-DD or YYYY-MM-DDTHH:MM:SS",
        s
    ))))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_current_time() -> Result<()> {
        let tool = DateTimeTool::new();
        let result = tool.call(r#"{"operation": "current_time"}"#).await?;
        assert!(result.contains("Current time:"));
        assert!(result.contains("Day of week:"));
        assert!(result.contains("Unix timestamp:"));
        Ok(())
    }

    #[tokio::test]
    async fn test_add_days() -> Result<()> {
        let tool = DateTimeTool::new();
        let result = tool
            .call(r#"{"operation": "add_duration", "date": "2026-05-23", "days": 14}"#)
            .await?;
        assert_eq!(result, "Result: 2026-06-06");
        Ok(())
    }

    #[tokio::test]
    async fn test_subtract_days() -> Result<()> {
        let tool = DateTimeTool::new();
        let result = tool
            .call(r#"{"operation": "add_duration", "date": "2026-05-23", "days": -14}"#)
            .await?;
        assert_eq!(result, "Result: 2026-05-09");
        Ok(())
    }

    #[tokio::test]
    async fn test_subtract_months() -> Result<()> {
        let tool = DateTimeTool::new();
        let result = tool
            .call(r#"{"operation": "add_duration", "date": "2026-05-23", "months": -3}"#)
            .await?;
        assert_eq!(result, "Result: 2026-02-23");
        Ok(())
    }

    #[tokio::test]
    async fn test_add_months() -> Result<()> {
        let tool = DateTimeTool::new();
        let result = tool
            .call(r#"{"operation": "add_duration", "date": "2026-01-15", "months": 3}"#)
            .await?;
        assert_eq!(result, "Result: 2026-04-15");
        Ok(())
    }

    #[tokio::test]
    async fn test_add_days_and_months() -> Result<()> {
        let tool = DateTimeTool::new();
        let result = tool
            .call(r#"{"operation": "add_duration", "date": "2026-01-15", "months": 2, "days": 10}"#)
            .await?;
        assert_eq!(result, "Result: 2026-03-25");
        Ok(())
    }

    #[tokio::test]
    async fn test_add_duration_no_params_returns_today() -> Result<()> {
        let tool = DateTimeTool::new();
        let result = tool.call(r#"{"operation": "add_duration"}"#).await?;
        let today = Utc::now().format("%Y-%m-%d").to_string();
        assert_eq!(result, format!("Result: {}", today));
        Ok(())
    }

    #[tokio::test]
    async fn test_time_until_future_date() -> Result<()> {
        let tool = DateTimeTool::new();
        let result = tool
            .call(r#"{"operation": "time_until", "target_datetime": "2099-12-31"}"#)
            .await?;
        assert!(result.contains("days,"));
        assert!(result.contains("hours,"));
        assert!(result.contains("minutes until"));
        Ok(())
    }

    #[tokio::test]
    async fn test_time_until_past_date() -> Result<()> {
        let tool = DateTimeTool::new();
        let result = tool
            .call(r#"{"operation": "time_until", "target_datetime": "2020-01-01"}"#)
            .await?;
        assert!(result.contains("days,"));
        assert!(result.contains("hours,"));
        assert!(result.contains("minutes since"));
        Ok(())
    }

    #[tokio::test]
    async fn test_time_until_with_base_date() -> Result<()> {
        let tool = DateTimeTool::new();
        let result = tool
            .call(r#"{"operation": "time_until", "target_datetime": "2026-12-25", "date": "2026-05-23"}"#)
            .await?;
        assert!(result.contains("days,"));
        assert!(result.contains("minutes until"));
        Ok(())
    }

    #[tokio::test]
    async fn test_time_until_missing_target() -> Result<()> {
        let tool = DateTimeTool::new();
        let result = tool.call(r#"{"operation": "time_until"}"#).await;
        assert!(result.is_err());
        Ok(())
    }

    #[tokio::test]
    async fn test_invalid_date_format() -> Result<()> {
        let tool = DateTimeTool::new();
        let result = tool
            .call(r#"{"operation": "add_duration", "date": "not-a-date", "days": 5}"#)
            .await;
        assert!(result.is_err());
        Ok(())
    }

    #[tokio::test]
    async fn test_invalid_datetime_format() -> Result<()> {
        let tool = DateTimeTool::new();
        let result = tool
            .call(r#"{"operation": "time_until", "target_datetime": "not-a-date"}"#)
            .await;
        assert!(result.is_err());
        Ok(())
    }

    #[test]
    fn test_datetime_tool_default() {
        let tool = DateTimeTool::default();
        assert_eq!(tool.function_name(), "datetime");
    }

    #[test]
    fn test_function_json_has_required_parameters() {
        let tool = DateTimeTool::new();
        let json = serde_json::to_string(&tool.function).expect("Failed to serialize function");

        let value: serde_json::Value =
            serde_json::from_str(&json).expect("Failed to parse function JSON");

        assert_eq!(value["name"], "datetime");

        let params = &value["parameters"];
        let required = params["required"]
            .as_array()
            .expect("Required should be an array");
        assert!(
            required.contains(&serde_json::json!("operation")),
            "operation should be in required array"
        );

        let properties = &params["properties"];
        let operation = &properties["operation"];
        assert_eq!(operation["type"], "string");
        let enum_values = operation["enum"]
            .as_array()
            .expect("enum should be an array");
        assert!(
            enum_values.contains(&serde_json::json!("current_time"))
        );
        assert!(
            enum_values.contains(&serde_json::json!("add_duration"))
        );
        assert!(
            enum_values.contains(&serde_json::json!("time_until"))
        );
    }
}
