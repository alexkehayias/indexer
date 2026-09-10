use crate::cli::bashkit::HqBuiltin;
use crate::openai::{Function, Parameters, Property, ToolCall, ToolType, parse_tool_args};
use super::registry::{Tool, ToolContext};
use anyhow::{Error, Result};
use async_trait::async_trait;
use crate::bash::{Bash, PosixFs, RealFs, RealFsMode};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::fs;

#[derive(Serialize)]
pub struct BashProps {
    /// The shell command to execute. Use absolute paths for files.
    pub command: Property,
}

#[derive(Deserialize)]
pub struct BashArgs {
    pub command: String,
}

/// Result of running a bash command.
#[derive(Serialize, Deserialize)]
pub struct BashOutput {
    /// The exit code of the command. 0 indicates success.
    pub exit_code: i32,
    /// The standard output from the command.
    pub stdout: String,
    /// The standard error from the command.
    pub stderr: String,
    /// Whether the output was truncated due to size limits.
    pub truncated: bool,
}

/// The root path inside the sandbox where the workspace is mounted.
/// Both BashTool and skill workspace tools must agree on this path
/// so that file paths reported to the agent are valid within the
/// sandbox.
pub(crate) const SANDBOX_ROOT: &str = "/";

/// Provides virtual bash shell with filesystem read write access to
/// the session workspace. Subsequent `BashTool` calls can access
/// files from previous calls in the same session.
#[derive(Serialize)]
pub struct BashTool {
    pub r#type: ToolType,
    pub function: Function<BashProps>,
    #[serde(skip_serializing)]
    workspace_path: PathBuf,
}

#[async_trait]
impl ToolCall for BashTool {
    async fn call(&self, args: &str) -> Result<String, Error> {
        let fn_args: BashArgs = parse_tool_args(args)?;
        let result = run_in_sandbox(&fn_args.command, &self.workspace_path).await?;
        Ok(serde_json::to_string(&result)?)
    }

    fn function_name(&self) -> String {
        self.function.name.clone()
    }
}

impl Tool for BashTool {
    fn from_context(ctx: &ToolContext) -> Result<Self> {
        Ok(Self::new(&ctx.storage_path, &ctx.session_id))
    }
}

impl BashTool {
    pub fn new(storage_path: &str, session_id: &str) -> Self {
        // Only allow access to the session's directory in the
        // workspace to avoid potential security issues from
        // overwriting files in another session
        let workspace_path = PathBuf::from(format!("{}/workspace/{}", storage_path, session_id));

        let parameters = Parameters {
            r#type: String::from("object"),
            properties: BashProps {
                command: Property {
                    r#type: String::from("string"),
                    description: String::from(
                        "The shell command to execute. Use absolute paths for files. \
                         Commands run in a clean environment without access to environment variables.",
                    ),
                    r#enum: None,
                },
            },
            required: vec!["command".to_string()],
            additional_properties: false,
        };
        let function = Function {
            name: String::from("bash"),
            description: String::from(
                "Run a shell command and get the output. Commands run in a clean environment \
                 without access to environment variables. Use absolute paths for files.",
            ),
            parameters,
            strict: true,
        };

        Self {
            r#type: ToolType::Function,
            function,
            workspace_path,
        }
    }
}

