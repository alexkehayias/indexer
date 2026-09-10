use crate::openai::{Function, Parameters, Property, ToolCall, ToolType, parse_tool_args};
use anyhow::{Error, Result, anyhow};
use super::registry::{Tool, ToolContext};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::fs;
use std::path::PathBuf;

const MAX_WORDS: usize = 2000;
const MEMORY_FILENAME: &str = "MEMORY.md";

#[derive(Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
enum MemoryOperation {
    Read,
    Write,
}

#[derive(Deserialize)]
struct MemoryArgs {
    operation: MemoryOperation,
    content: Option<String>,
}

#[derive(Serialize)]
pub struct MemoryProps {
    pub operation: Property,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<Property>,
}

#[derive(Serialize)]
pub struct MemoryTool {
    pub r#type: ToolType,
    pub function: Function<MemoryProps>,
    storage_path: String,
}

impl MemoryTool {
    pub fn new(storage_path: &str) -> Self {
        let function = Function {
            name: String::from("memory"),
            description: String::from(
                "Read from or write to persistent memory that persists across sessions. Use this when you learn something important about the user, their preferences, or context that should be remembered for future conversations. IMPORTANT: Keep memory concise and under 2000 words.",
            ),
            parameters: Parameters {
                r#type: String::from("object"),
                properties: MemoryProps {
                    operation: Property {
                        r#type: String::from("string"),
                        description: String::from("The operation to perform: 'read' or 'write'."),
                        r#enum: Some(vec![String::from("read"), String::from("write")]),
                    },
                    content: Some(Property {
                        r#type: String::from("string"),
                        description: String::from(
                            "The content to write (required for 'write' operation). Keep it concise and under 2000 words total.",
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
            storage_path: storage_path.to_string(),
        }
    }

    fn get_memory_file_path(&self) -> PathBuf {
        PathBuf::from(&self.storage_path)
            .join("workspace")
            .join(MEMORY_FILENAME)
    }
}

impl Default for MemoryTool {
    fn default() -> Self {
        let storage_path = String::from("./");
        Self::new(&storage_path)
    }
}

#[async_trait]
impl ToolCall for MemoryTool {
    async fn call(&self, args: &str) -> Result<String, Error> {
        let fn_args: MemoryArgs = parse_tool_args(args)?;
        let memory_path = self.get_memory_file_path();

        match fn_args.operation {
            MemoryOperation::Read => {
                if memory_path.exists() {
                    let content = fs::read_to_string(&memory_path).await?;
                    Ok(content)
                } else {
                    Ok("No memory yet".to_string())
                }
            }
            MemoryOperation::Write => {
                let content = fn_args
                    .content
                    .ok_or_else(|| anyhow!("Content is required for write operation"))?;

                // Validate word count
                let word_count = content.split_whitespace().count();
                if word_count > MAX_WORDS {
                    return Err(anyhow!(
                        "Memory exceeds {} words (currently {}). Please condense the memory.",
                        MAX_WORDS,
                        word_count
                    ));
                }

                // Ensure parent directory exists
                if let Some(parent) = memory_path.parent() {
                    fs::create_dir_all(parent).await?;
                }

                fs::write(&memory_path, &content).await?;
                Ok(format!(
                    "Memory saved ({} words). Current memory:\n\n{}",
                    word_count, content
                ))
            }
        }
    }

    fn function_name(&self) -> String {
        self.function.name.clone()
    }
}

impl Tool for MemoryTool {
    fn from_context(ctx: &ToolContext) -> Result<Self> {
        Ok(Self::new(&ctx.storage_path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_read_empty_memory() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let tool = MemoryTool::new(temp_dir.path().to_str().unwrap());

        let result = tool.call(r#"{"operation": "read"}"#).await?;
        assert_eq!(result, "No memory yet");

        Ok(())
    }

    #[tokio::test]
    async fn test_write_and_read_memory() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let tool = MemoryTool::new(temp_dir.path().to_str().unwrap());

        // Write memory
        let write_result = tool
            .call(r#"{"operation": "write", "content": "User prefers concise responses"}"#)
            .await?;
        assert!(write_result.contains("Memory saved"));
        assert!(write_result.contains("4 words"));

        // Read memory
        let read_result = tool.call(r#"{"operation": "read"}"#).await?;
        assert_eq!(read_result, "User prefers concise responses");

        Ok(())
    }

    #[tokio::test]
    async fn test_write_memory_creates_directory() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let nested_path = temp_dir.path().join("subdir").join("nested");
        let tool = MemoryTool::new(nested_path.to_str().unwrap());

        let result = tool
            .call(r#"{"operation": "write", "content": "Test memory"}"#)
            .await?;
        assert!(result.contains("Memory saved"));

        // Verify the file was created in the nested directory
        let memory_path = nested_path.join("workspace").join(MEMORY_FILENAME);
        assert!(memory_path.exists());

        Ok(())
    }

    #[tokio::test]
    async fn test_write_without_content_returns_error() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let tool = MemoryTool::new(temp_dir.path().to_str().unwrap());

        let result = tool.call(r#"{"operation": "write"}"#).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("Content is required"));

        Ok(())
    }

    #[tokio::test]
    async fn test_write_exceeds_word_limit() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let tool = MemoryTool::new(temp_dir.path().to_str().unwrap());

        // Create a string with more than 2000 words
        let long_content: String = "word ".repeat(2001);

        let result = tool
            .call(&format!(
                r#"{{"operation": "write", "content": "{}"}}"#,
                long_content
            ))
            .await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("exceeds 2000 words"));

        Ok(())
    }

    #[tokio::test]
    async fn test_write_at_word_limit() -> Result<()> {
        let temp_dir = TempDir::new()?;
        let tool = MemoryTool::new(temp_dir.path().to_str().unwrap());

        // Create exactly 2000 words
        let content: String = "word ".repeat(2000).trim().to_string();

        let result = tool
            .call(&format!(
                r#"{{"operation": "write", "content": "{}"}}"#,
                content
            ))
            .await?;
        assert!(result.contains("2000 words"));

        Ok(())
    }

    #[test]
    fn test_memory_tool_default() {
        let tool = MemoryTool::default();
        assert_eq!(tool.function_name(), "memory");
    }

    #[test]
    fn test_memory_tool_new() {
        let tool = MemoryTool::new("/tmp/test");
        assert_eq!(tool.storage_path, "/tmp/test");
        assert_eq!(tool.function_name(), "memory");
    }

    #[test]
    fn test_memory_function_json_has_required_parameters() {
        let tool = MemoryTool::new("/tmp/test");
        let json = serde_json::to_string(&tool.function).expect("Failed to serialize function");

        // Parse the JSON to verify structure
        let value: serde_json::Value =
            serde_json::from_str(&json).expect("Failed to parse function JSON");

        // Verify the function name
        assert_eq!(value["name"], "memory");

        // Get the parameters object
        let params = &value["parameters"];

        // Verify required array includes "operation"
        let required = params["required"]
            .as_array()
            .expect("Required should be an array");
        assert!(
            required.contains(&serde_json::json!("operation")),
            "operation should be in required array"
        );

        // Verify properties contain operation with enum
        let properties = &params["properties"];
        assert!(
            properties.get("operation").is_some(),
            "operation property should exist"
        );

        let operation = &properties["operation"];
        assert_eq!(operation["type"], "string");
        assert!(
            operation.get("enum").is_some(),
            "operation should have enum"
        );
        let enum_values = operation["enum"]
            .as_array()
            .expect("enum should be an array");
        assert!(
            enum_values.contains(&serde_json::json!("read")),
            "enum should contain 'read'"
        );
        assert!(
            enum_values.contains(&serde_json::json!("write")),
            "enum should contain 'write'"
        );

        // Verify content property exists (but is not required)
        assert!(
            properties.get("content").is_some(),
            "content property should exist"
        );
    }
}
