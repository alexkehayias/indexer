use crate::ai::skills::SkillRegistry;
use crate::ai::tools::registry::{Tool, ToolContext};
use crate::openai::{Function, Parameters, ToolCall, ToolType};
use anyhow::{Error, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

#[derive(Serialize)]
pub struct ListSkillsProps {}

#[derive(Deserialize)]
pub struct ListSkillsArgs {}

#[derive(Serialize)]
pub struct ListSkillsTool {
    pub r#type: ToolType,
    pub function: Function<ListSkillsProps>,
    #[serde(skip)]
    registry: SkillRegistry,
}

#[async_trait]
impl ToolCall for ListSkillsTool {
    async fn call(&self, _args: &str) -> Result<String, Error> {
        let skills = self.registry.list_skills();
        Ok(serde_json::to_string(&skills)?)
    }

    fn function_name(&self) -> String {
        self.function.name.clone()
    }
}

impl Tool for ListSkillsTool {
    fn from_context(ctx: &ToolContext) -> Result<Self> {
        Ok(Self::new(ctx.skill_registry_clone("list_skills")?))
    }
}

impl ListSkillsTool {
    pub fn new(registry: SkillRegistry) -> Self {
        let parameters = Parameters {
            r#type: String::from("object"),
            properties: ListSkillsProps {},
            required: vec![],
            additional_properties: false,
        };
        let function = Function {
            name: String::from("list_skills"),
            description: String::from(
                "List all available skills that can be used to help the user. \
                 Each skill has a name and description explaining when to use it.",
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
    use crate::ai::skills::{SkillRegistry, SkillSummary};
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
    async fn test_list_skills() {
        let temp = TempDir::new().unwrap();
        create_test_skill(temp.path(), "pdf-processing", "Process PDF files");
        create_test_skill(temp.path(), "code-review", "Review code for bugs");

        let registry = SkillRegistry::new(temp.path()).await.unwrap();
        let tool = ListSkillsTool::new(registry);

        let result = tool.call("{}").await.unwrap();
        let skills: Vec<SkillSummary> = serde_json::from_str(&result).unwrap();

        assert_eq!(skills.len(), 2);
        let names: Vec<_> = skills.iter().map(|s| s.name.clone()).collect();
        assert!(names.contains(&"code-review".to_string()));
        assert!(names.contains(&"pdf-processing".to_string()));
    }

    #[tokio::test]
    async fn test_list_skills_empty() {
        let temp = TempDir::new().unwrap();

        let registry = SkillRegistry::new(temp.path()).await.unwrap();
        let tool = ListSkillsTool::new(registry);

        let result = tool.call("{}").await.unwrap();
        let skills: Vec<SkillSummary> = serde_json::from_str(&result).unwrap();

        assert!(skills.is_empty());
    }
}
