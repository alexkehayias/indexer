use std::{collections::HashMap, time::Duration};
use tokio::sync::mpsc;

use anyhow::{Error, Result};
use async_trait::async_trait;
use erased_serde;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::fmt;
use tracing;

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq)]
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

impl Role {
    pub fn as_str(&self) -> &'static str {
        match self {
            Role::System => "system",
            Role::Assistant => "assistant",
            Role::User => "user",
            Role::Tool => "tool",
        }
    }
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

/// The content of a chat message. OpenAI accepts either a plain string or an
/// array of content parts; the array form carries multimodal input such as
/// images alongside text.
#[derive(Clone, Serialize, Deserialize, Debug, PartialEq)]
#[serde(untagged)]
pub enum MessageContent {
    Text(String),
    Parts(Vec<ContentPart>),
}

impl MessageContent {
    /// The text of this content, if any. For the parts form, returns the first
    /// text part, which is where a message's text is stored.
    pub fn text(&self) -> Option<&str> {
        match self {
            MessageContent::Text(text) => Some(text.as_str()),
            MessageContent::Parts(parts) => parts.iter().find_map(|part| match part {
                ContentPart::Text { text } => Some(text.as_str()),
                ContentPart::ImageUrl { .. } => None,
            }),
        }
    }
}

impl From<String> for MessageContent {
    fn from(text: String) -> Self {
        MessageContent::Text(text)
    }
}

impl From<&str> for MessageContent {
    fn from(text: &str) -> Self {
        MessageContent::Text(text.to_string())
    }
}

/// A single part of a multimodal message.
#[derive(Clone, Serialize, Deserialize, Debug, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentPart {
    Text { text: String },
    ImageUrl { image_url: ImageUrl },
}

impl ContentPart {
    pub fn text(text: &str) -> Self {
        ContentPart::Text {
            text: text.to_string(),
        }
    }

    /// An image part. `url` is either a `data:` URL or a workspace-relative
    /// path (e.g. `/files/photo.png`) that is hydrated into a data URL before
    /// the message is sent to the model.
    pub fn image(url: &str) -> Self {
        ContentPart::ImageUrl {
            image_url: ImageUrl {
                url: url.to_string(),
            },
        }
    }
}

#[derive(Clone, Serialize, Deserialize, Debug, PartialEq)]
pub struct ImageUrl {
    pub url: String,
}

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct Message {
    role: Role,
    #[serde(skip_serializing_if = "Option::is_none")]
    refusal: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<MessageContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<FunctionCall>>,
}

impl Message {
    pub fn role(&self) -> &Role {
        &self.role
    }

    /// The text content of this message, if any. Multimodal messages return
    /// their text part; messages with only images return `None`.
    pub fn text(&self) -> Option<&str> {
        self.content.as_ref().and_then(|content| content.text())
    }

    /// Returns the tool call ID this message is responding to, if any.
    ///
    /// Only set on `Tool` role messages produced by tool call responses.
    pub fn tool_call_id(&self) -> Option<&str> {
        self.tool_call_id.as_deref()
    }

    pub fn new(role: Role, content: &str) -> Self {
        Message {
            role,
            refusal: None,
            content: Some(MessageContent::Text(content.to_string())),
            tool_call_id: None,
            tool_calls: None,
        }
    }

    /// A message whose content is a list of parts, used for multimodal input.
    pub fn new_with_parts(role: Role, parts: Vec<ContentPart>) -> Self {
        Message {
            role,
            refusal: None,
            content: Some(MessageContent::Parts(parts)),
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
            content: Some(MessageContent::Text(content.to_string())),
            tool_call_id: Some(tool_call_id.to_string()),
            tool_calls: None,
        }
    }
}

#[derive(Serialize, Default)]
pub struct Property {
    pub r#type: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r#enum: Option<Vec<String>>,
}

impl Property {
    pub fn new(r#type: &str, description: &str) -> Self {
        Self {
            r#type: r#type.to_string(),
            description: description.to_string(),
            r#enum: None,
        }
    }

    pub fn with_enum(mut self, enum_values: Vec<String>) -> Self {
        self.r#enum = Some(enum_values);
        self
    }
}

/// JSON-schema property for an array parameter. [`Property`] has no `items`
/// field, so array params need their own struct.
#[derive(Serialize)]
pub struct ArrayProperty {
    pub r#type: String,
    pub description: String,
    pub items: Property,
}

