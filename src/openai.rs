use async_trait::async_trait;
use anyhow::{Error, Result};
use erased_serde;
use serde::Serialize;
use serde_json::{json, Value};
use std::env;

#[derive(Clone, Serialize)]
pub enum Role {
    #[serde(rename = "system")]
    System,
    #[serde(rename = "assistant")]
    Assistant,
    #[serde(rename = "user")]
    User,
    #[serde(rename = "tool")]
    Tool,
}

// Object {
//     "content": Null,
//     "refusal": Null,
//     "role": String("assistant"),
//     "tool_calls": Array [
//         Object {
//             "function": Object {
//                 "arguments": String("{\"query\":\"books\"}"),
//                 "name": String("search_notes")
//             },
//             "id": String("call_KCg5V0N5E7hHHrUwdefHBfgL"),
//             "type": String("function")
//         }
//     ]
// }
#[derive(Clone, Serialize)]
pub struct FunctionCallFn {
    pub arguments: String,
    pub name: String,
}

#[derive(Clone, Serialize)]
pub struct FunctionCall {
    pub function: FunctionCallFn,
    pub id: String,
    pub r#type: String,
}

#[derive(Clone, Serialize)]
pub struct Message {
    role: Role,
    #[serde(skip_serializing_if = "Option::is_none")]
    refusal: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<FunctionCall>>,
}

impl Message {
    pub fn new(role: Role, content: &str) -> Self {
        Message {
            role,
            refusal: None,
            content: Some(content.to_string()),
            tool_call_id: None,
            tool_calls: None,
        }
    }
    pub fn new_tool_call_request(tool_calls: Vec<FunctionCall>) -> Self {
        Message {
            role: Role::Assistant,
            refusal: None,
            content: None,
            tool_call_id: None,
            tool_calls: Some(tool_calls),
        }
    }
    pub fn new_tool_call_response(content: &str, tool_call_id: &str) -> Self {
        Message {
            role: Role::Tool,
            refusal: None,
            content: Some(content.to_string()),
            tool_call_id: Some(tool_call_id.to_string()),
            tool_calls: None,
        }
    }
}

#[derive(Serialize)]
pub struct Property {
    pub r#type: String,
    pub description: String,
}

#[derive(Serialize)]
pub struct Parameters<Props: Serialize> {
    pub r#type: String,
    pub properties: Props,
    pub required: Vec<String>,
    #[serde(rename = "additionalProperties")]
    pub additional_properties: bool,
}

#[derive(Serialize)]
pub struct Function<Props: Serialize> {
    pub name: String,
    pub description: String,
    pub parameters: Parameters<Props>,
    pub strict: bool,
}

#[derive(Serialize)]
pub enum ToolType {
    #[serde(rename = "function")]
    Function,
}

// Ugh. In order to pass around a collection of `Function` structs
// that can be dynamically dispatched using this trait, the trait
// object needs to implement `Serialize` but `serde` is not object
// safe so it will cause a compile error. Instead, we need to use
// `erased_serde` which _is_ object safe and can be used along with
// dynamic dispatch such that the calls to `serde::json` won't
// complain. Another way to do this is to use `typetag` which uses
// `erased_serde` and has somewhat nicer ergonomics. Still, the fact
// that you have to do these things and resolving the error is
// impossible without a good amount of Googling and ChatGPT'ing is
// annoying.
#[async_trait]
pub trait ToolCall: erased_serde::Serialize {
    async fn call(&self, args: &str) -> Result<String, Error>;
    fn function_name(&self) -> String;
}
erased_serde::serialize_trait_object!(ToolCall);

pub type BoxedToolCall = Box<dyn ToolCall + Send + Sync + 'static>;

pub async fn completion(
    messages: &Vec<Message>,
    tools: &Option<Vec<BoxedToolCall>>,
) -> Result<Value, Error> {
    let open_ai_key = env::var("OPENAI_API_KEY").unwrap();
    let mut payload = json!({
        "model": "gpt-4o-mini",
        "messages": messages,
    });
    if let Some(ref tools) = tools {
        payload["tools"] = json!(tools);
    }
    let response = reqwest::Client::new()
        .post("https://api.openai.com/v1/chat/completions")
        .bearer_auth(open_ai_key)
        .header("Content-Type", "application/json")
        .json(&payload)
        .send()
        .await?
        .json()
        .await?;

    Ok(response)
}
