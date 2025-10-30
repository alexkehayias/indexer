use async_trait::async_trait;
use std::time::Duration;
use tokio_rusqlite::Connection;
use uuid::Uuid;

use super::PeriodicJob;
use crate::{
    chat::{insert_chat_message, chat},
    config::AppConfig,
    notification::{
        broadcast_push_notification, find_all_notification_subscriptions, PushNotificationPayload
    },
    openai::{Role, Message, BoxedToolCall},
    tools::{CalendarTool, SearxSearchTool, WebsiteViewTool},
};

#[derive(Default, Debug)]
pub struct ResearchMeetingAttendees;

#[async_trait]
impl PeriodicJob for ResearchMeetingAttendees {
    fn interval(&self) -> Duration {
        Duration::from_secs(60 * 60) // Run every hour
    }

    async fn run_job(&self, config: &AppConfig, db: &Connection) {
        let AppConfig {
            note_search_api_url,
            vapid_key_path,
            openai_api_hostname,
            openai_api_key,
            openai_model,
            searxng_api_url,
            calendar_email,
            ..
        } = config;

        // Create tools for the chat
        let tools: Vec<BoxedToolCall> = vec![
            Box::new(CalendarTool::new(note_search_api_url)),
            Box::new(SearxSearchTool::new(searxng_api_url)),
            Box::new(WebsiteViewTool::new()),
        ];

        // Create a session ID for this job
        let session_id = Uuid::new_v4().to_string();

        // Early return if there is no calendar email specified.
        if calendar_email.is_none() {
            tracing::warn!(
                "Background job research_meeting_attendees failed: No calendar email address specified in AppConfig.",
            );
            return;
        }

        // Create a prompt for the chat to research meeting attendees
        let prompt = format!("Check my calendar for upcoming meetings and prepare a briefing for each meeting using the example below.

My email address to use for the calendar is {}

When researching each meeting's attendees you must carefully search for the person's name AND the website from their email address. For example, if the attendee's name is Matt Rumple and their email address is matt@example.com, you should search for \"Matt Rumple example.com\" or just the email \"matt@example.com\" to disambiguate who the attendee is. If you can't find relevant information about the attendee, just say what you tried and that there were no relevant results.

Ignore any attendees with an email address domain name that matches my domain name.

Use the example briefing for each meeting. Follow the example closely and do not add any other information outside of it. Be concise and to the point.

# Example briefing
*2025-09-10 08:00AM PT* Consultation
*Company*: Acme, Inc
*Website*: http://example.com
*Attendees*: Kristen Foo (VP of Finance), Frank Bar (VP of People)

*About Acme*
*Employees*: 100-200
*Industry*: Heathcare
*Summary*: Acme is a healthcare company that focuses on mental health and wellness. They offer a marketplace of therapists to match patients with.

*About Kristen Foo*
Kristen is the VP of Finance. She joined Acme 2 years ago. She previously worked at FooBar as the Accounting Controller. She posts about AI and accounting.

[LinkedIn profile](https://linkedin.com/in/kristinfoo)

*About Frank Bar*
Frank is the VP of People at Acme. He was previously HR Manager at Acme and before that he worked at Facebook as an HRBP. He posts about employee engagement and company culture.

[LinkedIn profile](https://linkedin.com/in/frank-bar)", calendar_email.clone().unwrap());

        // Create initial message for chat
        let history = vec![Message::new(
            Role::User,
            &prompt,
        )];

        // Create a new chat session with the tools
        let messages = chat(
            &Some(tools),
            &history,
            openai_api_hostname,
            openai_api_key,
            openai_model,
        )
            .await
            .expect("Chat session failed");

        // Store the chat messages so the session can be picked up later
        {
            for m in &messages {
                insert_chat_message(db, &session_id, m).await.unwrap();
            }
        }

        // Get the final response from the chat
        let summary = if let Some(last_msg) = history.last() {
            last_msg
                .content
                .clone()
                .unwrap_or_else(|| "No summary available".to_string())
        } else {
            "No response from chat".to_string()
        };

        let payload = PushNotificationPayload::new(
            "Background job update",
            &format!("Meeting attendee research complete: {}", summary).to_string(),
            Some(&format!("/chat/?session_id={session_id}")),
            None,
            None,
        );

        // Broadcast push notification to all subscribers
        let subscriptions = find_all_notification_subscriptions(db).await.unwrap();
        broadcast_push_notification(subscriptions, vapid_key_path.to_string(), payload).await;
    }
}