impl ArrayProperty {
    /// An array-of-strings parameter, e.g. a list of channel ids.
    pub fn strings(description: &str) -> Self {
        Self {
            r#type: "array".to_string(),
            description: description.to_string(),
            items: Property::new("string", "An item in the array."),
        }
    }
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
/// An error that a tool implementation can return to signal that the
/// LLM can recover from this error — typically by retrying the
/// operation or adjusting its approach based on the error message.
///
/// Unlike fatal errors (which crash the chat loop), recoverable errors
/// are returned to the LLM as tool response messages, giving it a
/// chance to adapt.
#[derive(Debug, Clone)]
pub struct RecoverableToolError {
    pub message: String,
}

impl RecoverableToolError {
    pub fn new(message: &str) -> Self {
        Self {
            message: message.to_string(),
        }
    }
}

impl fmt::Display for RecoverableToolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for RecoverableToolError {}

/// Parse tool arguments from a JSON string into the expected type.
/// Returns a [`RecoverableToolError`] on failure so the LLM can
/// correct the JSON and retry, instead of crashing the chat loop.
pub fn parse_tool_args<Args: serde::de::DeserializeOwned>(
    args: &str,
) -> Result<Args, RecoverableToolError> {
    serde_json::from_str(args).map_err(|e| {
        RecoverableToolError::new(&format!(
            "Invalid tool arguments: failed to parse JSON: {e}",
        ))
    })
}

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
//
// `type` is optional: OpenAI sends it on every chunk, but some
// OpenAI-compatible servers (e.g. ds4) omit it on the argument deltas.
#[derive(Debug, Serialize, Deserialize)]
#[serde(untagged)]
enum ToolCallChunk {
    Init {
        id: String,
        index: usize,
        function: FunctionInitDelta,
        #[serde(default)]
        r#type: Option<String>,
    },
    ArgsDelta {
        index: usize,
        function: FunctionArgsDelta,
        #[serde(default)]
        r#type: Option<String>,
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

// A single streaming delta, modeled as a bag of optional fields rather
// than an untagged enum. Servers vary in which fields they send (some
// include `role` with content, some send `reasoning` vs
// `reasoning_content`, some omit `type` on tool-call deltas) and a
// delta can carry more than one kind of field at once. A struct handles
// all of those and ignores unknown fields. An untagged enum with a
// catch-all variant was tried before, but the empty catch-all silently
// matched any object it didn't recognize — which truncated the stream
// whenever a delta failed to parse.
#[derive(Debug, Default, Deserialize)]
struct Delta {
    #[serde(default)]
    content: Option<String>,

    #[serde(default)]
    reasoning: Option<String>,

    // Qwen uses "reasoning_content" instead of "reasoning"
    #[serde(default)]
    reasoning_content: Option<String>,

    #[serde(default)]
    tool_calls: Option<Vec<ToolCallChunk>>,
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
    system_fingerprint: Option<String>,
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
        "stream_options": {"include_usage": true}
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
        .await?;

    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        anyhow::bail!("API returned error status {}: {}", status, body);
    }

    let mut stream = response.bytes_stream();

    let mut content_buf = String::from("");
    let mut reasoning_buf: String = String::from("");
    let mut tool_calls: HashMap<usize, ToolCallFinal> = HashMap::new();
    let mut buffer = String::new();

    'outer: while let Some(chunk) = stream.next().await {
        let chunk = chunk.expect("Invalid chunk");
        let chunk_str = match std::str::from_utf8(&chunk) {
            Ok(s) => s,
            Err(e) => {
                tracing::error!(
                    "Failed to decode chunk as UTF-8: {}. Chunk bytes (len={}): {:?}",
                    e,
                    chunk.len(),
                    &chunk[..chunk.len().min(100)]
                );
                return Err(anyhow::anyhow!("Failed to decode chunk as UTF-8: {}", e));
            }
        };

        // Append new data to buffer. This is necessary to handle SSE
        // fragmentation over HTTP/2 frames.
        buffer.push_str(chunk_str);

        // Process all complete SSE events from the buffer
        while let Some(event_end) = buffer.find("\n\n") {
            let event_data = buffer[..event_end].to_string();
            buffer = buffer[event_end + 2..].to_string();

            // Skip empty events
            let event_data = event_data.trim();
            if event_data.is_empty() {
                continue;
            }

            // Parse SSE events
            if !event_data.starts_with("data: ") {
                continue;
            }

            // Extract the JSON payload (after "data: ")
            let data = event_data[6..].trim();

            // Data can sometimes be empty. Not sure why.
            if data.is_empty() {
                continue;
            }

            // Forward the chunk to the receiver channel
            // (The result is ignored here because we want to complete
            // processing the response)
            let send_result = tx.send(data.to_string());
            if send_result.is_err() {
                tracing::warn!("Failed to send chunk to channel (receiver likely dropped)");
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

            // Accumulate every field present in this delta. A single
            // delta may carry more than one kind of field.
            if let Some(reasoning) = &choice.delta.reasoning {
                reasoning_buf += reasoning;
            }
            // Qwen3.5 sends back `reasoning_content`. This may
            // also be a breaking API change from LM Studio
            if let Some(reasoning_content) = &choice.delta.reasoning_content {
                reasoning_buf += reasoning_content;
            }
            if let Some(content) = &choice.delta.content {
                content_buf += content;
            }
            if let Some(tool_call_deltas) = &choice.delta.tool_calls {
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
                                r#type: r#type.clone().unwrap_or_else(|| "function".to_string()),
                            };
                            tool_calls.insert(*index, init_tool_call);
                        }
                        ToolCallChunk::ArgsDelta {
                            index, function, ..
                        } => {
                            tool_calls.entry(*index).and_modify(|v| {
                                v.function.arguments += &function.arguments;
                            });
                        }
                    }
                }
            }

