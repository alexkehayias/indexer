use std::env;
use std::fs;

use anyhow::{anyhow, Result};
use clap::{Parser, Subcommand};
use serde_json::json;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

mod indexing;
mod schema;
mod search;
use search::search_notes;
mod server;
use indexing::index_all;
mod git;
use git::{maybe_clone_repo, maybe_pull_and_reset_repo};
mod db;
mod source;
use db::{migrate_db, vector_db};
mod export;

#[derive(Subcommand)]
enum Command {
    /// Adds files to myapp
    Serve {
        /// Set the server host address
        #[arg(long, default_value = "127.0.0.1")]
        host: String,

        /// Set the server port
        #[arg(long, default_value = "2222")]
        port: String,
    },
    Index {
        #[arg(long, default_value = "false")]
        all: bool,
        #[arg(long, default_value = "false")]
        full_text: bool,
        #[arg(long, default_value = "false")]
        vector: bool,
    },
    Query {
        #[arg(long)]
        term: String,
        #[arg(long, default_value = "false")]
        vector: bool,
    },
}

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
#[command(propagate_version = true)]
struct Cli {
    /// Clone notes from version control
    #[arg(long, action, default_value = "false")]
    init: bool,

    #[command(subcommand)]
    command: Option<Command>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Cli::parse();

    let storage_path = env::var("INDEXER_STORAGE_PATH").unwrap_or("./".to_string());
    let index_path = format!("{}/index", storage_path);
    let notes_path = format!("{}/notes", storage_path);
    let vec_db_path = format!("{}/db", storage_path);

    // Default command
    if args.init {
        // Initialize the vector DB
        fs::create_dir_all(&vec_db_path)
            .unwrap_or_else(|err| println!("Ignoring vector DB create failed: {}", err));

        let db = vector_db(&vec_db_path).expect("Failed to connect to db");
        migrate_db(&db).expect("DB migration failed");

        // Create the index directory if it doesn't already exist
        fs::create_dir_all(&index_path)
            .unwrap_or_else(|err| println!("Ignoring index directory create failed: {}", err));

        // Clone the notes repo and index it
        let repo_url =
            env::var("INDEXER_NOTES_REPO_URL").expect("Missing env var INDEXER_NOTES_REPO_URL");
        let deploy_key_path = env::var("INDEXER_NOTES_DEPLOY_KEY_PATH")
            .expect("Missing env var INDEXER_NOTES_REPO_URL");
        maybe_clone_repo(&deploy_key_path, &repo_url, &notes_path);
    }

    // You can check for the existence of subcommands, and if found use their
    // matches just as you would the top level cmd
    match args.command {
        Some(Command::Serve { host, port }) => {
            server::serve(host, port, notes_path.clone(), index_path, vec_db_path).await;
        }
        Some(Command::Index {
            all,
            full_text,
            vector,
        }) => {
            if !all && !full_text && !vector {
                return Err(anyhow!(
                    "Missing value for index \"all\", \"full-text\", and/or \"vector\""
                ));
            }
            // If using the CLI only and not the webserver, set up tracing to
            // output to stdout and stderr
            tracing_subscriber::registry()
                .with(
                    tracing_subscriber::EnvFilter::try_from_default_env()
                        .unwrap_or_else(|_| format!("{}=debug", env!("CARGO_CRATE_NAME")).into()),
                )
                .with(tracing_subscriber::fmt::layer())
                .init();

            // Clone the notes repo
            let deploy_key_path = env::var("INDEXER_NOTES_DEPLOY_KEY_PATH")
                .expect("Missing env var INDEXER_NOTES_REPO_URL");
            maybe_pull_and_reset_repo(&deploy_key_path, &notes_path);

            let mut db = vector_db(&vec_db_path).expect("Failed to connect to db");

            if full_text {
                // Index for full text search
                index_all(&mut db, &index_path, &notes_path, true, false, None)
                    .expect("Indexing failed");
            }
            if vector {
                // Index for vector search
                index_all(&mut db, &index_path, &notes_path, false, true, None)
                    .expect("Indexing failed");
            }

            if all {
                index_all(&mut db, &index_path, &notes_path, true, true, None)
                    .expect("Indexing failed");
            }
        }
        Some(Command::Query { term, vector }) => {
            let db = vector_db(&vec_db_path).expect("Failed to connect to db");
            let results = search_notes(&index_path, &db, vector, &term, 20);
            println!(
                "{}",
                json!({
                    "query": term,
                    "results": results,
                })
            );
        }
        None => {}
    }

    Ok(())
}
