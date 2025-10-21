//! OAuth 2.0 token exchange & refresh for Gmail API

use anyhow::{Error, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tokio_rusqlite::Connection;

/// Response from Google's token endpoint
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct TokenResponse {
    pub access_token: String,
    pub expires_in: u64,
    pub refresh_token: Option<String>,
    pub scope: String,
    pub token_type: String,
}

const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";

/// Exchange authorization code for access and refresh tokens (see curl above)
pub async fn exchange_code_for_token(
    client_id: &str,
    client_secret: &str,
    code: &str,
    redirect_uri: &str,
) -> Result<TokenResponse, anyhow::Error> {
    let client = Client::new();

    let params = [
        ("code", code),
        ("client_id", client_id),
        ("client_secret", client_secret),
        ("redirect_uri", redirect_uri),
        ("grant_type", "authorization_code"),
    ];
    let res = client
        .post(TOKEN_URL)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .form(&params)
        .send()
        .await?;
    let status = res.status();
    let text = res.text().await.unwrap_or_default();
    if !status.is_success() {
        anyhow::bail!("Token exchange failed: {} ({})", status, text);
    }
    let token: TokenResponse = serde_json::from_str(&text)?;
    Ok(token)
}

/// Refresh access token using refresh token (see curl above)
pub async fn refresh_access_token(
    client_id: &str,
    client_secret: &str,
    refresh_token: &str,
) -> Result<TokenResponse, anyhow::Error> {
    let client = Client::new();

    let params = [
        ("client_id", client_id),
        ("client_secret", client_secret),
        ("refresh_token", refresh_token),
        ("grant_type", "refresh_token"),
    ];
    let res = client
        .post(TOKEN_URL)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .form(&params)
        .send()
        .await?;
    let status = res.status();
    let text = res.text().await.unwrap_or_default();
    if !status.is_success() {
        anyhow::bail!("Token refresh failed: {} ({})", status, text);
    }

    let mut token: TokenResponse = serde_json::from_str(&text)?;
    // The refresh endpoint does not always return refresh_token, so preserve the old one.
    token.refresh_token = Some(refresh_token.to_string());
    Ok(token)
}

pub async fn find_all_gmail_auth_emails(db: &Connection) -> Result<Vec<String>, Error> {
    let auths = db.call(|conn| {
        let result: Vec<String> = conn
            .prepare("SELECT id FROM auth WHERE service = 'gmail'")?
            .query_map([], |row| row.get(0))?
            .filter_map(Result::ok)
            .collect();
        Ok(result)
    });
    Ok(auths.await?)
}
