use serde_json::json;
use crate::openai::{Message, Role, ToolCall, FunctionCall, FunctionCallFn, completion};

pub async fn chat(history: &mut Vec<Message>, tools: &Option<Vec<Box<dyn ToolCall>>>) {
    let resp = completion(history, tools)
        .await
        .expect("OpenAI API call failed");

    // Tool calls need to be handled for the chat to proceed
    let tool_calls = resp["choices"][0]["message"]["tool_calls"].as_array();
    if let Some(tool_calls) = tool_calls {
        if let Some(ref tools_ref) = tools {
            // Handle each tool call
            for tool_call in tool_calls {
                let tool_call_id = &tool_call["id"].as_str().unwrap();
                let tool_call_function = &tool_call["function"];
                let tool_call_args =
                    tool_call_function["arguments"].as_str().unwrap();
                let tool_call_name = tool_call_function["name"].as_str().unwrap();

                // Call the tool and get the next completion from the result
                let tool_call_result = tools_ref
                    .iter()
                    .find(|i| *i.function_name() == *tool_call_name)
                    .unwrap_or_else(|| {
                        panic!(
                            "Received tool call that doesn't exist: {}",
                            tool_call_name
                        )
                    })
                    .call(tool_call_args);
                let tool_call_requests = vec![FunctionCall {
                    function: FunctionCallFn {
                        arguments: tool_call_args.to_string(),
                        name: tool_call_name.to_string(),
                    },
                    id: tool_call_id.to_string(),
                    r#type: String::from("function"),
                }];
                history.push(Message::new_tool_call_request(
                    tool_call_requests,
                ));
                history.push(Message::new_tool_call_response(
                    &tool_call_result,
                    tool_call_id,
                ));
            }
            let resp = completion(history, tools)
                .await
                .expect("OpenAI API call failed");
            // TODO: Is it possible the response is another tool call?
            let msg = resp["choices"][0]["message"]["content"]
                .as_str()
                .unwrap_or_else(|| {
                    panic!("RESP: {:?}\n\nHISTORY: {:?}", resp, json!(history))
                });
            println!("{}", msg);
            history.push(Message::new(Role::Assistant, msg));
        } else {
            panic!("Received tool call but no tools were specified");
        }
    } else {
        let msg = resp["choices"][0]["message"]["content"].as_str().unwrap();
        println!("{}", msg);
        history.push(Message::new(Role::Assistant, msg));
    }
}
