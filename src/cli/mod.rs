use anyhow::Result;
use clap::{Parser, Subcommand};
use std::env;
use std::time::Duration;

pub mod auth;
pub mod bashkit;
pub mod channel;
pub mod chat;
pub mod develop;
pub mod eval;
pub mod example_data;
pub mod index;
pub mod init;
pub mod job;
pub mod loop_cmd;
pub mod migrate;
pub mod projects;
pub mod query;
pub mod rebuild;
pub mod serve;
pub mod session;
pub mod tasks;
pub mod web;

use auth::ServiceKind;
use job::JobId;

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
        #[arg(long, action, default_value = "false")]
        skills: bool,
        #[arg(long, action, default_value = "false")]
        workspace: bool,
    },
    /// Migrate indices and db schema
    Migrate {
        #[arg(long, action, default_value = "false")]
        db: bool,
        #[arg(long, action, default_value = "false")]
        index: bool,
    },
    /// Run the server
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
        /// Index chat messages from non-background sessions
        #[arg(long, default_value = "false")]
        chat: bool,
    },
    /// Rebuild all indices from source
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
    /// Stream events from stdin to subscribers over a Unix domain socket (pub/sub)
    Channel {
        /// Channel ID (alphanumeric with dashes/underscores; identifies the socket path)
        id: String,
        /// Coalesce lines arriving within this window (ms) into a single event
        #[arg(long, default_value_t = 250)]
        debounce_ms: u64,
    },
    /// Subscribe to one or more channels and run an LLM chat on incoming events
    Loop {
        /// Channel ID to subscribe to (repeat for multiple channels)
        #[arg(long, num_args = 1..)]
        channel: Vec<String>,
        /// System prompt for the LLM (defaults to a multi-channel event assistant)
        #[arg(long)]
        prompt: Option<String>,
        /// Coalesce lines arriving within this window (ms) into a single event
        #[arg(long, default_value_t = 250)]
        debounce_ms: u64,
        /// Tool names to give the agent (repeat for multiple; defaults to bash+notify)
        #[arg(long, num_args = 1..)]
        tools: Vec<String>,
    },
    /// Set up a development worktree with herdr and Claude Code
    Develop {
        /// Branch/worktree name
        name: String,
        /// Skip initialization
        #[arg(long)]
        no_init: bool,
        /// Skip loading example data
        #[arg(long)]
        no_examples: bool,
        /// Starting port for scanning (default: 2222)
        #[arg(long)]
        base_port: Option<u16>,
    },
    /// Perform oauth and store credentials
    Auth {
        #[arg(long, value_enum)]
        service: ServiceKind,
    },
    /// Run a job
    Job {
        #[arg(long, value_enum)]
        id: JobId,
    },
    /// Run an eval
    Eval {
        #[arg(long)]
        file: String,
        /// Override the model from config
        #[arg(long)]
        model: Option<String>,
        /// Run without saving results to the database
        #[arg(long, default_value = "false")]
        dry_run: bool,
    },
    /// Web-related commands (fetch, etc.)
    Web {
        #[command(subcommand)]
        command: web::WebCommand,
    },
    /// Load example .org notes for development
    ExampleData {},
    /// List projects
    Projects {
        #[command(subcommand)]
        command: projects::ProjectsCommand,
    },
    /// Create, update, or delete tasks
    Tasks {
        #[command(subcommand)]
        command: TasksCommand,
    },
    /// Manage chat sessions
    Session {
        #[command(subcommand)]
        command: SessionCommand,
    },
}

