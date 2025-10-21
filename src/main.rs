use std::fs;
use std::path::PathBuf;

use clap::Parser;

use tantivy::collector::TopDocs;
use tantivy::query::QueryParser;
use tantivy::schema::*;
use tantivy::{doc, Index, IndexWriter, ReloadPolicy};

use orgize::ParseConfig;

fn note_schema() -> Schema {
    let mut schema_builder = Schema::builder();
    schema_builder.add_text_field("id", TEXT | STORED);
    schema_builder.add_text_field("title", TEXT | STORED);
    schema_builder.add_text_field("tags", TEXT | STORED);
    schema_builder.add_text_field("body", TEXT);
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

    // Parse the file from the path
    let content = fs::read_to_string(&path)?;
    let config = ParseConfig {
        ..Default::default()
    };
    let p = config.parse(&content);

    let props = p.document().properties().expect("Missing property drawer");
    let id_value = props.get("ID").expect("Missing org-id").to_string();
    let title_value = p.title().expect("No title found");
    let body_value = p.document().raw();
    let tags_all: Vec<String> = p.keywords().map(|s| s.raw()).collect();
    let tags_value = tags_all.join(",");

    // TODO: Replace this macro with the builder so that we can add
    // multiple keywords
    index_writer.add_document(doc!(
        id => id_value,
        title => title_value,
        body => body_value,
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


/// Simple program to greet a person
#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    /// Path to notes to index
    #[arg(short, long)]
    path: Option<String>,

    /// Search notes with query
    #[arg(short, long)]
    query: Option<String>,
}


fn main() -> tantivy::Result<()> {
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

    Ok(())
}
