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
}

impl ToolCall for NoteSearchTool {
    fn call(&self, args: &str) -> String {
        let fn_args: NoteSearchArgs = serde_json::from_str(args).unwrap();

        let curl = Command::new("sh")
            .arg("-c")
            .arg(format!(
                "curl --get --data-urlencode \"query={}\" \"http://localhost:2222/notes/search\"",
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

impl Default for NoteSearchTool {
    fn default() -> Self {
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
        }
    }
}