#[derive(Subcommand)]
enum TasksCommand {
    /// Create a new task
    Create {
        #[arg(long)]
        title: String,
        #[arg(long)]
        body: Option<String>,
        #[arg(long)]
        project: Option<String>,
        #[arg(long, default_value = "TODO")]
        status: String,
    },
    /// Update an existing task by UUID
    Update {
        id: String,
        #[arg(long)]
        title: Option<String>,
        #[arg(long)]
        body: Option<String>,
        #[arg(long)]
        status: Option<String>,
        /// Project ID or filename (skips slug-based lookup)
        #[arg(long)]
        project: Option<String>,
        /// Tags to add (comma-separated, e.g. "urgent,errands")
        #[arg(long)]
        add_tag: Option<String>,
        /// Tags to remove (comma-separated, e.g. "low-priority")
        #[arg(long)]
        remove_tag: Option<String>,
    },
    /// Delete a task by UUID
    Delete {
        id: String,
    },
    /// List tasks, optionally filtered by project and/or status
    List {
        /// Project name or ID (looks up by slug or :ID: property)
        #[arg(long)]
        project: Option<String>,
        #[arg(long)]
        status: Option<String>,
    },
    /// Move a task to a project file
    Refile {
        id: String,
        #[arg(long)]
        project: String,
    },
    /// Show full details of a task, including its body (markdown by default)
    Show {
        id: String,
        /// Output the raw, unparsed org-mode headline as stored in the file
        #[arg(long, default_value = "false")]
        raw: bool,
    },
}

#[derive(Subcommand)]
enum SessionCommand {
    /// Delete a chat session, its messages, search index entries, and
    /// workspace directory
    Delete {
        id: String,
    },
    /// Generate (or regenerate) a title and summary for a chat session
    Summarize {
        id: String,
    },
    /// List chat sessions with their IDs and titles
    List {},
}

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
#[command(propagate_version = true)]
#[command(arg_required_else_help = true)]
pub struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

/// Run the CLI, parsing args from the environment.
pub async fn run() -> Result<()> {
    let cli = Cli::parse();
    run_dispatch(cli).await
}

/// Run the CLI with explicit arguments (for programmatic use, e.g. bashkit).
///
/// The first element should be the program name (e.g. "hq"), followed by
/// subcommand and flags — matching what would be passed on the command line.
pub async fn run_with_args(args: Vec<String>) -> Result<()> {
    let cli = Cli::try_parse_from(args)?;
    run_dispatch(cli).await
}

