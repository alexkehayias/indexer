use crate::ai::skills::SkillRegistry;
use crate::ai::tools::registry::{Tool, ToolContext};
use crate::openai::{Function, Parameters, Property, ToolCall, ToolType, parse_tool_args};
use anyhow::{Error, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

#[derive(Serialize)]
pub struct ReadSkillFileProps {
    /// The name of the skill (e.g., 'pdf-processing').
    pub skill_name: Property,
    /// The relative path to the file within the skill directory.
    /// Examples: 'scripts/extract.py', 'references/guide.md', 'assets/logo.png'
    pub file_path: Property,
}

#[derive(Deserialize)]
pub struct ReadSkillFileArgs {
    pub skill_name: String,
    pub file_path: String,
}

/// Response from reading a skill file.
#[derive(Serialize, Deserialize)]
pub struct SkillFileContent {
    /// The skill name.
    pub skill_name: String,
    /// The requested file path.
    pub file_path: String,
    /// Whether the file was found.
    pub found: bool,
    /// The content of the file (only provided if found and is text).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// Error message if reading failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Serialize)]
pub struct ReadSkillFileTool {
    pub r#type: ToolType,
    pub function: Function<ReadSkillFileProps>,
    #[serde(skip)]
    registry: SkillRegistry,
}

#[async_trait]
impl ToolCall for ReadSkillFileTool {
    async fn call(&self, args: &str) -> Result<String, Error> {
        let fn_args: ReadSkillFileArgs = parse_tool_args(args)?;

        // Try to load the skill first
        let skill = match self.registry.load_skill(&fn_args.skill_name).await {
            Ok(s) => s,
            Err(e) => {
                let response = SkillFileContent {
                    skill_name: fn_args.skill_name.clone(),
                    file_path: fn_args.file_path.clone(),
                    found: false,
                    content: None,
                    error: Some(format!("Skill not found: {}", e)),
                };
                return Ok(serde_json::to_string(&response)?);
            }
        };

        // Try to read the file
        let content = skill.read_file(&fn_args.file_path).await;

        // Determine error message if file not found
        let (found, content, error) = match content {
            Some(c) => (true, Some(c), None),
            None => (
                false,
                None,
                Some(format!("File not found: {}", fn_args.file_path)),
            ),
        };

        let response = SkillFileContent {
            skill_name: fn_args.skill_name,
            file_path: fn_args.file_path,
            found,
            content,
            error,
        };

        Ok(serde_json::to_string(&response)?)
    }

    fn function_name(&self) -> String {
        self.function.name.clone()
    }
}

impl Tool for ReadSkillFileTool {
    fn from_context(ctx: &ToolContext) -> Result<Self> {
        Ok(Self::new(ctx.skill_registry_clone("read_skill_file")?))
    }
}

impl ReadSkillFileTool {
    pub fn new(registry: SkillRegistry) -> Self {
        let parameters = Parameters {
            r#type: String::from("object"),
            properties: ReadSkillFileProps {
                skill_name: Property {
                    r#type: String::from("string"),
                    description: String::from("The name of the skill (e.g., 'pdf-processing')."),
                    r#enum: None,
                },
                file_path: Property {
                    r#type: String::from("string"),
                    description: String::from(
                        "The relative path to the file within the skill directory. \
                         Examples: 'scripts/extract.py', 'references/guide.md'",
                    ),
                    r#enum: None,
                },
            },
            required: vec!["skill_name".to_string(), "file_path".to_string()],
            additional_properties: false,
        };
        let function = Function {
            name: String::from("read_skill_file"),
            description: String::from(
                "Read a file from within a skill's directory. Skills can have \
                 optional subdirectories: 'scripts/' for executable code, \
                 'references/' for documentation, and 'assets/' for other files. \
                 Returns the file content if found, or an error if not found.",
            ),
            parameters,
            strict: true,
        };

        Self {
            r#type: ToolType::Function,
            function,
            registry,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn create_test_skill_with_file(
        base_dir: &std::path::Path,
        name: &str,
        description: &str,
        file_path: &str,
        file_content: &str,
    ) {
        let skill_dir = base_dir.join(name);
        fs::create_dir_all(&skill_dir).unwrap();

        let skill_content = format!(
            r#"---
name: {}
description: {}
---

This is the body of {}.
"#,
            name, description, name
        );

        fs::write(skill_dir.join("SKILL.md"), skill_content).unwrap();

        // Create the subdirectory and file
        let full_path = skill_dir.join(file_path);
        if let Some(parent) = full_path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(full_path, file_content).unwrap();
    }

    #[tokio::test]
    async fn test_read_skill_file_found() {
        let temp = TempDir::new().unwrap();
        create_test_skill_with_file(
            temp.path(),
            "pdf-processing",
            "Process PDF files",
            "scripts/extract.py",
            "print('hello')",
        );

        let registry = SkillRegistry::new(temp.path()).await.unwrap();
        let tool = ReadSkillFileTool::new(registry);

        let result = tool
            .call(r#"{"skill_name": "pdf-processing", "file_path": "scripts/extract.py"}"#)
            .await
            .unwrap();
        let content: SkillFileContent = serde_json::from_str(&result).unwrap();

        assert_eq!(content.skill_name, "pdf-processing");
        assert_eq!(content.file_path, "scripts/extract.py");
        assert!(content.found);
        assert_eq!(content.content, Some("print('hello')".to_string()));
    }

    #[tokio::test]
    async fn test_read_skill_file_not_found() {
        let temp = TempDir::new().unwrap();
        create_test_skill_with_file(
            temp.path(),
            "pdf-processing",
            "Process PDF files",
            "scripts/extract.py",
            "print('hello')",
        );

        let registry = SkillRegistry::new(temp.path()).await.unwrap();
        let tool = ReadSkillFileTool::new(registry);

        let result = tool
            .call(r#"{"skill_name": "pdf-processing", "file_path": "scripts/missing.py"}"#)
            .await
            .unwrap();
        let content: SkillFileContent = serde_json::from_str(&result).unwrap();

        assert!(!content.found);
        assert!(content.error.is_some());
    }

    #[tokio::test]
    async fn test_read_skill_file_skill_not_found() {
        let temp = TempDir::new().unwrap();

        let registry = SkillRegistry::new(temp.path()).await.unwrap();
        let tool = ReadSkillFileTool::new(registry);

        let result = tool
            .call(r#"{"skill_name": "nonexistent", "file_path": "scripts/test.py"}"#)
            .await
            .unwrap();
        let content: SkillFileContent = serde_json::from_str(&result).unwrap();

        assert!(!content.found);
        assert!(content.error.is_some());
    }
}