            // Stop once the server signals the turn is complete. Checked
            // after accumulating so a final chunk carrying content and a
            // finish reason isn't dropped.
            if choice.finish_reason.is_some() {
                break 'outer;
            }
        }
    }

    // Leftover data here is expected: when we break out on a finish_reason
    // or [DONE], the remainder of that network chunk (final usage chunk and
    // [DONE]) is left unprocessed. It carries nothing we need, so just log at
    // trace level for debugging.
    if !buffer.is_empty() {
        tracing::trace!("Leftover data in buffer after processing: {:?}", buffer);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_role_serialization() {
        assert_eq!(serde_json::to_string(&Role::System).unwrap(), r#""system""#);
        assert_eq!(
            serde_json::to_string(&Role::Assistant).unwrap(),
            r#""assistant""#
        );
        assert_eq!(serde_json::to_string(&Role::User).unwrap(), r#""user""#);
        assert_eq!(serde_json::to_string(&Role::Tool).unwrap(), r#""tool""#);
    }

    #[test]
    fn test_role_deserialization() {
        let json = r#""system""#;
        assert_eq!(serde_json::from_str::<Role>(json).unwrap(), Role::System);

        let json = r#""assistant""#;
        assert_eq!(serde_json::from_str::<Role>(json).unwrap(), Role::Assistant);

        let json = r#""user""#;
        assert_eq!(serde_json::from_str::<Role>(json).unwrap(), Role::User);

        let json = r#""tool""#;
        assert_eq!(serde_json::from_str::<Role>(json).unwrap(), Role::Tool);
    }

    #[test]
    fn test_message_new() {
        let msg = Message::new(Role::User, "Hello world");
        assert_eq!(
            serde_json::to_string(&msg).unwrap(),
            r#"{"role":"user","content":"Hello world"}"#
        );

        let msg = Message::new(Role::Assistant, "I can help!");
        assert_eq!(
            serde_json::to_string(&msg).unwrap(),
            r#"{"role":"assistant","content":"I can help!"}"#
        );
    }

    #[test]
    fn test_message_with_parts_serialization() {
        let msg = Message::new_with_parts(
            Role::User,
            vec![
                ContentPart::text("what is this?"),
                ContentPart::image("data:image/png;base64,AAAA"),
            ],
        );
        assert_eq!(
            serde_json::to_string(&msg).unwrap(),
            r#"{"role":"user","content":[{"type":"text","text":"what is this?"},{"type":"image_url","image_url":{"url":"data:image/png;base64,AAAA"}}]}"#
        );
    }

    #[test]
    fn test_message_deserializes_string_content() {
        let msg: Message = serde_json::from_str(r#"{"role":"user","content":"hello"}"#).unwrap();
        assert_eq!(msg.text(), Some("hello"));
    }

    #[test]
    fn test_message_deserializes_parts_content() {
        let json = r#"{"role":"user","content":[{"type":"text","text":"hi"},{"type":"image_url","image_url":{"url":"/files/a.png"}}]}"#;
        let msg: Message = serde_json::from_str(json).unwrap();
        assert_eq!(msg.text(), Some("hi"));
    }

    #[test]
    fn test_message_new_tool_call_request() {
        let tool_calls = vec![FunctionCall {
            function: FunctionCallFn {
                arguments: r#"{"query":"books"}"#.to_string(),
                name: "search_notes".to_string(),
            },
            id: "call_test123".to_string(),
            r#type: "function".to_string(),
        }];

        let msg = Message::new_tool_call_request(tool_calls);
        assert_eq!(
            serde_json::to_string(&msg).unwrap(),
            r#"{"role":"assistant","tool_calls":[{"function":{"arguments":"{\"query\":\"books\"}","name":"search_notes"},"id":"call_test123","type":"function"}]}"#
        );
    }

    #[test]
    fn test_message_new_tool_call_response() {
        let msg = Message::new_tool_call_response("Found 3 books", "call_test123");
        assert_eq!(
            serde_json::to_string(&msg).unwrap(),
            r#"{"role":"tool","content":"Found 3 books","tool_call_id":"call_test123"}"#
        );
    }

    #[test]
    fn test_function_call_serialization() {
        let fc = FunctionCallFn {
            arguments: r#"{"query":"test"}"#.to_string(),
            name: "my_function".to_string(),
        };
        assert_eq!(
            serde_json::to_string(&fc).unwrap(),
            r#"{"arguments":"{\"query\":\"test\"}","name":"my_function"}"#
        );
    }

    #[test]
    fn test_function_call_deserialization() {
        let json = r#"{"arguments":"{\"query\":\"test\"}","name":"my_function"}"#;
        let fc: FunctionCallFn = serde_json::from_str(json).unwrap();
        assert_eq!(fc.name, "my_function");
        assert_eq!(fc.arguments, r#"{"query":"test"}"#);
    }

    #[test]
    fn test_function_call_full_serialization() {
        let fc = FunctionCall {
            function: FunctionCallFn {
                arguments: r#"{"query":"books"}"#.to_string(),
                name: "search_notes".to_string(),
            },
            id: "call_test123".to_string(),
            r#type: "function".to_string(),
        };
        let json = serde_json::to_string(&fc).unwrap();
        assert!(json.contains(r#""arguments":"{\"query\":\"books\"}"#));
        assert!(json.contains(r#""name":"search_notes""#));
        assert!(json.contains(r#""id":"call_test123""#));
        assert!(json.contains(r#""type":"function""#));
    }

    #[test]
    fn test_function_call_full_deserialization() {
        let json = r#"{
            "function": {"arguments":"{\"query\":\"books\"}","name":"search_notes"},
            "id":"call_test123",
            "type":"function"
        }"#;
        let fc: FunctionCall = serde_json::from_str(json).unwrap();
        assert_eq!(fc.id, "call_test123");
        assert_eq!(fc.r#type, "function");
        assert_eq!(fc.function.name, "search_notes");
        assert_eq!(fc.function.arguments, r#"{"query":"books"}"#);
    }

    #[test]
    fn test_tool_type_serialization() {
        assert_eq!(
            serde_json::to_string(&ToolType::Function).unwrap(),
            r#""function""#
        );
    }

    #[test]
    fn test_property_serialization() {
        let prop = Property {
            r#type: "string".to_string(),
            description: "The search query".to_string(),
            r#enum: None,
        };
        assert_eq!(
            serde_json::to_string(&prop).unwrap(),
            r#"{"type":"string","description":"The search query"}"#
        );
    }

    #[test]
    fn test_parameters_serialization() {
        let props = serde_json::json!({"query": {"type": "string", "description": "Search query"}});
        let params = Parameters {
            r#type: "object".to_string(),
            properties: props.clone(),
            required: vec!["query".to_string()],
            additional_properties: false,
        };
        let json = serde_json::to_value(&params).unwrap();
        assert_eq!(json["type"], "object");
        assert_eq!(json["required"].as_array().unwrap()[0], "query");
        assert_eq!(json["additionalProperties"], false);
    }

    #[test]
    fn test_function_serialization() {
        let props = serde_json::json!({"query": {"type": "string", "description": "Search query"}});
        let params = Parameters {
            r#type: "object".to_string(),
            properties: props,
            required: vec!["query".to_string()],
            additional_properties: false,
        };
        let func = Function {
            name: "search_notes".to_string(),
            description: "Search through notes".to_string(),
            parameters: params,
            strict: true,
        };
        let json = serde_json::to_value(&func).unwrap();
        assert_eq!(json["name"], "search_notes");
        assert_eq!(json["description"], "Search through notes");
    }

    #[test]
    fn test_delta_content_deserialization() {
        let json = r#"{"content":"Hello"}"#;
        let delta: Delta = serde_json::from_str(json).unwrap();
        assert_eq!(delta.content.as_deref(), Some("Hello"));
    }

    #[test]
    fn test_delta_reasoning_deserialization() {
        let json = r#"{"reasoning":"Thinking..."}"#;
        let delta: Delta = serde_json::from_str(json).unwrap();
        assert_eq!(delta.reasoning.as_deref(), Some("Thinking..."));
    }

    #[test]
    fn test_delta_reasoning_content_deserialization() {
        // Qwen uses "reasoning_content" instead of "reasoning"
        let json = r#"{"reasoning_content":"Thinking..."}"#;
        let delta: Delta = serde_json::from_str(json).unwrap();
        assert_eq!(delta.reasoning_content.as_deref(), Some("Thinking..."));
    }

    #[test]
    fn test_delta_empty_deserialization() {
        let json = r#"{}"#;
        let delta: Delta = serde_json::from_str(json).unwrap();
        assert!(delta.content.is_none());
        assert!(delta.reasoning.is_none());
        assert!(delta.reasoning_content.is_none());
        assert!(delta.tool_calls.is_none());
    }

    #[test]
    fn test_delta_ignores_unknown_fields() {
        // e.g. the leading `{"role":"assistant"}` delta some servers send
        let json = r#"{"role":"assistant"}"#;
        let delta: Delta = serde_json::from_str(json).unwrap();
        assert!(delta.content.is_none());
        assert!(delta.tool_calls.is_none());
    }

    #[test]
    fn test_delta_tool_call_deserialization() {
        let json = r#"{
            "tool_calls": [{
                "id":"call_abc",
                "index":0,
                "function":{"name":"search","arguments":"{\"q\":\"test\"}"},
                "type":"function"
            }]
        }"#;
        let delta: Delta = serde_json::from_str(json).unwrap();
        assert_eq!(delta.tool_calls.unwrap().len(), 1);
    }

    #[test]
    fn test_tool_call_chunk_init_deserialization() {
        let json = r#"{
            "id":"call_abc",
            "index":0,
            "function":{"name":"search","arguments":"{"},
            "type":"function"
        }"#;
        let chunk: ToolCallChunk = serde_json::from_str(json).unwrap();
        match chunk {
            ToolCallChunk::Init {
                id,
                index,
                function,
                r#type,
            } => {
                assert_eq!(id, "call_abc");
                assert_eq!(index, 0);
                assert_eq!(function.name, "search");
                assert_eq!(r#type.as_deref(), Some("function"));
            }
            _ => panic!("Expected Init variant"),
        }
    }

    #[test]
    fn test_tool_call_chunk_args_delta_deserialization() {
        let json = r#"{
            "index":0,
            "function":{"arguments":"\"q\":\"test\"}"},
            "type":"function"
        }"#;
        let chunk: ToolCallChunk = serde_json::from_str(json).unwrap();
        match chunk {
            ToolCallChunk::ArgsDelta {
                index,
                function,
                r#type,
            } => {
                assert_eq!(index, 0);
                assert_eq!(function.arguments, r#""q":"test"}"#);
                assert_eq!(r#type.as_deref(), Some("function"));
            }
            _ => panic!("Expected ArgsDelta variant"),
        }
    }

    #[test]
    fn test_tool_call_chunk_args_delta_without_type() {
        // ds4 omits `type` on argument deltas; must still parse as ArgsDelta
        let json = r#"{"index":0,"function":{"arguments":"{\"city\":\"Paris\"}"}}"#;
        let chunk: ToolCallChunk = serde_json::from_str(json).unwrap();
        match chunk {
            ToolCallChunk::ArgsDelta {
                index,
                function,
                r#type,
            } => {
                assert_eq!(index, 0);
                assert_eq!(function.arguments, r#"{"city":"Paris"}"#);
                assert!(r#type.is_none());
            }
            _ => panic!("Expected ArgsDelta variant"),
        }
    }

    #[test]
    fn test_completion_chunk_deserialization() {
        let json = r#"{
            "id":"chunk_123",
            "created":1234567890,
            "model":"gpt-4",
            "system_fingerprint":"fp_abc123",
            "choices":[{
                "index":0,
                "delta":{"content":"Hello"},
                "finish_reason":null
            }]
        }"#;
        let chunk: CompletionChunk = serde_json::from_str(json).unwrap();
        assert_eq!(chunk.id, "chunk_123");
        assert_eq!(chunk.created, 1234567890);
        assert_eq!(chunk.model, "gpt-4");
    }

    #[tokio::test]
    async fn test_completion_basic() {
        let mut server = mockito::Server::new_async().await;

        let response_body = r#"{
            "id": "chatcmpl-123",
            "object": "chat.completion",
            "created": 1694268190,
            "model": "gpt-4",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "Hello!"
                },
                "finish_reason": "stop"
            }]
        }"#;

        let mock = server
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(response_body)
            .create();

        let messages = vec![Message::new(Role::User, "Hi")];
        let result = completion(&messages, &None, server.url().as_str(), "test-key", "gpt-4").await;

        mock.assert();
        assert!(result.is_ok());

        let json = result.unwrap();
        assert_eq!(json["choices"][0]["message"]["content"], "Hello!");
    }

    #[tokio::test]
    async fn test_completion_with_tools() {
        let mut server = mockito::Server::new_async().await;

        let response_body = r#"{
            "id": "chatcmpl-123",
            "object": "chat.completion",
            "created": 1694268190,
            "model": "gpt-4",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "tool_calls": [{
                        "id": "call_abc123",
                        "type": "function",
                        "function": {
                            "name": "search_notes",
                            "arguments": "{\"query\":\"test\"}"
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        }"#;

        let mock = server
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(response_body)
            .create();

        let messages = vec![Message::new(Role::User, "Search for test")];

        // Create a mock tool
        #[derive(serde::Serialize)]
        struct MockTool;
        #[async_trait]
        impl ToolCall for MockTool {
            async fn call(&self, _args: &str) -> Result<String, Error> {
                Ok("mock result".to_string())
            }
            fn function_name(&self) -> String {
                "search_notes".to_string()
            }
        }

        let tools = Some(vec![Box::new(MockTool) as BoxedToolCall]);

        let result = completion(
            &messages,
            &tools,
            server.url().as_str(),
            "test-key",
            "gpt-4",
        )
        .await;

        mock.assert();
        assert!(result.is_ok());

        let json = result.unwrap();
        assert!(json["choices"][0]["message"]["tool_calls"].is_array());
    }

    #[tokio::test]
    async fn test_completion_stream_content() {
        let mut server = mockito::Server::new_async().await;

        // SSE response with content chunks
        let sse_response = r#"data: {"id":"chunk1","created":1234567890,"model":"gpt-4","system_fingerprint":"fp1","choices":[{"index":0,"delta":{"content":"Hello"},"finish_reason":null}]}

data: {"id":"chunk2","created":1234567890,"model":"gpt-4","system_fingerprint":"fp1","choices":[{"index":0,"delta":{"content":" World"},"finish_reason":null}]}

data: {"id":"chunk3","created":1234567890,"model":"gpt-4","system_fingerprint":"fp1","choices":[{"index":0,"delta":{"content":"!"},"finish_reason":"stop"}]}

data: [DONE]

"#;

        let mock = server
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_header("content-type", "text/event-stream")
            .with_body(sse_response)
            .create();

        let messages = vec![Message::new(Role::User, "Say hello")];
        let (tx, mut rx) = mpsc::unbounded_channel();
        let server_url = server.url();

        // Run completion_stream in a separate task
        let handle = tokio::spawn(async move {
            completion_stream(
                tx,
                &messages,
                &None,
                server_url.as_str(),
                "test-key",
                "gpt-4",
            )
            .await
        });

        // Wait for the task to complete
        let result = tokio::time::timeout(tokio::time::Duration::from_secs(5), handle).await;

        mock.assert();
        assert!(result.is_ok());
        assert!(result.unwrap().unwrap().is_ok());

        // The channel should have received the raw JSON chunks
        let mut chunk_count = 0;
        while rx.try_recv().is_ok() {
            chunk_count += 1;
        }
        assert!(chunk_count >= 3);
    }

    #[tokio::test]
    async fn test_completion_stream_tool_call() {
        let mut server = mockito::Server::new_async().await;

        // SSE response with tool call chunks
        let sse_response = r#"data: {"id":"chunk1","created":1234567890,"model":"gpt-4","system_fingerprint":"fp1","choices":[{"index":0,"delta":{"tool_calls":[{"id":"call_abc123","index":0,"function":{"name":"search_notes","arguments":"{\"query\":"},"type":"function"}]},"finish_reason":null}]}

data: {"id":"chunk2","created":1234567890,"model":"gpt-4","system_fingerprint":"fp1","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"test\"}"}}]},"finish_reason":null}]}

data: {"id":"chunk3","created":1234567890,"model":"gpt-4","system_fingerprint":"fp1","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":""}}]},"finish_reason":"stop"}]}

data: [DONE]

"#;

        let mock = server
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_header("content-type", "text/event-stream")
            .with_body(sse_response)
            .create();

        let messages = vec![Message::new(Role::User, "Search for test")];
        let (tx, _rx) = mpsc::unbounded_channel();
        let server_url = server.url();

        // Run completion_stream in a separate task
        let handle = tokio::spawn(async move {
            completion_stream(
                tx,
                &messages,
                &None,
                server_url.as_str(),
                "test-key",
                "gpt-4",
            )
            .await
        });

        // Wait for the task to complete
        let result = tokio::time::timeout(tokio::time::Duration::from_secs(5), handle).await;

        mock.assert();
        assert!(result.is_ok());
        assert!(result.unwrap().unwrap().is_ok());
    }

    // Regression test for ds4 streaming tool calls.
    // Unlike the OpenAI fixture, ds4 emits a leading `{"role":"assistant"}`
    // delta, streams content before the tool call, omits `type` on the
    // argument deltas, sends the tool-call init chunk with an empty
    // `arguments` string, and ends with `finish_reason:"tool_calls"` on an
    // empty delta. Previously the argument deltas failed to parse and were
    // silently swallowed by a catch-all `Stop` variant, truncating the stream
    // and leaving the tool arguments empty.
    #[tokio::test]
    async fn test_completion_stream_ds4_tool_call() {
        let mut server = mockito::Server::new_async().await;

        let sse_response = r#"data: {"id":"c","object":"chat.completion.chunk","created":1,"model":"test-model","choices":[{"index":0,"delta":{"role":"assistant"},"finish_reason":null}]}

data: {"id":"c","object":"chat.completion.chunk","created":1,"model":"test-model","choices":[{"index":0,"delta":{"content":"I'll check the weather in Paris for you."},"finish_reason":null}]}

data: {"id":"c","object":"chat.completion.chunk","created":1,"model":"test-model","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_47c4859f7facaccd5f14a35ddedda70e","type":"function","function":{"name":"get_weather","arguments":""}}]},"finish_reason":null}]}

