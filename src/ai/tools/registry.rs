use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use anyhow::{anyhow, Result};
use tokio_rusqlite::Connection;

use crate::ai::skills::SkillRegistry;
use crate::openai::{BoxedToolCall, ToolCall};

use super::{
    BashTool, CalendarTool, DateTimeTool, EmailSearchTool, EmailUnreadTool, ListSkillsTool,
    LoadSkillTool, MeetingSearchTool, MemoryTool, NoteSearchTool, NotifyTool, ReadSkillFileTool,
    SaveSkillTool, SearchSkillsTool, TasksDueTodayTool, TasksScheduledTodayTool, WebSearchTool,
    WebsiteViewTool, WorkOnSkillTool,
};

/// Shared dependencies needed to construct tools by name.
#[derive(Clone)]
pub struct ToolContext {
    pub db: Connection,
    pub api_base_url: String,
    pub storage_path: String,
    pub vapid_key_path: String,
    pub session_id: String,
    pub skill_registry: Option<Arc<RwLock<SkillRegistry>>>,
}

impl ToolContext {
    /// The shared skill-registry handle, or an error if none is configured.
    pub fn skill_registry_handle(&self, tool_name: &str) -> Result<Arc<RwLock<SkillRegistry>>> {
        self.skill_registry.clone().ok_or_else(|| {
            anyhow!("tool '{tool_name}' requires a skill registry, which is not configured")
        })
    }

    /// An owned clone of the shared skill registry, or an error if none is configured.
    pub fn skill_registry_clone(&self, tool_name: &str) -> Result<SkillRegistry> {
        let handle = self.skill_registry_handle(tool_name)?;
        let guard = handle
            .read()
            .map_err(|_| anyhow!("skill registry lock poisoned"))?;
        Ok(guard.clone())
    }

    /// The global skills directory path, or an error if none is configured.
    pub fn skills_dir(&self, tool_name: &str) -> Result<String> {
        let handle = self.skill_registry_handle(tool_name)?;
        let guard = handle
            .read()
            .map_err(|_| anyhow!("skill registry lock poisoned"))?;
        Ok(guard.dir_path().to_string_lossy().to_string())
    }
}

/// Construction trait. `function_name()` is inherited from [`ToolCall`], which stays
/// the single source of a tool's name.
pub trait Tool: ToolCall + Sized + Send + Sync + 'static {
    fn from_context(ctx: &ToolContext) -> Result<Self>;
}

type Ctor = Box<dyn Fn(&ToolContext) -> Result<BoxedToolCall> + Send + Sync>;

/// Maps a tool name (its `function_name`) to a constructor. Owns a [`ToolContext`].
pub struct ToolRegistry {
    context: ToolContext,
    constructors: HashMap<String, Ctor>,
}

impl ToolRegistry {
    /// A registry pre-populated with every built-in tool, using `context`.
    pub fn builtin(context: ToolContext) -> Self {
        let mut registry = Self {
            context,
            constructors: HashMap::new(),
        };
        registry.register::<BashTool>();
        registry.register::<WebsiteViewTool>();
        registry.register::<MemoryTool>();
        registry.register::<DateTimeTool>();
        registry.register::<NotifyTool>();
        registry.register::<CalendarTool>();
        registry.register::<NoteSearchTool>();
        registry.register::<MeetingSearchTool>();
        registry.register::<WebSearchTool>();
        registry.register::<EmailUnreadTool>();
        registry.register::<EmailSearchTool>();
        registry.register::<TasksDueTodayTool>();
        registry.register::<TasksScheduledTodayTool>();
        registry.register::<ListSkillsTool>();
        registry.register::<SearchSkillsTool>();
        registry.register::<LoadSkillTool>();
        registry.register::<ReadSkillFileTool>();
        registry.register::<SaveSkillTool>();
        registry.register::<WorkOnSkillTool>();
        registry
    }

    /// Register a tool type. Discovers its name by constructing one throwaway instance
    /// and reading `function_name()`. Tools whose `from_context` errors for this context
    /// (e.g. skill tools with no skill registry) are skipped.
    fn register<T: Tool>(&mut self) {
        let Ok(tool) = T::from_context(&self.context) else {
            return;
        };
        let name = tool.function_name();
        self.constructors.insert(
            name,
            Box::new(|ctx| Ok(Box::new(T::from_context(ctx)?) as BoxedToolCall)),
        );
    }

    /// Resolve a single tool by name.
    pub fn from_str(&self, name: &str) -> Result<BoxedToolCall> {
        let ctor = self.constructors.get(name).ok_or_else(|| {
            anyhow!(
                "unknown tool '{name}' (valid: {})",
                self.available_names().join(", ")
            )
        })?;
        ctor(&self.context)
    }

    /// Resolve a list of names, preserving order. Errors on the first unknown name.
    pub fn from_list(&self, names: &[String]) -> Result<Vec<BoxedToolCall>> {
        names.iter().map(|n| self.from_str(n)).collect()
    }

    /// Sorted tool names, for help / error messages.
    pub fn available_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.constructors.keys().cloned().collect();
        names.sort();
        names
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::db::{async_db, initialize_db};

    async fn test_context() -> ToolContext {
        let dir = tempfile::tempdir().unwrap();
        let db = async_db(dir.path().to_str().unwrap()).await.unwrap();
        db.call(|conn| {
            initialize_db(conn)?;
            Ok(())
        })
        .await
        .unwrap();
        ToolContext {
            db,
            api_base_url: "http://localhost:2222".to_string(),
            storage_path: dir.path().to_string_lossy().to_string(),
            vapid_key_path: String::new(),
            session_id: "test-session".to_string(),
            skill_registry: None,
        }
    }

    #[tokio::test]
    async fn resolves_known_tools() {
        let registry = ToolRegistry::builtin(test_context().await);
        assert_eq!(registry.from_str("bash").unwrap().function_name(), "bash");
        assert_eq!(
            registry.from_str("datetime").unwrap().function_name(),
            "datetime"
        );
    }

    #[tokio::test]
    async fn resolves_list_preserving_order() {
        let registry = ToolRegistry::builtin(test_context().await);
        let tools = registry
            .from_list(&["bash".to_string(), "datetime".to_string()])
            .unwrap();
        let names: Vec<String> = tools.iter().map(|t| t.function_name()).collect();
        assert_eq!(names, vec!["bash", "datetime"]);
    }

    #[tokio::test]
    async fn errors_on_unknown_tool() {
        let registry = ToolRegistry::builtin(test_context().await);
        let err = registry.from_str("bogus").err().unwrap().to_string();
        assert!(err.contains("unknown tool 'bogus'"));
    }

    #[tokio::test]
    async fn skill_tools_unavailable_without_registry() {
        // Skill tools are skipped at registration when the context has no
        // skill registry, so requesting one reports it as unknown.
        let registry = ToolRegistry::builtin(test_context().await);
        let err = registry
            .from_str("list_skills")
            .err()
            .unwrap()
            .to_string();
        assert!(err.contains("unknown tool 'list_skills'"));
        assert!(!registry.available_names().contains(&"list_skills".to_string()));
    }
}
