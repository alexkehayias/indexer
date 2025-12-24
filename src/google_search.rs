use anyhow::{Error, Result};
use reqwest;
use serde::Deserialize;


#[derive(Deserialize)]
struct GoogleSearchResponse {
    // When there are no results, Google responds without the
    // `items` key
    items: Option<Vec<SearchItem>>,
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
    base_url: Option<&str>
) -> Result<Vec<SearchItem>, Error> {
    let desired = num_results.unwrap_or(10) as usize;
    let mut collected: Vec<SearchItem> = Vec::new();
    let mut start_index: u32 = 1; // Google Custom Search uses 1‑based start index
    let base_url = base_url.unwrap_or("https://www.googleapis.com/customsearch/v1");
    let client = reqwest::Client::new();

    while collected.len() < desired {
        // Number of items to request this page (max 10, but not exceeding remaining needed)
        let per_page = std::cmp::min(10, (desired - collected.len()) as u32);
        let mut url = reqwest::Url::parse(base_url).expect("Invalid base URL");
        url.query_pairs_mut()
            .append_pair("key", api_key)
            .append_pair("cx", cx_id)
            .append_pair("q", query)
            .append_pair("num", &per_page.to_string())
            .append_pair("start", &start_index.to_string());

        let resp = client.get(url).send().await?.error_for_status()?;
        let body: GoogleSearchResponse = resp.json().await?;
        let items = body.items.unwrap_or_default();
        let count = items.len();

        collected.extend(items);

       // Move to next page if there is one
        if count < per_page as usize {
            break;
        }
        start_index += per_page;
    }

    // Trim to the exact number requested (in case we fetched extra)
    collected.truncate(desired);
    Ok(collected)
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Result;
    use std::fs;

    #[tokio::test]
    async fn test_search_google() -> Result<()> {
        let mut server = mockito::Server::new_async().await;
        let base_url = format!("{}/{}", server.url(), "customsearch/v1");

        let mock_resp = fs::read_to_string("./tests/data/google_search_results.json").unwrap();
        let mock = server
            .mock("GET", "/customsearch/v1")
            .match_query(mockito::Matcher::AllOf(vec![
                mockito::Matcher::UrlEncoded("key".into(), "test_key".into()),
                mockito::Matcher::UrlEncoded("cx".into(), "test_cx".into()),
                mockito::Matcher::UrlEncoded("q".into(), "test query".into()),
                mockito::Matcher::UrlEncoded("num".into(), "10".into()),
                mockito::Matcher::UrlEncoded("start".into(), "1".into()),
            ]))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(mock_resp)
            .create_async().await;

        let result = search_google("test query", "test_key", "test_cx", Some(10), Some(&base_url)).await?;

        // Sanity check
        mock.assert_async().await;

        // The mock data contains 7 items
        assert_eq!(result.len(), 7);
        assert_eq!(
            result[0].title,
            "Top Rows Alternatives in 2025"
        );
        assert_eq!(
            result[0].link,
            "https://slashdot.org/software/p/Rows/alternatives"
        );
        assert_eq!(
            result[0].snippet,
            "Enhance your productivity in spreadsheets with our AI assistant tailored for Excel and Google Sheets, designed to create and decipher formulas effortlessly."
        );

        Ok(())
    }
}
