use anyhow::{anyhow, bail, Error, Result};
use futures_util::future::try_join_all;
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_rusqlite::Connection;

use crate::openai::{
    BoxedToolCall, FunctionCall, FunctionCallFn, Message, Role, completion,
    completion_stream,
};

async fn handle_tool_call(tools: &Vec<BoxedToolCall>, tool_call: &Value) -> Result<Vec<Message>, Error> {
    let tool_call_id = &tool_call["id"].as_str().ok_or(anyhow!("Tool call missing ID: {}", tool_call))?;
    let tool_call_function = &tool_call["function"];
    let tool_call_args = tool_call_function["arguments"].as_str().ok_or(anyhow!("Tool call missing arguments: {}", tool_call))?;
    let tool_call_name = tool_call_function["name"].as_str().ok_or(anyhow!("Tool call missing name: {}", tool_call))?;

    tracing::debug!("\nTool call: {}\nargs: {}", &tool_call_name, &tool_call_args);

    // Call the tool and get the next completion from the result
    let tool_call_result = tools
        .iter()
        .find(|i| *i.function_name() == *tool_call_name)
        .ok_or( anyhow!("Received tool call that doesn't exist: {}", tool_call_name))?
        .call(tool_call_args)
        .await?;

    let tool_call_request = vec![FunctionCall {
        function: FunctionCallFn {
            arguments: tool_call_args.to_string(),
            name: tool_call_name.to_string(),
        },
        id: tool_call_id.to_string(),
        r#type: String::from("function"),
    }];
    let results = vec![
        Message::new_tool_call_request(tool_call_request),
        Message::new_tool_call_response(
            &tool_call_result,
            tool_call_id,
        )
    ];

    Ok(results)
}

async fn handle_tool_calls(
    tools: &Vec<BoxedToolCall>,
    tool_calls: &Vec<Value>,
) -> Result<Vec<Message>, Error> {
    // Run each tool call concurrently and return them in order. I'm
    // not sure if ordering really matters for OpenAI compatible API
    // implementations, but better to be safe. This could also be
    // done using a `futures::stream` and `FutureUnordered` which
    // would be more efficient as it runs on the same thread, but that
    // causes lifetime issues that I don't understand how to get
    // around.
    let futures = tool_calls.iter().map(|call| handle_tool_call(tools, call));
    // Flatten the results to match what the API is expecting.
    let results = try_join_all(futures).await?.into_iter().flatten().collect();
    Ok(results)
}

/// Runs the next turn in chat by passing a transcript to the LLM for
/// the next response. Can return multiple messages when there are
/// tool calls.
pub async fn chat(
    tools: &Option<Vec<BoxedToolCall>>,
    history: &Vec<Message>,
    api_hostname: &str,
    api_key: &str,
    model: &str,
) -> Result<Vec<Message>, Error> {
    let mut updated_history = history.to_owned();
    let mut messages = Vec::new();

    let mut resp = completion(history, tools, api_hostname, api_key, model)
        .await?;

    let tools_ref = tools
        .as_ref()
        .expect("Received tool call but no tools were specified");

    // Tool calls need to be handled for the chat to proceed
    while let Some(tool_calls) = resp["choices"][0]["message"]["tool_calls"].as_array() {
        if tool_calls.is_empty() {
            break;
        }

        let tool_call_msgs = handle_tool_calls(tools_ref, tool_calls).await?;
        for m in tool_call_msgs.into_iter() {
            messages.push(m.clone());
            updated_history.push(m);
        }

        // Provide the results of the tool calls back to the chat
        resp = completion(&updated_history, tools, api_hostname, api_key, model)
            .await?;
    }

    if let Some(msg) = resp["choices"][0]["message"]["content"].as_str() {
        messages.push(Message::new(Role::Assistant, msg));
    } else {
        panic!("No message received. Resp:\n\n {}", resp);
    }

    Ok(messages)
}

/// Runs the next turn in chat by passing a transcript to the LLM and
/// the next response is streamed via the transmitter channel
/// `tx`. Also returns the next messages so they can be processed
/// further. Can return multiple messages when there are tool calls.
pub async fn chat_stream(
    tx: mpsc::UnboundedSender<String>,
    tools: &Option<Vec<BoxedToolCall>>,
    history: &Vec<Message>,
    api_hostname: &str,
    api_key: &str,
    model: &str,
) -> Result<Vec<Message>, Error> {
    let mut updated_history = history.to_owned();
    let mut messages = Vec::new();

    let mut resp = completion_stream(tx.clone(), history, tools, api_hostname, api_key, model)
        .await?;

    // Tool calls need to be handled for the chat to proceed
    while let Some(tool_calls) = resp["choices"][0]["message"]["tool_calls"].as_array() {
        if tool_calls.is_empty() {
            break;
        }
        let tools_ref = tools
            .as_ref()
            .expect("Received tool call but no tools were specified");

        // TODO: Update this to be streaming
        let tool_call_msgs = handle_tool_calls(tools_ref, tool_calls).await?;
        for m in tool_call_msgs.into_iter() {
            messages.push(m.clone());
            updated_history.push(m);
        }

        // Provide the results of the tool calls back to the chat
        resp = completion_stream(tx.clone(), &updated_history, tools, api_hostname, api_key, model)
            .await?;
    }

    if let Some(msg) = resp["choices"][0]["message"]["content"].as_str() {
        messages.push(Message::new(Role::Assistant, msg));
    } else {
        bail!("No message received. Resp:\n\n {}", resp);
    }

    Ok(messages)
}

pub async fn insert_chat_message(
    db: &Connection,
    session_id: &str,
    msg: &Message,
) -> Result<usize, Error> {
    let s_id = session_id.to_owned();
    let data = json!(msg).to_string();
    let result = db
        .call(move |conn| {
            let mut stmt =
                conn.prepare("INSERT INTO chat_message (session_id, data) VALUES (?, ?)")?;
            let result = stmt.execute([s_id, data])?;
            Ok(result)
        })
        .await?;

    Ok(result)
}

pub async fn find_chat_session_by_id(
    db: &Connection,
    session_id: &str,
) -> Result<Vec<Message>, Error> {
    let s_id = session_id.to_owned();
    let history = db.call(move |conn| {
        let mut stmt = conn.prepare("SELECT data FROM chat_message WHERE session_id=?")?;
        let rows = stmt
            .query_map([s_id], |i| {
                let val: String = i.get(0)?;
                let msg: Message = serde_json::from_str(&val).unwrap();
                Ok(msg)
            })?
            .filter_map(Result::ok)
            .collect::<Vec<Message>>();
        Ok(rows)
    });
    Ok(history.await?)
}
