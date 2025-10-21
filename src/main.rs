use std::env;

use clap::Parser;

use tantivy::collector::TopDocs;
use tantivy::query::QueryParser;
use tantivy::schema::*;
use tantivy::{Index, ReloadPolicy};

mod schema;
use schema::note_schema;
mod indexing;
mod search;
mod server;
use indexing::index_notes_all;
mod git;
mod source;
use git::{maybe_clone_repo, maybe_pull_and_reset_repo};

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    /// Path to notes to index
    #[arg(short, long, action)]
    reindex: bool,

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

    if args.reindex {
        // Clone the notes repo and index it
        let repo_url =
            env::var("INDEXER_NOTES_REPO_URL").expect("Missing env var INDEXER_NOTES_REPO_URL");
        let deploy_key_path = env::var("INDEXER_NOTES_DEPLOY_KEY_PATH")
            .expect("Missing env var INDEXER_NOTES_REPO_URL");
        maybe_pull_and_reset_repo(&repo_url, deploy_key_path);
        indexing::index_notes_all();
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

    if args.init {
        // Clone the notes repo and index it
        let repo_url =
            env::var("INDEXER_NOTES_REPO_URL").expect("Missing env var INDEXER_NOTES_REPO_URL");
        let deploy_key_path = env::var("INDEXER_NOTES_DEPLOY_KEY_PATH")
            .expect("Missing env var INDEXER_NOTES_REPO_URL");
        maybe_clone_repo(repo_url, deploy_key_path);
        index_notes_all();
    }

    if args.serve {
        server::serve(args.host, args.port).await;
    }

    Ok(())
}
