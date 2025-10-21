use async_trait::async_trait;
use std::time::Duration;
use tokio_rusqlite::Connection;
use uuid::Uuid;

use super::PeriodicJob;
use crate::{
    chat::insert_chat_message,
    config::AppConfig,
    notification::{
        PushNotificationPayload, broadcast_push_notification, find_all_notification_subscriptions,
    },
    oauth::find_all_gmail_auth_emails,
};

#[derive(Default, Debug)]
pub struct ProcessEmail;

#[async_trait]
impl PeriodicJob for ProcessEmail {
    fn interval(&self) -> Duration {
        Duration::from_secs(60 * 60 * 2)
    }

    async fn run_job(&self, config: &AppConfig, db: &Connection) {
        let AppConfig {
            note_search_api_url,
            vapid_key_path,
            openai_api_hostname,
            openai_api_key,
            openai_model,
            ..
        } = config;
        let emails = { find_all_gmail_auth_emails(db).await.expect("Query failed") };

        let session_id = Uuid::new_v4().to_string();
        let history = crate::agents::email::email_chat_response(
            note_search_api_url,
            emails,
            openai_api_hostname,
            openai_api_key,
            openai_model,
        )
        .await;
        let last_msg = history.last().unwrap();
        let summary = last_msg.content.clone().unwrap();

        // Store the chat messages so the session can be picked up later
        {
            for m in history {
                insert_chat_message(db, &session_id, &m).await.unwrap();
            }
        }

        // Broadcast push notification to all subscribers, using a new read lock for DB/config each time
        let payload = PushNotificationPayload::new(
            "Background job update",
            &format!("Emails processed! {}", summary),
            None,
            None,
        );
        let subscriptions = find_all_notification_subscriptions(db).await.unwrap();
        broadcast_push_notification(subscriptions, vapid_key_path.to_string(), payload).await;
    }
}
