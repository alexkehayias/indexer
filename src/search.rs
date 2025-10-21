use tantivy::collector::TopDocs;
use tantivy::query::QueryParser;
use tantivy::schema::*;
use tantivy::{Index, ReloadPolicy};

use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};
use rusqlite::{Connection, Result};
use zerocopy::AsBytes;

use super::schema::note_schema;

// Fulltext search of all notes
pub fn search_notes(query: &str) -> Vec<NamedFieldDocument> {
    let schema = note_schema();
    let index_path = tantivy::directory::MmapDirectory::open("./.index").expect("Index not found");
    let idx =
        Index::open_or_create(index_path, schema.clone()).expect("Unable to open or create index");
    let title = schema.get_field("title").unwrap();
    let body = schema.get_field("body").unwrap();

    let reader = idx
        .reader_builder()
        .reload_policy(ReloadPolicy::OnCommitWithDelay)
        .try_into()
        .expect("Reader failed to load");

    let searcher = reader.searcher();
    let query_parser = QueryParser::for_index(&idx, vec![title, body]);

    let query = query_parser
        .parse_query(query)
        .expect("Failed to parse query");

    searcher
        .search(&query, &TopDocs::with_limit(10))
        .expect("Search failed")
        .iter()
        .map(|(_score, doc_addr)| {
            searcher
                .doc::<TantivyDocument>(*doc_addr)
                .expect("Doc not found")
                .to_named_doc(&schema)
        })
        .collect()
}

/// Returns the note ID and similarity distance for the query. Results
/// are ordered by ascending distance because sqlite-vec only supports
/// ascending distance.
pub fn search_similar_notes(db: &Connection, query: &str) -> Result<Vec<(String, f64)>> {
    let embeddings_model = TextEmbedding::try_new(
        InitOptions::new(EmbeddingModel::BGESmallENV15).with_show_download_progress(true),
    )
    .unwrap();
    let query_vector = embeddings_model.embed(vec![query], None).unwrap();
    let q = query_vector[0].clone();
    let result: Vec<(String, f64)> = db
        .prepare(
            r"
          SELECT
            note_meta.id,
            distance
          FROM vec_items
          JOIN note_meta on note_meta_id=note_meta.id
          WHERE embedding MATCH ? AND k = 10
          ORDER BY distance
          LIMIT 10
        ",
        )?
        .query_map([q.as_bytes()], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(result)
}
