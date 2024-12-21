use std::hash::{Hash, Hasher};

use super::schema::note_schema;
use super::source::notes;
use crate::export::PlainTextExport;
use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};
use orgize::rowan::ast::AstNode;
use orgize::ParseConfig;
use rusqlite::{Connection, Result};
use std::fs;
use std::hash::DefaultHasher;
use std::path::PathBuf;
use tantivy::schema::*;
use tantivy::{doc, Index, IndexWriter};
use text_splitter::{ChunkConfig, TextSplitter};
use tiktoken_rs::{cl100k_base, CoreBPE};
use zerocopy::AsBytes;

#[derive(Debug)]
struct Task {
    id: String,
    title: String,
    body: String,
    status: String,
    tags: Option<String>,
    #[allow(dead_code)]
    scheduled: Option<String>,
    #[allow(dead_code)]
    deadline: Option<String>,
}

struct Note {
    id: String,
    title: String,
    body: String,
    tags: Option<String>,
    tasks: Vec<Task>,
}

/// Parse the content into a `Note`
fn parse_note(content: &str) -> Note {
    let config = ParseConfig {
        todo_keywords: (
            vec!["TODO".to_string(), "WAITING".to_string()],
            vec![
                "DONE".to_string(),
                "CANCELED".to_string(),
                "SOMEDAY".to_string(),
            ],
        ),
        ..Default::default()
    };
    let p = config.parse(content);

    let props = p.document().properties().expect(
        "Missing property
drawer",
    );
    let id = props.get("ID").expect("Missing org-id").to_string();
    let title = p.title().expect("No title found");

    // TODO: Remove the title and the tasks when indexing the body so it's
    // not duplicated
    // let title_text_range = org_doc.first_headline()?.text_range();
    // p.replace_range(title_text_range, "");
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
        None
    } else {
        Some(filetags[0].to_owned().join(","))
    };

    // Collect all of the tasks in the note file
    let tasks: Vec<Task> = p
        .document()
        .headlines()
        .filter_map(|i| -> Option<Task> {
            if let Some(status) = i.todo_keyword().map(|j| j.to_string()) {
                let task_title = i.title_raw().trim().to_string();

                // Tasks sometimes don't have an org-id. These tasks are ignored.
                let mut hasher = DefaultHasher::new();
                task_title.hash(&mut hasher);
                let default_id = hasher.finish().to_string();

                // Note: Can't use a question mark operator as that
                // will cause an early return rather than handling the
                // case where properties don't exist
                let task_properties = i.properties();
                let id = if let Some(task_props) = task_properties {
                    // Properties might exist but the ID might be missing
                    task_props.get("ID").map(|j| j.to_string()).unwrap_or(default_id)
                } else {
                    default_id
                };

                // Extract note body into markdown format This is
                // useful since LLMs are typically tune for markdown
                let mut plain_text = PlainTextExport::default();
                plain_text.render(i.syntax());
                let task_body = plain_text.finish();

                let tag_string = i
                    .tags()
                    .map(|j| j.to_string())
                    .collect::<Vec<String>>()
                    .join(",");
                let tags = if tag_string.is_empty() {
                    None
                } else {
                    Some(tag_string)
                };

                let mut scheduled = None;
                let mut deadline = None;
                if let Some(planning) = i.planning() {
                    scheduled = planning.scheduled().map(|t| {
                        format!(
                            "{}-{}-{}",
                            t.year_start().unwrap(),
                            t.month_start().unwrap(),
                            t.day_start().unwrap()
                        )
                    });
                    deadline = planning.deadline().map(|t| {
                        format!(
                            "{}-{}-{}",
                            t.year_start().unwrap(),
                            t.month_start().unwrap(),
                            t.day_start().unwrap()
                        )
                    });
                }

                let task = Task {
                    id,
                    title: task_title,
                    body: task_body,
                    tags,
                    status,
                    scheduled,
                    deadline,
                };

                tracing::debug!("Found task {} {}", task.status, task.title);

                return Some(task);
            }
            None
        })
        .collect();

    Note {
        id,
        title,
        body,
        tags,
        tasks,
    }
}

