use crate::ai::tools::registry::{Tool, ToolConfig};
use crate::openai::{
    Function, Parameters, Property, RecoverableToolError, ToolCall, ToolType, parse_tool_args,
};
use anyhow::{Error, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tokio::fs;

use super::sandbox::{ExecResult, shared_sandbox};

#[derive(Serialize)]
pub struct PythonProps {
    /// The Python code to execute.
    pub code: Property,
}

#[derive(Deserialize)]
pub struct PythonArgs {
    pub code: String,
}

/// Result of running Python code in the sandbox.
#[derive(Serialize, Deserialize)]
pub struct PythonOutput {
    /// The JSON value of the last expression, or `null`.
    pub value: serde_json::Value,
    /// Anything the code wrote to stdout.
    pub stdout: String,
    /// Anything the code wrote to stderr.
    pub stderr: String,
}

/// Runs Python code in an isolated WebAssembly sandbox. The only host access is
/// the session workspace, mounted at `/`, shared with the bash tool.
#[derive(Serialize)]
pub struct PythonTool {
    pub r#type: ToolType,
    pub function: Function<PythonProps>,
    #[serde(skip_serializing)]
    workspace_path: PathBuf,
}

#[async_trait]
impl ToolCall for PythonTool {
    async fn call(&self, args: &str) -> Result<String, Error> {
        let fn_args: PythonArgs = parse_tool_args(args)?;

        // The sandbox preopens this directory, so it must exist on the host
        // first. Create it asynchronously rather than blocking the async worker.
        fs::create_dir_all(&self.workspace_path).await?;

        // The sandbox is CPU-bound and can run for up to the timeout, so keep
        // it off the async runtime's worker threads.
        let code = fn_args.code;
        let workspace = self.workspace_path.clone();
        let exec =
            tokio::task::spawn_blocking(move || -> Result<ExecResult, RecoverableToolError> {
                let sandbox =
                    shared_sandbox().map_err(|e| RecoverableToolError::new(&e.to_string()))?;
                sandbox
                    .exec(&code, &workspace)
                    .map_err(|e| RecoverableToolError::new(&e.to_string()))
            })
            .await??;

        let value = serde_json::from_str(&exec.value)
            .unwrap_or_else(|_| serde_json::Value::String(exec.value.clone()));
        let output = PythonOutput {
            value,
            stdout: exec.stdout,
            stderr: exec.stderr,
        };

        Ok(serde_json::to_string(&output)?)
    }

    fn function_name(&self) -> String {
        Self::NAME.to_string()
    }
}

impl Tool for PythonTool {
    const NAME: &'static str = "python";

    fn from_config(conf: &ToolConfig) -> Result<Self> {
        Ok(Self::new(&conf.storage_path, &conf.session_id))
    }
}

impl PythonTool {
    pub fn new(storage_path: &str, session_id: &str) -> Self {
        // The same session workspace the bash tool mounts, so files written by
        // one tool are visible to the other.
        let workspace_path = PathBuf::from(format!("{}/workspace/{}", storage_path, session_id));

        let parameters = Parameters {
            r#type: String::from("object"),
            properties: PythonProps {
                code: Property {
                    r#type: String::from("string"),
                    description: String::from(
                        "The Python code to execute. The value of the last expression is returned. \
                         Use print() to produce stdout output.",
                    ),
                    r#enum: None,
                },
            },
            required: vec![String::from("code")],
            additional_properties: false,
        };
        let function = Function {
            name: Self::NAME.to_string(),
            description: String::from(
                "Execute Python code in an isolated sandbox and get the result. Files are read \
                 and written relative to `/`, which is the session workspace shared with the \
                 bash tool; nothing outside it is accessible. There is no network or environment \
                 access and only a subset of the standard library is available. Returns the value \
                 of the last expression along with anything written to stdout/stderr.",
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

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn temp_python_tool() -> PythonTool {
        let temp_dir = std::env::temp_dir();
        let temp_dir = temp_dir.to_string_lossy();
        let session_id = Uuid::new_v4().to_string();
        PythonTool::new(&temp_dir, &session_id)
    }

    #[tokio::test]
    async fn test_python_expression() {
        let tool = temp_python_tool();
        let result = tool.call(r#"{"code": "21 * 2"}"#).await.unwrap();
        let output: PythonOutput = serde_json::from_str(&result).unwrap();

        assert_eq!(output.value, serde_json::json!(42));
    }

    #[tokio::test]
    async fn test_python_statements() {
        let tool = temp_python_tool();
        let code = "def fib(n):\n    a, b = 0, 1\n    for _ in range(n):\n        a, b = b, a + b\n    return a\nfib(10)";
        let args = serde_json::json!({ "code": code }).to_string();
        let result = tool.call(&args).await.unwrap();
        let output: PythonOutput = serde_json::from_str(&result).unwrap();

        assert_eq!(output.value, serde_json::json!(55));
    }

    #[tokio::test]
    async fn test_python_stdout() {
        let tool = temp_python_tool();
        let result = tool.call(r#"{"code": "print(\"hello\")"}"#).await.unwrap();
        let output: PythonOutput = serde_json::from_str(&result).unwrap();

        assert_eq!(output.stdout.trim(), "hello");
    }

    #[tokio::test]
    async fn test_python_error_is_recoverable() {
        let tool = temp_python_tool();
        let err = tool.call(r#"{"code": "1 / 0"}"#).await.unwrap_err();
        let recoverable = err
            .downcast_ref::<RecoverableToolError>()
            .expect("python execution errors should be recoverable by the agent");
        assert!(recoverable.message.contains("ZeroDivisionError"));
    }

    #[tokio::test]
    async fn test_python_workspace_roundtrip() {
        let tool = temp_python_tool();

        // Write a file, then read it back in a separate call. The workspace
        // persists across calls even though each exec gets a fresh namespace.
        tool.call(r#"{"code": "open('/note.txt', 'w').write('hello')"}"#)
            .await
            .unwrap();
        let result = tool
            .call(r#"{"code": "open('/note.txt').read()"}"#)
            .await
            .unwrap();
        let output: PythonOutput = serde_json::from_str(&result).unwrap();

        assert_eq!(output.value, serde_json::json!("hello"));

        // The file lands at the workspace root on the host, so the bash tool
        // (which mounts the same directory at `/`) sees the same file.
        let host_path = tool.workspace_path.join("note.txt");
        assert_eq!(std::fs::read_to_string(host_path).unwrap(), "hello");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_python_cannot_escape_workspace() {
        let temp_dir = std::env::temp_dir();
        let session_id = Uuid::new_v4().to_string();
        let tool = PythonTool::new(&temp_dir.to_string_lossy(), &session_id);

        // A host file outside the workspace, reached via a symlink planted
        // inside it. WASI/cap-std must not follow the link out of the preopen.
        let secret = temp_dir.join(format!("secret-{}", Uuid::new_v4()));
        std::fs::write(&secret, "top-secret").unwrap();
        let workspace = temp_dir.join("workspace").join(&session_id);
        std::fs::create_dir_all(&workspace).unwrap();
        std::os::unix::fs::symlink(&secret, workspace.join("escape")).unwrap();

        match tool.call(r#"{"code": "open('/escape').read()"}"#).await {
            Ok(result) => {
                let output: PythonOutput = serde_json::from_str(&result).unwrap();
                assert_ne!(output.value, serde_json::json!("top-secret"));
            }
            Err(e) => assert!(e.downcast_ref::<RecoverableToolError>().is_some()),
        }
    }

    #[tokio::test]
    async fn test_python_function_name() {
        let tool = temp_python_tool();
        assert_eq!(tool.function_name(), "python");
    }
}
