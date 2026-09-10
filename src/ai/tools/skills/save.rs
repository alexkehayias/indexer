use crate::ai::skills::validation::validate_skill_name;
use crate::ai::skills::{Skill, SkillRegistry};
use crate::ai::tools::registry::{Tool, ToolContext};
use crate::ai::tools::skills::copy_dir;
use crate::openai::{Function, Parameters, Property, ToolCall, ToolType};
use anyhow::{Error, Result, anyhow};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use tokio::fs;

#[derive(Serialize)]
pub struct SaveSkillProps {
    pub name: Property,
}

#[derive(Deserialize)]
pub struct SaveSkillArgs {
    pub name: String,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct SaveSkillResult {
    pub name: String,
    /// Path where the skill was saved in the global skills directory
    pub skills_path: Option<String>,
    /// Whether this was a new skill or an update to an existing one
    pub created: Option<bool>,
    /// Whether the SKILL.md passed validation
    pub valid: bool,
    /// Validation error message if SKILL.md is invalid, with guidance on how to fix it
    pub validation_error: Option<String>,
    /// Non-fatal warning about the save (e.g. registry reload failure).
    /// The file was saved, but the skill may not be usable until the
    /// server is restarted.
    pub warning: Option<String>,
}

#[derive(Serialize)]
pub struct SaveSkillTool {
    pub r#type: ToolType,
    pub function: Function<SaveSkillProps>,
    #[serde(skip)]
    skills_path: PathBuf,
    #[serde(skip)]
    workspace_path: PathBuf,
    #[serde(skip)]
    registry: Arc<RwLock<SkillRegistry>>,
}

#[async_trait]
impl ToolCall for SaveSkillTool {
    async fn call(&self, args: &str) -> Result<String, Error> {
        let fn_args: SaveSkillArgs = serde_json::from_str(args).unwrap();
        let name = fn_args.name;

        validate_skill_name(&name)?;

        let workspace_skill = self.workspace_path.join(&name);
        if !workspace_skill.exists() || !workspace_skill.is_dir() {
            tracing::error!("Workspace skill does not exist at: {:?}", &workspace_skill);
            return Err(anyhow!(
                "Skill '{}' not found in workspace. Use work_on_skill first to copy it, \
                 or create the directory and SKILL.md using bash.",
                name
            ));
        }

        let skill_file = workspace_skill.join("SKILL.md");
        if !skill_file.exists() {
            tracing::error!("SKILL.md file doesn't exist at: {:?}", &skill_file);
            return Ok(serde_json::to_string(&SaveSkillResult {
                name: name.clone(),
                skills_path: None,
                created: None,
                valid: false,
                validation_error: Some(format!(
                    "SKILL.md not found in workspace directory '{}'. \
                     Every skill must have a SKILL.md file with YAML frontmatter containing 'name' and 'description'. \
                     Create the file using bash, then try save_skill again.",
                    workspace_skill.display()
                )),
                warning: None,
            })?);
        }

        // Validate the SKILL.md is well-formed by parsing it
        if let Err(e) = Skill::load_from_directory(&workspace_skill).await {
            tracing::error!("Skill is invalid: {:?}", &workspace_skill);
            return Ok(serde_json::to_string(&SaveSkillResult {
                name: name.clone(),
                skills_path: None,
                created: None,
                valid: false,
                validation_error: Some(format!(
                    "Invalid SKILL.md: {}. \
                     Fix the file using bash and try save_skill again. \
                     A valid SKILL.md has YAML frontmatter with 'name' and 'description' fields, \
                     e.g.:\n---\nname: {}\ndescription: <short description>\n---\n<skill instructions>",
                    e, name
                )),
                warning: None,
            })?);
        }

        // Replace the host directory for this skill with the
        // workspace version
        let global_dest = self.skills_path.join(&name);
        let created = !global_dest.exists();
        if global_dest.exists() {
            fs::remove_dir_all(&global_dest).await?;
        }
        copy_dir(&workspace_skill, &global_dest).await?;

        // Reload the registry to pick up changes
        let reload_error = 'block: {
            // Clone the registry while holding the lock, then drop
            // the lock before doing async I/O
            let mut reg = match self.registry.write() {
                Ok(guard) => guard.clone(),
                Err(_) => {
                    break 'block Some(
                        "Skill saved to disk but registry is not available. \
                         The skill may not be usable until the server is restarted."
                            .to_string(),
                    );
                }
            };

            match reg.reload().await {
                Ok(()) => {
                    if let Ok(mut guard) = self.registry.write() {
                        *guard = reg;
                    }
                    None
                }
                Err(e) => {
                    tracing::error!("Failed to reload skill registry after save: {}", e);
                    Some(format!(
                        "Skill saved to disk but registry reload failed ({}). \
                         The skill may not be available until the server is restarted.",
                        e
                    ))
                }
            }
        };

