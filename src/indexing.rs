use regex::Regex;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;

use crate::export::MarkdownExport;
use crate::fts::schema::note_schema;
use crate::source::{note_filter, notes};
use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};
use orgize::ParseConfig;
use orgize::rowan::ast::AstNode;
use tokio_rusqlite::{Connection, Result};
use std::fs;
use std::hash::DefaultHasher;
use tantivy::schema::*;
use tantivy::{Index, IndexWriter, doc};
use text_splitter::{ChunkConfig, TextSplitter};
use tiktoken_rs::{CoreBPE, cl100k_base};
use zerocopy::IntoBytes;

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

/// Parse the content into a `Note`
fn parse_note(content: &str) -> Note {
    let config = ParseConfig {
        todo_keywords: (
            vec![
                "TODO".to_string(),
                "NEXT".to_string(),
                "WAITING".to_string(),
            ],
            vec![
                "DONE".to_string(),
                "CANCELED".to_string(),
                "SOMEDAY".to_string(),
            ],
        ),
        ..Default::default()
    };
    let p = config.parse(content);
    let d = p.document();

    let props = d.properties().expect("Missing property drawer");
    let note_id = props.get("ID").expect("Missing org-id").to_string();
    let note_title = p.title().expect("No title found");
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

    // TODO: Remove the title and the tasks when indexing the body so it's
    // not duplicated
    // let title_text_range = org_doc.first_headline()?.text_range();
    // p.replace_range(title_text_range, "");
    let note_body = d.raw();

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
        let tags = if tag_string.is_empty() {
            None
        } else {
            Some(tag_string.clone())
        };
        let title = i.title_raw().trim().to_string();

        // Tasks sometimes don't have an org-id. These tasks are ignored.
        let mut hasher = DefaultHasher::new();
        title.hash(&mut hasher);
        let default_id = hasher.finish().to_string();

        // Note: Can't use a question mark operator as that
        // will cause an early return rather than handling the
        // case where properties don't exist
        let task_properties = i.properties();
        let id = if let Some(task_props) = task_properties {
            // Properties might exist but the ID might be missing
            task_props
                .get("ID")
                .map(|j| j.to_string())
                .unwrap_or(default_id)
        } else {
            default_id
        };

        // Extract note body into markdown format This is
        // useful since LLMs are typically tune for markdown
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

    Note {
        id: note_id,
        title: note_title,
        category: note_category,
        body: note_body,
        tags: note_tags,
        tasks,
        meetings,
        headings,
    }
}

enum DocType {
    Note,
    Task,
    Meeting,
    Heading,
}

impl DocType {
    fn to_str(&self) -> &'static str {
        match self {
            DocType::Note => "note",
            DocType::Task => "task",
            DocType::Meeting => "meeting",
            DocType::Heading => "heading",
        }
    }
}

// Deletes and then writes the document to the index
fn index_note_full_text(
    index_writer: &mut IndexWriter,
    schema: &Schema,
    file_name_value: &str,
    note: &Note,
) -> tantivy::Result<()> {
    // Delete the document first to get upsert behavior
    let id = schema.get_field("id")?;
    let term_id = Term::from_field_text(id, &note.id);
    index_writer.delete_term(term_id);

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

/// Index the embeddings for the note
/// Target model has N tokens or roughly a M sized context window
///
/// Algorithm:
/// 1. If the note text is less than N tokens, embed the whole thing
/// 2. Otherwise, split the text into N tokens
/// 3. Calculate the embeddings for each chunk
/// 4. Store the embedding vector in the sqlite database
/// 5. Include metadata about the source of the chunk for further
///    retrieval and to avoid duplicating rows
fn index_note_vector(
    db: &mut rusqlite::Connection,
    embeddings_model: &TextEmbedding,
    splitter: &TextSplitter<CoreBPE>,
    note: &Note,
) -> Result<()> {
    // Generate embeddings and store it in the DB
    let mut embedding_stmt =
        db.prepare("INSERT OR REPLACE INTO vec_items(note_meta_id, embedding) VALUES (?, ?)")?;
    let mut embedding_update_stmt =
        db.prepare("UPDATE vec_items set embedding = ? WHERE note_meta_id = ?")?;

    let embeddings: Vec<Vec<Vec<f32>>> = splitter
        .chunks(&note.body)
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
            .execute(tokio_rusqlite::params![note.id, embedding.as_bytes()])
            .unwrap_or_else(|_| {
                embedding_update_stmt
                    .execute(tokio_rusqlite::params![embedding.as_bytes(), note.id])
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
    let mut note_meta_stmt = db.prepare(
        "REPLACE INTO note_meta(id, type, category, file_name, title, tags, body) VALUES (?, ?, ?, ?, ?, ?, ?)",
    )?;

    // Update the note meta table
    note_meta_stmt
        // TODO: Don't hardcode the note path, save the file name instead
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
use std::sync::Arc;

pub async fn index_all(
    db: &Connection,
    index_dir_path: &str,
    notes_dir_path: &str,
    index_full_text: bool,
    index_vector: bool,
    paths: Option<Vec<PathBuf>>,
) -> Result<()> {
    let embeddings_model = Arc::new(
        TextEmbedding::try_new(
            InitOptions::new(EmbeddingModel::BGESmallENV15).with_show_download_progress(true),
        )
        .unwrap(),
    );
    let tokenizer = cl100k_base().unwrap();
    let max_tokens = 1280;
    let splitter = Arc::new(TextSplitter::new(ChunkConfig::new(max_tokens).with_sizer(tokenizer)));

    let note_paths: Vec<PathBuf> = if let Some(path_bufs) = paths {
        note_filter(notes_dir_path, path_bufs)
    } else {
        notes(notes_dir_path)
    };

    let index_path =
        tantivy::directory::MmapDirectory::open(index_dir_path).expect("Index not found");
    let schema = note_schema();
    let idx =
        Index::open_or_create(index_path, schema.clone()).expect("Unable to open or create index");
    let mut index_writer: IndexWriter = idx
        .writer(50_000_000)
        .expect("Index writer failed to initialize");

    for p in note_paths.iter() {
        tracing::debug!("Indexing note: {:?}", p);

        // Arc the shared items so that it can be safely passed to the
        // async closure.
        let file_name = Arc::new(p.file_name().unwrap().to_str().unwrap().to_owned());
        let content = fs::read_to_string(p).unwrap_or_else(|err| panic!("Error {} file: {:?}", err, p));
        let note = Arc::new(parse_note(&content));

        let embeddings_model = Arc::clone(&embeddings_model);
        let splitter = Arc::clone(&splitter);
        let note_inner = Arc::clone(&note);
        let file_name_inner = Arc::clone(&file_name);

        db.call(move |conn| {
            index_note_meta(conn, &file_name_inner, &note_inner).expect("Upserting note meta failed");

            if index_vector {
                index_note_vector(conn, &embeddings_model, &splitter, &note_inner)
                    .expect("Upserting note vector failed");
            }
            Ok(())
        }).await.expect("DB work failed");

        if index_full_text {
            index_note_full_text(&mut index_writer, &schema, &file_name, &note)
                .expect("Updating full text search failed");
        }
    }

    index_writer
        .commit()
        .expect("Full text search index failed to commit");

    Ok(())
}