data: {"id":"c","object":"chat.completion.chunk","created":1,"model":"test-model","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{"}}]},"finish_reason":null}]}

data: {"id":"c","object":"chat.completion.chunk","created":1,"model":"test-model","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"city\":\""}}]},"finish_reason":null}]}

data: {"id":"c","object":"chat.completion.chunk","created":1,"model":"test-model","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"Paris"}}]},"finish_reason":null}]}

data: {"id":"c","object":"chat.completion.chunk","created":1,"model":"test-model","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\""}}]},"finish_reason":null}]}

data: {"id":"c","object":"chat.completion.chunk","created":1,"model":"test-model","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"}"}}]},"finish_reason":null}]}

data: {"id":"c","object":"chat.completion.chunk","created":1,"model":"test-model","choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}

data: [DONE]

"#;

        let mock = server
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_header("content-type", "text/event-stream")
            .with_body(sse_response)
            .create();

        let messages = vec![Message::new(Role::User, "What is the weather in Paris?")];
        let (tx, _rx) = mpsc::unbounded_channel();
        let server_url = server.url();

        let result = tokio::time::timeout(
            tokio::time::Duration::from_secs(5),
            completion_stream(tx, &messages, &None, server_url.as_str(), "test-key", "m"),
        )
        .await
        .unwrap()
        .unwrap();

        mock.assert();

        let calls = result["choices"][0]["message"]["tool_calls"]
            .as_array()
            .expect("expected tool_calls in response");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0]["function"]["name"], "get_weather");
        assert_eq!(calls[0]["function"]["arguments"], r#"{"city":"Paris"}"#);
    }

    // Regression test for the omlx inference server. It differs from both
    // OpenAI and ds4: it returns the entire tool call (name and complete
    // arguments) in a single Init chunk rather than streaming the arguments.
    // It also interleaves `keepalive` chunks carrying a role+empty-content
    // delta, a role-only delta with no `finish_reason` key at all, and
    // `reasoning_content` deltas. It still ends with an empty delta carrying
    // `finish_reason`, followed by a usage chunk with an empty `choices` array
    // that must not be parsed.
    #[tokio::test]
    async fn test_completion_stream_omlx_tool_call() {
        let mut server = mockito::Server::new_async().await;

        let sse_response = r#"data: {"id":"c","object":"chat.completion.chunk","created":0,"model":"keepalive","choices":[{"index":0,"delta":{"role":"assistant","content":""},"finish_reason":null}]}

data: {"id":"c","object":"chat.completion.chunk","created":1,"model":"test-model","choices":[{"index":0,"delta":{"role":"assistant"}}]}

data: {"id":"c","object":"chat.completion.chunk","created":1,"model":"test-model","choices":[{"index":0,"delta":{"reasoning_content":"The user wants the weather."}}]}

data: {"id":"c","object":"chat.completion.chunk","created":1,"model":"test-model","choices":[{"index":0,"delta":{"content":"\n\n"}}]}

data: {"id":"c","object":"chat.completion.chunk","created":1,"model":"test-model","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_085bd900","type":"function","function":{"name":"get_weather","arguments":"{\"city\": \"Paris\"}"}}]}}]}

