use regex::Regex;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};
use orgize::rowan::ast::AstNode;
use tantivy::schema::*;
use tantivy::{Index, IndexWriter, doc};
use text_splitter::{ChunkConfig, TextSplitter};
use tiktoken_rs::{CoreBPE, cl100k_base};
use tokio::fs;
use tokio_rusqlite::Connection;
use zerocopy::IntoBytes;

use super::export::MarkdownExport;
use super::fts::schema::note_schema;
use super::source::{note_filter, notes};
use crate::ai::chat::db::{
    find_chat_session_by_id, get_non_background_sessions, session_has_background_tag,
};
use crate::core::fastembed_cache_dir;
use crate::core::orgmode::{is_archive_file, ORG_ARCHIVE_SUFFIX};
use crate::openai::{Message, Role};

#[derive(Debug, Clone)]
struct Task {
    id: String,
    title: String,
    category: String,
    body: String,
    status: String,
    tags: Option<String>,
    scheduled: Option<String>,
    deadline: Option<String>,
    closed: Option<String>,
}

#[derive(Debug, Clone)]
struct Meeting {
    id: String,
    title: String,
    category: String,
    body: String,
    tags: Option<String>,
    date: String,
}

#[derive(Debug, Clone)]
struct Heading {
    id: String,
    title: String,
    category: String,
    body: String,
    tags: Option<String>,
}

#[derive(Debug, Clone)]
struct Note {
    id: String,
    title: String,
    category: String,
    body: String,
    tags: Option<String>,
    tasks: Vec<Task>,
    meetings: Vec<Meeting>,
    headings: Vec<Heading>,
}

/// Parse the content into a `Note`.
///
/// `file_name` is the note's path relative to the notes root. `source_id` is
/// the document-level `:ID:` of the note an archive file was archived from
/// (e.g. `work.org` for `work.org_archive`); it is used only when the content
/// itself carries no `:ID:`. Resolution is the caller's responsibility so this
/// stays free of file I/O.
fn parse_note(file_name: &str, source_id: Option<&str>, content: &str) -> Result<Note> {
    let config = crate::org::todo_keywords_config();
    let p = config.parse(content);
    let d = p.document();

    let is_archive = is_archive_file(file_name);

    // Org archive files (`work.org_archive`) carry no document-level `:ID:`;
    // fall back to the source file's ID so archived content groups with the
    // note it was archived from.
    let note_id = document_id(&d).or_else(|| source_id.map(str::to_owned)).with_context(|| {
        format!(
            "Missing org-id for note '{file_name}'{}",
            if is_archive {
                " and unable to resolve the source file's ID"
            } else {
                ""
            }
        )
    })?;
    // Archive files carry no `#+TITLE:`; fall back to the source filename stem
    // (e.g. `work` from `projects/work.org_archive`) so parsing succeeds.
    let note_title = p
        .title()
        .or_else(|| {
            is_archive.then(|| {
                let stem = file_name.rsplit('/').next().unwrap_or(file_name);
                stem.strip_suffix(ORG_ARCHIVE_SUFFIX).unwrap_or(stem).to_string()
            })
        })
        .context("No title found")?;
    let note_category = p
        .keywords()
        .filter_map(|k| match k.key().to_string().as_str() {
            "CATEGORY" => Some(k.value().to_string()),
            _ => None,
        })
        .collect::<Vec<String>>()
        .first()
        .unwrap_or(&note_title.to_lowercase().replace(" ", "_"))
        .trim()
        .to_owned();

    let mut note_body_md = MarkdownExport::default();
    note_body_md.render(d.syntax());
    let note_body = note_body_md.finish();

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
    let note_tags = if filetags.is_empty() {
        None
    } else {
        Some(filetags[0].to_owned().join(","))
    };

    let mut tasks: Vec<Task> = Vec::new();
    let mut meetings: Vec<Meeting> = Vec::new();
    let mut headings: Vec<Heading> = Vec::new();

    let date_regex = Regex::new(r"(\d{4})-(\d{2})-(\d{2})").unwrap();
    for i in p.document().headlines() {
        let tag_string = i
            .tags()
            .map(|j| j.to_string())
            .collect::<Vec<String>>()
            .join(",");
        // Mark every entry indexed from an archive file as archived so it can
        // be distinguished from live content in search and listings.
        let tags = match (tag_string.is_empty(), is_archive) {
            (true, false) => None,
            (true, true) => Some("archive".to_string()),
            (false, false) => Some(tag_string.clone()),
            (false, true) => Some(format!("{tag_string},archive")),
        };
        let title = i.title_raw().trim().to_string();

        let id = match i
            .properties()
            .and_then(|p| p.get("ID"))
        {
            Some(id) => id.to_string(),
            None => {
                tracing::warn!(
                    "Skipping heading with missing ID: '{}'",
                    title
                );
                continue;
            }
        };

        let mut plain_text = MarkdownExport::default();
        plain_text.render(i.syntax());
        let body = plain_text.finish();

        // Handle meetings
        if tag_string.contains("meeting") {
            // Parse it from the headline to get the meeting date
            // since this is always added as part of the org-mode
            // capture template
            let mut dates = vec![];
            for (_, [year, month, day]) in date_regex.captures_iter(&title).map(|c| c.extract()) {
                dates.push(format!("{}-{}-{}", year, month, day));
            }
            let date = dates.first().map(|d| d.to_string()).unwrap_or_else(|| {
                println!(
                    "Meeting missing date! {}, file: {}",
                    title.clone(),
                    note_title.clone()
                );
                String::from("2000-01-01")
            });

            let meeting = Meeting {
                id,
                title,
                category: note_category.clone(),
                body,
                tags,
                date,
            };
            meetings.push(meeting);
            continue;
        }

        // Handle tasks
        if let Some(status) = i.todo_keyword().map(|j| j.to_string().to_lowercase()) {
            let mut scheduled = None;
            let mut deadline = None;
            let mut closed = None;
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
                closed = planning.closed().map(|t| {
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
                title,
                category: note_category.clone(),
                body,
                tags,
                status,
                scheduled,
                deadline,
                closed,
            };
            tasks.push(task);
            continue;
        }

        // Handle all other headings
        let heading = Heading {
            id,
            title,
            category: note_category.clone(),
            body,
            tags,
        };
        headings.push(heading);
    }

    Ok(Note {
        id: note_id,
        title: note_title,
        category: note_category,
        body: note_body,
        tags: note_tags,
        tasks,
        meetings,
        headings,
    })
}

/// For `.org_archive` files (Org mode's archive location), resolve the
/// document-level `:ID:` of the source file the entries were archived from
/// (e.g. `work.org_archive` -> `work.org`). Returns `None` when the file isn't
/// an archive or the source's ID can't be determined.
fn archive_source_id(notes_dir_path: &str, file_name: &str) -> Option<String> {
    let source_name = format!("{}.org", file_name.strip_suffix(ORG_ARCHIVE_SUFFIX)?);
    let source_path = if file_name.starts_with('/') {
        source_name
    } else {
        format!("{notes_dir_path}/{source_name}")
    };
    let content = std::fs::read_to_string(&source_path).ok()?;
    let config = crate::org::todo_keywords_config();
    document_id(&config.parse(&content).document())
}

/// The document-level `:ID:` property of a parsed Org document.
fn document_id(d: &orgize::ast::Document) -> Option<String> {
    d.properties()
        .and_then(|props| props.get("ID").map(|id| id.to_string()))
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DocType {
    Note,
    Task,
    Meeting,
    Heading,
    Chat,
}

impl DocType {
    fn to_str(&self) -> &'static str {
        match self {
            DocType::Note => "note",
            DocType::Task => "task",
            DocType::Meeting => "meeting",
            DocType::Heading => "heading",
            DocType::Chat => "chat",
        }
    }
}

impl FromStr for DocType {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // Re‑use Serde's deserializer
        Ok(serde_json::from_str(&format!("\"{s}\""))?)
    }
}