enum DocType {
    Note,
    Task,
}

impl DocType {
    fn to_str(&self) -> &'static str {
        match self {
            DocType::Note => "note",
            DocType::Task => "task",
        }
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
    let r#type = schema.get_field("type")?;
    let title = schema.get_field("title")?;
    let body = schema.get_field("body")?;
    let tags = schema.get_field("tags")?;
    let status = schema.get_field("status")?;
    let file_name = schema.get_field("file_name")?;

    // Parse the file from the path
    let content = fs::read_to_string(&path)?;
    let file_name_value = path.file_name().unwrap().to_string_lossy().into_owned();
    let note_type = DocType::Note.to_str();
    let Note {
        id: note_id,
        title: note_title,
        body: note_body,
        tags: note_tags,
        tasks: note_tasks,
    } = parse_note(&content);

    let mut doc = doc!(
        id => note_id,
        r#type => note_type,
        title => note_title,
        body => note_body,
        file_name => file_name_value.clone(),
    );

    // This needs to be done outside of the `doc!` macro
    if let Some(tag_list) = note_tags {
        doc.add_text(tags, tag_list);
    }
    index_writer.add_document(doc)?;

    // Index each task
    for t in note_tasks.into_iter() {
        let task_type = DocType::Task.to_str();
        let mut doc = doc!(
            id => t.id,
            r#type => task_type,
            title => t.title,
            body => t.body,
            status => t.status,
            file_name => file_name_value.clone(),
        );
        if let Some(tag_list) = t.tags {
            doc.add_text(tags, tag_list);
        }
        index_writer.add_document(doc)?;
    }

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

pub fn index_note_vector(
    db: &mut Connection,
    embeddings_model: &TextEmbedding,
    splitter: &TextSplitter<CoreBPE>,
    note_path: &str,
) -> Result<()> {
    // Generate embeddings and store it in the DB
    let mut note_meta_stmt = db.prepare(
        "REPLACE INTO note_meta(id, file_name, title, tags, body) VALUES (?, ?, ?, ?, ?)",
    )?;
    let mut embedding_stmt =
        db.prepare("INSERT OR REPLACE INTO vec_items(note_meta_id, embedding) VALUES (?, ?)")?;
    let mut embedding_update_stmt =
        db.prepare("UPDATE vec_items set embedding = ? WHERE note_meta_id = ?")?;

    tracing::debug!("Vector indexing note: {}", &note_path);

    let content = fs::read_to_string(note_path).unwrap();
    let note = parse_note(&content);

    // Update the note meta table
    note_meta_stmt
        // TODO: Don't hardcode the note path, save the file name instead
        // TODO: Add task type and status
        .execute(rusqlite::params![
            note.id, note_path, note.title, note.tags, note.body
        ])
        .expect("Note meta upsert failed");

    let embeddings: Vec<Vec<Vec<f32>>> = splitter
        .chunks(&content)
        .map(|chunk| {
            embeddings_model
                .embed(vec![chunk], None)
                .expect("Failed to generate embeddings")
        })
        .collect();

    for embedding in embeddings.concat() {
        // Upserts are not currently supported by sqlite for
        // virtual tables like the vector embeddings table so this
        // attempts to insert a new row and then falls back to an
        // update statement.
        embedding_stmt
            .execute(rusqlite::params![note.id, embedding.as_bytes()])
            .unwrap_or_else(|_| {
                embedding_update_stmt
                    .execute(rusqlite::params![embedding.as_bytes(), note.id])
                    .expect("Update failed")
            });
    }

    Ok(())
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
    for p in notes(notes_path).iter() {
        let file_name = p.to_str().unwrap();
        index_note_vector(db, &embeddings_model, &splitter, file_name)?;
    }

    Ok(())
}
