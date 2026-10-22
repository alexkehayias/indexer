use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct PushSubscription {
    pub endpoint: String,
    pub p256dh: String,
    pub auth: String,
}

#[derive(Serialize, Clone)]
/// If you need to add more application specific notification data, it
/// should go in here and then the service-worker.js can access the
/// data in the notification event.
struct PushNotificationData {
    // The URL to open when the notification is clicked
    url: String,
}

#[derive(Serialize, Clone)]
pub struct PushNotificationAction {
    action: String,
    title: String,
    icon: String,
}

#[derive(Serialize, Clone)]
pub struct PushNotificationPayload {
    pub title: String,
    pub body: String,
    pub actions: Vec<PushNotificationAction>,
    #[serde(skip_serializing_if = "Option::is_none")]
    // When a tag is set, sending new notifications with the same tag
    // will update the user's notification if they have not interacted
    // with it yet.
    pub tag: Option<String>,
    data: PushNotificationData,
}

impl PushNotificationPayload {
    pub fn new(
        title: &str,
        body: &str,
        url: Option<&str>,
        actions: Option<Vec<PushNotificationAction>>,
        tag: Option<&str>,
    ) -> Self {
        Self {
            title: title.to_string(),
            body: body.to_string(),
            actions: actions.unwrap_or_default(),
            tag: tag.map(|s| s.to_string()),
            data: PushNotificationData {
                url: url.map(|u| u.to_string()).unwrap_or("/".to_string()),
            },
        }
    }
}
