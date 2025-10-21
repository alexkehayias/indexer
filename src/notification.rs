use anyhow::Error;
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