data: {"id":"c","object":"chat.completion.chunk","created":1,"model":"test-model","choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}

data: {"id":"c","object":"chat.completion.chunk","created":1,"model":"test-model","choices":[],"usage":{"prompt_tokens":10,"completion_tokens":5,"total_tokens":15}}

data: [DONE]

"#;

        let mock = server
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_header("content-type", "text/event-stream")
            .with_body(sse_response)
            .create();

        let messages = vec![Message::new(Role::User, "What is the weather in Paris?")];
        let (tx, _rx) = mpsc::unbounded_channel();
        let server_url = server.url();

        let result = tokio::time::timeout(
            tokio::time::Duration::from_secs(5),
            completion_stream(tx, &messages, &None, server_url.as_str(), "test-key", "m"),
        )
        .await
        .unwrap()
        .unwrap();

        mock.assert();

        let calls = result["choices"][0]["message"]["tool_calls"]
            .as_array()
            .expect("expected tool_calls in response");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0]["function"]["name"], "get_weather");
        assert_eq!(calls[0]["function"]["arguments"], r#"{"city": "Paris"}"#);
    }

    #[tokio::test]
    async fn test_completion_stream_reasoning() {
        let mut server = mockito::Server::new_async().await;

        // SSE response with reasoning chunks
        let sse_response = r#"data: {"id":"chunk1","created":1234567890,"model":"gpt-4","system_fingerprint":"fp1","choices":[{"index":0,"delta":{"reasoning":"Thinking"},"finish_reason":null}]}

data: {"id":"chunk2","created":1234567890,"model":"gpt-4","system_fingerprint":"fp1","choices":[{"index":0,"delta":{"reasoning":"..."},"finish_reason":null}]}

data: {"id":"chunk3","created":1234567890,"model":"gpt-4","system_fingerprint":"fp1","choices":[{"index":0,"delta":{"content":"Done!"},"finish_reason":"stop"}]}

data: [DONE]

"#;

        let mock = server
            .mock("POST", "/v1/chat/completions")
            .with_status(200)
            .with_header("content-type", "text/event-stream")
            .with_body(sse_response)
            .create();

        let messages = vec![Message::new(Role::User, "Think about this")];
        let (tx, _rx) = mpsc::unbounded_channel();
        let server_url = server.url();

        // Run completion_stream in a separate task
        let handle = tokio::spawn(async move {
            completion_stream(
                tx,
                &messages,
                &None,
                server_url.as_str(),
                "test-key",
                "gpt-4",
            )
            .await
        });

        // Wait for the task to complete
        let result = tokio::time::timeout(tokio::time::Duration::from_secs(5), handle).await;

        mock.assert();
        assert!(result.is_ok());
        assert!(result.unwrap().unwrap().is_ok());
    }

    #[test]
    fn test_recoverable_tool_error_creation() {
        let err = RecoverableToolError::new("Request timed out");
        assert_eq!(err.message, "Request timed out");
    }

    #[test]
    fn test_recoverable_tool_error_display() {
        let err = RecoverableToolError::new("Server returned 500");
        assert_eq!(format!("{}", err), "Server returned 500");
    }

    #[test]
    fn test_recoverable_tool_error_downcast_from_anyhow() {
        let err: anyhow::Error = RecoverableToolError::new("temporary failure").into();
        let downcasted = err.downcast_ref::<RecoverableToolError>();
        assert!(downcasted.is_some());
        assert_eq!(downcasted.unwrap().message, "temporary failure");
    }

    #[test]
    fn test_recoverable_tool_error_non_recoverable_not_downcastable() {
        let err: anyhow::Error = anyhow::anyhow!("fatal database error");
        let downcasted = err.downcast_ref::<RecoverableToolError>();
        assert!(downcasted.is_none());
    }
}
