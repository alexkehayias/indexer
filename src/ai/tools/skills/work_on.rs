use crate::ai::skills::{validation::validate_skill_directory, validation::validate_skill_name};
use crate::ai::tools::bash::SANDBOX_ROOT;
use crate::ai::tools::registry::{Tool, ToolContext};
use crate::ai::tools::skills::copy_dir;
use crate::openai::{Function, Parameters, Property, ToolCall, ToolType};
use anyhow::{Error, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tokio::fs;

#[derive(Serialize)]
pub struct WorkOnSkillProps {
    pub name: Property,
}

#[derive(Deserialize)]
pub struct WorkOnSkillArgs {
    pub name: String,
}

#[derive(Serialize, Deserialize)]
pub struct WorkOnSkillResult {
    pub name: String,
    /// Path where the skill files are available in the workspace
    pub path: String,
    /// Whether this is a new skill (not yet in the global skills directory)
    pub created: bool,
}

#[derive(Serialize)]
pub struct WorkOnSkillTool {
    pub r#type: ToolType,
    pub function: Function<WorkOnSkillProps>,
    #[serde(skip)]
    skills_path: PathBuf,
    #[serde(skip)]
    workspace_path: PathBuf,
}

#[async_trait]
impl ToolCall for WorkOnSkillTool {
    async fn call(&self, args: &str) -> Result<String, Error> {
        let fn_args: WorkOnSkillArgs = serde_json::from_str(args).unwrap();
        let skill_name = fn_args.name;
        validate_skill_name(&skill_name)?;

        let dest = self.workspace_path.join(&skill_name);
        let skill_source = self.skills_path.join(&skill_name);
        let created = !skill_source.exists() || !skill_source.is_dir();

        if dest.exists() {
            // Workspace already has this skill — return it as-is to
            // avoid discarding uncommitted edits from an interrupted
            // workflow.
        } else if !created {
            validate_skill_directory(&skill_source).await?;
            copy_dir(&skill_source, &dest).await?;
        } else {
            fs::create_dir_all(&dest).await?;
        }

        // Map the location of the skill in the agent's sandbox.
        // The path must be within the sandbox root that BashTool
        // mounts the workspace at.
        let workspace_skill_path = format!("{}{}", SANDBOX_ROOT, skill_name);

        let result = WorkOnSkillResult {
            name: skill_name,
            path: workspace_skill_path,
            created,
        };

        Ok(serde_json::to_string(&result)?)
    }

    fn function_name(&self) -> String {
        self.function.name.clone()
    }
}

impl Tool for WorkOnSkillTool {
    fn from_context(ctx: &ToolContext) -> Result<Self> {
        let skills_dir = ctx.skills_dir("work_on_skill")?;
        Ok(Self::new(&skills_dir, &ctx.storage_path, &ctx.session_id))
    }
}

impl WorkOnSkillTool {
    pub fn new(skills_path: &str, storage_path: &str, session_id: &str) -> Self {
        let workspace_path = PathBuf::from(format!("{}/workspace/{}", storage_path, session_id));

        let parameters = Parameters {
            r#type: String::from("object"),
            properties: WorkOnSkillProps {
                name: Property {
                    r#type: String::from("string"),
                    description: String::from(
                        "The name of the skill to work on. If it doesn't exist yet, an empty directory will be created in your workspace.",
                    ),
                    r#enum: None,
                },
            },
            required: vec!["name".to_string()],
            additional_properties: false,
        };
        let function = Function {
            name: String::from("work_on_skill"),
            description: String::from(
                "Prepare a skill for editing in your workspace. If the skill already exists, it is copied into your workspace. \
                 If it doesn't exist yet, an empty directory is created so you can build a new skill from scratch. \
                 After editing files with bash, use save_skill to commit changes back.",
            ),
            parameters,
            strict: true,
        };

        Self {
            r#type: ToolType::Function,
            function,
            skills_path: PathBuf::from(skills_path),
            workspace_path,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn create_test_skill(base_dir: &std::path::Path, name: &str) {
        let skill_dir = base_dir.join(name);
        std::fs::create_dir_all(&skill_dir).unwrap();

        let skill_content = format!(
            r#"---
name: {}
description: A test skill
---

Test body.
"#,
            name
        );

        std::fs::write(skill_dir.join("SKILL.md"), skill_content).unwrap();
    }

    #[tokio::test]
    async fn test_work_on_skill_copies_to_workspace() {
        let temp = TempDir::new().unwrap();
        let skills_dir = temp.path().join("skills");
        std::fs::create_dir_all(&skills_dir).unwrap();

        create_test_skill(&skills_dir, "test-skill");

        let storage_path = temp.path().to_string_lossy().to_string();
        let session_id = "test-session";
        let tool = WorkOnSkillTool::new(&skills_dir.to_string_lossy(), &storage_path, session_id);

        let result = tool.call(r#"{"name": "test-skill"}"#).await.unwrap();
        let output: WorkOnSkillResult = serde_json::from_str(&result).unwrap();

        assert_eq!(output.name, "test-skill");

        let workspace_skill = temp
            .path()
            .join("workspace")
            .join(session_id)
            .join("test-skill");
        assert!(workspace_skill.join("SKILL.md").exists());
    }

    #[tokio::test]
    async fn test_work_on_skill_invalid_name() {
        let temp = TempDir::new().unwrap();
        let skills_dir = temp.path().join("skills");
        std::fs::create_dir_all(&skills_dir).unwrap();

        let storage_path = temp.path().to_string_lossy().to_string();
        let tool = WorkOnSkillTool::new(&skills_dir.to_string_lossy(), &storage_path, "session");

        let result = tool.call(r#"{"name": "INVALID"}"#).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_work_on_skill_new_creates_empty_dir() {
        let temp = TempDir::new().unwrap();
        let skills_dir = temp.path().join("skills");
        std::fs::create_dir_all(&skills_dir).unwrap();

        let storage_path = temp.path().to_string_lossy().to_string();
        let session_id = "test-session";
        let tool = WorkOnSkillTool::new(&skills_dir.to_string_lossy(), &storage_path, session_id);

        let result = tool.call(r#"{"name": "new-skill"}"#).await.unwrap();
        let output: WorkOnSkillResult = serde_json::from_str(&result).unwrap();

        assert_eq!(output.name, "new-skill");
        assert!(output.created);

        let workspace_skill = temp
            .path()
            .join("workspace")
            .join(session_id)
            .join("new-skill");
        assert!(workspace_skill.is_dir());
    }

    #[tokio::test]
    async fn test_work_on_existing_skill_created_is_false() {
        let temp = TempDir::new().unwrap();
        let skills_dir = temp.path().join("skills");
        std::fs::create_dir_all(&skills_dir).unwrap();

        create_test_skill(&skills_dir, "existing-skill");

        let storage_path = temp.path().to_string_lossy().to_string();
        let session_id = "test-session";
        let tool = WorkOnSkillTool::new(&skills_dir.to_string_lossy(), &storage_path, session_id);

        let result = tool.call(r#"{"name": "existing-skill"}"#).await.unwrap();
        let output: WorkOnSkillResult = serde_json::from_str(&result).unwrap();

        assert!(!output.created);
    }
}
