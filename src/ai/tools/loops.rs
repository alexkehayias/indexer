use super::{Tool, ToolConfig};
use crate::cli::channel::validate_channel_id;
use crate::loops::db::{delete_loop, find_all_loops, insert_loop};
use crate::loops::models::Loop;
use crate::loops::supervisor::{self, LoopSupervisorConfig};
use crate::openai::{
    Function, Parameters, Property, RecoverableToolError, ToolCall, ToolType, parse_tool_args,
};
use anyhow::{Error, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio_rusqlite::Connection;
use uuid::Uuid;

const DEFAULT_TOOLS: [&str; 2] = ["bash", "notify"];

/// JSON-schema property for an array parameter. `openai::Property` has no
/// `items` field, so array params need their own struct.
#[derive(Serialize)]
pub struct ArrayProperty {
    pub r#type: String,
    pub description: String,
    pub items: Property,
}

impl ArrayProperty {
    fn strings(description: &str) -> Self {
        Self {
            r#type: "array".to_string(),
            description: description.to_string(),
            items: Property::new("string", "An item in the array."),
        }
    }
}

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

/// Ensure a `channel` row exists for each id so the loop's `loop_channel`
/// links satisfy the foreign key. Existing rows are left untouched, preserving
/// a channel's registered source and debounce.
async fn ensure_channels(db: &Connection, channels: &[String]) -> Result<()> {
    let channels = channels.to_vec();
    db.call(move |conn| {
        let mut stmt =
            conn.prepare("INSERT OR IGNORE INTO channel (id, source) VALUES (?1, 'socket')")?;
        for channel in &channels {
            stmt.execute([channel])?;
        }
        Ok(())
    })
    .await
    .map_err(Error::new)
}

/// Map a database failure to a recoverable error, pointing the user at
/// `hq migrate` when the `loop` table is missing.
fn db_error(action: &str, err: anyhow::Error) -> RecoverableToolError {
    let msg = err.to_string();
    if msg.contains("no such table") {
        RecoverableToolError::new(
            "The loop storage table is missing. Run `hq migrate` to update the database, then try again.",
        )
    } else {
        RecoverableToolError::new(&format!("Failed to {action} loop: {msg}"))
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
    #[serde(default)]
    system_prompt: Option<String>,
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

        let id = Uuid::new_v4().to_string();
        let channels_display = fn_args.channels.join(", ");

        ensure_channels(&self.config.db, &fn_args.channels)
            .await
            .map_err(|e| Error::from(db_error("create", e)))?;

        let record = Loop {
            id: id.clone(),
            channels: fn_args.channels,
            system_prompt: fn_args.system_prompt,
            tools: fn_args
                .tools
                .unwrap_or_else(|| DEFAULT_TOOLS.iter().map(|s| s.to_string()).collect()),
            created_at: String::new(),
        };

        insert_loop(&self.config.db, &record)
            .await
            .map_err(|e| Error::from(db_error("create", e)))?;

        // Persisted before starting so a failed start can be rolled back cleanly.
        if let Err(e) = supervisor::start(record, self.config.clone()).await {
            let _ = delete_loop(&self.config.db, &id).await;
            return Err(Error::from(RecoverableToolError::new(&format!(
                "Failed to start loop '{id}': {e}. Make sure the channel publisher is running."
            ))));
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
                        "Optional system prompt for the loop's chat. Defaults to a generic assistant prompt.",
                    ),
                    tools: ArrayProperty::strings(
                        "Optional tool names the loop's chat may call (defaults to [\"bash\", \"notify\"]).",
                    ),
                },
                required: vec![String::from("channels")],
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
        let loops = find_all_loops(&self.config.db)
            .await
            .map_err(|e| Error::from(db_error("list", e)))?;

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
            let prompt = record.system_prompt.as_deref().unwrap_or("(default)");
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

        let deleted = delete_loop(&self.config.db, &fn_args.id)
            .await
            .map_err(|e| Error::from(db_error("delete", e)))?;
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
        assert_eq!(required, &vec![serde_json::json!("channels")]);
    }

    #[tokio::test]
    async fn create_loop_invalid_channel_is_recoverable() {
        let (_dir, config) = setup().await;
        let tool = CreateLoopTool::new(config);
        let err = tool.call(r#"{"channels": ["../bad"]}"#).await.unwrap_err();
        let recoverable = err.downcast_ref::<RecoverableToolError>();
        assert!(recoverable.is_some());
        assert!(recoverable.unwrap().message.contains("Invalid channel"));
    }

    #[tokio::test]
    async fn ensure_channels_creates_rows_and_preserves_existing() {
        let (_dir, config) = setup().await;

        // A channel that already exists with a non-socket source.
        config
            .db
            .call(|conn| {
                conn.execute(
                    "INSERT INTO channel (id, source) VALUES ('gamma', 'server')",
                    [],
                )?;
                Ok(())
            })
            .await
            .unwrap();

        ensure_channels(
            &config.db,
            &["alpha".to_string(), "beta".to_string(), "gamma".to_string()],
        )
        .await
        .unwrap();

        let rows = config
            .db
            .call(|conn| {
                let mut stmt = conn.prepare("SELECT id, source FROM channel ORDER BY id")?;
                let rows = stmt
                    .query_map([], |i| Ok((i.get::<_, String>(0)?, i.get::<_, String>(1)?)))?
                    .filter_map(Result::ok)
                    .collect::<Vec<(String, String)>>();
                Ok(rows)
            })
            .await
            .unwrap();

        assert_eq!(
            rows,
            vec![
                ("alpha".to_string(), "socket".to_string()),
                ("beta".to_string(), "socket".to_string()),
                ("gamma".to_string(), "server".to_string()),
            ]
        );
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
