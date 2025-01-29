use crate::openai::{Function, Property, ToolCall, ToolType};
use serde::{Deserialize, Serialize};
use serde_json;
use std::process::Command;

#[derive(Serialize)]
pub struct NoteSearchProps {
    pub query: Property,
}

#[derive(Deserialize)]
pub struct NoteSearchArgs {
    pub query: String,
}

#[derive(Serialize)]
pub struct NoteSearchTool {
    pub r#type: ToolType,
    pub function: Function<NoteSearchProps>,
}

impl ToolCall for NoteSearchTool {
    fn call(&self, args: &str) -> String {
        let fn_args: NoteSearchArgs = serde_json::from_str(args).unwrap();

        let curl = Command::new("sh")
            .arg("-c")
            .arg(format!(
                "curl \"http://localhost:2222/notes/search?query={}\"",
                fn_args.query
            ))
            .output()
            .expect("failed to execute process");

        let stdout = std::str::from_utf8(&curl.stdout).expect("Failed to parse stdout");
        stdout.to_string()
    }

    fn function_name(&self) -> String {
        self.function.name.clone()
    }
}