        let result = SaveSkillResult {
            name,
            skills_path: Some(global_dest.to_string_lossy().to_string()),
            created: Some(created),
            valid: true,
            validation_error: None,
            warning: reload_error,
        };
        Ok(serde_json::to_string(&result)?)
    }

    fn function_name(&self) -> String {
        self.function.name.clone()
    }
}

impl Tool for SaveSkillTool {
    fn from_context(ctx: &ToolContext) -> Result<Self> {
        let handle = ctx.skill_registry_handle("save_skill")?;
        let skills_dir = ctx.skills_dir("save_skill")?;
        Ok(Self::new(&skills_dir, &ctx.storage_path, &ctx.session_id, handle))
    }
}

impl SaveSkillTool {
    pub fn new(
        skills_path: &str,
        storage_path: &str,
        session_id: &str,
        registry: Arc<RwLock<SkillRegistry>>,
    ) -> Self {
        let workspace_path = PathBuf::from(format!("{}/workspace/{}", storage_path, session_id));

        let parameters = Parameters {
            r#type: String::from("object"),
            properties: SaveSkillProps {
                name: Property {
                    r#type: String::from("string"),
                    description: String::from(
                        "The name of the skill to save from your workspace to the global skills directory.",
                    ),
                    r#enum: None,
                },
            },
            required: vec!["name".to_string()],
            additional_properties: false,
        };
        let function = Function {
            name: String::from("save_skill"),
            description: String::from(
                "Save a skill from your workspace back to the global skills directory. \
                 The SKILL.md file is validated before saving. After saving, the skill \
                 registry is reloaded so changes are immediately available.",
            ),
            parameters,
            strict: true,
        };

        Self {
            r#type: ToolType::Function,
            function,
            skills_path: PathBuf::from(skills_path),
            workspace_path,
            registry,
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
    async fn test_save_skill_new() {
        let temp = TempDir::new().unwrap();
        let skills_dir = temp.path().join("skills");
        std::fs::create_dir_all(&skills_dir).unwrap();

        let storage_path = temp.path().to_string_lossy().to_string();
        let session_id = "test-session";
        let workspace = temp.path().join("workspace").join(session_id);

        // Create skill in workspace (simulating agent having created it)
        create_test_skill(&workspace, "new-skill");

        let registry = Arc::new(RwLock::new(SkillRegistry::new(&skills_dir).await.unwrap()));
        let tool = SaveSkillTool::new(
            &skills_dir.to_string_lossy(),
            &storage_path,
            session_id,
            registry.clone(),
        );

        let result = tool.call(r#"{"name": "new-skill"}"#).await.unwrap();
        let output: SaveSkillResult = serde_json::from_str(&result).unwrap();

        assert_eq!(output.name, "new-skill");
        assert!(output.created.unwrap());
        assert!(output.valid);

        // Verify it was copied to global path
        let saved_skill = skills_dir.join("new-skill").join("SKILL.md");
        assert!(saved_skill.exists());
    }

    #[tokio::test]
    async fn test_save_skill_update_existing() {
        let temp = TempDir::new().unwrap();
        let skills_dir = temp.path().join("skills");
        std::fs::create_dir_all(&skills_dir).unwrap();

        // Create existing skill at global path
        create_test_skill(&skills_dir, "existing-skill");

        let storage_path = temp.path().to_string_lossy().to_string();
        let session_id = "test-session";
        let workspace = temp.path().join("workspace").join(session_id);

        // Create modified version in workspace
        create_test_skill(&workspace, "existing-skill");
        let skill_content = r#"---
name: existing-skill
description: Updated description
---

Updated body.
"#;
        std::fs::write(
            workspace.join("existing-skill").join("SKILL.md"),
            skill_content,
        )
        .unwrap();

        let registry = Arc::new(RwLock::new(SkillRegistry::new(&skills_dir).await.unwrap()));
        let tool = SaveSkillTool::new(
            &skills_dir.to_string_lossy(),
            &storage_path,
            session_id,
            registry.clone(),
        );

        let result = tool.call(r#"{"name": "existing-skill"}"#).await.unwrap();
        let output: SaveSkillResult = serde_json::from_str(&result).unwrap();

        assert_eq!(output.name, "existing-skill");
        assert!(!output.created.unwrap());

        // Verify the updated content is at global path
        let saved =
            std::fs::read_to_string(skills_dir.join("existing-skill").join("SKILL.md")).unwrap();
        assert!(saved.contains("Updated description"));
    }

    #[tokio::test]
    async fn test_save_skill_not_in_workspace() {
        let temp = TempDir::new().unwrap();
        let skills_dir = temp.path().join("skills");
        std::fs::create_dir_all(&skills_dir).unwrap();

        let storage_path = temp.path().to_string_lossy().to_string();
        let registry = Arc::new(RwLock::new(SkillRegistry::new(&skills_dir).await.unwrap()));
        let tool = SaveSkillTool::new(
            &skills_dir.to_string_lossy(),
            &storage_path,
            "session",
            registry.clone(),
        );

        let result = tool.call(r#"{"name": "missing"}"#).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_save_skill_invalid_name() {
        let temp = TempDir::new().unwrap();
        let skills_dir = temp.path().join("skills");
        std::fs::create_dir_all(&skills_dir).unwrap();

        let storage_path = temp.path().to_string_lossy().to_string();
        let registry = Arc::new(RwLock::new(SkillRegistry::new(&skills_dir).await.unwrap()));
        let tool = SaveSkillTool::new(
            &skills_dir.to_string_lossy(),
            &storage_path,
            "session",
            registry.clone(),
        );

        let result = tool.call(r#"{"name": "INVALID"}"#).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_save_skill_registry_reloaded() {
        let temp = TempDir::new().unwrap();
        let skills_dir = temp.path().join("skills");
        std::fs::create_dir_all(&skills_dir).unwrap();

        let storage_path = temp.path().to_string_lossy().to_string();
        let session_id = "test-session";
        let workspace = temp.path().join("workspace").join(session_id);

        create_test_skill(&workspace, "brand-new");

        let registry = Arc::new(RwLock::new(SkillRegistry::new(&skills_dir).await.unwrap()));

        // Registry should not have the skill yet
        {
            let guard = registry.read().unwrap();
            assert!(!guard.has_skill("brand-new"));
        }

        let tool = SaveSkillTool::new(
            &skills_dir.to_string_lossy(),
            &storage_path,
            session_id,
            registry.clone(),
        );

        tool.call(r#"{"name": "brand-new"}"#).await.unwrap();

        // After save, registry should have the skill
        {
            let guard = registry.read().unwrap();
            assert!(guard.has_skill("brand-new"));
        }
    }

    #[tokio::test]
    async fn test_save_skill_invalid_frontmatter_returns_guidance() {
        let temp = TempDir::new().unwrap();
        let skills_dir = temp.path().join("skills");
        std::fs::create_dir_all(&skills_dir).unwrap();

        let storage_path = temp.path().to_string_lossy().to_string();
        let session_id = "test-session";
        let workspace = temp.path().join("workspace").join(session_id);
        let skill_dir = workspace.join("bad-skill");
        std::fs::create_dir_all(&skill_dir).unwrap();

        // SKILL.md with missing frontmatter
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "Just some text without frontmatter",
        )
        .unwrap();

        let registry = Arc::new(RwLock::new(SkillRegistry::new(&skills_dir).await.unwrap()));
        let tool = SaveSkillTool::new(
            &skills_dir.to_string_lossy(),
            &storage_path,
            session_id,
            registry.clone(),
        );

        // Should succeed (not error) but indicate invalid
        let result = tool.call(r#"{"name": "bad-skill"}"#).await.unwrap();
        let output: SaveSkillResult = serde_json::from_str(&result).unwrap();

        assert!(!output.valid);
        assert!(output.validation_error.is_some());
        assert!(output.skills_path.is_none());

        // Should not have been saved to global path
        assert!(!skills_dir.join("bad-skill").exists());
    }

    #[tokio::test]
    async fn test_save_skill_missing_skill_md_returns_guidance() {
        let temp = TempDir::new().unwrap();
        let skills_dir = temp.path().join("skills");
        std::fs::create_dir_all(&skills_dir).unwrap();

        let storage_path = temp.path().to_string_lossy().to_string();
        let session_id = "test-session";
        let workspace = temp.path().join("workspace").join(session_id);
        // Directory exists but no SKILL.md
        std::fs::create_dir_all(workspace.join("no-md-skill")).unwrap();

        let registry = Arc::new(RwLock::new(SkillRegistry::new(&skills_dir).await.unwrap()));
        let tool = SaveSkillTool::new(
            &skills_dir.to_string_lossy(),
            &storage_path,
            session_id,
            registry.clone(),
        );

        let result = tool.call(r#"{"name": "no-md-skill"}"#).await.unwrap();
        let output: SaveSkillResult = serde_json::from_str(&result).unwrap();

        assert!(!output.valid);
        assert!(output.validation_error.is_some());
    }
}
