//! Gmail API client for listing unread mail, fetching threads, sending replies

use base64::{Engine as _, engine::general_purpose::URL_SAFE};
use chrono::{Duration, Utc};
use reqwest::Client;
use serde::{Deserialize, Serialize};

/// Message and thread structures from Gmail API documentation
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct MessageResponse {
    pub id: String,
    #[serde(rename = "threadId")]
    pub thread_id: String,
}

#[derive(Debug, Deserialize)]
pub struct ListMessagesResponse {
    pub messages: Option<Vec<MessageResponse>>,
    #[serde(rename = "nextPageToken")]
    pub next_page_token: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Thread {
    pub id: String,
    pub messages: Vec<Message>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub id: String,
    #[serde(rename = "threadId")]
    pub thread_id: String,
    pub snippet: Option<String>,
    pub payload: Option<MessagePayload>,
    #[serde(rename = "labelIds")]
    pub label_ids: Option<Vec<String>>,
    #[serde(rename = "internalDate")]
    pub internal_date: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessagePartBody {
    #[serde(rename = "attachmentId")]
    attachment_id: Option<String>,
    size: u64,
    // Base64 encoded
    data: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessagePart {
    #[serde(rename = "partId")]
    pub part_id: String,
    #[serde(rename = "mimeType")]
    pub mimetype: String,
    pub body: Option<MessagePartBody>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessagePayload {
    pub headers: Option<Vec<MessageHeader>>,
    pub body: Option<MessagePartBody>,
    pub parts: Option<Vec<MessagePart>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageHeader {
    pub name: String,
    pub value: String,
}

fn decode_base64(data: &str) -> String {
    URL_SAFE
        .decode(data)
        .ok()
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .unwrap_or_else(|| {
            tracing::error!("Base64 decode failed for: {}", data);
            String::from("Failed to decode")
        })
}

/// Extract the body from the Gmail API message payload.
///
/// To get the body of an email:
/// - The email messsage can either have a `payload.body.data` or one or more `parts[].body.data`.
/// - Parts might have an HTML version of the message as well as a plain text version of the body
///   Use the `parts[].mimetype` field to distinguish which it is
/// - When there is a `body.attachment_id` that indicates a file that was attached
pub fn extract_body(message: &Message) -> String {
    let payload = message.payload.clone().unwrap();

    if let Some(body) = &payload.body {
        if let Some(data) = &body.data {
            return decode_base64(data);
        }
    }

    if let Some(parts) = &payload.parts {
        for part in parts {
            if part.mimetype == "text/html" {
                if let Some(body) = &part.body {
                    // Skip attachments
                    if body.attachment_id.is_some() {
                        continue;
                    }
                    // Return the first non-empty body found in parts
                    if let Some(data) = &body.data {
                        return decode_base64(data);
                    }
                }
            }
            if part.mimetype == "text/plain" {
                if let Some(body) = &part.body {
                    // Skip attachments
                    if body.attachment_id.is_some() {
                        continue;
                    }
                    // Return the first non-empty body found in parts
                    if let Some(data) = &body.data {
                        return decode_base64(data);
                    }
                }
            }
        }
    }

    String::new()
}

/// List unread messages from the last N days
/// curl: see spec
pub async fn list_unread_messages(
    access_token: &str,
    n_days: i64,
) -> Result<Vec<MessageResponse>, anyhow::Error> {
    let client = Client::new();
    let after_date = (Utc::now() - Duration::days(n_days))
        .format("%Y/%m/%d")
        .to_string();
    let url = format!(
        "https://gmail.googleapis.com/gmail/v1/users/me/messages?labelIds=UNREAD&q=is:unread%20after:{}%20in:inbox",
        after_date
    );
    let res = client.get(&url).bearer_auth(access_token).send().await?;
    let status = res.status();
    let text = res.text().await.unwrap_or_default();
    if !status.is_success() {
        anyhow::bail!("Unread fetch failed: {} ({})", status, text);
    }
    let msgs: ListMessagesResponse = serde_json::from_str(&text)?;
    Ok(msgs.messages.unwrap_or_default())
}

/// Fetch full thread for a given threadId
/// curl: see spec
pub async fn fetch_thread(
    access_token: String,
    thread_id: String,
) -> Result<Thread, anyhow::Error> {
    let client = Client::new();
    let url = format!(
        "https://gmail.googleapis.com/gmail/v1/users/me/threads/{}?format=full",
        thread_id
    );
    let res = client.get(&url).bearer_auth(access_token).send().await?;
    let status = res.status();
    let text = res.text().await.unwrap_or_default();
    if !status.is_success() {
        anyhow::bail!("Thread fetch failed: {} ({})", status, text);
    }
    let thread: Thread = serde_json::from_str(&text)?;
    Ok(thread)
}

/// Send a reply to a message/thread
/// curl: see spec (see MIME construction below)
pub async fn send_reply(
    access_token: &str,
    thread_id: &str,
    to: &str,
    subject: &str,
    reply_to_message_id: &str,
    reply_body: &str,
) -> Result<(), anyhow::Error> {
    // Note: "me" as "From" will be replaced by Gmail
    let mime = format!(
        "From: me\nTo: {to}\nSubject: Re: {subject}\nIn-Reply-To: <{msgid}>\nReferences: <{msgid}>\n\n{body}",
        to = to,
        subject = subject,
        msgid = reply_to_message_id,
        body = reply_body
    );
    let raw_encoded = base64_url_no_pad(&mime);
    let client = Client::new();
    let url = "https://gmail.googleapis.com/gmail/v1/users/me/messages/send";
    let payload = serde_json::json!({
        "raw": raw_encoded,
        "threadId": thread_id,
    });
    let res = client
        .post(url)
        .bearer_auth(access_token)
        .json(&payload)
        .send()
        .await?;
    let status = res.status();
    let text = res.text().await.unwrap_or_default();
    if !status.is_success() {
        anyhow::bail!("Send failed: {} ({})", status, text);
    }
    Ok(())
}

/// Helper: base64url encode w/out padding
fn base64_url_no_pad(input: &str) -> String {
    URL_SAFE.encode(input.as_bytes())
}
