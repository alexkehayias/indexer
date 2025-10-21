//! Google Calendar API client for listing meetings and attendees

use anyhow::Result;
use chrono::{DateTime, Utc};
use reqwest::Client;
use serde::{Deserialize, Serialize};

/// Represents a Google Calendar event (meeting)
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Event {
    pub id: String,
    pub summary: Option<String>,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub attendees: Option<Vec<Attendee>>,
}

/// Represents an attendee of a Google Calendar event
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Attendee {
    pub email: String,
    pub display_name: Option<String>,
}

/// Response structure for listing events
#[derive(Debug, Deserialize)]
pub struct ListEventsResponse {
    pub items: Option<Vec<CalendarEvent>>,
}

/// Calendar event as returned by Google API (intermediate structure)
#[derive(Debug, Deserialize)]
pub struct CalendarEvent {
    pub id: String,
    pub summary: Option<String>,
    pub start: EventDateTime,
    pub end: EventDateTime,
    pub attendees: Option<Vec<EventAttendee>>,
}

/// DateTime structure from Google Calendar API
#[derive(Debug, Deserialize)]
pub struct EventDateTime {
    #[serde(rename = "dateTime")]
    pub date_time: Option<String>,
}

/// Attendee structure from Google Calendar API
#[derive(Debug, Deserialize)]
pub struct EventAttendee {
    pub email: String,
    #[serde(rename = "displayName")]
    pub display_name: Option<String>,
}

impl From<CalendarEvent> for Event {
    fn from(calendar_event: CalendarEvent) -> Self {
        Event {
            id: calendar_event.id,
            summary: calendar_event.summary,
            start: DateTime::parse_from_rfc3339(
                &calendar_event.start.date_time.unwrap_or_default(),
            )
            .unwrap()
            .with_timezone(&Utc),
            end: DateTime::parse_from_rfc3339(&calendar_event.end.date_time.unwrap_or_default())
                .unwrap()
                .with_timezone(&Utc),
            attendees: calendar_event
                .attendees
                .map(|atts| atts.into_iter().map(|a| a.into()).collect()),
        }
    }
}

impl From<EventAttendee> for Attendee {
    fn from(event_attendee: EventAttendee) -> Self {
        Attendee {
            email: event_attendee.email,
            display_name: event_attendee.display_name,
        }
    }
}

/// List events (meetings) within a given date range
pub async fn list_events(
    access_token: &str,
    calendar_id: &str,
    start_time: DateTime<Utc>,
    end_time: DateTime<Utc>,
) -> Result<Vec<Event>> {
    let client = Client::new();
    let url = format!(
        "https://www.googleapis.com/calendar/v3/calendars/{}/events",
        calendar_id
    );

    let res = client
        .get(&url)
        .bearer_auth(access_token)
        .query(&[
            ("timeMin", start_time.to_rfc3339()),
            ("timeMax", end_time.to_rfc3339()),
            ("singleEvents", "true".to_string()),
            ("orderBy", "startTime".to_string()),
        ])
        .send()
        .await?;

    let status = res.status();
    let text = res.text().await.unwrap_or_default();

    if !status.is_success() {
        anyhow::bail!("Events fetch failed: {} ({})", status, text);
    }

    let response: ListEventsResponse = serde_json::from_str(&text)?;
    let events = response
        .items
        .unwrap_or_default()
        .into_iter()
        .map(|e| e.into())
        .collect();

    Ok(events)
}
