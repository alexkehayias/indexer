use anyhow::{Error, Result};
use serde_json::{Value, json};
use tokio_rusqlite::Connection;

use crate::openai::{
    BoxedToolCall, FunctionCall, FunctionCallFn, Message, Role, ToolCall, completion,
};

async fn handle_tool_calls(
    tools: &Vec<Box<dyn ToolCall + Send + Sync + 'static>>,
    history: &mut Vec<Message>,
    accum_new: &mut Vec<Message>,
    tool_calls: &Vec<Value>,
) {
    // Handle each tool call
    for tool_call in tool_calls {
        let tool_call_id = &tool_call["id"].as_str().unwrap();
        let tool_call_function = &tool_call["function"];
        let tool_call_args = tool_call_function["arguments"].as_str().unwrap();
        let tool_call_name = tool_call_function["name"].as_str().unwrap();

        // Call the tool and get the next completion from the result
        let tool_call_result = tools
            .iter()
            .find(|i| *i.function_name() == *tool_call_name)
            .unwrap_or_else(|| panic!("Received tool call that doesn't exist: {}", tool_call_name))
            .call(tool_call_args)
            .await
            .expect("Tool call returned an error");

        let tool_call_requests = vec![FunctionCall {
            function: FunctionCallFn {
                arguments: tool_call_args.to_string(),
                name: tool_call_name.to_string(),
            },
            id: tool_call_id.to_string(),
            r#type: String::from("function"),
        }];
        history.push(Message::new_tool_call_request(tool_call_requests.clone()));
        history.push(Message::new_tool_call_response(
            &tool_call_result,
            tool_call_id,
        ));
        accum_new.push(Message::new_tool_call_request(tool_call_requests));
        accum_new.push(Message::new_tool_call_response(
            &tool_call_result,
            tool_call_id,
        ));
    }
}

/// Appends one or more messages to `history` and new messages to
/// `accum_new` so there's no need to diff after calling this to
/// figure out what's new.
pub async fn chat(
    tools: &Option<Vec<BoxedToolCall>>,
    history: &mut Vec<Message>,
    accum_new: &mut Vec<Message>,
    api_hostname: &str,
    api_key: &str,
    model: &str,
) {
    let mut resp = completion(history, tools, api_hostname, api_key, model)
        .await
        .expect("OpenAI API call failed");

    // Tool calls need to be handled for the chat to proceed
    while let Some(tool_calls) = resp["choices"][0]["message"]["tool_calls"].as_array() {
        if tool_calls.is_empty() {
            break;
        }
        let tools_ref = tools
            .as_ref()
            .expect("Received tool call but no tools were specified");
        handle_tool_calls(tools_ref, history, accum_new, tool_calls).await;

        // Provide the results of the tool calls back to the chat
        resp = completion(history, tools, api_hostname, api_key, model)
            .await
            .expect("OpenAI API call failed");
    }

    if let Some(msg) = resp["choices"][0]["message"]["content"].as_str() {
        history.push(Message::new(Role::Assistant, msg));
        accum_new.push(Message::new(Role::Assistant, msg));
    } else {
        panic!("No message received. Resp:\n\n {}", resp);
    }
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
