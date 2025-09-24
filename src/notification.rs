use anyhow::{Error, Result};
use serde::{Deserialize, Serialize};
use tokio_rusqlite::Connection;
use web_push::{
    ContentEncoding, HyperWebPushClient, SubscriptionInfo, VapidSignatureBuilder, WebPushClient,
    WebPushMessageBuilder,
};

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
    data: PushNotificationData,
}

impl PushNotificationPayload {
    pub fn new(
        title: &str,
        body: &str,
        url: Option<&str>,
        actions: Option<Vec<PushNotificationAction>>,
    ) -> Self {
        Self {
            title: title.to_string(),
            body: body.to_string(),
            actions: actions.map_or(Vec::new(), |u| u),
            data: PushNotificationData {
                url: url.map(|u| u.to_string()).unwrap_or("/".to_string()),
            },
        }
    }
}

pub async fn send_push_notification(
    vapid_private_pem_path: String,
    endpoint: String,
    p256dh: String,
    auth: String,
    payload: PushNotificationPayload,
) -> Result<(), Error> {
    // Create subscription info
    let subscription_info = SubscriptionInfo::new(endpoint, p256dh, auth);

    // Read the VAPID signing material from the PEM file
    let file = std::fs::File::open(vapid_private_pem_path)?;
    let sig_builder = VapidSignatureBuilder::from_pem(file, &subscription_info)?.build()?;

    // Create the message with payload
    let mut builder = WebPushMessageBuilder::new(&subscription_info);
    let content = serde_json::to_string(&payload)?;
    builder.set_payload(ContentEncoding::Aes128Gcm, content.as_bytes());
    builder.set_vapid_signature(sig_builder);
    let message = builder.build()?;

    // Send the notification
    let client = HyperWebPushClient::new();
    let result = client.send(message).await;

    if let Err(error) = result {
        println!("An error occured: {:?}", error);
    }

    Ok(())
}

pub async fn broadcast_push_notification(
    subscriptions: Vec<PushSubscription>,
    vapid_key_path: String,
    payload: PushNotificationPayload,
) {
    let mut tasks = tokio::task::JoinSet::new();
    for sub in subscriptions {
        let vapid = vapid_key_path.clone();
        tasks.spawn(send_push_notification(
            vapid,
            sub.endpoint,
            sub.p256dh,
            sub.auth,
            payload.clone(),
        ));
    }
    while let Some(_res) = tasks.join_next().await {}
}

pub async fn find_all_notification_subscriptions(
    db: &Connection,
) -> Result<Vec<PushSubscription>, Error> {
    let subscriptions = db.call(|conn| {
        let mut stmt = conn.prepare("SELECT endpoint, p256dh, auth FROM push_subscription")?;
        let rows = stmt
            .query_map([], |i| {
                Ok(PushSubscription {
                    endpoint: i.get(0)?,
                    p256dh: i.get(1)?,
                    auth: i.get(2)?,
                })
            })?
            .filter_map(Result::ok)
            .collect::<Vec<PushSubscription>>();
        Ok(rows)
    });
    Ok(subscriptions.await?)
}
