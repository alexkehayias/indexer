use crate::openai::{
    BoxedToolCall, FunctionCall, FunctionCallFn, Message, Role, ToolCall, completion,
};
use serde_json::Value;

async fn handle_tool_calls(
    history: &mut Vec<Message>,
    tools: &Vec<Box<dyn ToolCall + Send + Sync + 'static>>,
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
        history.push(Message::new_tool_call_request(tool_call_requests));
        history.push(Message::new_tool_call_response(
            &tool_call_result,
            tool_call_id,
        ));
    }
}

pub async fn chat(history: &mut Vec<Message>, tools: &Option<Vec<BoxedToolCall>>) {
    let mut resp = completion(history, tools)
        .await
        .expect("OpenAI API call failed");

    // Tool calls need to be handled for the chat to proceed
    while let Some(tool_calls) = resp["choices"][0]["message"]["tool_calls"].as_array() {
        let tools_ref = tools
            .as_ref()
            .expect("Received tool call but no tools were specified");
        handle_tool_calls(history, tools_ref, tool_calls).await;

        // Provide the results of the tool calls back to the chat
        resp = completion(history, tools)
            .await
            .expect("OpenAI API call failed");
    }

    if let Some(msg) = resp["choices"][0]["message"]["content"].as_str() {
        history.push(Message::new(Role::Assistant, msg));
    } else {
        panic!("No message received. Resp:\n\n {}", resp);
    }
}
