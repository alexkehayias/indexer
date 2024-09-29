use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, RwLock};

use clap::Parser;

use tantivy::collector::TopDocs;
use tantivy::query::QueryParser;
use tantivy::schema::*;
use tantivy::{Index, IndexWriter, ReloadPolicy};

use axum::extract::Query;
use axum::{
    extract::State,
    response::Json,
    routing::{get, post},
    Router,
};
use orgize::ParseConfig;
use serde::Deserialize;
use serde_json::{json, Value};
use tower_http::cors::CorsLayer;
use tower_http::services::ServeDir;
use tower_http::trace::TraceLayer;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

mod schema;
use schema::note_schema;
mod search;
use search::search_notes;
mod server;
mod indexing;
mod source;

// Clone a repo if it doesn't already exist
fn maybe_clone_repo(url: String, deploy_key_path: String) {
    let git_clone = Command::new("sh")
        .arg("-c")
        .arg(format!("GIT_SSH_COMMAND='ssh -i {} -o IdentitiesOnly=yes' git clone {}", deploy_key_path, url))
        .output()
        .expect("failed to execute process");

    let stdout = std::str::from_utf8(&git_clone.stdout).expect("Failed to parse stdout");
    let stderr = std::str::from_utf8(&git_clone.stderr).expect("Failed to parse stderr");
    println!("stdout: {}\nstderr: {}", stdout, stderr);
}

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    /// Path to notes to index
    #[arg(short, long)]
    path: Option<String>,

    /// Search notes with query
    #[arg(short, long)]
    query: Option<String>,

    /// Clone notes from version control
    #[arg(short, long, action)]
    init: bool,

    /// Run the server
    #[arg(short, long, action)]
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

    let schema = note_schema();
    let index_path = tantivy::directory::MmapDirectory::open("./.index")?;
    let idx = Index::open_or_create(index_path, schema.clone())?;

    if let Some(notes_path) = args.path {
        let mut index_writer: IndexWriter = idx.writer(50_000_000)?;

        for note in source::notes(&notes_path) {
            let _ = indexing::index_note(&mut index_writer, &schema, note);
        }

        index_writer.commit()?;
    }

    if let Some(query) = args.query {
        let reader = idx
            .reader_builder()
            .reload_policy(ReloadPolicy::OnCommitWithDelay)
            .try_into()?;
        let searcher = reader.searcher();
        let title = schema.get_field("title").unwrap();
        let body = schema.get_field("body").unwrap();
        let query_parser = QueryParser::for_index(&idx, vec![title, body]);

        let query = query_parser.parse_query(&query)?;

        let top_docs = searcher.search(&query, &TopDocs::with_limit(10))?;
        for (_score, doc_address) in top_docs {
            let retrieved_doc: TantivyDocument = searcher.doc(doc_address)?;
            println!("{}", retrieved_doc.to_json(&schema));
        }
    }

    // if args.init {
    //     // Clone the notes repo and index it
    //     let repo_url =
    //         env::var("INDEXER_NOTES_REPO_URL").expect("Missing env var INDEXER_NOTES_REPO_URL");
    //     let deploy_key_path = env::var("INDEXER_NOTES_DEPLOY_KEY_PATH")
    //         .expect("Missing env var INDEXER_NOTES_REPO_URL");
    //     maybe_clone_repo(repo_url, deploy_key_path);
    //     let _res = index_notes().await;
    // }

    if args.serve {
        server::serve(args.host, args.port).await;
    }

    Ok(())
}
