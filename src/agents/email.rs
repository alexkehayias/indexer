use crate::chat;
use crate::openai::{Message, Role, ToolCall};
use crate::tools::EmailUnreadTool;

/// Email reader and responder agent.
pub async fn email_chat_response(
    api_base_url: &str,
    emails: Vec<String>,
    openai_api_hostname: &str,
    openai_api_key: &str,
    openai_model: &str,
) -> Vec<Message> {
    let email_unread_tool = EmailUnreadTool::new(api_base_url);
    let tools: Option<Vec<Box<dyn ToolCall + Send + Sync + 'static>>> =
        Some(vec![Box::new(email_unread_tool)]);

    let system_msg = format!(
        "You are an email assistant AI. Summarize, search, and analyze emails on behalf of the user for the following users: {}",
        emails.join(", ")
    );
    let user_msg = "Summarize my unread emails.";

    let mut history = vec![
        Message::new(Role::System, &system_msg),
        Message::new(Role::User, user_msg),
    ];
    let mut accum_new: Vec<Message> = Vec::new();
    chat::chat(
        &tools,
        &mut history,
        &mut accum_new,
        openai_api_hostname,
        openai_api_key,
        openai_model,
    )
    .await;

    history
}
