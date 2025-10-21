use async_trait::async_trait;
use std::time::Duration;
use tokio_rusqlite::Connection;

use super::PeriodicJob;
use crate::{config::AppConfig, notification::{broadcast_push_notification, find_all_notification_subscriptions}, oauth::find_all_gmail_auth_emails};

pub struct ProcessEmail;

impl Default for ProcessEmail {
    fn default() -> Self {
        Self {}
    }
}

#[async_trait]
impl PeriodicJob for ProcessEmail {

    fn interval(&self) -> Duration {
        Duration::from_secs(60*60*2)
    }

    async fn run_job(&self, config: &AppConfig, db: &Connection) {
        let AppConfig {note_search_api_url, vapid_key_path, ..} = config;
        let emails = {
            find_all_gmail_auth_emails(db).await.expect("Query failed")
        };

        // let session_id = Uuid::new_v4().to_string();
        let history = crate::agents::email::email_chat_response(note_search_api_url, emails).await;
        let last_msg = history.last().unwrap();
        let summary = last_msg.content.clone().unwrap();

        // Store in AppState
        // {
        //     let mut state = state_clone.write().unwrap();
        //     state.chat_sessions.insert(
        //         session_id.clone(),
        //         ChatSession {
        //             session_id: session_id.clone(),
        //             transcript: history.clone(),
        //         },
        //     );
        // }

        // Broadcast push notification to all subscribers, using a new read lock for DB/config each time
        let subscriptions = find_all_notification_subscriptions(db).await.unwrap();
        broadcast_push_notification(
            subscriptions,
            vapid_key_path.to_string(),
            format!("Emails processed! {}", summary).to_string(),
        ).await;
    }
}
