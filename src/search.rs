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

fn search_similar_notes(query: &str) -> Result<()> {
    let db = Connection::open_in_memory()?;

    let embeddings_model = TextEmbedding::try_new(
        InitOptions::new(EmbeddingModel::BGESmallENV15).with_show_download_progress(true),
    )
    .unwrap();
    let query_vector = embeddings_model.embed(vec![query], None).unwrap();
    let query = query_vector[0].clone();
    let result: Vec<(i64, f64)> = db
        .prepare(
            r"
          SELECT
            rowid,
            distance
          FROM vec_items
          JOIN note_meta on rowid=note_meta.vec_id
          WHERE embedding MATCH ? AND k = 3
          ORDER BY distance
          LIMIT 3
        ",
        )?
        .query_map([query.as_bytes()], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<Vec<_>, _>>()?;
    println!("{:?}", result);
    Ok(())
}