async fn run_dispatch(cli: Cli) -> Result<()> {
    let storage_path = env::var("HQ_STORAGE_PATH").unwrap_or("./".to_string());
    let index_path = format!("{}/index", storage_path);
    let notes_path = format!("{}/notes", storage_path);
    let vec_db_path = format!("{}/db", storage_path);

    match cli.command {
        Some(Command::Init { db, index, notes, skills, workspace }) => {
            init::run(db, index, notes, skills, workspace, &vec_db_path, &index_path, &notes_path).await?;
        }
        Some(Command::Migrate { db, index }) => {
            migrate::run(db, index, &vec_db_path, &index_path).await?;
        }
        Some(Command::Serve { host, port }) => {
            serve::run(host, port).await;
        }
        Some(Command::Index {
            all,
            full_text,
            vector,
            chat,
        }) => {
            index::run(
                all,
                full_text,
                vector,
                chat,
                &index_path,
                &notes_path,
                &vec_db_path,
            )
            .await?;
        }
        Some(Command::Rebuild {}) => {
            rebuild::run(&index_path, &notes_path, &vec_db_path).await?;
        }
        Some(Command::Query { term, vector }) => {
            query::run(term, vector, &index_path, &vec_db_path).await?;
        }
        Some(Command::Chat {}) => {
            chat::run(&vec_db_path).await?;
        }
        Some(Command::Channel { id, debounce_ms }) => {
            channel::run(&storage_path, &id, Duration::from_millis(debounce_ms)).await?;
        }
        Some(Command::Loop {
            channel,
            prompt,
            debounce_ms,
            tools,
        }) => {
            let api_hostname =
                env::var("HQ_LOCAL_LLM_HOST").unwrap_or_else(|_| "https://api.openai.com".to_string());
            let api_key =
                env::var("OPENAI_API_KEY").unwrap_or_else(|_| "thiswontworkforopenai".to_string());
            let model =
                env::var("HQ_LOCAL_LLM_MODEL").unwrap_or_else(|_| "gpt-4.1-mini".to_string());
            let vapid_key_path = env::var("HQ_VAPID_KEY_PATH").unwrap_or_else(|_| String::new());
            let db = crate::core::db::async_db(&vec_db_path).await?;
            loop_cmd::run(
                db,
                &storage_path,
                &api_hostname,
                &api_key,
                &model,
                &vapid_key_path,
                &channel,
                Duration::from_millis(debounce_ms),
                &tools,
                prompt.as_deref(),
            )
            .await?;
        }
        Some(Command::Develop { name, no_init, no_examples, base_port }) => {
            develop::run(name, no_init, no_examples, base_port).await?;
        }
        Some(Command::Auth { service }) => {
            auth::run(service, &vec_db_path).await?;
        }
        Some(Command::Job { id }) => {
            job::run(id).await?;
        }
        Some(Command::Eval { model, file, dry_run }) => {
            let api_key = env::var("OPENAI_API_KEY").unwrap_or_else(|_| "thiswontworkforopenai".to_string());
            let api_hostname = env::var("HQ_LOCAL_LLM_HOST").unwrap_or_else(|_| "https://api.openai.com".to_string());
            let model = model.unwrap_or_else(|| env::var("HQ_LOCAL_LLM_MODEL").expect("Missing model name"));

            eval::run(vec_db_path, api_hostname, api_key, model, file, dry_run).await?;
        }
        Some(Command::Web { command }) => {
            web::run(command).await?;
        }
        Some(Command::ExampleData {}) => {
            example_data::run(&notes_path, &index_path, &vec_db_path).await?;
        }
        Some(Command::Projects { command }) => {
            let db = crate::core::db::async_db(&vec_db_path).await?;
            projects::run(command, &db).await?;
        }
        Some(Command::Tasks { command }) => {
            let task_db = crate::core::db::async_db(&vec_db_path).await?;
            match command {
            TasksCommand::Create {
                title,
                body,
                project,
                status,
            } => {
                tasks::run_create(
                    &task_db,
                    &notes_path,
                    &index_path,
                    &title,
                    body.as_deref(),
                    project.as_deref(),
                    &status,
                )
                .await?;
            }
            TasksCommand::Update {
                id,
                title,
                body,
                status,
                project,
                add_tag,
                remove_tag,
            } => {
                let add_tags = add_tag
                    .as_deref()
                    .map(tasks::parse_tag_list)
                    .unwrap_or_default();
                let remove_tags = remove_tag
                    .as_deref()
                    .map(tasks::parse_tag_list)
                    .unwrap_or_default();
                tasks::run_update(
                    &task_db,
                    &notes_path,
                    &index_path,
                    &id,
                    title.as_deref(),
                    body.as_deref(),
                    status.as_deref(),
                    project.as_deref(),
                    &add_tags,
                    &remove_tags,
                )
                .await?;
            }
            TasksCommand::Delete { id } => {
                tasks::run_delete(&task_db, &notes_path, &index_path, &id).await?;
            }
            TasksCommand::List { project, status } => {
                tasks::run_list(&task_db, &notes_path, project.as_deref(), status.as_deref()).await?;
            }
            TasksCommand::Refile { id, project } => {
                tasks::run_refile(&task_db, &notes_path, &index_path, &id, &project).await?;
            }
            TasksCommand::Show { id, raw } => {
                let result = tasks::run_show(&task_db, &notes_path, &id, raw).await?;
                print!("{result}");
            }
        }
        }
        Some(Command::Session { command }) => match command {
            SessionCommand::Delete { id } => {
                session::run_delete(&vec_db_path, &index_path, &storage_path, &id).await?;
            }
            SessionCommand::Summarize { id } => {
                let api_hostname = env::var("HQ_LOCAL_LLM_HOST")
                    .unwrap_or_else(|_| "https://api.openai.com".to_string());
                let api_key = env::var("OPENAI_API_KEY")
                    .unwrap_or_else(|_| "thiswontworkforopenai".to_string());
                let model = env::var("HQ_LOCAL_LLM_MODEL")
                    .unwrap_or_else(|_| "gpt-4.1-mini".to_string());
                let db = crate::core::db::async_db(&vec_db_path).await?;
                session::run_summarize(db, &api_hostname, &api_key, &model, &id).await?;
            }
            SessionCommand::List {} => {
                let db = crate::core::db::async_db(&vec_db_path).await?;
                session::run_list(db).await?;
            }
        },
        None => {}
    }

    Ok(())
}
