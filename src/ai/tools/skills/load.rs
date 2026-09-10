use crate::ai::skills::{Skill, SkillRegistry};
use crate::ai::tools::registry::{Tool, ToolContext};
use crate::openai::{Function, Parameters, Property, ToolCall, ToolType, parse_tool_args};
use anyhow::{Error, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Serialize)]
pub struct LoadSkillProps {
    /// The name of the skill to load.
    pub name: Property,
}

#[derive(Deserialize)]
pub struct LoadSkillArgs {
    pub name: String,
}

/// The full skill content returned by the tool.
#[derive(Serialize, Deserialize)]
pub struct SkillContent {
    pub name: String,
    pub description: String,
    pub body: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compatibility: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<HashMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allowed_tools: Option<String>,
}

impl From<&Skill> for SkillContent {
    fn from(skill: &Skill) -> Self {
        SkillContent {
            name: skill.frontmatter.name.clone(),
            description: skill.frontmatter.description.clone(),
            body: skill.body.clone(),
            license: skill.frontmatter.license.clone(),
            compatibility: skill.frontmatter.compatibility.clone(),
            metadata: skill.frontmatter.metadata.clone(),
            allowed_tools: skill.frontmatter.allowed_tools.clone(),
        }
    }
}

#[derive(Serialize)]
pub struct LoadSkillTool {
    pub r#type: ToolType,
    pub function: Function<LoadSkillProps>,
    #[serde(skip)]
    registry: SkillRegistry,
}

#[async_trait]
impl ToolCall for LoadSkillTool {
    async fn call(&self, args: &str) -> Result<String, Error> {
        let fn_args: LoadSkillArgs = parse_tool_args(args)?;

        let skill = self.registry.load_skill(&fn_args.name).await?;
        let result = SkillContent::from(&skill);
        Ok(serde_json::to_string(&result)?)
    }

    fn function_name(&self) -> String {
        self.function.name.clone()
    }
}

impl Tool for LoadSkillTool {
    fn from_context(ctx: &ToolContext) -> Result<Self> {
        Ok(Self::new(ctx.skill_registry_clone("load_skill")?))
    }
}

impl LoadSkillTool {
    pub fn new(registry: SkillRegistry) -> Self {
        let parameters = Parameters {
            r#type: String::from("object"),
            properties: LoadSkillProps {
                name: Property {
                    r#type: String::from("string"),
                    description: String::from(
                        "The name of the skill to load (e.g., 'pdf-processing').",
                    ),
                    r#enum: None,
                },
            },
            required: vec!["name".to_string()],
            additional_properties: false,
        };
        let function = Function {
            name: String::from("load_skill"),
            description: String::from(
                "Load the full content of a skill by name. This returns \
                 the skill's description, body (instructions), license, \
                 compatibility info, and allowed tools. Use this after finding \
                 a skill with list_skills or search_skills to get the full details.",
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

    fn create_test_skill(base_dir: &std::path::Path, name: &str, description: &str) {
        let skill_dir = base_dir.join(name);
        fs::create_dir_all(&skill_dir).unwrap();

        let skill_content = format!(
            r#"---
name: {}
description: {}
license: MIT
---

This is the body of {}.
"#,
            name, description, name
        );

        fs::write(skill_dir.join("SKILL.md"), skill_content).unwrap();
    }

    #[tokio::test]
    async fn test_load_skill() {
        let temp = TempDir::new().unwrap();
        create_test_skill(temp.path(), "pdf-processing", "Process PDF files");

        let registry = SkillRegistry::new(temp.path()).await.unwrap();
        let tool = LoadSkillTool::new(registry);

        let result = tool.call(r#"{"name": "pdf-processing"}"#).await.unwrap();
        let content: SkillContent = serde_json::from_str(&result).unwrap();

        assert_eq!(content.name, "pdf-processing");
        assert_eq!(content.description, "Process PDF files");
        assert!(content.body.contains("This is the body"));
        assert_eq!(content.license, Some("MIT".to_string()));
    }

    #[tokio::test]
    async fn test_load_skill_not_found() {
        let temp = TempDir::new().unwrap();
        create_test_skill(temp.path(), "pdf-processing", "Process PDF files");

        let registry = SkillRegistry::new(temp.path()).await.unwrap();
        let tool = LoadSkillTool::new(registry);

        let result = tool.call(r#"{"name": "nonexistent"}"#).await;
        assert!(result.is_err());
    }
}
