use std::env;
use std::fs;

use clap::Parser;
use serde_json::json;

mod indexing;
mod schema;
mod search;
use search::search_notes;
mod server;
use indexing::{index_notes_all, index_notes_vector_all};
mod git;
use git::{maybe_clone_repo, maybe_pull_and_reset_repo};
mod db;
mod source;
use db::{migrate_db, vector_db};

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    /// Path to notes to index
    #[arg(long, action)]
    index: bool,

    /// Search notes with query
    #[arg(long)]
    query: Option<String>,

    /// Clone notes from version control
    #[arg(long, action)]
    init: bool,

    /// Run the server
    #[arg(long, action)]
    serve: bool,

    /// Set the server host address
    #[arg(long, default_value = "127.0.0.1")]
    host: String,

    /// Set the server port
    #[arg(long, default_value = "1111")]
    port: String,
}

#[tokio::main]
async fn main() -> tantivy::Result<()> {
    let args = Args::parse();

    let index_path = "./.index";
    let notes_path = "./notes";
    let vec_db_path = "./db";

    if args.init {
        // Initialize the vector DB
        fs::create_dir(vec_db_path)
            .unwrap_or_else(|err| println!("Ignoring vector DB create failed: {}", err));

        let db = vector_db(vec_db_path).expect("Failed to connect to db");
        migrate_db(&db).expect("DB migration failed");

        // Create the index directory if it doesn't already exist
        fs::create_dir(index_path)
            .unwrap_or_else(|err| println!("Ignoring index directory create failed: {}", err));

        // Clone the notes repo and index it
        let repo_url =
            env::var("INDEXER_NOTES_REPO_URL").expect("Missing env var INDEXER_NOTES_REPO_URL");
        let deploy_key_path = env::var("INDEXER_NOTES_DEPLOY_KEY_PATH")
            .expect("Missing env var INDEXER_NOTES_REPO_URL");
        maybe_clone_repo(repo_url, deploy_key_path);
    }

    if args.index {
        // Clone the notes repo
        let repo_url =
            env::var("INDEXER_NOTES_REPO_URL").expect("Missing env var INDEXER_NOTES_REPO_URL");
        let deploy_key_path = env::var("INDEXER_NOTES_DEPLOY_KEY_PATH")
            .expect("Missing env var INDEXER_NOTES_REPO_URL");
        maybe_pull_and_reset_repo(&repo_url, deploy_key_path);

        // Index for full text search
        index_notes_all(index_path, notes_path);

        // Index for vector search
        let mut db = vector_db(vec_db_path).expect("Failed to connect to db");
        index_notes_vector_all(&mut db, notes_path).expect("Failed to vector index notes");
    }

    if let Some(query) = args.query {
        let db = vector_db(vec_db_path).expect("Failed to connect to db");
        let fts_results = search_notes(&db, &query, true);
        println!(
            "{}",
            json!({
                "query": query,
                "type": "full_text",
                "results": fts_results,
            })
        );
    }

    if args.serve {
        server::serve(args.host, args.port).await;
    }

    Ok(())
}