// Deletes and then writes the document to the index
fn index_note_full_text(
    index_writer: &mut IndexWriter,
    schema: &Schema,
    file_name_value: &str,
    note: &Note,
) -> tantivy::Result<()> {
    let is_archive = is_archive_file(file_name_value);

    let id = schema.get_field("id")?;
    let r#type = schema.get_field("type")?;
    let title = schema.get_field("title")?;
    let category = schema.get_field("category")?;
    let body = schema.get_field("body")?;
    let tags = schema.get_field("tags")?;
    let status = schema.get_field("status")?;
    let file_name = schema.get_field("file_name")?;

    let note_type = DocType::Note.to_str();

    let Note {
        id: note_id,
        title: note_title,
        category: note_category,
        body: note_body,
        tags: note_tags,
        tasks: note_tasks,
        meetings: note_meetings,
        headings: note_headings,
    } = note;

    // Archive files share the source note's ID; skip the note-level doc (and
    // its delete, which would otherwise remove the source's doc) so archives
    // don't clobber the source note. Their entries are indexed below.
    if !is_archive {
        let term_id = Term::from_field_text(id, &note.id);
        index_writer.delete_term(term_id);

        let mut doc = doc!(
            id => note_id.as_str(),
            r#type => note_type,
            title => note_title.as_str(),
            category => note_category.clone(),
            body => note_body.as_str(),
            file_name => file_name_value,
        );

        // This needs to be done outside of the `doc!` macro
        if let Some(tag_list) = note_tags {
            doc.add_text(tags, tag_list);
        }
        index_writer.add_document(doc)?;
    }

    // Index each meeting
    for m in note_meetings.iter() {
        // Delete first to get upsert behavior
        let meeting_term_id = Term::from_field_text(id, &m.id);
        index_writer.delete_term(meeting_term_id);

        let meeting_type = DocType::Meeting.to_str();
        let mut doc = doc!(
            id => m.id.clone(),
            r#type => meeting_type,
            title => m.title.clone(),
            category => note_category.clone(),
            body => m.body.clone(),
            file_name => file_name_value,
        );
        if let Some(tag_list) = m.tags.clone() {
            doc.add_text(tags, tag_list);
        }
        index_writer.add_document(doc)?;
    }

    // Index each task
    for t in note_tasks.iter() {
        // Delete first to get upsert behavior
        let task_term_id = Term::from_field_text(id, &t.id);
        index_writer.delete_term(task_term_id);

        let task_type = DocType::Task.to_str();
        let mut doc = doc!(
            id => t.id.clone(),
            r#type => task_type,
            title => t.title.clone(),
            category => note_category.clone(),
            body => t.body.clone(),
            status => t.status.clone(),
            file_name => file_name_value,
        );
        if let Some(tag_list) = t.tags.clone() {
            doc.add_text(tags, tag_list);
        }
        index_writer.add_document(doc)?;
    }

    // Index each heading
    for h in note_headings.iter() {
        // Delete first to get upsert behavior
        let heading_term_id = Term::from_field_text(id, &h.id);
        index_writer.delete_term(heading_term_id);

        let heading_type = DocType::Heading.to_str();
        let mut doc = doc!(
            id => h.id.clone(),
            r#type => heading_type,
            title => h.title.clone(),
            category => note_category.clone(),
            body => h.body.clone(),
            file_name => file_name_value,
        );
        if let Some(tag_list) = h.tags.clone() {
            doc.add_text(tags, tag_list);
        }
        index_writer.add_document(doc)?;
    }

    Ok(())
}

