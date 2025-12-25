use std::{collections::HashMap, time::Duration};
use tokio::sync::mpsc;

use anyhow::{Error, Result};
use async_trait::async_trait;
use erased_serde;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Serialize, Deserialize, Debug)]
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
#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct FunctionCallFn {
    pub arguments: String,
    pub name: String,
}

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct FunctionCall {
    pub function: FunctionCallFn,
    pub id: String,
    pub r#type: String,
}

#[derive(Clone, Serialize, Deserialize, Debug)]
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
    api_hostname: &str,
    api_key: &str,
    model: &str,
) -> Result<Value, Error> {
    let mut payload = json!({
        "model": model,
        "messages": messages,
    });
    if let Some(tools) = tools {
        payload["tools"] = json!(tools);
    }
    let url = format!("{}/v1/chat/completions", api_hostname.trim_end_matches("/"));
    let response = reqwest::Client::new()
        .post(url)
        .bearer_auth(api_key)
        .header("Content-Type", "application/json")
        .timeout(Duration::from_secs(60 * 10))
        .json(&payload)
        .send()
        .await?
        .json()
        .await?;

    Ok(response)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct FunctionInitDelta {
    name: String,
    arguments: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct FunctionArgsDelta {
    arguments: String,
}

// OpenAI has two different deltas to handle for tool calls that are
// slightly different and hard to notice, one with initial fields and
// then subsequent deltas for streaming the function arguments.
#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
enum ToolCallChunk {
    Init {
        id: String,
        index: usize,
        function: FunctionInitDelta,
        r#type: String,
    },
    ArgsDelta {
        index: usize,
        function: FunctionArgsDelta,
        r#type: String,
    },
}

// HACK: Streaming tool calls results in an incomplete struct until
// all the deltas are streamed so we need this "final" version of the
// tool call data even though it's largely a duplicate of the other
// tool call related structs
#[derive(Debug, Serialize, Deserialize)]
struct FunctionFinal {
    name: String,
    arguments: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct ToolCallFinal {
    id: String,
    index: usize,
    function: FunctionFinal,
    r#type: String,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum Delta {
    Content { content: String },

    Reasoning { reasoning: String },

    ToolCall { tool_calls: Vec<ToolCallChunk> },

    Stop {},
}

#[derive(Debug, Deserialize)]
struct CompletionChunkChoice {
    #[allow(dead_code)]
    index: usize,
    delta: Delta,
    finish_reason: Option<String>,
    #[allow(dead_code)]
    logprobs: Option<String>,
}

#[derive(Debug, Deserialize)]
struct CompletionChunk {
    #[allow(dead_code)]
    id: String,
    #[allow(dead_code)]
    created: usize,
    #[allow(dead_code)]
    model: String,
    #[allow(dead_code)]
    system_fingerprint: String,
    choices: Vec<CompletionChunkChoice>,
}

pub async fn completion_stream(
    tx: mpsc::UnboundedSender<String>,
    messages: &Vec<Message>,
    tools: &Option<Vec<BoxedToolCall>>,
    api_hostname: &str,
    api_key: &str,
    model: &str,
) -> Result<Value, Error> {
    let mut payload = json!({
        "model": model,
        "messages": messages,
        "stream": true,
    });
    if let Some(tools) = tools {
        payload["tools"] = json!(tools);
    }
    let url = format!("{}/v1/chat/completions", api_hostname.trim_end_matches("/"));
    let response = reqwest::Client::new()
        .post(url)
        .bearer_auth(api_key)
        .header("Content-Type", "application/json")
        .timeout(Duration::from_secs(60 * 5))
        .json(&payload)
        .send()
        .await?;

    let mut stream = response.bytes_stream();

    let mut content_buf = String::from("");
    let mut reasoning_buf: String = String::from("");
    let mut tool_calls: HashMap<usize, ToolCallFinal> = HashMap::new();

    'outer: while let Some(chunk) = stream.next().await {
        let chunk = chunk.expect("Invalid chunk");
        let chunk_str = std::str::from_utf8(&chunk)?.trim();

        // The result is ignored here because we want to complete
        // processing the response
        let _ = tx.send(
            chunk_str
                .strip_prefix("data: ")
                .expect("Failed to strip prefix")
                .to_string(),
        );

        // Parse SSE events
        if !chunk_str.starts_with("data: ") {
            continue;
        }

        // Sometimes there are multiple json rows within the same chunk
        let rows: Vec<&str> = chunk_str.split("data: ").collect();

        for data_str in rows.into_iter() {
            let data = data_str.trim();

            // Data can sometimes be empty. Not sure why.
            if data.is_empty() {
                continue;
            }

            // Handle the end of the stream
            if data == "[DONE]" {
                break 'outer;
            }

            // Process the delta
            let chunk = serde_json::from_str::<CompletionChunk>(data).inspect_err(|e| {
                tracing::error!("Parsing completion chunk failed for {}\nError:{}", data, e)
            })?;
            let choice = chunk.choices.first().expect("Missing choices field");

            match &choice.delta {
                Delta::Reasoning { reasoning } => {
                    if choice.finish_reason.is_some() {
                        break 'outer;
                    }
                    reasoning_buf += &reasoning.clone();
                }
                Delta::Content { content } => {
                    if choice.finish_reason.is_some() {
                        break 'outer;
                    }

                    content_buf += &content.clone();
                }
                Delta::ToolCall {
                    tool_calls: tool_call_deltas,
                } => {
                    if choice.finish_reason.is_some() {
                        break 'outer;
                    }
                    for tool_call_delta in tool_call_deltas.iter() {
                        match tool_call_delta {
                            ToolCallChunk::Init {
                                id,
                                index,
                                function,
                                r#type,
                            } => {
                                let init_tool_call = ToolCallFinal {
                                    index: *index,
                                    id: id.clone(),
                                    function: FunctionFinal {
                                        name: function.name.clone(),
                                        arguments: function.arguments.clone(),
                                    },
                                    r#type: r#type.clone(),
                                };
                                tool_calls.insert(*index, init_tool_call);
                            }
                            ToolCallChunk::ArgsDelta {
                                index, function, ..
                            } => {
                                tool_calls.entry(*index).and_modify(|v| {
                                    let args = function.arguments.clone();
                                    v.function.arguments += &args;
                                });
                            }
                        }
                    }
                }
                Delta::Stop {} => {
                    break 'outer;
                }
            }
        }
    }

    // Handle if this is a tool call or a content message
    if !tool_calls.is_empty() {
        let tool_call_message = tool_calls.values().collect::<Vec<_>>();
        let out = json!({
            "choices": [{"message": {"tool_calls": tool_call_message}}]
        });
        return Ok(out);
    }

    let out = json!({
        "choices": [
            {"message": {"content": content_buf}}
        ]
    });
    Ok(out)
}
