use crate::ai::skills::{SkillRegistry, SkillSummary};
use crate::ai::tools::registry::{Tool, ToolContext};
use crate::openai::{Function, Parameters, Property, ToolCall, ToolType, parse_tool_args};
use anyhow::{Error, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

#[derive(Serialize)]
pub struct SearchSkillsProps {
    /// The search query to find relevant skills.
    pub query: Property,
}

#[derive(Deserialize)]
pub struct SearchSkillsArgs {
    pub query: String,
}

#[derive(Serialize)]
pub struct SearchSkillsTool {
    pub r#type: ToolType,
    pub function: Function<SearchSkillsProps>,
    #[serde(skip)]
    registry: SkillRegistry,
}

#[async_trait]
impl ToolCall for SearchSkillsTool {
    async fn call(&self, args: &str) -> Result<String, Error> {
        let fn_args: SearchSkillsArgs = parse_tool_args(args)?;

        let skills = self.registry.search(&fn_args.query);
        let result: Vec<SkillSummary> = skills;
        Ok(serde_json::to_string(&result)?)
    }

    fn function_name(&self) -> String {
        self.function.name.clone()
    }
}

impl Tool for SearchSkillsTool {
    fn from_context(ctx: &ToolContext) -> Result<Self> {
        Ok(Self::new(ctx.skill_registry_clone("search_skills")?))
    }
}

impl SearchSkillsTool {
    pub fn new(registry: SkillRegistry) -> Self {
        let parameters = Parameters {
            r#type: String::from("object"),
            properties: SearchSkillsProps {
                query: Property {
                    r#type: String::from("string"),
                    description: String::from(
                        "The keyword to search for in skill names and descriptions.",
                    ),
                    r#enum: None,
                },
            },
            required: vec!["query".to_string()],
            additional_properties: false,
        };
        let function = Function {
            name: String::from("search_skills"),
            description: String::from(
                "Search for skills by keyword in their name or description. \
                 Returns skills sorted by relevance: exact name match first, \
                 then partial name match, then description match.",
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
---

This is the body of {}.
"#,
            name, description, name
        );

        fs::write(skill_dir.join("SKILL.md"), skill_content).unwrap();
    }

    #[tokio::test]
    async fn test_search_skills_exact_name() {
        let temp = TempDir::new().unwrap();
        create_test_skill(temp.path(), "pdf-processing", "Process PDF files");
        create_test_skill(temp.path(), "code-review", "Review code for bugs");

        let registry = SkillRegistry::new(temp.path()).await.unwrap();
        let tool = SearchSkillsTool::new(registry);

        let result = tool.call(r#"{"query": "pdf-processing"}"#).await.unwrap();
        let skills: Vec<SkillSummary> = serde_json::from_str(&result).unwrap();

        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "pdf-processing");
    }

    #[tokio::test]
    async fn test_search_skills_partial_name() {
        let temp = TempDir::new().unwrap();
        create_test_skill(temp.path(), "pdf-processing", "Process PDF files");
        create_test_skill(temp.path(), "code-review", "Review code for bugs");

        let registry = SkillRegistry::new(temp.path()).await.unwrap();
        let tool = SearchSkillsTool::new(registry);

        let result = tool.call(r#"{"query": "pdf"}"#).await.unwrap();
        let skills: Vec<SkillSummary> = serde_json::from_str(&result).unwrap();

        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "pdf-processing");
    }

    #[tokio::test]
    async fn test_search_skills_description() {
        let temp = TempDir::new().unwrap();
        create_test_skill(temp.path(), "pdf-processing", "Process PDF files");
        create_test_skill(temp.path(), "code-review", "Review code for bugs");

        let registry = SkillRegistry::new(temp.path()).await.unwrap();
        let tool = SearchSkillsTool::new(registry);

        let result = tool.call(r#"{"query": "bugs"}"#).await.unwrap();
        let skills: Vec<SkillSummary> = serde_json::from_str(&result).unwrap();

        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "code-review");
    }

    #[tokio::test]
    async fn test_search_skills_no_match() {
        let temp = TempDir::new().unwrap();
        create_test_skill(temp.path(), "pdf-processing", "Process PDF files");

        let registry = SkillRegistry::new(temp.path()).await.unwrap();
        let tool = SearchSkillsTool::new(registry);

        let result = tool.call(r#"{"query": "nonexistent"}"#).await.unwrap();
        let skills: Vec<SkillSummary> = serde_json::from_str(&result).unwrap();

        assert!(skills.is_empty());
    }
}