/// Generate embeddings for the note body chunks.
/// Target model has N tokens or roughly a M sized context window
///
/// Algorithm:
/// 1. If the note text is less than N tokens, embed the whole thing
/// 2. Otherwise, split the text into N tokens
/// 3. Calculate the embeddings for each chunk
fn generate_embeddings(
    embeddings_model: &mut TextEmbedding,
    splitter: &TextSplitter<CoreBPE>,
    note_body: &str,
) -> Vec<Vec<f32>> {
    splitter
        .chunks(note_body)
        .flat_map(|chunk| {
            embeddings_model
                .embed(vec![chunk], None)
                .expect("Failed to generate embeddings")
        })
        .collect()
}

/// Store the embedding vector in the sqlite database.
///
/// Upserts are not currently supported by sqlite for virtual tables
/// like the vector embeddings table so this attempts to insert a new
/// row and then falls back to an update statement.
fn store_embeddings_in_db(
    db: &mut rusqlite::Connection,
    note_id: &str,
    embeddings: Vec<Vec<f32>>,
) -> Result<()> {
    let mut embedding_stmt =
        db.prepare("INSERT OR REPLACE INTO vec_items(note_meta_id, embedding) VALUES (?, ?)")?;
    let mut embedding_update_stmt =
        db.prepare("UPDATE vec_items set embedding = ? WHERE note_meta_id = ?")?;

    for embedding in embeddings {
        embedding_stmt
            .execute(tokio_rusqlite::params![note_id, embedding.as_bytes()])
            .unwrap_or_else(|_| {
                embedding_update_stmt
                    .execute(tokio_rusqlite::params![embedding.as_bytes(), note_id])
                    .expect("Update failed")
            });
    }

    Ok(())
}

