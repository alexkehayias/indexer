use std::env;
use std::fs;
use std::path::PathBuf;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::process::Command;

use clap::Parser;

use tantivy::collector::TopDocs;
use tantivy::query::QueryParser;
use tantivy::schema::*;
use tantivy::{doc, Index, IndexWriter, ReloadPolicy};

use axum::{
    routing::{get, post},
    response::Json,
    Router,
    extract::State,
};
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;
use tower_http::services::ServeDir;
use axum::extract::Query;
use serde::Deserialize;

use serde_json::{Value, json};
use orgize::ParseConfig;

fn note_schema() -> Schema {
    let mut schema_builder = Schema::builder();
    schema_builder.add_text_field("id", TEXT | STORED);
    schema_builder.add_text_field("title", TEXT | STORED);
    schema_builder.add_text_field("tags", TEXT | STORED);
    schema_builder.add_text_field("body", TEXT);
    schema_builder.add_text_field("file_name", TEXT);
    schema_builder.build()
}

// There is no such thing as updates in tantivy so this function will
// produce duplicates if called repeatedly
fn index_note(
    index_writer: &mut IndexWriter,
    schema: &Schema,
    path: PathBuf,
) -> tantivy::Result<()> {
    println!("Indexing note: {}", &path.display());

    let id = schema.get_field("id")?;
    let title = schema.get_field("title")?;
    let body = schema.get_field("body")?;
    let tags = schema.get_field("tags")?;
    let file_name = schema.get_field("file_name")?;

    // Parse the file from the path
    let content = fs::read_to_string(&path)?;
    let config = ParseConfig {
        ..Default::default()
    };
    let p = config.parse(&content);

    let props = p.document().properties().expect("Missing property
drawer");
    let id_value = props.get("ID").expect("Missing org-id").to_string();
    let file_name_value = path.file_name().unwrap().to_string_lossy().into_owned();
    let title_value = p.title().expect("No title found");
    let body_value = p.document().raw();
    let filetags: Vec<Vec<String>> = p.keywords()
        .filter_map(|k| match k.key().to_string().as_str() {
            "FILETAGS" => Some(k.value()
                               .to_string()
                               .trim()
                               .split(" ")
                               .map(|s| s.to_string())
                               .collect()),
           _ => None
        })
        .collect();

    // For now, tags are a comma separated string which should
    // allow it to still be searchable
    let tags_value = if filetags.is_empty() {
        String::new()
    } else {
        filetags[0].to_owned().join(",")
    };

    index_writer.add_document(doc!(
        id => id_value,
        title => title_value,
        body => body_value,
        file_name => file_name_value,
        tags => tags_value,
    ))?;

    Ok(())
}

// Get first level files in the directory, does not follow sub directories
fn notes(path: &str) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(path) else {
        return vec![];
    };

    // TODO: make this recursive if there is more than one directory of notes
    entries
        .flatten()
        .flat_map(|entry| {
            let Ok(meta) = entry.metadata() else {
                return vec![];
            };
            // Skip directories and non org files
            let path = entry.path();
            let ext = path.extension().unwrap_or_default();
            let name = path.file_name().unwrap_or_default();
            if meta.is_file() && ext == "org" && name != "config.org" && name != "capture.org" {
                return vec![entry.path()];
            }
            vec![]
        })
        .collect()
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
    #[arg(long, default_value="127.0.0.1")]
    host: String,

    /// Set the server port
    #[arg(long, default_value="1111")]
    port: String,
}


type SharedState = Arc<RwLock<AppState>>;

#[derive(Default)]
struct AppState {
    // Stores the latest search hit selected by the user
    latest_selection: HashMap<String, String>,
}

#[derive(Debug, Deserialize)]
struct SetLatest {
    id: String,
    title: String,
}

async fn kv_get(State(state): State<SharedState>) -> Json<Value> {
    let resp = json!({
        "id": state.read().unwrap().latest_selection.get("id"),
        "title": state.read().unwrap().latest_selection.get("title"),
    });
    Json(resp)
}

