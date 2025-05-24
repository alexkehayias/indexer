#[derive(Clone, Debug)]
pub struct AppConfig {
    pub notes_path: String,
    pub index_path: String,
    pub deploy_key_path: String,
    pub vapid_key_path: String,
    pub note_search_api_url: String,
    pub searxng_api_url: String,
    pub gmail_api_client_id: String,
    pub gmail_api_client_secret: String,
}
