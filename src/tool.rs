use crate::openai::{Function, Parameters, Property, ToolCall, ToolType};
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
    api_base_url: String,
}

impl ToolCall for NoteSearchTool {
    fn call(&self, args: &str) -> String {
        let fn_args: NoteSearchArgs = serde_json::from_str(args).unwrap();

        let curl = Command::new("sh")
            .arg("-c")
            .arg(format!(
                "curl --get --data-urlencode \"query={}\" \"{}/notes/search\"",
                fn_args.query, self.api_base_url
            ))
            .output()
            .expect("failed to execute process");

        if !&curl.status.success() {
            let stderr = std::str::from_utf8(&curl.stderr).expect("Failed to parse stderr");
            panic!("Note search API request failed: {}", stderr);
        }
        let stdout = std::str::from_utf8(&curl.stdout).expect("Failed to parse stdout");
        stdout.to_string()
    }

    fn function_name(&self) -> String {
        self.function.name.clone()
    }
}

impl NoteSearchTool {
    pub fn new(api_base_url: &str) -> Self {
        let function = Function {
            name: String::from("search_notes"),
            description: String::from("Find notes the user has written about."),
            parameters: Parameters {
                r#type: String::from("object"),
                properties: NoteSearchProps {
                    query: Property {
                        r#type: String::from("string"),
                        description: String::from("The query to use for searching notes that should be short and optimized for search.")
                    }
                },
                required: vec![String::from("query")],
                additional_properties: false,
            },
            strict: true,
        };
        Self {
            r#type: ToolType::Function,
            function,
            api_base_url: api_base_url.to_string(),
        }
    }
}

impl Default for NoteSearchTool {
    fn default() -> Self {
        Self::new("http://localhost:2222")
    }
}
