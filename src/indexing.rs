use super::schema::note_schema;
use super::source::notes;
use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};
use orgize::ParseConfig;
use rusqlite::{Connection, Result};
use std::fs;
use std::path::PathBuf;
use tantivy::schema::*;
use tantivy::{doc, Index, IndexWriter};
use text_splitter::{ChunkConfig, TextSplitter};
use tiktoken_rs::cl100k_base;
use zerocopy::AsBytes;

struct Note {
    id: String,
    title: String,
    body: String,
    tags: String,
}

/// Parse the content into a `Note`
fn parse_note(content: &str) -> Note {
    let config = ParseConfig {
        ..Default::default()
    };
    let p = config.parse(content);

    let props = p.document().properties().expect(
        "Missing property
drawer",
    );
    let id = props.get("ID").expect("Missing org-id").to_string();
    let title = p.title().expect("No title found");
    let body = p.document().raw();
    let filetags: Vec<Vec<String>> = p
        .keywords()
        .filter_map(|k| match k.key().to_string().as_str() {
            "FILETAGS" => Some(
                k.value()
                    .to_string()
                    .trim()
                    .split(" ")
                    .map(|s| s.to_string())
                    .collect(),
            ),
            _ => None,
        })
        .collect();

    // For now, tags are a comma separated string which should
    // allow it to still be searchable
    let tags = if filetags.is_empty() {
        String::new()
    } else {
        filetags[0].to_owned().join(",")
    };

    Note {
        id,
        title,
        body,
        tags,
    }
}

// There is no such thing as updates in tantivy so this function will
// produce duplicates if called repeatedly
pub fn index_note(
    index_writer: &mut IndexWriter,
    schema: &Schema,
    path: PathBuf,
) -> tantivy::Result<()> {
    tracing::debug!("Indexing note: {}", &path.display());

    let id = schema.get_field("id")?;
    let title = schema.get_field("title")?;
    let body = schema.get_field("body")?;
    let tags = schema.get_field("tags")?;
    let file_name = schema.get_field("file_name")?;

    // Parse the file from the path
    let content = fs::read_to_string(&path)?;
    let file_name_value = path.file_name().unwrap().to_string_lossy().into_owned();
    let note = parse_note(&content);

    index_writer.add_document(doc!(
        id => note.id,
        title => note.title,
        body => note.body,
        file_name => file_name_value,
        tags => note.tags,
    ))?;

    Ok(())
}

pub fn index_notes_all(index_path: &str, notes_path: &str) {
    fs::remove_dir_all(index_path).expect("Failed to remove index directory");
    fs::create_dir(index_path).expect("Failed to recreate index directory");

    let index_path = tantivy::directory::MmapDirectory::open(index_path).expect("Index not found");
    let schema = note_schema();
    let idx =
        Index::open_or_create(index_path, schema.clone()).expect("Unable to open or create index");
    let mut index_writer: IndexWriter = idx
        .writer(50_000_000)
        .expect("Index writer failed to initialize");

    for note in notes(notes_path) {
        let _ = index_note(&mut index_writer, &schema, note);
    }

    index_writer.commit().expect("Index write failed");
}

/// Index each note's embeddings
/// Target model has N tokens or roughly a M sized context window
///
/// Algorithm:
/// 1. If the note text is less than N tokens, embed the whole thing
/// 2. Otherwise, split the text into N tokens
/// 3. Calculate the embeddings for each chunk
/// 4. Store the embedding vector in the sqlite database
/// 5. Include metadata about the source of the chunk for further
///    retrieval and to avoid duplicating rows
pub fn index_notes_vector_all(db: &mut Connection, notes_path: &str) -> Result<()> {
    let embeddings_model = TextEmbedding::try_new(
        InitOptions::new(EmbeddingModel::BGESmallENV15).with_show_download_progress(true),
    )
    .unwrap();

    let tokenizer = cl100k_base().unwrap();
    // Targeting Llama 3.2 with a context window of 128k tokens means
    // we can stuff around 100 documents
    let max_tokens = 1280;
    let splitter = TextSplitter::new(ChunkConfig::new(max_tokens).with_sizer(tokenizer));

    // Generate embeddings and store it in the DB
    let mut note_meta_stmt = db.prepare(
        "REPLACE INTO note_meta(id, file_name, title, tags, body) VALUES (?, ?, ?, ?, ?)",
    )?;
    let mut embedding_stmt =
        db.prepare("INSERT OR REPLACE INTO vec_items(note_meta_id, embedding) VALUES (?, ?)")?;
    let mut embedding_update_stmt =
        db.prepare("UPDATE vec_items set embedding = ? WHERE note_meta_id = ?")?;
    for p in notes(notes_path).iter() {
        let content = fs::read_to_string(p).unwrap();
        let note = parse_note(&content);

        // Note IDs are unique by the filename
        let file_name = p
            .file_name()
            .expect("No file name found")
            .to_str()
            .expect("Failed to convert file name to string");

        // Update the note meta table
        note_meta_stmt
            .execute(rusqlite::params![
                note.id, file_name, note.title, note.tags, note.body
            ])
            .expect("Note meta upsert failed");

        // Read the file content into a String
        let content = fs::read_to_string(p)
            .unwrap_or_else(|_| panic!("Failed to read note {}", &p.display()));

        // Assume that chunks returns an iterator of &str
        let mut accum = Vec::new();
        for chunk in splitter.chunks(content.as_str()) {
            // Convert the &str chunk into an owned String
            accum.push(chunk.to_string());
        }
        tracing::debug!("Generating embeddings for note {}", &p.display());
        let items = embeddings_model
            .embed(accum, None)
            .expect("Failed to generate embeddings");
        for item in items {
            // Upserts are not currently supported by sqlite for
            // virtual tables like the vector embeddings table so this
            // attempts to insert a new row and then falls back to an
            // update statement.
            embedding_stmt
                .execute(rusqlite::params![note.id, item.as_bytes()])
                .unwrap_or_else(|_| {
                    embedding_update_stmt
                        .execute(rusqlite::params![item.as_bytes(), note.id])
                        .expect("Update failed")
                });
        }
    }

    Ok(())
}
