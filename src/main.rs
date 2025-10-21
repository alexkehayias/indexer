use anyhow::{anyhow, Result};
use clap::{Parser, Subcommand};
use rustyline::error::ReadlineError;
use rustyline::DefaultEditor;
use serde_json::json;
use std::env;
use std::fs;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use indexer::chat::chat;
use indexer::db::{migrate_db, vector_db};
use indexer::git::{maybe_clone_repo, maybe_pull_and_reset_repo};
use indexer::indexing::index_all;
use indexer::openai::{Message, Role, ToolCall};
use indexer::search::search_notes;
use indexer::server;
use indexer::tool::NoteSearchTool;

#[derive(Subcommand)]
enum Command {
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
    /// Initialize indices and clone notes from version control
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
    let deploy_key_path =
        env::var("INDEXER_NOTES_DEPLOY_KEY_PATH").expect("Missing env var INDEXER_NOTES_REPO_URL");
    let vapid_key_path =
        env::var("INDEXER_VAPID_KEY_PATH").expect("Missing env var INDEXER_VAPID_KEY_PATH");

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
        maybe_clone_repo(&deploy_key_path, &repo_url, &notes_path);
    }

    // You can check for the existence of subcommands, and if found use their
    // matches just as you would the top level cmd
    match args.command {
        Some(Command::Serve { host, port }) => {
            server::serve(
                host,
                port,
                notes_path.clone(),
                index_path,
                vec_db_path,
                deploy_key_path,
                vapid_key_path,
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
            let results = search_notes(&index_path, &db, vector, &term, 20);
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
            let tools: Option<Vec<Box<dyn ToolCall + Send + Sync + 'static>>> =
                Some(vec![Box::new(note_search_tool)]);
            // TODO: Window the list of history
            let mut history = vec![Message::new(Role::System, "You are a helpful assistant.")];

            loop {
                let readline = rl.readline("> ");
                match readline {
                    Ok(line) => {
                        history.push(Message::new(Role::User, line.as_str()));
                        chat(&mut history, &tools).await;
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
