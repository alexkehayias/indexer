use crate::ai::tools::registry::{Tool, ToolConfig};
use crate::openai::{
    Function, Parameters, Property, RecoverableToolError, ToolCall, ToolType, parse_tool_args,
};
use anyhow::{Error, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

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

/// Runs Python code in an isolated WebAssembly sandbox with no access to the
/// host filesystem, environment, or network.
#[derive(Serialize)]
pub struct PythonTool {
    pub r#type: ToolType,
    pub function: Function<PythonProps>,
}

#[async_trait]
impl ToolCall for PythonTool {
    async fn call(&self, args: &str) -> Result<String, Error> {
        let fn_args: PythonArgs = parse_tool_args(args)?;

        // The sandbox is CPU-bound and can run for up to the timeout, so keep
        // it off the async runtime's worker threads.
        let code = fn_args.code;
        let exec =
            tokio::task::spawn_blocking(move || -> Result<ExecResult, RecoverableToolError> {
                let sandbox =
                    shared_sandbox().map_err(|e| RecoverableToolError::new(&e.to_string()))?;
                sandbox
                    .exec(&code)
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

    fn from_config(_conf: &ToolConfig) -> Result<Self> {
        Ok(Self::new())
    }
}

impl PythonTool {
    pub fn new() -> Self {
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
                "Execute Python code in an isolated sandbox and get the result. The sandbox has \
                 no filesystem, network, or environment access and only a subset of the standard \
                 library is available. Returns the value of the last expression along with \
                 anything written to stdout/stderr.",
            ),
            parameters,
            strict: true,
        };

        Self {
            r#type: ToolType::Function,
            function,
        }
    }
}

impl Default for PythonTool {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_python_expression() {
        let tool = PythonTool::new();
        let result = tool.call(r#"{"code": "21 * 2"}"#).await.unwrap();
        let output: PythonOutput = serde_json::from_str(&result).unwrap();

        assert_eq!(output.value, serde_json::json!(42));
    }

    #[tokio::test]
    async fn test_python_statements() {
        let tool = PythonTool::new();
        let code = "def fib(n):\n    a, b = 0, 1\n    for _ in range(n):\n        a, b = b, a + b\n    return a\nfib(10)";
        let args = serde_json::json!({ "code": code }).to_string();
        let result = tool.call(&args).await.unwrap();
        let output: PythonOutput = serde_json::from_str(&result).unwrap();

        assert_eq!(output.value, serde_json::json!(55));
    }

    #[tokio::test]
    async fn test_python_stdout() {
        let tool = PythonTool::new();
        let result = tool.call(r#"{"code": "print(\"hello\")"}"#).await.unwrap();
        let output: PythonOutput = serde_json::from_str(&result).unwrap();

        assert_eq!(output.stdout.trim(), "hello");
    }

    #[tokio::test]
    async fn test_python_error_is_recoverable() {
        let tool = PythonTool::new();
        let err = tool.call(r#"{"code": "1 / 0"}"#).await.unwrap_err();
        let recoverable = err
            .downcast_ref::<RecoverableToolError>()
            .expect("python execution errors should be recoverable by the agent");
        assert!(recoverable.message.contains("ZeroDivisionError"));
    }

    #[tokio::test]
    async fn test_python_function_name() {
        let tool = PythonTool::new();
        assert_eq!(tool.function_name(), "python");
    }
}
