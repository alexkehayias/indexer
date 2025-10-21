use crate::chat;
use crate::openai::{Message, Role};

/// Email reader and responder agent.
pub async fn email_chat_response(user_message: &str, tools: Option<Vec<crate::openai::BoxedToolCall>>) -> String {
    let system_message = "You are an email assistant AI. Summarize, search, and analyze emails on behalf of the user.";

    let mut history = vec![
        Message::new(Role::System, system_message),
        Message::new(Role::User, user_message),
    ];

    chat::chat(&mut history, &tools).await;
    // Return the most recent assistant message
    history
        .last()
        .and_then(|msg| msg.content.clone())
        .unwrap_or_else(|| "No response generated.".to_string())
}
