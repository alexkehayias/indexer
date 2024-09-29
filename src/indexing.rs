use std::fs;
use std::path::PathBuf;
use tantivy::schema::*;
use tantivy::{doc, Index, IndexWriter};
use orgize::ParseConfig;
use super::source::notes;
use super::schema::note_schema;

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

pub fn index_notes_all() {
    let notes_path = "./notes";
    let index_path = "./.index";
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
