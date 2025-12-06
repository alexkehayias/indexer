use async_trait::async_trait;
use std::time::Duration;
use tokio_rusqlite::Connection;

use crate::chat::find_chat_session_by_id;
use crate::config::AppConfig;
use crate::openai::{Message, Role, completion};

#[derive(Debug)]
pub struct GenerateSessionTitles;

#[async_trait]
impl crate::jobs::PeriodicJob for GenerateSessionTitles {
    fn interval(&self) -> Duration {
        // Run every 10 minutes
        Duration::from_secs(60 * 60 * 2)
    }

    async fn run_job(&self, config: &AppConfig, db_conn: &Connection) {
        tracing::info!("Starting session title/summary generation job");

        // Find sessions that don't have a title or summary
        let sessions_to_update = db_conn
            .call(move |conn| {
                let mut stmt = conn.prepare(
                    "SELECT DISTINCT s.id FROM session s
                 LEFT JOIN chat_message cm ON s.id = cm.session_id
                 WHERE (s.title IS NULL OR s.title = '')
                 AND (s.summary IS NULL OR s.summary = '')
                 AND cm.session_id IS NOT NULL",
                )?;

                let rows = stmt
                    .query_map([], |row| {
                        let session_id: String = row.get(0)?;
                        Ok(session_id)
                    })?
                    .filter_map(Result::ok)
                    .collect::<Vec<String>>();

                Ok(rows)
            })
            .await;

        if let Ok(sessions) = sessions_to_update {
            for session_id in sessions {
                // Get the chat transcript for this session
                match find_chat_session_by_id(db_conn, &session_id).await {
                    Ok(transcript) => {
                        if !transcript.is_empty() {
                            // Generate title and summary from the transcript
                            if let Err(e) = generate_and_update_session_info(
                                config,
                                db_conn,
                                &session_id,
                                &transcript,
                            )
                            .await
                            {
                                tracing::error!(
                                    "Failed to generate title/summary for session {}: {}",
                                    session_id,
                                    e
                                );
                            }
                        }
                    }
                    Err(e) => {
                        tracing::error!(
                            "Failed to fetch transcript for session {}: {}",
                            session_id,
                            e
                        );
                    }
                }
            }
        }

        tracing::info!("Completed session title/summary generation job");
    }
}

async fn generate_and_update_session_info(
    config: &AppConfig,
    db_conn: &Connection,
    session_id: &str,
    transcript: &[Message],
) -> Result<(), anyhow::Error> {
    // Create a prompt for the LLM to generate title and summary
    let prompt = create_session_prompt(transcript);

    // Prepare the messages for the LLM
    let messages = vec![
        Message::new(
            Role::System,
            "You are an assistant that generates concise titles and summaries for chat sessions based on the conversation content.",
        ),
        Message::new(Role::User, &prompt),
    ];

    // Call the LLM to generate title and summary
    let response = completion(
        &messages,
        &None, // No tools needed for this task
        &config.openai_api_hostname,
        &config.openai_api_key,
        &config.openai_model,
    )
    .await?;

    // Extract the generated title and summary from the response
    if let Some(content) = response["choices"][0]["message"]["content"].as_str() {
        // Try to parse the JSON response
        match serde_json::from_str::<serde_json::Value>(content) {
            Ok(json_response) => {
                if let (Some(title), Some(summary)) = (
                    json_response["title"].as_str(),
                    json_response["summary"].as_str(),
                ) {
                    let session_id_owned = session_id.to_string();
                    let title_owned = title.to_string();
                    let summary_owned = summary.to_string();

                    // Update the session in the database
                    db_conn
                        .call(move |conn| {
                            let mut stmt = conn.prepare(
                                "UPDATE session SET title = ?, summary = ? WHERE id = ?",
                            )?;
                            stmt.execute([title_owned, summary_owned, session_id_owned])?;
                            Ok(())
                        })
                        .await?;
                } else {
                    tracing::warn!("LLM response missing title or summary fields: {}", content);
                }
            }
            // Don't do anything but log it if it didn't work
            Err(e) => {
                tracing::error!(
                    "Failed to parse LLM response as JSON for session {}: {} - Response: {}",
                    session_id,
                    e,
                    content
                );
            }
        }
    } else {
        tracing::warn!("No content in LLM response for session {}", session_id);
    }

    Ok(())
}

fn create_session_prompt(transcript: &[Message]) -> String {
    // Convert transcript to a readable format for the LLM
    let mut conversation = String::new();

    // We'll just use a simple format without role distinction for now
    for message in transcript {
        if let Some(content) = &message.content {
            conversation.push_str(&format!("{}\n", content));
        }
    }

    // Create the prompt for the LLM
    format!(
        "Based on this chat conversation, generate a concise title and summary for the session. Return ONLY a JSON object with 'title' and 'summary' fields. The title should be 5-10 words, and the summary should be a short paragraph (2-3 sentences). Do not include any other text, just the JSON object.\n\nConversation:\n{}",
        conversation
    )
}
