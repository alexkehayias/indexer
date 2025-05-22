use anyhow::{Error, Result};
use tokio_rusqlite::Connection;
use serde::{Deserialize, Serialize};
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

pub async fn send_push_notification(
    vapid_private_pem_path: String,
    endpoint: String,
    p256dh: String,
    auth: String,
    payload: String,
) -> Result<(), Error> {

    // Create subscription info
    let subscription_info = SubscriptionInfo::new(endpoint, p256dh, auth);

    // Read the VAPID signing material from the PEM file
    let file = std::fs::File::open(vapid_private_pem_path)?;
    let sig_builder = VapidSignatureBuilder::from_pem(file, &subscription_info)?.build()?;

    // Create the message with payload
    let mut builder = WebPushMessageBuilder::new(&subscription_info);
    builder.set_payload(ContentEncoding::Aes128Gcm, payload.as_bytes());
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
    message: String,
) {
    let mut tasks = tokio::task::JoinSet::new();
    for sub in subscriptions {
        let vapid = vapid_key_path.clone();
        let msg = message.clone();
        tasks.spawn(send_push_notification(
            vapid,
            sub.endpoint,
            sub.p256dh,
            sub.auth,
            msg,
        ));
    }
    while let Some(_res) = tasks.join_next().await {}
}

pub async fn find_all_notification_subscriptions(db: &Connection) -> Result<Vec<PushSubscription>, Error> {
    let subscriptions = db.call(|conn| {
        let mut stmt = conn.prepare("SELECT endpoint, p256dh, auth FROM push_subscription")?;
        let rows = stmt.query_map([], |i| {
            Ok(PushSubscription {
                endpoint: i.get(0)?,
                p256dh: i.get(1)?,
                auth: i.get(2)?
            })})?
            .filter_map(Result::ok)
            .collect::<Vec<PushSubscription>>();
        Ok(rows)
    });
    Ok(subscriptions.await?)
}
