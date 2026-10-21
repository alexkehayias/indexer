use anyhow::{Error, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{Tool, ToolConfig};
use crate::channels::db::ensure_channels;
use crate::channels::models::ChannelOwner;
use crate::cli::channel::validate_channel_id;
use crate::loops::db::{delete_loop, find_all_loops, insert_loop};
use crate::loops::models::Loop;
use crate::loops::supervisor::{self, LoopSupervisorConfig};
use crate::loops::tools::resolve_loop_tools;
use crate::openai::{
    ArrayProperty, Function, Parameters, Property, RecoverableToolError, ToolCall, ToolType,
    parse_tool_args,
};

/// Build a [`LoopSupervisorConfig`] from the shared tool context.
fn loop_supervisor_config(conf: &ToolConfig) -> LoopSupervisorConfig {
    LoopSupervisorConfig {
        db: conf.db.clone(),
        api_base_url: conf.api_base_url.clone(),
        api_hostname: conf.api_hostname.clone(),
        api_key: conf.api_key.clone(),
        model: conf.model.clone(),
        storage_path: conf.storage_path.clone(),
        vapid_key_path: conf.vapid_key_path.clone(),
    }
}

#[derive(Serialize)]
pub struct CreateLoopProps {
    pub channels: ArrayProperty,
    pub system_prompt: Property,
    pub tools: ArrayProperty,
}

#[derive(Deserialize)]
struct CreateLoopArgs {
    channels: Vec<String>,
    system_prompt: String,
    #[serde(default)]
    tools: Option<Vec<String>>,
}

#[derive(Serialize)]
pub struct CreateLoopTool {
    pub r#type: ToolType,
    pub function: Function<CreateLoopProps>,
    #[serde(skip)]
    config: LoopSupervisorConfig,
}

#[async_trait]
impl ToolCall for CreateLoopTool {
    async fn call(&self, args: &str) -> Result<String, Error> {
        let fn_args: CreateLoopArgs = parse_tool_args(args)?;

        if fn_args.channels.is_empty() {
            return Err(Error::from(RecoverableToolError::new(
                "At least one channel is required.",
            )));
        }
        for channel in &fn_args.channels {
            if let Err(e) = validate_channel_id(channel) {
                return Err(Error::from(RecoverableToolError::new(&format!(
                    "Invalid channel '{channel}': {e}"
                ))));
            }
        }
        if fn_args.system_prompt.trim().is_empty() {
            return Err(Error::from(RecoverableToolError::new(
                "A system prompt is required — describe what the loop should do with each event.",
            )));
        }

        let id = Uuid::new_v4().to_string();
        let channels_display = fn_args.channels.join(", ");

        ensure_channels(&self.config.db, &fn_args.channels, &ChannelOwner::Socket).await?;

        let record = Loop {
            id: id.clone(),
            channels: fn_args.channels,
            system_prompt: fn_args.system_prompt,
            tools: resolve_loop_tools(fn_args.tools.as_deref().unwrap_or(&[])),
            created_at: String::new(),
        };

        insert_loop(&self.config.db, &record).await?;

        // Persisted before starting so a failed start can be rolled back cleanly.
        if let Err(e) = supervisor::start(record, self.config.clone()).await {
            let _ = delete_loop(&self.config.db, &id).await;
            return Err(e);
        }

        Ok(format!(
            "Created loop '{id}' listening on channel(s): {channels_display}. It is now running."
        ))
    }

    fn function_name(&self) -> String {
        Self::NAME.to_string()
    }
}

impl Tool for CreateLoopTool {
    const NAME: &'static str = "create_loop";

    fn from_config(conf: &ToolConfig) -> Result<Self> {
        Ok(Self::new(loop_supervisor_config(conf)))
    }
}

impl CreateLoopTool {
    pub fn new(config: LoopSupervisorConfig) -> Self {
        let function = Function {
            name: Self::NAME.to_string(),
            description: String::from(
                "Create a persistent loop that subscribes to one or more channels and runs an LLM turn for every event published to them. The loop keeps running across server restarts. Use list_loops to inspect loops and delete_loop to remove one.",
            ),
            parameters: Parameters {
                r#type: String::from("object"),
                properties: CreateLoopProps {
                    channels: ArrayProperty::strings(
                        "Channel IDs the loop should subscribe to. Each must be alphanumeric with dashes/underscores. The channel publisher must already be running.",
                    ),
                    system_prompt: Property::new(
                        "string",
                        "Instructions for the loop, appended to the autonomous-agent system prompt. Describe what the loop should do with each event; events arrive as \"[channel] <text>\" user messages.",
                    ),
                    tools: ArrayProperty::strings(
                        "Optional tool names the loop's chat may call (defaults to [\"bash\", \"notify\"]). The bash tool is always included so the loop can read and write its state/ files.",
                    ),
                },
                required: vec![String::from("channels"), String::from("system_prompt")],
                additional_properties: false,
            },
            strict: false,
        };
        Self {
            r#type: ToolType::Function,
            function,
            config,
        }
    }
}

#[derive(Serialize)]
pub struct ListLoopsProps {}

#[derive(Serialize)]
pub struct ListLoopsTool {
    pub r#type: ToolType,
    pub function: Function<ListLoopsProps>,
    #[serde(skip)]
    config: LoopSupervisorConfig,
}

#[async_trait]
impl ToolCall for ListLoopsTool {
    async fn call(&self, _args: &str) -> Result<String, Error> {
        let loops = find_all_loops(&self.config.db).await?;

        if loops.is_empty() {
            return Ok("No loops are configured.".to_string());
        }

        let running = supervisor::running_ids();
        let mut out = String::new();
        for record in &loops {
            let status = if running.iter().any(|id| id == &record.id) {
                "running"
            } else {
                "stopped"
            };
            let prompt = &record.system_prompt;
            out.push_str(&format!(
                "- {} ({status})\n  channels: {}\n  tools: {}\n  prompt: {}\n",
                record.id,
                record.channels.join(", "),
                record.tools.join(", "),
                prompt,
            ));
        }
        Ok(out)
    }

    fn function_name(&self) -> String {
        Self::NAME.to_string()
    }
}

impl Tool for ListLoopsTool {
    const NAME: &'static str = "list_loops";

    fn from_config(conf: &ToolConfig) -> Result<Self> {
        Ok(Self::new(loop_supervisor_config(conf)))
    }
}

impl ListLoopsTool {
    pub fn new(config: LoopSupervisorConfig) -> Self {
        let function = Function {
            name: Self::NAME.to_string(),
            description: String::from(
                "List all persistent channel loops, including whether each is currently running.",
            ),
            parameters: Parameters {
                r#type: String::from("object"),
                properties: ListLoopsProps {},
                required: vec![],
                additional_properties: false,
            },
            strict: false,
        };
        Self {
            r#type: ToolType::Function,
            function,
            config,
        }
    }
}

#[derive(Serialize)]
pub struct DeleteLoopProps {
    pub id: Property,
}

#[derive(Deserialize)]
struct DeleteLoopArgs {
    id: String,
}

#[derive(Serialize)]
pub struct DeleteLoopTool {
    pub r#type: ToolType,
    pub function: Function<DeleteLoopProps>,
    #[serde(skip)]
    config: LoopSupervisorConfig,
}

#[async_trait]
impl ToolCall for DeleteLoopTool {
    async fn call(&self, args: &str) -> Result<String, Error> {
        let fn_args: DeleteLoopArgs = parse_tool_args(args)?;

        let deleted = delete_loop(&self.config.db, &fn_args.id).await?;
        let stopped = supervisor::stop(&fn_args.id);

        if deleted == 0 && !stopped {
            return Err(Error::from(RecoverableToolError::new(&format!(
                "No loop with id '{}' was found.",
                fn_args.id
            ))));
        }

        Ok(format!("Deleted loop '{}'.", fn_args.id))
    }

    fn function_name(&self) -> String {
        Self::NAME.to_string()
    }
}

impl Tool for DeleteLoopTool {
    const NAME: &'static str = "delete_loop";

    fn from_config(conf: &ToolConfig) -> Result<Self> {
        Ok(Self::new(loop_supervisor_config(conf)))
    }
}

impl DeleteLoopTool {
    pub fn new(config: LoopSupervisorConfig) -> Self {
        let function = Function {
            name: Self::NAME.to_string(),
            description: String::from(
                "Delete a persistent channel loop by id and stop it if it is running.",
            ),
            parameters: Parameters {
                r#type: String::from("object"),
                properties: DeleteLoopProps {
                    id: Property::new("string", "The id of the loop to delete."),
                },
                required: vec![String::from("id")],
                additional_properties: false,
            },
            strict: false,
        };
        Self {
            r#type: ToolType::Function,
            function,
            config,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::db::{async_db, initialize_db};

    async fn setup() -> (tempfile::TempDir, LoopSupervisorConfig) {
        let dir = tempfile::tempdir().unwrap();
        let db = async_db(dir.path().to_str().unwrap()).await.unwrap();
        db.call(|conn| {
            initialize_db(conn)?;
            Ok(())
        })
        .await
        .unwrap();

        let storage_path = dir.path().to_str().unwrap().to_string();
        let config = LoopSupervisorConfig {
            db,
            api_base_url: "http://localhost".to_string(),
            api_hostname: "localhost".to_string(),
            api_key: "test-key".to_string(),
            model: "test-model".to_string(),
            storage_path,
            vapid_key_path: dir.path().join("vapid").to_str().unwrap().to_string(),
        };
        (dir, config)
    }

    #[tokio::test]
    async fn create_loop_schema_describes_array_items() {
        let (_dir, config) = setup().await;
        let tool = CreateLoopTool::new(config);
        let json = serde_json::to_value(&tool.function).unwrap();

        let properties = &json["parameters"]["properties"];
        assert_eq!(properties["channels"]["type"], "array");
        assert_eq!(properties["channels"]["items"]["type"], "string");
        assert_eq!(properties["tools"]["type"], "array");
        assert_eq!(properties["tools"]["items"]["type"], "string");

        let required = json["parameters"]["required"].as_array().unwrap();
        assert_eq!(
            required,
            &vec![
                serde_json::json!("channels"),
                serde_json::json!("system_prompt")
            ]
        );
    }

    #[tokio::test]
    async fn create_loop_invalid_channel_is_recoverable() {
        let (_dir, config) = setup().await;
        let tool = CreateLoopTool::new(config);
        let err = tool
            .call(r#"{"channels": ["../bad"], "system_prompt": "Do the thing."}"#)
            .await
            .unwrap_err();
        let recoverable = err.downcast_ref::<RecoverableToolError>();
        assert!(recoverable.is_some());
        assert!(recoverable.unwrap().message.contains("Invalid channel"));
    }

    #[tokio::test]
    async fn create_loop_requires_system_prompt() {
        let (_dir, config) = setup().await;
        let tool = CreateLoopTool::new(config);

        // Omitted entirely: serde rejects the args before the handler runs.
        let err = tool.call(r#"{"channels": ["alpha"]}"#).await.unwrap_err();
        let recoverable = err.downcast_ref::<RecoverableToolError>();
        assert!(recoverable.is_some());
        assert!(recoverable.unwrap().message.contains("system_prompt"));

        // Present but blank: rejected by the handler's validation.
        let err = tool
            .call(r#"{"channels": ["alpha"], "system_prompt": "  "}"#)
            .await
            .unwrap_err();
        let recoverable = err.downcast_ref::<RecoverableToolError>();
        assert!(recoverable.is_some());
        assert!(recoverable.unwrap().message.contains("system prompt"));
    }

    #[tokio::test]
    async fn delete_loop_unknown_id_is_recoverable() {
        let (_dir, config) = setup().await;
        let tool = DeleteLoopTool::new(config);
        let err = tool.call(r#"{"id": "missing"}"#).await.unwrap_err();
        let recoverable = err.downcast_ref::<RecoverableToolError>();
        assert!(recoverable.is_some());
        assert!(recoverable.unwrap().message.contains("No loop with id"));
    }

    #[tokio::test]
    async fn list_loops_empty_table_is_friendly() {
        let (_dir, config) = setup().await;
        let tool = ListLoopsTool::new(config);
        let out = tool.call("{}").await.unwrap();
        assert!(out.contains("No loops"));
    }
}
