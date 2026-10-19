//! Tools for agent-owned channels.
//!
//! An agent (a loop, or a chat session) can create a channel it owns and emit
//! events to it. Ownership is recorded as `channel.created_by` — the caller's
//! session id — so only the creating agent may emit. Other agents subscribe to
//! the channel by id like any other, letting one agent publish a stream for
//! loops to react to.

use anyhow::{Error, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio_rusqlite::Connection;

use super::{Tool, ToolConfig};
use crate::cli::channel::validate_channel_id;
use crate::loops::db::{find_channel, insert_channel};
use crate::loops::registry;
use crate::openai::{
    Function, Parameters, Property, RecoverableToolError, ToolCall, ToolType, parse_tool_args,
};

/// Source recorded for agent-created channels. Server channels are served
/// in-process by the [`registry`], so the creating agent can emit to them
/// directly.
const SERVER_SOURCE: &str = "server";

#[derive(Serialize)]
pub struct CreateChannelProps {
    pub id: Property,
}

#[derive(Deserialize)]
struct CreateChannelArgs {
    id: String,
}

#[derive(Serialize)]
pub struct CreateChannelTool {
    pub r#type: ToolType,
    pub function: Function<CreateChannelProps>,
    #[serde(skip)]
    db: Connection,
    #[serde(skip)]
    session_id: String,
}

#[async_trait]
impl ToolCall for CreateChannelTool {
    async fn call(&self, args: &str) -> Result<String, Error> {
        let fn_args: CreateChannelArgs = parse_tool_args(args)?;

        if let Err(e) = validate_channel_id(&fn_args.id) {
            return Err(Error::from(RecoverableToolError::new(&format!(
                "Invalid channel '{}': {e}",
                fn_args.id
            ))));
        }

        match find_channel(&self.db, &fn_args.id).await? {
            Some(existing) => {
                if existing.created_by.as_deref() != Some(self.session_id.as_str()) {
                    return Err(Error::from(RecoverableToolError::new(&format!(
                        "Channel '{}' already exists and belongs to another agent.",
                        fn_args.id
                    ))));
                }
                // Bring it live again if the process restarted since creation.
                // Idempotent: existing subscribers keep their receiver.
                registry::ensure_registered(&fn_args.id);
                Ok(format!("Channel '{}' is ready.", fn_args.id))
            }
            None => {
                insert_channel(&self.db, &fn_args.id, SERVER_SOURCE, Some(&self.session_id))
                    .await?;
                registry::ensure_registered(&fn_args.id);
                Ok(format!(
                    "Created channel '{}'. Emit to it with emit_channel; loops can subscribe to it by id.",
                    fn_args.id
                ))
            }
        }
    }

    fn function_name(&self) -> String {
        Self::NAME.to_string()
    }
}

impl Tool for CreateChannelTool {
    const NAME: &'static str = "create_channel";

    fn from_config(conf: &ToolConfig) -> Result<Self> {
        Ok(Self::new(conf.db.clone(), &conf.session_id))
    }
}

impl CreateChannelTool {
    pub fn new(db: Connection, session_id: &str) -> Self {
        let function = Function {
            name: Self::NAME.to_string(),
            description: String::from(
                "Create a channel you own. Loops can subscribe to it by id, and only you can emit events to it with emit_channel. Use this to publish a stream of messages for loops to react to.",
            ),
            parameters: Parameters {
                r#type: String::from("object"),
                properties: CreateChannelProps {
                    id: Property::new(
                        "string",
                        "Channel id: alphanumeric with dashes/underscores. Must not already exist unless you created it.",
                    ),
                },
                required: vec![String::from("id")],
                additional_properties: false,
            },
            strict: false,
        };
        Self {
            r#type: ToolType::Function,
            function,
            db,
            session_id: session_id.to_string(),
        }
    }
}

#[derive(Serialize)]
pub struct EmitChannelProps {
    pub id: Property,
    pub message: Property,
}

#[derive(Deserialize)]
struct EmitChannelArgs {
    id: String,
    message: String,
}

#[derive(Serialize)]
pub struct EmitChannelTool {
    pub r#type: ToolType,
    pub function: Function<EmitChannelProps>,
    #[serde(skip)]
    db: Connection,
    #[serde(skip)]
    session_id: String,
}

#[async_trait]
impl ToolCall for EmitChannelTool {
    async fn call(&self, args: &str) -> Result<String, Error> {
        let fn_args: EmitChannelArgs = parse_tool_args(args)?;

        let channel = find_channel(&self.db, &fn_args.id).await?.ok_or_else(|| {
            Error::from(RecoverableToolError::new(&format!(
                "No channel with id '{}'. Create one with create_channel first.",
                fn_args.id
            )))
        })?;

        if channel.created_by.as_deref() != Some(self.session_id.as_str()) {
            return Err(Error::from(RecoverableToolError::new(&format!(
                "You can only emit to channels you created; '{}' belongs to another agent.",
                fn_args.id
            ))));
        }

        // Bring it live if the process restarted since creation, then publish.
        registry::ensure_registered(&fn_args.id);
        let _ = registry::publish(&fn_args.id, fn_args.message);

        Ok(format!("Emitted to channel '{}'.", fn_args.id))
    }

    fn function_name(&self) -> String {
        Self::NAME.to_string()
    }
}

impl Tool for EmitChannelTool {
    const NAME: &'static str = "emit_channel";

    fn from_config(conf: &ToolConfig) -> Result<Self> {
        Ok(Self::new(conf.db.clone(), &conf.session_id))
    }
}

impl EmitChannelTool {
    pub fn new(db: Connection, session_id: &str) -> Self {
        let function = Function {
            name: Self::NAME.to_string(),
            description: String::from(
                "Emit a message on a channel you created. Fails if the channel does not exist or was created by another agent.",
            ),
            parameters: Parameters {
                r#type: String::from("object"),
                properties: EmitChannelProps {
                    id: Property::new("string", "The id of the channel to emit to."),
                    message: Property::new("string", "The message text to publish."),
                },
                required: vec![String::from("id"), String::from("message")],
                additional_properties: false,
            },
            strict: false,
        };
        Self {
            r#type: ToolType::Function,
            function,
            db,
            session_id: session_id.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::db::{async_db, initialize_db};
    use crate::loops::registry::{broadcast_stream, subscribe};
    use futures::StreamExt;
    use std::time::Duration;
    use uuid::Uuid;

    async fn setup() -> (tempfile::TempDir, Connection) {
        let dir = tempfile::tempdir().unwrap();
        let db = async_db(dir.path().to_str().unwrap()).await.unwrap();
        db.call(|conn| {
            initialize_db(conn)?;
            Ok(())
        })
        .await
        .unwrap();
        (dir, db)
    }

    /// A channel id unique to one test; the registry is process-wide.
    fn unique_id() -> String {
        format!("chan-{}", Uuid::new_v4())
    }

    #[tokio::test]
    async fn create_channel_schema_requires_id() {
        let (_dir, db) = setup().await;
        let tool = CreateChannelTool::new(db, "agent-a");
        let json = serde_json::to_value(&tool.function).unwrap();

        assert_eq!(json["parameters"]["properties"]["id"]["type"], "string");
        let required = json["parameters"]["required"].as_array().unwrap();
        assert_eq!(required, &vec![serde_json::json!("id")]);
    }

    #[tokio::test]
    async fn create_channel_invalid_id_is_recoverable() {
        let (_dir, db) = setup().await;
        let tool = CreateChannelTool::new(db, "agent-a");
        let err = tool.call(r#"{"id": "../bad"}"#).await.unwrap_err();
        let recoverable = err.downcast_ref::<RecoverableToolError>();
        assert!(recoverable.is_some());
        assert!(recoverable.unwrap().message.contains("Invalid channel"));
    }

    #[tokio::test]
    async fn create_channel_records_owner_and_registers_publisher() {
        let (_dir, db) = setup().await;
        let id = unique_id();
        let tool = CreateChannelTool::new(db.clone(), "agent-a");
        tool.call(&format!(r#"{{"id": "{id}"}}"#)).await.unwrap();

        let channel = find_channel(&db, &id).await.unwrap().unwrap();
        assert_eq!(channel.source, "server");
        assert_eq!(channel.created_by.as_deref(), Some("agent-a"));
        assert!(subscribe(&id).is_some());
    }

    #[tokio::test]
    async fn create_channel_is_idempotent_for_the_owner() {
        let (_dir, db) = setup().await;
        let id = unique_id();
        let tool = CreateChannelTool::new(db, "agent-a");
        tool.call(&format!(r#"{{"id": "{id}"}}"#)).await.unwrap();
        let out = tool.call(&format!(r#"{{"id": "{id}"}}"#)).await.unwrap();
        assert!(out.contains("ready"), "{out}");
    }

    #[tokio::test]
    async fn create_channel_rejects_another_agents_channel() {
        let (_dir, db) = setup().await;
        let id = unique_id();
        CreateChannelTool::new(db.clone(), "agent-a")
            .call(&format!(r#"{{"id": "{id}"}}"#))
            .await
            .unwrap();

        let err = CreateChannelTool::new(db, "agent-b")
            .call(&format!(r#"{{"id": "{id}"}}"#))
            .await
            .unwrap_err();
        let recoverable = err.downcast_ref::<RecoverableToolError>();
        assert!(recoverable.is_some());
        assert!(recoverable.unwrap().message.contains("another agent"));
    }

    #[tokio::test]
    async fn emit_delivers_to_subscribers() {
        let (_dir, db) = setup().await;
        let id = unique_id();
        CreateChannelTool::new(db.clone(), "agent-a")
            .call(&format!(r#"{{"id": "{id}"}}"#))
            .await
            .unwrap();

        let mut events = broadcast_stream(subscribe(&id).unwrap());
        EmitChannelTool::new(db, "agent-a")
            .call(&format!(r#"{{"id": "{id}", "message": "hello"}}"#))
            .await
            .unwrap();

        let event = tokio::time::timeout(Duration::from_secs(5), events.next())
            .await
            .expect("timed out waiting for event")
            .unwrap();
        assert_eq!(event, "hello");
    }

    #[tokio::test]
    async fn emit_rejects_channel_owned_by_another_agent() {
        let (_dir, db) = setup().await;
        let id = unique_id();
        CreateChannelTool::new(db.clone(), "agent-a")
            .call(&format!(r#"{{"id": "{id}"}}"#))
            .await
            .unwrap();

        let err = EmitChannelTool::new(db, "agent-b")
            .call(&format!(r#"{{"id": "{id}", "message": "hi"}}"#))
            .await
            .unwrap_err();
        let recoverable = err.downcast_ref::<RecoverableToolError>();
        assert!(recoverable.is_some());
        assert!(recoverable.unwrap().message.contains("only emit"));
    }

    #[tokio::test]
    async fn emit_unknown_channel_is_recoverable() {
        let (_dir, db) = setup().await;
        let err = EmitChannelTool::new(db, "agent-a")
            .call(r#"{"id": "missing", "message": "hi"}"#)
            .await
            .unwrap_err();
        let recoverable = err.downcast_ref::<RecoverableToolError>();
        assert!(recoverable.is_some());
        assert!(recoverable.unwrap().message.contains("No channel"));
    }
}
