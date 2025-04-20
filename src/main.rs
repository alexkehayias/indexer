use anyhow::{anyhow, Result};
use clap::{Parser, Subcommand};
use indexer::schema::note_schema;
use rustyline::error::ReadlineError;
use rustyline::DefaultEditor;
use serde_json::json;
use std::env;
use std::fs;
use tantivy::Index;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use indexer::aql;
use indexer::chat::chat;
use indexer::db::{initialize_db, migrate_db, vector_db};
use indexer::git::{maybe_clone_repo, maybe_pull_and_reset_repo};
use indexer::indexing::index_all;
use indexer::openai::{Message, Role, ToolCall};
use indexer::search::search_notes;
use indexer::server;
use indexer::tool::{NoteSearchTool, SearxSearchTool};

#[derive(Subcommand)]
enum Command {
    /// Initialize indices and clone notes from version control
    Init {
        #[arg(long, action, default_value = "false")]
        db: bool,
        #[arg(long, action, default_value = "false")]
        index: bool,
        #[arg(long, action, default_value = "false")]
        notes: bool,
    },
    /// Migrate indices and db schema
    Migrate {
        #[arg(long, action, default_value = "false")]
        db: bool,
        #[arg(long, action, default_value = "false")]
        index: bool,
    },
    /// Run the API server
    Serve {
        /// Set the server host address
        #[arg(long, default_value = "127.0.0.1")]
        host: String,

        /// Set the server port
        #[arg(long, default_value = "2222")]
        port: String,
    },
    /// Index notes
    Index {
        #[arg(long, default_value = "false")]
        all: bool,
        #[arg(long, default_value = "false")]
        full_text: bool,
        #[arg(long, default_value = "false")]
        vector: bool,
    },
    /// Query the search index
    Query {
        #[arg(long)]
        term: String,
        #[arg(long, default_value = "false")]
        vector: bool,
    },
    /// Start a chat bot session
    Chat {},
}

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
#[command(propagate_version = true)]
struct Cli {
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
    let deploy_key_path =
        env::var("INDEXER_NOTES_DEPLOY_KEY_PATH").expect("Missing env var INDEXER_NOTES_REPO_URL");
    let vapid_key_path =
        env::var("INDEXER_VAPID_KEY_PATH").expect("Missing env var INDEXER_VAPID_KEY_PATH");

    // Handle each sub command
    match args.command {
        Some(Command::Init { db, index, notes }) => {
            if !db && !index && !notes {
                return Err(anyhow!(
                    "Missing value for init \"--db\", \"--index\", and/or \"--notes\""
                ));
            }

            if db {
                println!("Initializing db...");
                // Initialize the vector DB
                fs::create_dir_all(&vec_db_path)
                    .unwrap_or_else(|err| println!("Ignoring vector DB create failed: {}", err));

                let db = vector_db(&vec_db_path).expect("Failed to connect to db");
                initialize_db(&db).expect("DB initialization failed");
                println!("Finished initializing db");
            }

            if index {
                println!("Initializing search index...");
                // Create the index directory if it doesn't already exist
                fs::create_dir_all(&index_path).unwrap_or_else(|err| {
                    println!("Ignoring index directory create failed: {}", err)
                });
                println!("Finished initializing search index...");
            }

            // Clone and reset the notes repo to origin/main
            if notes {
                println!("Cloning notes repo from git...");
                let deploy_key_path = env::var("INDEXER_NOTES_DEPLOY_KEY_PATH")
                    .expect("Missing env var INDEXER_NOTES_REPO_URL");
                // Clone the notes repo and index it
                let repo_url = env::var("INDEXER_NOTES_REPO_URL")
                    .expect("Missing env var INDEXER_NOTES_REPO_URL");
                maybe_clone_repo(&deploy_key_path, &repo_url, &notes_path);
                println!("Finished cloning and resetting notes from git");
            }
        }
        Some(Command::Migrate { db, index }) => {
            // Run the DB migration script
            if db {
                println!("Migrating db...");
                let db = vector_db(&vec_db_path).expect("Failed to connect to db");
                migrate_db(&db).unwrap_or_else(|err| eprintln!("DB migration failed {}", err));
                println!("Finished migrating db");
            }

            // Delete and recreate the index
            if index {
                println!("Migrating search index...");
                fs::remove_dir_all(index_path.clone()).expect("Failed to delete index directory");
                fs::create_dir(index_path.clone()).expect("Failed to recreate index directory");
                let index_path =
                    tantivy::directory::MmapDirectory::open(index_path).expect("Index not found");
                let schema = note_schema();
                Index::open_or_create(index_path, schema.clone())
                    .expect("Unable to open or create index");
                println!("Finished migrating search index");
                println!(
                    "NOTE: You will need to re-populate the index by running --index --full-text"
                );
            }
        }
        Some(Command::Serve { host, port }) => {
            let note_search_api_url = env::var("INDEXER_NOTE_SEARCH_API_URL")
                .unwrap_or(format!("http://{}:{}", host, port));

            server::serve(
                host.clone(),
                port,
                notes_path.clone(),
                index_path,
                vec_db_path,
                deploy_key_path,
                vapid_key_path,
                note_search_api_url,
                env::var("INDEXER_SEARXNG_API_URL")
                    .unwrap_or(format!("http://{}:{}", host, "8080")),
            )
            .await;
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
            let query = aql::parse_query(&term).expect("Parsing AQL failed");
            let results = search_notes(&index_path, &db, vector, &query, 20);
            println!(
                "{}",
                json!({
                    "query": term,
                    "results": results,
                })
            );
        }
        Some(Command::Chat {}) => {
            let mut rl = DefaultEditor::new().expect("Editor failed");

            let note_search_tool = NoteSearchTool::default();
            let searx_search_tool = SearxSearchTool::default();
            let tools: Option<Vec<Box<dyn ToolCall + Send + Sync + 'static>>> = Some(vec![
                Box::new(note_search_tool),
                Box::new(searx_search_tool),
            ]);
            // TODO: Window the list of history
            let mut history = vec![Message::new(Role::System, "You are a helpful assistant.")];

            loop {
                let readline = rl.readline("> ");
                match readline {
                    Ok(line) => {
                        history.push(Message::new(Role::User, line.as_str()));
                        chat(&mut history, &tools).await;
                        println!("{}", history.last().unwrap().content.clone().unwrap());
                    }
                    Err(ReadlineError::Interrupted) => break,
                    Err(ReadlineError::Eof) => break,
                    Err(err) => {
                        println!("Error: {:?}", err);
                        break;
                    }
                }
            }
        }
        None => {}
    }

    Ok(())
}