/// Upsert meta information about the note. This is the canonical data
/// representing the note that all other indexes refer to by ID. It
/// should always be safe to query an index and then lookup the
/// note(s) by ID.
fn index_note_meta(db: &mut rusqlite::Connection, file_name: &str, note: &Note) -> Result<()> {
    let is_archive = is_archive_file(file_name);
    let mut note_meta_stmt = db.prepare(
        "REPLACE INTO note_meta(id, type, category, file_name, title, tags, body) VALUES (?, ?, ?, ?, ?, ?, ?)",
    )?;

    // Archive files share the source note's ID, so skip the note-level row to
    // avoid clobbering the source's row and to keep archives out of the
    // project listing. Only their meetings/headings/tasks are indexed.
    if !is_archive {
        note_meta_stmt
            .execute(tokio_rusqlite::params![
                note.id,
                "note",
                note.category,
                file_name,
                note.title,
                note.tags,
                note.body
            ])
            .expect("Note meta upsert failed");
    }

    let mut meeting_meta_stmt = db.prepare(
        "REPLACE INTO note_meta(id, type, category, file_name, title, tags, body, date) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )?;

    let mut heading_meta_stmt = db.prepare(
        "REPLACE INTO note_meta(id, type, category, file_name, title, tags, body) VALUES (?, ?, ?, ?, ?, ?, ?)",
    )?;

    let mut task_meta_stmt = db.prepare(
        "REPLACE INTO note_meta(id, type, category, file_name, title, tags, body, status, scheduled, deadline, closed) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )?;

    for m in note.meetings.iter() {
        meeting_meta_stmt
            .execute(tokio_rusqlite::params![
                m.id, "meeting", m.category, file_name, m.title, m.tags, m.body, m.date
            ])
            .expect("Note meta upsert failed for meeting");
    }

    for t in note.headings.iter() {
        heading_meta_stmt
            .execute(tokio_rusqlite::params![
                t.id, "heading", t.category, file_name, t.title, t.tags, t.body
            ])
            .expect("Note meta upsert failed for heading");
    }

    for t in note.tasks.iter() {
        task_meta_stmt
            .execute(tokio_rusqlite::params![
                t.id,
                "task",
                t.category,
                file_name,
                t.title,
                t.tags,
                t.body,
                t.status,
                t.scheduled,
                t.deadline,
                t.closed
            ])
            .expect("Note meta upsert failed for task");
    }

    Ok(())
}

/// This is the primary function to call for indexing. Coordinates
/// saving notes in the db, full text search index, and vector
/// storage. This needs to be done in one to avoid parsing org mode
/// notes many times for each index.
pub async fn index_all(
    db: &Connection,
    index_dir_path: &str,
    notes_dir_path: &str,
    index_full_text: bool,
    index_vector: bool,
    paths: Option<Vec<PathBuf>>,
) -> Result<()> {
    let embeddings_model = Arc::new(Mutex::new(
        TextEmbedding::try_new(
            InitOptions::new(EmbeddingModel::BGESmallENV15)
                .with_show_download_progress(true)
                .with_cache_dir(fastembed_cache_dir()),
        )
        .unwrap(),
    ));
    let tokenizer = cl100k_base().unwrap();
    let max_tokens = 1280;
    let splitter = Arc::new(TextSplitter::new(
        ChunkConfig::new(max_tokens).with_sizer(tokenizer),
    ));

    let note_paths: Vec<PathBuf> = if let Some(path_bufs) = paths {
        note_filter(notes_dir_path, path_bufs).await
    } else {
        notes(notes_dir_path).await
    };

    let index_path =
        tantivy::directory::MmapDirectory::open(index_dir_path).expect("Index not found");
    let schema = note_schema();
    let idx =
        Index::open_or_create(index_path, schema.clone()).expect("Unable to open or create index");
    let mut index_writer: IndexWriter = idx
        .writer(50_000_000)
        .expect("Index writer failed to initialize");

    // Collect all notes for full-text indexing (done in a single blocking task later)
    let mut full_text_notes: Vec<(String, Note)> = Vec::new();

    for p in note_paths.iter() {
        tracing::debug!("Indexing note: {:?}", p);

        let content = match fs::read_to_string(&p).await {
            Ok(content) => content,
            Err(e) => {
                tracing::warn!("Skipping note {:?}: {}", p, e);
                continue;
            }
        };
        // Arc the shared items so that it can be safely passed to the
        // async closure. Store the path relative to the notes root so
        // files in subdirectories (notes/, projects/) are locatable.
        let file_name = Arc::new(
            p.strip_prefix(notes_dir_path)
                .unwrap_or_else(|_| p.as_path())
                .to_str()
                .unwrap_or_default()
                .to_owned(),
        );
        let source_id = if is_archive_file(file_name.as_str()) {
            archive_source_id(notes_dir_path, file_name.as_str())
        } else {
            None
        };
        let note = match parse_note(file_name.as_str(), source_id.as_deref(), &content) {
            Ok(note) => Arc::new(note),
            Err(e) => {
                tracing::warn!("Skipping note {:?}: {}", p, e);
                continue;
            }
        };
        let note_id = note.id.clone();
        let note_body = note.body.clone();
        let embeddings_model = Arc::clone(&embeddings_model);
        let splitter = Arc::clone(&splitter);
        let note_inner = Arc::clone(&note);
        let file_name_inner = Arc::clone(&file_name);

        // First, store the note meta in the database
        db.call(move |conn| {
            index_note_meta(conn, &file_name_inner, &note_inner)
                .expect("Upserting note meta failed");
            Ok(())
        })
        .await
        .expect("DB work failed");

        // If vector indexing is enabled, generate embeddings asynchronously
        // and then store them in the database
        if index_vector {
            // Spawn a blocking task for the CPU-intensive embedding generation
            let embeddings = tokio::task::spawn_blocking(move || {
                generate_embeddings(&mut embeddings_model.lock().unwrap(), &splitter, &note_body)
            })
            .await
            .expect("Embedding generation task failed");

            // Store the pre-generated embeddings in the database
            db.call(move |conn| {
                store_embeddings_in_db(conn, &note_id, embeddings)
                    .expect("Storing embeddings in DB failed");
                Ok(())
            })
            .await
            .expect("DB work failed for embeddings");
        }

        // Collect note for batch full-text indexing later
        if index_full_text {
            full_text_notes.push(((*file_name).clone(), (*note).clone()));
        }
    }

    // Perform all full-text indexing in a single blocking task
    if index_full_text {
        tokio::task::spawn_blocking(move || {
            for (file_name, note) in full_text_notes.iter() {
                index_note_full_text(&mut index_writer, &schema, file_name, note)
                    .expect("Updating full text search failed");
            }

            // Commit the index writer
            index_writer
                .commit()
                .expect("Full text search index failed to commit");
        })
        .await
        .expect("Full-text indexing task failed");
    }

    Ok(())
}

/// Re-index a single org file into the SQLite metadata and full-text indexes.
///
/// Used after task create/update so the changes are immediately queryable
/// instead of waiting for a full `hq index`. The index directory must already
/// exist (created by init / `hq index`); we intentionally do not create it.
pub async fn index_single_file(
    db: &Connection,
    index_dir_path: &str,
    notes_path: &str,
    file_path: PathBuf,
) -> Result<()> {
    let file_name = file_path
        .strip_prefix(notes_path)
        .unwrap_or_else(|_| file_path.as_path())
        .to_str()
        .unwrap_or_default()
        .to_owned();

    let content = fs::read_to_string(&file_path)
        .await
        .with_context(|| format!("Failed to read file: {:?}", file_path))?;
    let source_id = if is_archive_file(&file_name) {
        archive_source_id(notes_path, &file_name)
    } else {
        None
    };
    let note = parse_note(&file_name, source_id.as_deref(), &content)?;

    // Index metadata in SQLite (backs tasks list / projects list)
    let file_name_for_db = file_name.clone();
    let note_for_db = note.clone();
    db.call(move |conn| {
        index_note_meta(conn, &file_name_for_db, &note_for_db)
            .expect("Upserting note meta failed");
        Ok(())
    })
    .await
    .expect("DB work failed");

    // Index full text in Tantivy
    let index_path =
        tantivy::directory::MmapDirectory::open(index_dir_path).expect("Index not found");
    let schema = note_schema();
    let idx =
        Index::open_or_create(index_path, schema.clone()).expect("Unable to open or create index");
    let mut index_writer: IndexWriter = idx
        .writer(50_000_000)
        .expect("Index writer failed to initialize");

    index_note_full_text(&mut index_writer, &schema, &file_name, &note)
        .expect("Updating full text search failed");

    index_writer
        .commit()
        .expect("Full text search index failed to commit");

    Ok(())
}

/// Remove a task (or any indexed document) from all indexes by its ID.
/// Used after deleting a task from an org file.
pub async fn remove_task_from_indexes(
    db: &Connection,
    index_dir_path: &str,
    task_id: &str,
) -> Result<()> {
    // Remove from Tantivy full-text index. Best-effort: if the index directory
    // doesn't exist there's nothing to remove, but DB cleanup must still run.
    if let Ok(index_path) = tantivy::directory::MmapDirectory::open(index_dir_path) {
        let schema = note_schema();
        let idx = Index::open_or_create(index_path, schema.clone())
            .expect("Unable to open or create index");
        let mut index_writer: IndexWriter = idx
            .writer(50_000_000)
            .expect("Index writer failed to initialize");

        let id_field = schema.get_field("id").expect("Failed to get 'id' field");
        let term_id = Term::from_field_text(id_field, task_id);
        index_writer.delete_term(term_id);

        index_writer
            .commit()
            .expect("Full text search index failed to commit");
    }

    // Remove from SQLite metadata and vector embeddings
    let task_id_owned = task_id.to_owned();
    db.call(move |conn| {
        conn.execute(
            "DELETE FROM vec_items WHERE note_meta_id = ?",
            [&task_id_owned],
        )
        .ok();
        conn.execute("DELETE FROM note_meta WHERE id = ?", [&task_id_owned])
            .ok();
        Ok(())
    })
    .await
    .expect("DB cleanup failed");

    Ok(())
}

/// Index a single chat message into the full-text search index
pub fn index_chat_message_full_text(
    index_writer: &mut IndexWriter,
    schema: &Schema,
    message_id: &str,
    title: Option<&str>,
    msg: &Message,
) -> anyhow::Result<()> {
    // Skip system and tool messages
    let role = msg.role();
    if *role == Role::System || *role == Role::Tool {
        return Ok(());
    }

    let id = schema.get_field("id").context("Failed to get 'id' field")?;
    let r#type = schema
        .get_field("type")
        .context("Failed to get 'type' field")?;
    let title_field = schema
        .get_field("title")
        .context("Failed to get 'title' field")?;
    let body = schema
        .get_field("body")
        .context("Failed to get 'body' field")?;
    let chat_role = schema
        .get_field("chat_role")
        .context("Failed to get 'chat_role' field")?;

    // Delete existing doc for upsert behavior (use message_id directly)
    let term_id = Term::from_field_text(id, message_id);
    index_writer.delete_term(term_id);

    let chat_type = DocType::Chat.to_str();
    let role_str = role.as_str();

    // Get message content - use empty string if None
    let msg_content = msg.content.as_deref().unwrap_or("");

    let doc = doc!(
        id => message_id,
        r#type => chat_type,
        title_field => title.unwrap_or(""),
        body => msg_content,
        chat_role => role_str,
    );

    index_writer
        .add_document(doc)
        .context("Failed to add document")?;

    Ok(())
}

/// Index all chat messages from non-background sessions
pub async fn index_all_chat_sessions(db: &Connection, index_dir_path: &str) -> Result<()> {
    // Get all non-background sessions
    let sessions = get_non_background_sessions(db).await?;

    if sessions.is_empty() {
        tracing::info!("No non-background chat sessions found to index");
        return Ok(());
    }

    tracing::info!("Indexing {} non-background chat sessions", sessions.len());

    let index_path =
        tantivy::directory::MmapDirectory::open(index_dir_path).expect("Index not found");
    let schema = note_schema();
    let idx =
        Index::open_or_create(index_path, schema.clone()).expect("Unable to open or create index");
    let mut index_writer: IndexWriter = idx
        .writer(50_000_000)
        .expect("Index writer failed to initialize");

    // Collect all messages from all sessions
    for session in &sessions {
        let messages = find_chat_session_by_id(db, &session.id).await?;

        for (message_id, msg) in messages.iter() {
            // Skip system and tool role messages
            let role = msg.role();
            if *role == Role::System || *role == Role::Tool {
                continue;
            }

            if let Err(e) = index_chat_message_full_text(
                &mut index_writer,
                &schema,
                message_id,
                session.title.as_deref(),
                msg,
            ) {
                tracing::error!(
                    "Failed to index chat message {} from session {}: {}",
                    message_id,
                    session.id,
                    e
                );
            }
        }
    }

    // Commit the index writer
    tokio::task::spawn_blocking(move || {
        index_writer
            .commit()
            .expect("Chat full text search index failed to commit");
    })
    .await
    .expect("Chat indexing task failed");

    tracing::info!("Finished indexing chat sessions");

    Ok(())
}

/// Index multiple chat messages in a single task with one writer and commit.
///
/// This avoids the LockFailure error that occurs when multiple IndexWriters
/// try to access the same index simultaneously.
pub async fn index_chat_messages(
    db: &Connection,
    index_dir_path: &str,
    session_id: &str,
    messages: Vec<Message>,
) -> Result<()> {
    // Skip if session has background tag
    let is_background = session_has_background_tag(db, session_id).await?;
    if is_background {
        return Ok(());
    }

    let messages_len = messages.len();

    // Get session title and message IDs for this session
    let session_id_owned = session_id.to_string();
    let (title, message_ids): (Option<String>, Vec<String>) = db
        .call(move |conn| {
            let title: Option<String> = conn
                .query_row(
                    "SELECT title FROM session WHERE id = ?",
                    [&session_id_owned],
                    |row| row.get(0),
                )
                .ok();

            // Get all message IDs for this session (we'll index the new ones)
            let mut stmt = conn.prepare(
                "SELECT id FROM chat_message WHERE session_id = ? ORDER BY rowid DESC LIMIT ?",
            )?;
            let message_ids: Vec<String> = stmt
                .query_map([&session_id_owned, &messages_len.to_string()], |row| {
                    row.get(0)
                })?
                .filter_map(|r| r.ok())
                .collect();

            Ok((title, message_ids))
        })
        .await?;

    // Reverse to get oldest first (to match message order)
    let mut message_ids: Vec<String> = message_ids.into_iter().rev().collect();
    // Pad with placeholder IDs if needed
    while message_ids.len() < messages_len {
        message_ids.push(format!("msg_{}", message_ids.len()));
    }

    // Filter messages to index (skip system and tool)
    let messages_to_index: Vec<(String, Message)> = messages
        .into_iter()
        .filter(|msg| *msg.role() != Role::System && *msg.role() != Role::Tool)
        .zip(message_ids.iter())
        .map(|(msg, id)| (id.clone(), msg))
        .collect();

    if messages_to_index.is_empty() {
        return Ok(());
    }

    // Open the index and create ONE writer for all messages
    let index_path =
        tantivy::directory::MmapDirectory::open(index_dir_path).expect("Index not found");
    let schema = note_schema();
    let idx =
        Index::open_or_create(index_path, schema.clone()).expect("Unable to open or create index");
    let mut index_writer: IndexWriter = idx
        .writer(50_000_000)
        .expect("Index writer failed to initialize");

    // Index all messages using the same writer
    for (message_id, msg) in &messages_to_index {
        index_chat_message_full_text(
            &mut index_writer,
            &schema,
            message_id,
            title.as_deref(),
            msg,
        )?;
    }

    // Commit once after all messages are indexed
    tokio::task::spawn_blocking(move || {
        index_writer
            .commit()
            .expect("Chat messages batch indexing failed to commit");
    })
    .await
    .expect("Chat message batch indexing task failed");

    Ok(())
}

/// Delete all chat messages for a session from the tantivy full-text search
/// index.
///
/// The FTS schema has no `session_id` field — chat messages are indexed by
/// their message UUID (`id`, a STRING field). To delete a session's messages,
/// we first look up all `chat_message.id` values for the session from the
/// database, then call `delete_term` on each one. Must be called BEFORE
/// `delete_chat_session`, while the message rows still exist in the DB.
///
/// Returns early with `Ok(())` if the session has no messages — tantivy
/// has nothing to delete.
pub async fn delete_chat_session_index(
    db: &Connection,
    index_dir_path: &str,
    session_id: &str,
) -> Result<()> {
    let s_id = session_id.to_string();
    let message_ids: Vec<String> = db
        .call(move |conn| {
            let mut stmt = conn.prepare("SELECT id FROM chat_message WHERE session_id = ?")?;
            let ids: Vec<String> = stmt
                .query_map([&s_id], |row| row.get(0))?
                .filter_map(|r| r.ok())
                .collect();
            Ok(ids)
        })
        .await?;

    if message_ids.is_empty() {
        return Ok(());
    }

    let index_path = tantivy::directory::MmapDirectory::open(index_dir_path)
        .context("Failed to open tantivy index directory")?;
    let schema = note_schema();
    let idx = Index::open_or_create(index_path, schema.clone())
        .context("Unable to open or create tantivy index")?;
    let mut index_writer: IndexWriter = idx
        .writer(50_000_000)
        .context("Tantivy index writer failed to initialize")?;

    let id_field = schema
        .get_field("id")
        .context("Failed to get 'id' field from schema")?;
    for message_id in &message_ids {
        let term = Term::from_field_text(id_field, message_id);
        index_writer.delete_term(term);
    }

    tokio::task::spawn_blocking(move || {
        index_writer
            .commit()
            .expect("Tantivy delete for chat session failed to commit");
    })
    .await
    .context("Tantivy delete task failed")?;

    tracing::info!(
        "Deleted {} chat message(s) from tantivy index for session {}",
        message_ids.len(),
        session_id
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    /// Verify that all example .org notes parse successfully.
    ///
    /// Every note must have a document-level property drawer with an :ID:,
    /// and every headline must also have its own property drawer with an :ID:.
    #[test]
    fn test_example_notes_parse_successfully() {
        let examples_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/notes");
        let mut tested = 0;

        for entry in fs::read_dir(&examples_dir).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().map_or(false, |e| e == "org") {
                let content = fs::read_to_string(&path).unwrap();
                let result = parse_note("", None, &content);
                assert!(
                    result.is_ok(),
                    "Failed to parse example note {:?}: {}",
                    path.file_name().unwrap(),
                    result.unwrap_err()
                );
                tested += 1;
            }
        }

        assert!(tested > 0, "No .org files found in examples/notes/");
    }

    /// Parsing a `.org_archive` file resolves the source note's document-level
    /// `:ID:` (so archived content groups with its source note) and tags every
    /// indexed entry with `archive`. The archive itself carries no document-level
    /// `:ID:`, matching Org mode's archive files.
    #[test]
    fn test_parse_archive_resolves_source_id_and_tags() {
        let dir = TempDir::new().unwrap();
        let notes_root = dir.path();

        let projects_dir = notes_root.join("projects");
        fs::create_dir_all(&projects_dir).unwrap();

        fs::write(
            projects_dir.join("work.org"),
            ":PROPERTIES:\n:ID: source-work\n:END:\n#+TITLE: Work\n\n* TODO Live task\n:PROPERTIES:\n:ID: live-task\n:END:\n",
        )
        .unwrap();

        fs::write(
            projects_dir.join("work.org_archive"),
            "Archived entries from file work.org\n\n* DONE Archived task\n:PROPERTIES:\n:ARCHIVE_TIME: 2022-10-16 Sun 09:37\n:ID: archived-task\n:END:\nCLOSED: [2022-10-16 Sun 09:37]\n\n* Archived heading\n:PROPERTIES:\n:ID: archived-heading\n:END:\n",
        )
        .unwrap();

        let file_name = "projects/work.org_archive";
        let archive = fs::read_to_string(projects_dir.join("work.org_archive")).unwrap();
        let source_id = archive_source_id(notes_root.to_str().unwrap(), file_name);
        let note = parse_note(file_name, source_id.as_deref(), &archive).unwrap();

        // Same document-level ID as the source note.
        assert_eq!(note.id, "source-work");

        let mut tagged = 0;
        for t in &note.tasks {
            assert!(
                t.tags.as_deref().unwrap_or("").contains("archive"),
                "task missing archive tag: {:?}",
                t.tags
            );
            tagged += 1;
        }
        for h in &note.headings {
            assert!(
                h.tags.as_deref().unwrap_or("").contains("archive"),
                "heading missing archive tag: {:?}",
                h.tags
            );
            tagged += 1;
        }
        assert!(tagged > 0, "expected at least one tagged archived entry");
    }

    /// ===== Tests for `delete_chat_session_index` =====

    use crate::ai::chat::db::{get_or_create_session, insert_chat_message};
    use crate::ai::chat::models::SessionMode;
    use crate::core::db::{async_db, initialize_db};
    use tantivy::ReloadPolicy;
    use tempfile::TempDir;

    /// Set up a test DB with chat schema initialized, in a temp dir.
    async fn test_db() -> (Connection, TempDir) {
        let dir = TempDir::new().unwrap();
        let vec_db_path = dir.path().to_str().unwrap().to_string();
        let db = async_db(&vec_db_path).await.unwrap();
        db.call(|conn| {
            initialize_db(conn).unwrap();
            Ok(())
        })
        .await
        .unwrap();
        (db, dir)
    }

    /// Count tantivy docs by message ID. Returns the number of hits.
    ///
    /// Forces a reader reload so we see committed deletes; we want to
    /// verify that docs are actually gone after `delete_chat_session_index`
    /// commits, not just queued for deletion.
    fn count_docs_by_id(idx: &Index, message_ids: &[String]) -> usize {
        let reader = idx
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()
            .unwrap();
        reader.reload().unwrap();
        let searcher = reader.searcher();

        let id_field = idx.schema().get_field("id").unwrap();
        let mut total = 0;
        for mid in message_ids {
            let term = tantivy::Term::from_field_text(id_field, mid);
            let query = tantivy::query::TermQuery::new(
                term,
                tantivy::schema::IndexRecordOption::Basic,
            );
            total += searcher
                .search(&query, &tantivy::collector::Count)
                .unwrap();
        }
        total
    }

    /// Set up a chat session with two messages and index them.
    async fn setup_session_with_messages(
        db: &Connection,
        session_id: &str,
    ) -> Vec<Message> {
        get_or_create_session(db, session_id, &[], SessionMode::Chat)
            .await
            .unwrap();
        let msg1 = Message::new(Role::User, "Hello world");
        let msg2 = Message::new(Role::Assistant, "Hi there!");
        insert_chat_message(db, session_id, &msg1).await.unwrap();
        insert_chat_message(db, session_id, &msg2).await.unwrap();

        // Return the messages in insertion order for `index_chat_messages`
        vec![msg1, msg2]
    }

    #[tokio::test]
    async fn test_delete_chat_session_index_removes_messages() {
        let (db, db_dir) = test_db().await;
        // Index directory is a separate temp dir (matches production layout
        // where DB and index live under $HQ_STORAGE_PATH/{db,index})
        let idx_dir = TempDir::new().unwrap();
        let index_dir_path = idx_dir.path().to_str().unwrap().to_string();

        // Set up session and index its messages
        let session_id = "test-session-index-1";
        let messages = setup_session_with_messages(&db, session_id).await;

        // Index the messages — this writes their IDs (from DB) to tantivy
        index_chat_messages(&db, &index_dir_path, session_id, messages)
            .await
            .unwrap();

        // Get the message IDs from DB so we can search for them in tantivy
        let s_id = session_id.to_string();
        let message_ids: Vec<String> = db
            .call(move |conn| {
                let mut stmt =
                    conn.prepare("SELECT id FROM chat_message WHERE session_id = ?")?;
                let ids: Vec<String> = stmt
                    .query_map([&s_id], |row| row.get(0))?
                    .filter_map(|r| r.ok())
                    .collect();
                Ok(ids)
            })
            .await
            .unwrap();
        assert_eq!(message_ids.len(), 2, "expected 2 message IDs from DB");

        // Open the index and verify docs are present
        let idx_path = tantivy::directory::MmapDirectory::open(&index_dir_path).unwrap();
        let idx = Index::open_or_create(idx_path, note_schema()).unwrap();
        // Need to reload reader after the index_chat_messages commit
        let initial_count = count_docs_by_id(&idx, &message_ids);
        // Note: index_chat_messages uses spawn_blocking for commit; reader
        // should see it after reload. Count may be 2 (both messages present).
        assert!(
            initial_count > 0,
            "expected docs in index after indexing, got {initial_count}"
        );

        // Now delete the session's messages from tantivy
        delete_chat_session_index(&db, &index_dir_path, session_id)
            .await
            .unwrap();

        // Verify docs are gone from tantivy
        let final_count = count_docs_by_id(&idx, &message_ids);
        assert_eq!(
            final_count, 0,
            "expected 0 docs after delete, got {final_count}"
        );

        // The DB rows are untouched (delete_chat_session_index doesn't touch DB)
        let remaining: i64 = db
            .call(move |conn| {
                Ok(conn.query_row(
                    "SELECT COUNT(*) FROM chat_message WHERE session_id = ?",
                    rusqlite::params![session_id],
                    |row| row.get(0),
                )?)
            })
            .await
            .unwrap();
        assert_eq!(remaining, 2, "DB rows should be untouched by index delete");

        drop(db_dir);
    }

    #[tokio::test]
    async fn test_delete_chat_session_index_no_messages() {
        let (db, _db_dir) = test_db().await;
        let idx_dir = TempDir::new().unwrap();
        let index_dir_path = idx_dir.path().to_str().unwrap().to_string();

        // Session exists but has no messages — should be a no-op
        let session_id = "test-session-no-messages";
        get_or_create_session(&db, session_id, &[], SessionMode::Chat)
            .await
            .unwrap();

        // Should succeed without indexing anything
        let result = delete_chat_session_index(&db, &index_dir_path, session_id).await;
        assert!(result.is_ok(), "expected Ok for empty session, got {result:?}");
    }

    #[tokio::test]
    async fn test_delete_chat_session_index_nonexistent_session() {
        let (db, _db_dir) = test_db().await;
        let idx_dir = TempDir::new().unwrap();
        let index_dir_path = idx_dir.path().to_str().unwrap().to_string();

        // Session doesn't exist — no messages, so this is a no-op
        let result = delete_chat_session_index(&db, &index_dir_path, "nonexistent-session").await;
        assert!(
            result.is_ok(),
            "expected Ok for nonexistent session, got {result:?}"
        );
    }
}
