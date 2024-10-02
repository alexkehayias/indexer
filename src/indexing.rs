use super::schema::note_schema;
use super::source::notes;
use orgize::ParseConfig;
use std::fs;
use std::path::PathBuf;
use tantivy::schema::*;
use tantivy::{doc, Index, IndexWriter};

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
    let config = ParseConfig {
        ..Default::default()
    };
    let p = config.parse(&content);

    let props = p.document().properties().expect(
        "Missing property
drawer",
    );
    let id_value = props.get("ID").expect("Missing org-id").to_string();
    let file_name_value = path.file_name().unwrap().to_string_lossy().into_owned();
    let title_value = p.title().expect("No title found");
    let body_value = p.document().raw();
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


use rusqlite::{ffi::sqlite3_auto_extension, Connection, Result};
use sqlite_vec::sqlite3_vec_init;
use zerocopy::AsBytes;
use fastembed::{TextEmbedding, InitOptions, EmbeddingModel};

/// Index each note's embeddings
/// Target model has N tokens or roughly a M sized context window
///
/// Algorithm:
/// 1. If the note text is less than N chars, embed the whole thing
/// 2. Otherwise, split the text into N chars using semantic chunks
/// 3. Calculate the embeddings for each chunk
/// 4. Store the embedding vector in the sqlite database
/// 5. Include metadata about the source of the chunk for further
/// retrieval and to avoid duplicating rows
pub fn index_notes_vector_all(index_path: &str, notes_path: &str) -> Result<()> {
    unsafe {
        sqlite3_auto_extension(Some(std::mem::transmute(sqlite3_vec_init as *const ())));
    }

    // TODO: Change this to persistent file
    let db = Connection::open_in_memory()?;
    let v: Vec<f32> = vec![0.1, 0.2, 0.3];

    // TODO: generate these from inputted text
    let items: Vec<(usize, Vec<f32>)> = vec![
        (1, vec![0.1, 0.1, 0.1, 0.1]),
        (2, vec![0.2, 0.2, 0.2, 0.2]),
        (3, vec![0.3, 0.3, 0.3, 0.3]),
        (4, vec![0.4, 0.4, 0.4, 0.4]),
        (5, vec![0.5, 0.5, 0.5, 0.5]),
    ];
    println!("{x}");

    // TODO: Move this to initialization
    db.execute(
        "CREATE VIRTUAL TABLE vec_items USING vec0(embedding float[4])",
        [],
    )?;
    let mut stmt = db.prepare("INSERT INTO vec_items(rowid, embedding) VALUES (?, ?)")?;
    for item in items {
        stmt.execute(rusqlite::params![item.0, item.1.as_bytes()])?;
    }

    // TODO: Move this to a similarity search function
    let query: Vec<f32> = vec![0.3, 0.3, 0.3, 0.3];
    let result: Vec<(i64, f64)> = db
        .prepare(
            r"
          SELECT
            rowid,
            distance
          FROM vec_items
          WHERE embedding MATCH ?1
          ORDER BY distance
          LIMIT 3
        ",
        )?
        .query_map([query.as_bytes()], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    println!("{:?}", result);

    let model = TextEmbedding::try_new(
        InitOptions::new(EmbeddingModel::AllMiniLML6V2).with_show_download_progress(true),
    ).unwrap();

    let documents = vec![
        "passage: Hello, World!",
        "query: Hello, World!",
        "passage: This is an example passage.",
        // You can leave out the prefix but it's recommended
        "fastembed-rs is licensed under Apache  2.0"
    ];

    // Generate embeddings with the default batch size, 256
    let embeddings = model.embed(documents, None).unwrap();

    println!("Embeddings length: {}", embeddings.len());
    println!("Embedding dimension: {}", embeddings[0].len());

    Ok(())
}