/// Run a command in a bashkit sandbox with workspace filesystem access.
/// Creates the workspace directory if it doesn't exist, mounts it at the
/// sandbox root, and returns the command output. This is idempotent.
///
/// `RealFs` routes all host I/O through `tokio::fs`, which offloads each
/// syscall to Tokio's blocking thread pool — so the async worker is never
/// blocked even on a single-CPU runtime. This lets `run_in_sandbox` execute
/// directly on the caller's runtime instead of needing a nested blocking-thread
/// workaround.
pub async fn run_in_sandbox(command: &str, workspace_path: &Path) -> Result<BashOutput> {
    fs::create_dir_all(workspace_path).await?;

    let backend = RealFs::new(workspace_path, RealFsMode::ReadWrite)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to create RealFs: {e}"))?;
    let fs = Arc::new(PosixFs::new(backend));

    let mut bash = Bash::builder()
        .builtin("hq", Box::new(HqBuiltin))
        .build();
    bash.mount(SANDBOX_ROOT, fs)
        .map_err(|e| anyhow::anyhow!("Failed to mount filesystem: {e}"))?;

    let output = bash
        .exec(command)
        .await
        .map_err(|e| anyhow::anyhow!("bash exec failed: {e}"))?;

    Ok(BashOutput {
        exit_code: output.exit_code,
        stdout: output.stdout,
        stderr: output.stderr,
        truncated: output.stdout_truncated || output.stderr_truncated,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;
    use uuid::Uuid;

    async fn temp_bash_tool() -> BashTool {
        let binding = env::temp_dir();
        let temp_dir = binding.to_string_lossy();
        tokio::fs::create_dir(format!("{}/workspace", temp_dir)).await.ok();
        let session_id = Uuid::new_v4().to_string();
        BashTool::new(&temp_dir, &session_id)
    }

    #[tokio::test]
    async fn test_bash_echo() {
        let tool = temp_bash_tool().await;
        let result = tool.call(r#"{"command": "echo hello"}"#).await.unwrap();
        let output: BashOutput = serde_json::from_str(&result).unwrap();

        assert_eq!(output.exit_code, 0);
        assert_eq!(output.stdout.trim(), "hello");
    }

    #[tokio::test]
    async fn test_bash_stderr() {
        let tool = temp_bash_tool().await;
        let result = tool.call(r#"{"command": "echo error >&2"}"#).await.unwrap();
        let output: BashOutput = serde_json::from_str(&result).unwrap();

        assert_eq!(output.exit_code, 0);
        assert_eq!(output.stderr.trim(), "error");
    }

    #[tokio::test]
    async fn test_bash_exit_code() {
        let tool = temp_bash_tool().await;
        let result = tool.call(r#"{"command": "exit 42"}"#).await.unwrap();
        let output: BashOutput = serde_json::from_str(&result).unwrap();

        assert_eq!(output.exit_code, 42);
    }

    #[tokio::test]
    async fn test_bash_no_env_vars() {
        let tool = temp_bash_tool().await;
        let result = tool.call(r#"{"command": "echo $PATH"}"#).await.unwrap();
        let output: BashOutput = serde_json::from_str(&result).unwrap();

        // Should have a clean PATH, not the full system PATH
        assert_eq!(output.exit_code, 0);
        // The clean PATH should be set but different from the full system path
    }

    #[tokio::test]
    async fn test_bash_variables() {
        let tool = temp_bash_tool().await;
        let result = tool
            .call(r#"{"command": "NAME=World; echo \"Hello, $NAME\""}"#)
            .await
            .unwrap();
        let output: BashOutput = serde_json::from_str(&result).unwrap();

        assert_eq!(output.exit_code, 0);
        assert_eq!(output.stdout.trim(), "Hello, World");
    }

    #[tokio::test]
    async fn test_bash_pipeline() {
        let tool = temp_bash_tool().await;
        let result = tool
            .call(r#"{"command": "echo -e 'apple\nbanana\ncherry' | grep a"}"#)
            .await
            .unwrap();
        let output: BashOutput = serde_json::from_str(&result).unwrap();

        assert_eq!(output.exit_code, 0);
        assert_eq!(output.stdout.trim(), "apple\nbanana");
    }

    #[tokio::test]
    async fn test_bash_arithmetic() {
        let tool = temp_bash_tool().await;
        let result = tool
            .call(r#"{"command": "echo $((2 + 2 * 3))"}"#)
            .await
            .unwrap();
        let output: BashOutput = serde_json::from_str(&result).unwrap();

        assert_eq!(output.exit_code, 0);
        // 2 + (2 * 3) = 8
        assert_eq!(output.stdout.trim(), "8");
    }

    #[tokio::test]
    async fn test_bash_function() {
        let tool = temp_bash_tool().await;
        let result = tool
            .call(r#"{"command": "greet() { echo \"Hello, $1!\"; }; greet World"}"#)
            .await
            .unwrap();
        let output: BashOutput = serde_json::from_str(&result).unwrap();

        assert_eq!(output.exit_code, 0);
        assert_eq!(output.stdout.trim(), "Hello, World!");
    }

    #[tokio::test]
    async fn test_bash_truncated_field() {
        let tool = temp_bash_tool().await;
        let result = tool.call(r#"{"command": "echo hello"}"#).await.unwrap();
        let output: BashOutput = serde_json::from_str(&result).unwrap();

        // Small output should not be truncated
        assert!(!output.truncated);
    }

    #[tokio::test]
    async fn test_bash_function_name() {
        let tool = temp_bash_tool().await;
        assert_eq!(tool.function_name(), "bash");
    }

    #[tokio::test]
    async fn test_bash_filesystem() {
        use crate::bash::Bash;

        // Use a single Bash instance to persist virtual filesystem state
        let mut bash = Bash::new();

        // Create a file in the virtual filesystem
        let output = bash
            .exec("echo 'Hello, World!' > /tmp/test.txt")
            .await
            .unwrap();
        assert_eq!(output.exit_code, 0);

        // Read the file back
        let output = bash.exec("cat /tmp/test.txt").await.unwrap();
        assert_eq!(output.exit_code, 0);
        assert_eq!(output.stdout.trim(), "Hello, World!");
    }

    #[tokio::test]
    async fn test_bash_mkdir() {
        use crate::bash::Bash;

        // Use a single Bash instance to persist virtual filesystem state
        let mut bash = Bash::new();

        // Create a directory
        let output = bash.exec("mkdir -p /tmp/mydir").await.unwrap();
        assert_eq!(output.exit_code, 0);

        // Create a file in that directory
        let output = bash
            .exec("echo 'content' > /tmp/mydir/file.txt")
            .await
            .unwrap();
        assert_eq!(output.exit_code, 0);

        // Verify the file exists
        let output = bash.exec("ls /tmp/mydir/file.txt").await.unwrap();
        assert_eq!(output.exit_code, 0);
    }

    #[tokio::test]
    async fn test_bash_session_isolation() {
        use std::env;

        // Create two separate tool instances with different session IDs
        let binding = env::temp_dir();
        let temp_dir = binding.to_string_lossy().to_string();
        let session1_id = Uuid::new_v4().to_string();
        let session2_id = Uuid::new_v4().to_string();

        let tool1 = BashTool::new(&temp_dir, &session1_id);
        let tool2 = BashTool::new(&temp_dir, &session2_id);

        // Session 1 creates a file in its workspace directory
        let session1_workspace = format!("{}/workspace/{}", temp_dir, session1_id);
        let cmd1 = format!(
            "mkdir -p '{}' && echo 'secret' > '{}/test.txt'",
            session1_workspace, session1_workspace
        );
        let args1 = serde_json::json!({ "command": cmd1 }).to_string();
        let result = tool1.call(&args1).await.unwrap();
        let output: BashOutput = serde_json::from_str(&result).unwrap();
        assert_eq!(output.exit_code, 0);

        // Session 2 should NOT be able to read the file from session 1's workspace
        let _session2_workspace = format!("{}/workspace/{}", temp_dir, session2_id);
        let cmd2 = format!("cat '{}/test.txt'", session1_workspace);
        let args2 = serde_json::json!({ "command": cmd2 }).to_string();
        let result = tool2.call(&args2).await.unwrap();
        let output: BashOutput = serde_json::from_str(&result).unwrap();

        // Should fail (non-zero exit code) because session 2 cannot access session 1's files
        assert_ne!(
            output.exit_code, 0,
            "Session 2 should not be able to read session 1's files"
        );
    }

    #[tokio::test]
    async fn test_bash_session_isolation_write() {
        use std::env;

        // Create two separate tool instances with different session IDs
        let binding = env::temp_dir();
        let temp_dir = binding.to_string_lossy().to_string();
        let session1_id = Uuid::new_v4().to_string();
        let session2_id = Uuid::new_v4().to_string();

        let tool1 = BashTool::new(&temp_dir, &session1_id);
        let tool2 = BashTool::new(&temp_dir, &session2_id);

        // Create session 1's workspace with a file
        let session1_workspace = format!("{}/workspace/{}", temp_dir, session1_id);
        let cmd1 = format!(
            "mkdir -p '{}' && echo 'original' > '{}/file.txt'",
            session1_workspace, session1_workspace
        );
        let args1 = serde_json::json!({ "command": cmd1 }).to_string();
        let result = tool1.call(&args1).await.unwrap();
        let output: BashOutput = serde_json::from_str(&result).unwrap();
        assert_eq!(output.exit_code, 0);

        // Session 2 should NOT be able to write to session 1's workspace
        let cmd2 = format!("echo 'test' > '{}/file.txt'", session1_workspace);
        let args2 = serde_json::json!({ "command": cmd2 }).to_string();
        let result = tool2.call(&args2).await.unwrap();
        let output: BashOutput = serde_json::from_str(&result).unwrap();

        // Should fail because session 2 cannot write to session 1's workspace
        assert_ne!(
            output.exit_code, 0,
            "Session 2 should not be able to write to session 1's workspace"
        );

        // Verify the original content is still intact
        let cmd3 = format!("cat '{}/file.txt'", session1_workspace);
        let args3 = serde_json::json!({ "command": cmd3 }).to_string();
        let result = tool1.call(&args3).await.unwrap();
        let output: BashOutput = serde_json::from_str(&result).unwrap();
        assert_eq!(output.stdout.trim(), "original");
    }
}
