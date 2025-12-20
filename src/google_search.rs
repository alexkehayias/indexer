use anyhow::{Error, Result};
use reqwest;
use serde::Deserialize;


#[derive(Deserialize)]
struct GoogleSearchResponse {
    items: Vec<SearchItem>,
}

#[derive(Deserialize)]
pub struct SearchItem {
    pub title: String,
    pub link: String,
    pub snippet: String,
}

pub async fn search_google(
    query: &str,
    api_key: &str,
    cx_id: &str,
    num_results: Option<u8>,
) -> Result<Vec<SearchItem>, Error> {
    let desired = num_results.unwrap_or(10) as usize;
    let mut collected: Vec<SearchItem> = Vec::new();
    let mut start_index: u32 = 1; // Google Custom Search uses 1‑based start index
    let client = reqwest::Client::new();

    while collected.len() < desired {
        // Number of items to request this page (max 10, but not exceeding remaining needed)
        let per_page = std::cmp::min(10, (desired - collected.len()) as u32);
        let mut url = reqwest::Url::parse("https://www.googleapis.com/customsearch/v1")
            .expect("Invalid base URL");
        url.query_pairs_mut()
            .append_pair("key", api_key)
            .append_pair("cx", cx_id)
            .append_pair("q", query)
            .append_pair("num", &per_page.to_string())
            .append_pair("start", &start_index.to_string());

        let resp = client.get(url).send().await?.error_for_status()?;
        let body: GoogleSearchResponse = resp.json().await?;
        if body.items.is_empty() {
            break; // No more results available
        }
        collected.extend(body.items);
        start_index += per_page; // Move to next page start index
    }

    // Trim to the exact number requested (in case we fetched extra)
    collected.truncate(desired);
    Ok(collected)
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;

    #[tokio::test]
    #[ignore = "Requires valid Google API key and CX ID"]
    async fn test_search_google() -> Result<()> {
        let api_key = "test";
        let cx_id = "test";
        let result = search_google("test query", api_key, cx_id, Some(3)).await?;
        assert!(!result.is_empty(), "Expected non‑empty result");
        Ok(())
    }
}