async fn kv_set(State(state): State<SharedState>, Json(data): Json<SetLatest>) {
    state.write().unwrap().latest_selection.insert(String::from("id"), data.id);
    state.write().unwrap().latest_selection.insert(String::from("title"), data.title);
}

// Fulltext search of all notes
async fn search(Query(params): Query<HashMap<String, String>>) -> Json<Value> {
    let schema = note_schema();
    let index_path = tantivy::directory::MmapDirectory::open("./.index").expect("Index not found");
    let idx = Index::open_or_create(index_path, schema.clone()).expect("Unable to open or create index");
    let title = schema.get_field("title").unwrap();
    let body = schema.get_field("body").unwrap();

    let reader = idx
        .reader_builder()
        .reload_policy(ReloadPolicy::OnCommitWithDelay)
        .try_into().expect("Reader failed to load");

    let searcher = reader.searcher();
    let query_parser = QueryParser::for_index(&idx, vec![title, body]);

    let results: Vec<NamedFieldDocument> = if let Some(query) = params.get("query") {
        let query = query_parser.parse_query(query).expect("Failed to parse query");

        searcher.search(&query, &TopDocs::with_limit(10))
            .expect("Search failed")
            .iter()
            .map(|(_score, doc_addr) | {
                searcher.doc::<TantivyDocument>(*doc_addr)
                    .expect("Doc not found")
                    .to_named_doc(&schema)
            })
            .collect()
    } else {
        Vec::new()
    };

    let resp = json!({
        "query": params.get("query"),
        "results": results,
    });
    Json(resp)
}

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

// Build the index for all notes
async fn index_notes() -> Json<Value> {
    let notes_path = "./notes";
    let schema = note_schema();
    let index_path = tantivy::directory::MmapDirectory::open("./.index").expect("Index not found");
    let idx = Index::open_or_create(index_path, schema.clone()).expect("Unable to open or create index");
    let mut index_writer: IndexWriter = idx.writer(50_000_000).expect("Index writer failed to initialize");

    for note in notes(notes_path) {
        let _ = index_note(&mut index_writer, &schema, note);
    }

    index_writer.commit().expect("Index write failed");

    let resp = json!({
        "success": true,
    });
    Json(resp)
}

// Run the server
async fn serve(host: String, port: String) {
    let shared_state = SharedState::default();
    let cors = CorsLayer::permissive();
    let serve_dir = ServeDir::new("./web-ui/src");

    let app = Router::new()
        // Search API endpoint
        .route("/notes/search", get(search))
        // Storage for selected search hits
        .route("/notes/search/latest", get(kv_get).post(kv_set))
        // Search API endpoint
        .route("/notes/index", post(index_notes))
        // Static server of assets in ./web-ui
        .nest_service("/", serve_dir.clone())
        .layer(TraceLayer::new_for_http())
        .layer(cors)
        .with_state(Arc::clone(&shared_state));

    let listener = tokio::net::TcpListener::bind(format!("{}:{}", host, port))
        .await
        .unwrap();

    println!("Server started. Listening on {}", listener.local_addr().unwrap());
    axum::serve(listener, app).await.unwrap();
}

#[tokio::main]
async fn main() -> tantivy::Result<()> {
    let args = Args::parse();

    let schema = note_schema();
    let index_path = tantivy::directory::MmapDirectory::open("./.index")?;
    let idx = Index::open_or_create(index_path, schema.clone())?;

    if let Some(notes_path) = args.path {
        let mut index_writer: IndexWriter = idx.writer(50_000_000)?;

        for note in notes(&notes_path) {
            let _ = index_note(&mut index_writer, &schema, note);
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

    if args.serve {
        if args.init {
            // Clone the notes repo and index it
            let repo_url = env::var("INDEXER_NOTES_REPO_URL").expect("Missing env var INDEXER_NOTES_REPO_URL");
            let deploy_key_path = env::var("INDEXER_NOTES_DEPLOY_KEY_PATH").expect("Missing env var INDEXER_NOTES_REPO_URL");
            maybe_clone_repo(repo_url, deploy_key_path);
            let _res = index_notes().await;
        }

        serve(args.host, args.port).await;
    }

    Ok(())
}
