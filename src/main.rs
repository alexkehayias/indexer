use anyhow::{Result, anyhow};
use clap::{Parser, Subcommand, ValueEnum};
use rustyline::DefaultEditor;
use rustyline::error::ReadlineError;
use serde_json::json;
use std::env;
use std::fs;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

use indexer::aql;
use indexer::chat::chat;
use indexer::db::{initialize_db, migrate_db, vector_db};
use indexer::fts::utils::recreate_index;
use indexer::git::{maybe_clone_repo, maybe_pull_and_reset_repo};
use indexer::indexing::index_all;
use indexer::openai::{Message, Role, ToolCall};
use indexer::search::search_notes;
use indexer::server;
use indexer::tool::{NoteSearchTool, SearxSearchTool, EmailUnreadTool};

#[derive(ValueEnum, Clone)]
enum ServiceKind {
    Gmail,
}

impl ServiceKind {
    fn to_str(&self) -> &'static str {
        match self {
            ServiceKind::Gmail => "gmail",
        }
    }
}

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
    /// Rebuild all of the indices from source
    Rebuild {},
    /// Query the search index
    Query {
        #[arg(long)]
        term: String,
        #[arg(long, default_value = "false")]
        vector: bool,
    },
    /// Start a chat bot session
    Chat {},
    /// Perform OAuth authentication and print tokens
    Auth {
        #[arg(long, value_enum)]
        service: ServiceKind,
    },
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
                recreate_index(&index_path);
                println!("Finished migrating search index");
                println!(
                    "NOTE: You will need to re-populate the index by running --index --full-text"
                );
            }
        }
        Some(Command::Serve { host, port }) => {
            let note_search_api_url = env::var("INDEXER_NOTE_SEARCH_API_URL")
                .unwrap_or(format!("http://{}:{}", host, port));
            let searxng_api_url = env::var("INDEXER_SEARXNG_API_URL")
                .unwrap_or(format!("http://{}:{}", host, "8080"));
            let gmail_client_id =
                std::env::var("INDEXER_GMAIL_CLIENT_ID").expect("Missing INDEXER_GMAIL_CLIENT_ID");
            let gmail_client_secret = std::env::var("INDEXER_GMAIL_CLIENT_SECRET")
                .expect("Missing INDEXER_GMAIL_CLIENT_SECRET");

            server::serve(
                host.clone(),
                port,
                notes_path.clone(),
                index_path,
                vec_db_path,
                deploy_key_path,
                vapid_key_path,
                note_search_api_url,
                searxng_api_url,
                gmail_client_id,
                gmail_client_secret,
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
        Some(Command::Rebuild {}) => {
            tracing_subscriber::registry()
                .with(
                    tracing_subscriber::EnvFilter::try_from_default_env()
                        .unwrap_or_else(|_| format!("{}=debug", env!("CARGO_CRATE_NAME")).into()),
                )
                .with(tracing_subscriber::fmt::layer())
                .init();

            let mut db = vector_db(&vec_db_path).expect("Failed to connect to db");

            // Delete all note metadata and vector data
            println!("Deleting all meta data in the db...");
            db.execute("DELETE FROM vec_items", [])
                .expect("Failed to delete vec_items data");
            db.execute("DELETE FROM note_meta", [])
                .expect("Failed to delete note_meta data");
            println!("Finished deleting all meta data the db...");

            // Remove the full text search index
            println!("Recreating search index...");
            recreate_index(&index_path);
            println!("Finished recreating search index");

            // Index everything
            index_all(&mut db, &index_path, &notes_path, true, true, None)
                .expect("Indexing failed");
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

            let note_search_tool = env::var("INDEXER_NOTE_SEARCH_API_URL")
                .map(|url| NoteSearchTool::new(&url))
                .unwrap_or_default();
            let email_unread_tool = env::var("INDEXER_NOTE_SEARCH_API_URL")
                .map(|url| EmailUnreadTool::new(&url))
                .unwrap_or_default();
            let searx_search_tool = env::var("INDEXER_SEARXNG_API_URL")
                .map(|url| SearxSearchTool::new(&url))
                .unwrap_or_default();
            let tools: Option<Vec<Box<dyn ToolCall + Send + Sync + 'static>>> = Some(vec![
                Box::new(note_search_tool),
                Box::new(searx_search_tool),
                Box::new(email_unread_tool),
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
        Some(Command::Auth { service }) => {
            match service {
                ServiceKind::Gmail => {
                    use indexer::oauth::exchange_code_for_token;
                    use std::io::{self, Write};

                    // Prompt the user for their email address
                    print!("Enter the email address you are authenticating: ");
                    io::stdout().flush().unwrap();
                    let mut user_email = String::new();
                    io::stdin()
                        .read_line(&mut user_email)
                        .expect("Failed to read email address");
                    let user_email = user_email.trim();

                    let client_id = std::env::var("INDEXER_GMAIL_CLIENT_ID")
                        .expect("Set INDEXER_GMAIL_CLIENT_ID in your environment");
                    let client_secret = std::env::var("INDEXER_GMAIL_CLIENT_SECRET")
                        .expect("Set INDEXER_GMAIL_CLIENT_SECRET in your environment");
                    let redirect_uri = std::env::var("INDEXER_GMAIL_REDIRECT_URI")
                        .unwrap_or_else(|_| "urn:ietf:wg:oauth:2.0:oob".to_string());
                    let scope = "https://www.googleapis.com/auth/gmail.modify";
                    let auth_url = format!(
                        "https://accounts.google.com/o/oauth2/v2/auth?client_id={}&redirect_uri={}&response_type=code&scope={}&access_type=offline&prompt=consent",
                        urlencoding::encode(&client_id),
                        urlencoding::encode(&redirect_uri),
                        urlencoding::encode(scope)
                    );
                    println!(
                        "\nPlease open the following URL in your browser and authorize access:\n\n{}\n",
                        auth_url
                    );
                    print!("Paste the authorization code shown by Google here: ");
                    io::stdout().flush().unwrap();
                    let mut code = String::new();
                    io::stdin()
                        .read_line(&mut code)
                        .expect("Failed to read code");
                    let code = code.trim();

                    let token =
                        exchange_code_for_token(&client_id, &client_secret, code, &redirect_uri)
                            .await?;

                    // Store the refresh token in the DB and use that to fetch an access token from now on.
                    let db = vector_db(&vec_db_path).expect("Failed to connect to db");
                    let refresh_token = token
                        .refresh_token
                        .clone()
                        .ok_or(anyhow!("No refresh token in response"))?;

                    db.execute(
                        "INSERT INTO auth (id, service, refresh_token) VALUES (?1, ?2, ?3)
                         ON CONFLICT(id) DO UPDATE SET service = excluded.service, refresh_token = excluded.refresh_token",
                        (&user_email, service.to_str(), &refresh_token),
                    )
                    .expect("Failed to insert/update refresh token in DB");

                    println!("Refresh token for {} saved to DB.", user_email);
                }
            }
        }
        None => {}
    }

    Ok(())
}
