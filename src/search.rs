use serde::Serialize;
use tantivy::collector::TopDocs;
use tantivy::query::QueryParser;
use tantivy::schema::*;
use tantivy::{Index, ReloadPolicy};
use itertools::Itertools;
use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};
use rusqlite::{Connection, Result};
use zerocopy::AsBytes;

use super::schema::note_schema;

#[derive(Serialize)]
pub enum SearchHitType {
    #[serde(rename = "full_text")]
    FullText,
    #[serde(rename = "similarity")]
    Similarity,
}

#[derive(Serialize)]
pub struct SearchHit {
    r#type: SearchHitType,
    score: f32,
    title: String,
    id: String,
    file_name: String,
    tags: Option<String>,
}

fn fulltext_search(index_path: &str, query: &str) -> Vec<SearchHit> {
    let schema = note_schema();
    let index_path = tantivy::directory::MmapDirectory::open(index_path).expect("Index not found");
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
        .map(|(score, doc_addr)| {
            let doc = searcher
                .doc::<TantivyDocument>(*doc_addr)
                .expect("Doc not found")
                .to_named_doc(&schema)
                .0;

            // Parse the document into a more reasonable format
            // Wow this is gross
            let title_val = doc.get("title").unwrap()[0]
                .as_ref()
                .as_str()
                .unwrap()
                .to_string();
            let id_val = doc.get("id").unwrap()[0]
                .as_ref()
                .as_str()
                .unwrap()
                .to_string();
            let tags_val = doc.get("tags").map(|v| {
                v[0]
                    .as_ref()
                    .as_str()
                    .unwrap()
                    .to_string()
            });

            let file_name_val = doc.get("file_name").unwrap()[0]
                .as_ref()
                .as_str()
                .unwrap()
                .to_string();
            SearchHit {
                r#type: SearchHitType::FullText,
                score: *score,
                id: id_val,
                title: title_val,
                tags: tags_val,
                file_name: file_name_val,
            }
        })
        .collect()
}

/// Returns the note ID and similarity distance for the query. Results
/// are ordered by ascending distance because sqlite-vec only supports
/// ascending distance.
pub fn search_similar_notes(db: &Connection, query: &str) -> Result<Vec<SearchHit>> {
    let embeddings_model = TextEmbedding::try_new(
        InitOptions::new(EmbeddingModel::BGESmallENV15).with_show_download_progress(true),
    )
    .unwrap();
    let query_vector = embeddings_model.embed(vec![query], None).unwrap();
    let q = query_vector[0].clone();
    let result: Vec<SearchHit> = db
        .prepare(
            r"
          SELECT
            note_meta.id,
            note_meta.file_name,
            note_meta.title,
            note_meta.tags,
            distance
          FROM vec_items
          JOIN note_meta on note_meta_id=note_meta.id
          WHERE embedding MATCH ? AND k = 10
          ORDER BY distance
          LIMIT 10
        ",
        )?
        .query_map([q.as_bytes()], |r| {
            Ok(SearchHit {
                r#type: SearchHitType::Similarity,
                id: r.get(0)?,
                file_name: r.get(1)?,
                title: r.get(2)?,
                tags: r.get(3)?,
                score: r.get(4)?
            })
        })?
        .collect::<Result<Vec<SearchHit>, _>>()?;
    Ok(result)
}

// Performs a full-text search of all notes for the given query. If
// `include_similarity`, also includes vector search results appended
// to the end of the list of results. This way, if there is a keyword
// search miss, there may be semantically similar results.
pub fn search_notes(
    index_path: &str,
    db: &Connection,
    query: &str,
    include_similarity: bool,
) -> Vec<SearchHit> {
    if include_similarity {
        let mut result = fulltext_search(index_path, query);
        let mut vec_search_result = search_similar_notes(db, query).unwrap_or_default();

        // Combine the results, dedupe, then sort by score
        result.append(&mut vec_search_result);
        result.into_iter().unique_by(|i| i.id.clone()).collect()
    } else {
        fulltext_search(index_path, query)
    }
}
